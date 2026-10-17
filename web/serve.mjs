// Local development server: node web/serve.mjs [port]
import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
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
      && value.wheelScrollLines >= 0) return value;
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
        await scan(absolutePath);
      } else if (entry.isFile()) {
        files.push({ path, kind: 'file', size: info.size, ...metadata(info) });
      }
    }));
  }

  await scan(sims3Root);
  const byPath = (a, b) => a.path.toLowerCase() < b.path.toLowerCase() ? -1 : a.path.toLowerCase() > b.path.toLowerCase() ? 1 : 0;
  files.sort(byPath);
  directories.sort(byPath);
  sims3FilesByPath = new Map(files.map(({ path }) => [path.toLowerCase(), join(sims3Root, ...path.split('/'))]));
  if (!sims3FilesByPath.has('game/bin/ts3w.exe')) {
    throw new Error(`${sims3Root}\\Game\\Bin\\TS3W.exe was not found`);
  }
  return { root: sims3Root, entryPath: 'Game\\Bin\\TS3W.exe', files, directories };
}

function getSims3Manifest() {
  if (!sims3ManifestPromise) {
    sims3ManifestPromise = scanSims3Install().catch((error) => {
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
