// Local development server: node web/serve.mjs [port]
import { createServer } from 'node:http';
import { createReadStream, watch } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { readdir, stat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { extname, join, normalize, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(
  process.env.ANYWASM_WEB_ROOT || fileURLToPath(new URL('.', import.meta.url)),
);
const sims3Root = resolve(process.env.ANYWASM_SIMS3_ROOT || 'D:\\The Sims 3');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.awb': 'application/octet-stream', '.css': 'text/css' };
const port = Number(process.argv[2] || 8080);
let sims3ManifestPromise = null;
let sims3FilesByPath = new Map();
let sims3EntriesByPath = new Map();
let sims3ChangeSequence = 0;
let sims3ChangeLog = [];
let sims3Watcher = null;
let sims3WatchError = '';
let sims3InventoryReady = false;
let sims3ManifestChangeSequence = 0;
const sims3ChangeFeedId = randomUUID();
let pendingSims3Paths = new Map();
let pendingSims3FullReconcile = false;
let pendingSims3Flush = null;
let sims3ReconcileChain = Promise.resolve();
let sims3LastFullReconcileAt = 0;

const FILE_NOTIFY_CHANGE_FILE_NAME = 0x0001;
const FILE_NOTIFY_CHANGE_DIR_NAME = 0x0002;
const FILE_NOTIFY_CHANGE_ATTRIBUTES = 0x0004;
const FILE_NOTIFY_CHANGE_SIZE = 0x0008;
const FILE_NOTIFY_CHANGE_LAST_WRITE = 0x0010;
const FILE_NOTIFY_CHANGE_LAST_ACCESS = 0x0020;
const FILE_NOTIFY_CHANGE_CREATION = 0x0040;
const MAX_SIM_CHANGE_LOG = 8192;
const MAX_SIM_CHANGES_PER_RESPONSE = 1024;
const PARTIAL_SIM_CHANGE_FILTERS = process.platform === 'win32'
  ? [FILE_NOTIFY_CHANGE_ATTRIBUTES]
  : [];

function readHostSystemParameters() {
  if (process.platform !== 'win32') {
    return { available: false, reason: 'host is not Windows' };
  }
  const script = resolve(root, '..', 'scripts', 'query_windows_cursor_shadow.ps1');
  const query = spawnSync('powershell.exe', [
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script,
  ], { encoding: 'utf8', windowsHide: true, timeout: 10000, maxBuffer: 64 * 1024 });
  if (query.error || query.status !== 0) {
    return {
      available: false,
      reason: (query.error?.message || query.stderr || `PowerShell exited with ${query.status}`).trim(),
    };
  }
  try {
    const value = JSON.parse(query.stdout.trim());
    if (value.available === true
      && typeof value.cursorShadow === 'boolean'
      && typeof value.mouseSonar === 'boolean'
      && Number.isInteger(value.mouseTrails)
      && value.mouseTrails >= 0
      && Number.isInteger(value.wheelScrollLines)
      && value.wheelScrollLines >= 0) {
      const stickyKeysAvailable = value.stickyKeysAvailable === true
        && Number.isInteger(value.stickyKeysFlags)
        && value.stickyKeysFlags >= 0
        && value.stickyKeysFlags <= 0xffff_ffff;
      return {
        ...value,
        stickyKeysAvailable,
        stickyKeysFlags: stickyKeysAvailable ? value.stickyKeysFlags : 0,
      };
    }
    return { available: false, reason: 'Windows returned invalid system parameter values' };
  } catch (error) {
    return { available: false, reason: `could not parse Windows cursor-shadow setting: ${error.message}` };
  }
}

const hostSystemParameters = readHostSystemParameters();

function readHostSims3Registry() {
  if (process.platform !== 'win32') {
    return { available: false, reason: 'host is not Windows' };
  }
  const script = resolve(root, '..', 'scripts', 'query_sims3_registry.ps1');
  const query = spawnSync('powershell.exe', [
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script, sims3Root,
  ], { encoding: 'utf8', windowsHide: true, timeout: 30000, maxBuffer: 16 * 1024 * 1024 });
  if (query.error || query.status !== 0) {
    return {
      available: false,
      reason: (query.error?.message || query.stderr || `PowerShell exited with ${query.status}`).trim(),
    };
  }
  try {
    const value = JSON.parse(query.stdout.trim());
    if (value.available === true
      && typeof value.installPath === 'string'
      && value.installPath.replace(/[\\/]+$/, '').toLowerCase() === sims3Root.replace(/[\\/]+$/, '').toLowerCase()
      && Array.isArray(value.keys)) return value;
    return { available: false, reason: value.reason || 'Windows returned an invalid Sims registry snapshot' };
  } catch (error) {
    return { available: false, reason: `could not parse Sims registry snapshot: ${error.message}` };
  }
}

const hostSims3Registry = readHostSims3Registry();

function json(res, status, value) {
  const body = Buffer.from(JSON.stringify(value));
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'content-length': body.length, 'cache-control': 'no-store' });
  res.end(body);
}

async function scanSims3Install() {
  const files = [];
  const directories = [];
  const entriesByPath = new Map();
  const metadata = (info) => ({
    lastAccessTime: Number.isFinite(info.atimeMs) ? Math.floor(info.atimeMs / 1000) : null,
    lastWriteTime: Number.isFinite(info.mtimeMs) ? Math.floor(info.mtimeMs / 1000) : null,
    creationTime: Number.isFinite(info.birthtimeMs) ? Math.floor(info.birthtimeMs / 1000) : null,
    mode: info.mode,
    nlink: info.nlink,
  });
  async function scan(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    await Promise.all(entries.map(async (entry) => {
      const absolutePath = join(directory, entry.name);
      const info = await stat(absolutePath);
      const path = relative(sims3Root, absolutePath).split(sep).join('/');
      if (entry.isDirectory()) {
        directories.push({ path, kind: 'directory', size: 0, ...metadata(info) });
        entriesByPath.set(path.toLowerCase(), {
          path,
          kind: 'directory',
          size: 0,
          atimeMs: info.atimeMs,
          mtimeMs: info.mtimeMs,
          birthtimeMs: info.birthtimeMs,
          mode: info.mode,
        });
        await scan(absolutePath);
      } else if (entry.isFile()) {
        files.push({ path, kind: 'file', size: info.size, ...metadata(info) });
        entriesByPath.set(path.toLowerCase(), {
          path,
          kind: 'file',
          size: info.size,
          atimeMs: info.atimeMs,
          mtimeMs: info.mtimeMs,
          birthtimeMs: info.birthtimeMs,
          mode: info.mode,
        });
      }
    }));
  }

  await scan(sims3Root);
  const byPath = (a, b) => a.path.toLowerCase() < b.path.toLowerCase() ? -1 : a.path.toLowerCase() > b.path.toLowerCase() ? 1 : 0;
  files.sort(byPath);
  directories.sort(byPath);
  sims3FilesByPath = new Map(files.map(({ path }) => [path.toLowerCase(), join(sims3Root, ...path.split('/'))]));
  sims3EntriesByPath = entriesByPath;
  if (!sims3FilesByPath.has('game/bin/ts3w.exe')) {
    throw new Error(`${sims3Root}\\Game\\Bin\\TS3W.exe was not found`);
  }
  return { root: sims3Root, entryPath: 'Game\\Bin\\TS3W.exe', files, directories };
}

function queueSims3Change(path, filter, kind, size = 0) {
  if (!filter) return;
  sims3ChangeSequence++;
  sims3ChangeLog.push({ sequence: sims3ChangeSequence, path, filter, kind, size });
  if (sims3ChangeLog.length > MAX_SIM_CHANGE_LOG) {
    sims3ChangeLog.splice(0, sims3ChangeLog.length - MAX_SIM_CHANGE_LOG);
  }
}

function metadataChangeFilter(previous, current) {
  let filter = 0;
  if (previous.kind !== current.kind) {
    filter |= current.kind === 'directory' ? FILE_NOTIFY_CHANGE_DIR_NAME : FILE_NOTIFY_CHANGE_FILE_NAME;
  }
  if (previous.atimeMs !== current.atimeMs) filter |= FILE_NOTIFY_CHANGE_LAST_ACCESS;
  if (previous.size !== current.size) filter |= FILE_NOTIFY_CHANGE_SIZE;
  if (previous.mtimeMs !== current.mtimeMs) filter |= FILE_NOTIFY_CHANGE_LAST_WRITE;
  if (previous.mode !== current.mode) filter |= FILE_NOTIFY_CHANGE_ATTRIBUTES;
  if (previous.birthtimeMs !== current.birthtimeMs) filter |= FILE_NOTIFY_CHANGE_CREATION;
  return filter;
}

function nameFilter(kind) {
  return kind === 'directory' ? FILE_NOTIFY_CHANGE_DIR_NAME : FILE_NOTIFY_CHANGE_FILE_NAME;
}

function removeSims3Entry(pathKey, { notify = true } = {}) {
  const previous = sims3EntriesByPath.get(pathKey);
  if (!previous) return;
  sims3EntriesByPath.delete(pathKey);
  sims3FilesByPath.delete(pathKey);
  if (notify) queueSims3Change(previous.path, nameFilter(previous.kind), 'removed', previous.size);
  if (previous.kind === 'directory') {
    const prefix = `${pathKey}/`;
    for (const childKey of [...sims3EntriesByPath.keys()]) {
      if (childKey.startsWith(prefix)) {
        const child = sims3EntriesByPath.get(childKey);
        sims3EntriesByPath.delete(childKey);
        sims3FilesByPath.delete(childKey);
        if (notify && child) queueSims3Change(child.path, nameFilter(child.kind), 'removed', child.size);
      }
    }
  }
}

function entryFromStat(pathKey, info) {
  const path = pathKey;
  return {
    path,
    kind: info.isDirectory() ? 'directory' : 'file',
    size: info.isFile() ? info.size : 0,
    atimeMs: info.atimeMs,
    mtimeMs: info.mtimeMs,
    birthtimeMs: info.birthtimeMs,
    mode: info.mode,
  };
}

async function addSims3Tree(pathKey, info = null, forceNameChange = false) {
  const absolutePath = join(sims3Root, ...pathKey.split('/'));
  const statInfo = info || await stat(absolutePath);
  const current = entryFromStat(pathKey, statInfo);
  const previous = sims3EntriesByPath.get(pathKey);
  if (previous && previous.kind !== current.kind) removeSims3Entry(pathKey);
  const existing = sims3EntriesByPath.get(pathKey);
  sims3EntriesByPath.set(pathKey, current);
  if (current.kind === 'file') sims3FilesByPath.set(pathKey, absolutePath);
  else sims3FilesByPath.delete(pathKey);
  if (!existing) {
    queueSims3Change(current.path, nameFilter(current.kind) | FILE_NOTIFY_CHANGE_CREATION, 'created', current.size);
  } else {
    const filter = metadataChangeFilter(existing, current)
      | (forceNameChange ? nameFilter(current.kind) : 0);
    queueSims3Change(current.path, filter, 'changed', current.size);
  }
  if (current.kind === 'directory' && !existing) {
    const children = await readdir(absolutePath, { withFileTypes: true });
    await Promise.all(children.map(async (child) => {
      if (!child.isFile() && !child.isDirectory()) return;
      const childKey = `${pathKey}/${child.name}`.toLowerCase();
      let childInfo;
      try {
        childInfo = await stat(join(absolutePath, child.name));
      } catch (error) {
        if (error?.code === 'ENOENT') return;
        throw error;
      }
      if (childInfo.isDirectory()) await addSims3Tree(childKey, childInfo);
      else if (childInfo.isFile()) await addSims3Tree(childKey, childInfo);
    }));
  }
}

async function reconcileSims3Path(pathKey, forceNameChange = false) {
  const absolutePath = join(sims3Root, ...pathKey.split('/'));
  let info;
  try {
    info = await stat(absolutePath);
  } catch (error) {
    if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') {
      removeSims3Entry(pathKey);
      return;
    }
    throw error;
  }
  if (!info.isFile() && !info.isDirectory()) {
    removeSims3Entry(pathKey);
    return;
  }
  await addSims3Tree(pathKey, info, forceNameChange);
}

async function reconcileSims3Install() {
  const previousEntries = new Map(sims3EntriesByPath);
  await scanSims3Install();
  const currentEntries = sims3EntriesByPath;

  for (const [pathKey, previous] of previousEntries) {
    if (!currentEntries.has(pathKey)) {
      queueSims3Change(previous.path, nameFilter(previous.kind), 'removed', previous.size);
    }
  }
  for (const [pathKey, current] of currentEntries) {
    const previous = previousEntries.get(pathKey);
    if (!previous) {
      queueSims3Change(
        current.path,
        nameFilter(current.kind) | FILE_NOTIFY_CHANGE_CREATION,
        'created',
        current.size,
      );
      continue;
    }
    if (previous.kind !== current.kind) {
      queueSims3Change(previous.path, nameFilter(previous.kind), 'removed', previous.size);
      queueSims3Change(
        current.path,
        nameFilter(current.kind) | FILE_NOTIFY_CHANGE_CREATION,
        'created',
        current.size,
      );
      continue;
    }
    const filter = metadataChangeFilter(previous, current)
      | (previous.path === current.path ? 0 : nameFilter(current.kind));
    if (filter) queueSims3Change(current.path, filter, 'changed', current.size);
  }
}

function scheduleSims3Reconcile(pathKey = null, eventType = 'change') {
  if (pathKey) pendingSims3Paths.set(pathKey, eventType);
  else pendingSims3FullReconcile = true;
  if (!pendingSims3Paths.size && !pendingSims3FullReconcile) return;
  if (!sims3InventoryReady || pendingSims3Flush) return;
  const fullReconcileDelay = pendingSims3FullReconcile
    ? Math.max(100, 1000 - (Date.now() - sims3LastFullReconcileAt))
    : 100;
  pendingSims3Flush = setTimeout(() => {
    pendingSims3Flush = null;
    const reconcileAll = pendingSims3FullReconcile;
    pendingSims3FullReconcile = false;
    const paths = [...pendingSims3Paths];
    pendingSims3Paths.clear();
    sims3ReconcileChain = sims3ReconcileChain
      .then(async () => {
        if (reconcileAll) {
          sims3LastFullReconcileAt = Date.now();
          await reconcileSims3Install();
        } else {
          await Promise.all(paths.map(([path, eventType]) =>
            reconcileSims3Path(path, eventType === 'rename')));
        }
        if (sims3Watcher) sims3WatchError = '';
      })
      .catch((error) => {
        sims3WatchError = error?.message || String(error);
        console.warn(`[sims3 watcher] ${sims3WatchError}`);
      })
      .finally(() => {
        if (pendingSims3FullReconcile) scheduleSims3Reconcile();
        else if (pendingSims3Paths.size) {
          const [path, eventType] = pendingSims3Paths.entries().next().value;
          scheduleSims3Reconcile(path, eventType);
        }
      });
  }, fullReconcileDelay);
}

function startSims3Watcher() {
  if (sims3Watcher || sims3WatchError) return;
  try {
    sims3Watcher = watch(sims3Root, { recursive: true }, (_eventType, filename) => {
      if (filename == null) {
        // Windows can report a recursive notification overflow without a filename. Keep the
        // watcher alive and rebuild the streamed inventory so known changes still reach guests.
        scheduleSims3Reconcile();
        return;
      }
      const pathKey = normalizeSims3FilePath(filename.toString());
      if (pathKey) scheduleSims3Reconcile(pathKey, _eventType);
    });
    sims3Watcher.on('error', (error) => {
      sims3WatchError = error?.message || String(error);
      sims3Watcher?.close();
      sims3Watcher = null;
      console.warn(`[sims3 watcher] ${sims3WatchError}`);
    });
  } catch (error) {
    sims3WatchError = error?.message || String(error);
    console.warn(`[sims3 watcher] ${sims3WatchError}`);
  }
}

function getSims3Manifest() {
  if (!sims3ManifestPromise) {
    startSims3Watcher();
    sims3ManifestChangeSequence = sims3ChangeSequence;
    sims3ManifestPromise = scanSims3Install()
      .then((manifest) => {
        sims3InventoryReady = true;
        if (pendingSims3FullReconcile) {
          scheduleSims3Reconcile();
        } else if (pendingSims3Paths.size) {
          const [path, eventType] = pendingSims3Paths.entries().next().value;
          scheduleSims3Reconcile(path, eventType);
        }
        return {
          ...manifest,
          changeSequence: sims3ManifestChangeSequence,
          changeFeedId: sims3ChangeFeedId,
        };
      })
      .catch((error) => {
        sims3ManifestPromise = null;
        throw error;
      });
  }
  return sims3ManifestPromise;
}

function normalizeSims3FilePath(value) {
  if (typeof value !== 'string' || !value || value.startsWith('/') || value.includes(':') || value.includes('\0')) return null;
  const components = value.replaceAll('\\', '/').split('/');
  if (components.some((part) => !part || part === '.' || part === '..')) return null;
  return components.map((part) => part.toLowerCase()).join('/');
}

async function serveFile(req, res, path) {
  const file = await stat(path);
  if (!file.isFile()) throw new Error('not a file');
  const headers = {
    'content-type': types[extname(path)] || 'application/octet-stream',
    'accept-ranges': 'bytes',
    'content-length': file.size,
    'cache-control': 'no-store',
    // Cross-origin isolation enables SharedArrayBuffer for the future threaded build.
    'cross-origin-opener-policy': 'same-origin',
    'cross-origin-embedder-policy': 'require-corp',
  };
  const range = req.headers.range;
  if (range) {
    const match = /^bytes=(\d*)-(\d*)$/.exec(range);
    if (!match || range.includes(',')) {
      res.writeHead(416, { ...headers, 'content-range': `bytes */${file.size}` }).end();
      return;
    }
    const size = file.size;
    const suffix = match[1] === '';
    const requestedStart = suffix ? Math.max(0, size - Number(match[2])) : Number(match[1]);
    const start = requestedStart;
    const end = suffix || match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1);
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || start >= size || end < start) {
      res.writeHead(416, { ...headers, 'content-range': `bytes */${size}` }).end();
      return;
    }
    res.writeHead(206, {
      ...headers,
      'content-length': end - start + 1,
      'content-range': `bytes ${start}-${end}/${size}`,
    });
    if (req.method === 'HEAD') res.end();
    else createReadStream(path, { start, end }).pipe(res);
    return;
  }
  res.writeHead(200, headers);
  if (req.method === 'HEAD') res.end();
  else createReadStream(path).pipe(res);
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/__local/sims3/changes') {
    const sinceText = url.searchParams.get('since') ?? '0';
    const since = Number(sinceText);
    if (!/^\d+$/.test(sinceText) || !Number.isSafeInteger(since) || since < 0) {
      json(res, 400, { error: 'since must be a nonnegative sequence number' });
      return;
    }
    try {
      await getSims3Manifest();
    } catch (error) {
      json(res, 200, {
        available: false,
        reason: error.message || String(error),
        feedId: sims3ChangeFeedId,
        sequence: sims3ChangeSequence,
        latestSequence: sims3ChangeSequence,
        changes: [],
      });
      return;
    }
    if (!sims3Watcher || sims3WatchError) {
      json(res, 200, {
        available: false,
        reason: sims3WatchError || 'recursive host-folder watching is unavailable',
        feedId: sims3ChangeFeedId,
        sequence: sims3ChangeSequence,
        latestSequence: sims3ChangeSequence,
        changes: [],
      });
      return;
    }
    const oldestSequence = sims3ChangeLog[0]?.sequence ?? sims3ChangeSequence + 1;
    const truncated = since < oldestSequence - 1;
    const changes = sims3ChangeLog
      .filter((change) => change.sequence > since)
      .slice(0, MAX_SIM_CHANGES_PER_RESPONSE);
    json(res, 200, {
      available: true,
      feedId: sims3ChangeFeedId,
      sequence: changes.at(-1)?.sequence ?? sims3ChangeSequence,
      latestSequence: sims3ChangeSequence,
      oldestSequence,
      truncated,
      partialFilters: PARTIAL_SIM_CHANGE_FILTERS,
      changes,
    });
    return;
  }
  if (url.pathname === '/__local/sims3/manifest') {
    try {
      json(res, 200, await getSims3Manifest());
    } catch (error) {
      json(res, 404, { error: error.message || String(error) });
    }
    return;
  }
  if (url.pathname === '/__local/system-parameters') {
    json(res, 200, hostSystemParameters);
    return;
  }
  if (url.pathname === '/__local/sims3/registry') {
    json(res, 200, hostSims3Registry);
    return;
  }
  if (url.pathname === '/__local/sims3/file') {
    const key = normalizeSims3FilePath(url.searchParams.get('path'));
    const path = key && sims3FilesByPath.get(key);
    if (!path) {
      json(res, 404, { error: 'file is not in the Sims 3 install manifest' });
      return;
    }
    try {
      await serveFile(req, res, path);
    } catch {
      json(res, 404, { error: 'file is no longer available in the Sims 3 install' });
    }
    return;
  }

  let p = decodeURIComponent(url.pathname).replace(/^[/\\]+/, '');
  if (!p || p.endsWith('/') || p.endsWith('\\')) p = join(p, 'index.html');
  p = normalize(p).replace(/^(\.\.[/\\])+/, '');
  try {
    await serveFile(req, res, join(root, p));
  } catch {
    res.writeHead(404).end('not found');
  }
});

server.listen(port, '127.0.0.1', () => console.log(`serving ${root} on http://localhost:${server.address().port} (127.0.0.1); Sims 3 source ${sims3Root}`));
