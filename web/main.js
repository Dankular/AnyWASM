import { PackageManager } from './package-manager.js';

const $ = (id) => document.getElementById(id);
const term = $('term');
const stat = $('stat');
const diagnostic = $('diagnostic');
const loading = $('loading');
const loadMessage = $('load-message');
let worker = null;

function capabilities() {
  const caps = [
    ['WebAssembly', typeof WebAssembly === 'object', 'required'],
    ['Module workers', typeof Worker !== 'undefined', 'required'],
    ['WebGPU', !!navigator.gpu, 'needed for graphics programs'],
    ['OffscreenCanvas transfer', typeof HTMLCanvasElement.prototype.transferControlToOffscreen === 'function', 'needed for worker rendering'],
    ['SharedArrayBuffer workers', typeof SharedArrayBuffer !== 'undefined' && globalThis.crossOriginIsolated === true, 'optional parallel guest CPU execution'],
  ];
  $('caps').innerHTML = caps
    .map(([n, ok, why]) => `<div class="cap"><b>${n}</b><span class="${ok ? 'ok' : 'bad'}">${ok ? 'available' : 'missing'}</span> <span class="dim">· ${why}</span></div>`)
    .join('');
  return Object.fromEntries(caps.map(([n, ok]) => [n, ok]));
}

function print(text, cls) {
  const span = document.createElement('span');
  if (cls) span.className = cls;
  span.textContent = text.replace(/\r\n/g, '\n');
  term.appendChild(span);
  term.scrollTop = term.scrollHeight;
}

function showLoading(message) {
  loadMessage.textContent = message;
  loading.hidden = false;
}

function clearLoading() {
  loading.hidden = true;
  loadMessage.textContent = '';
}

function failLoading(label, message) {
  clearLoading();
  stat.textContent = `${label}: ${message}`;
  stat.dataset.status = '3';
  stat.dataset.exit = '-1';
  $('stop').disabled = true;
}

async function run(bundleBytes, label, assetUrl = null, assetPackage = null) {
  const reuseWorker = !!worker && stat.dataset.status !== undefined;
  if (worker && !reuseWorker) worker.postMessage({ type: 'dispose' });
  term.textContent = '';
  stat.textContent = `Starting ${label}…`;
  diagnostic.hidden = true;
  diagnostic.replaceChildren();
  delete stat.dataset.status;
  $('stop').disabled = false;
  showLoading('Loading WebAssembly runtime…');
  let wasm;
  try {
    const response = await fetch(new URL('./anywasm_wasm.wasm', import.meta.url));
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    wasm = await response.arrayBuffer();
  } catch (e) {
    failLoading(label, `could not load WebAssembly runtime (${e.message})`);
    return;
  }
  if (!reuseWorker) {
    const workerUrl = new URL('./worker.js', import.meta.url);
    if (new URLSearchParams(location.search).has('sharedCpuWorkers')) {
      workerUrl.searchParams.set('sharedCpuWorkers', '');
    }
    worker = new Worker(workerUrl, { type: 'module' });
    window.__worker = worker; // exposed for automated tests
  }

  // A fresh canvas per run: a canvas can only be transferred to a worker once.
  const old = $('screen');
  const canvas = document.createElement('canvas');
  canvas.id = 'screen'; canvas.width = 640; canvas.height = 480; canvas.hidden = true; canvas.tabIndex = 0;
  old.replaceWith(canvas);
  const setInputSize = attachInput(canvas);
  const off = typeof canvas.transferControlToOffscreen === 'function'
    ? canvas.transferControlToOffscreen()
    : null;

  worker.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'ready') clearLoading();
    else if (m.type === 'load-progress') {
      if (m.trace) window.__workerDiag = m.trace;
      showLoading(m.message);
    }
    else if (m.type === 'trace-progress') window.__workerDiag = m.trace;
    else if (m.type === 'asset-progress') {
      const fetched = (m.fetchedBytes / (1024 * 1024)).toFixed(1);
      const total = (Number(m.packageLength) / (1024 * 1024)).toFixed(1);
      showLoading(`Streaming game data: ${fetched} MiB fetched · ${total} MiB package`);
    }
    else if (m.type === 'out') print(m.text, m.stream === 'stderr' ? 'err' : '');
    else if (m.type === 'window') {
      canvas.hidden = false;
      canvas.focus({ preventScroll: true });
      canvas.style.aspectRatio = `${m.width} / ${m.height}`;
      setInputSize(m.width, m.height);
      if (m.title) document.title = `${m.title} - AnyWASM`;
    } else if (m.type === 'gpuinfo') {
      window.__gpuInfo = m;
    } else if (m.type === 'gpu') {
      window.__gpuStats = m;
    } else if (m.type === 'pixels') {
      window.__pixels = m;
    } else if (m.type === 'texture-readback') {
      window.__textureReadback = m;
    } else if (m.type === 'status') {
      if (m.trace) window.__workerDiag = m.trace;
      if (m.jitStats) window.__jitStats = m.jitStats;
      clearLoading();
      const names = { 0: 'stopped', 1: 'exited', 2: 'stopped at an unimplemented import', 3: 'crashed' };
      const detail = m.status === 1 ? `exit code ${m.exitCode}` : m.status === 2 ? '' : m.message;
      const executionMode = m.executionMode === 'shared-cpu-workers'
        ? `${m.executionWorkerCount} shared CPU workers`
        : 'cooperative CPU';
      const executionReason = m.executionModeReason ? ` (${m.executionModeReason})` : '';
      window.__executionMode = {
        mode: m.executionMode || 'cooperative',
        workerCount: m.executionWorkerCount || 0,
        reason: m.executionModeReason || '',
      };
      stat.textContent = `${label}: ${names[m.status]}${detail ? ' (' + detail + ')' : ''} · ${m.instructions.toLocaleString()} instructions · ${m.mips.toFixed(1)} M instr/s · ${executionMode}${executionReason}${m.frames ? ' · ' + m.frames + ' frames' : ''}`;
      if (m.status === 2) {
        const title = document.createElement('strong');
        title.textContent = 'Stopped at an unimplemented import';
        const message = document.createElement('code');
        message.textContent = m.message || 'Import name unavailable.';
        const guidance = document.createElement('span');
        guidance.textContent = 'Run anywasm analyze <program.exe> to see all statically imported symbols.';
        diagnostic.replaceChildren(title, message, guidance);
        diagnostic.hidden = false;
      }
      stat.dataset.status = String(m.status);
      stat.dataset.exit = String(m.exitCode);
      $('stop').disabled = true;
    }
  };
  worker.onerror = (e) => {
    worker = null;
    window.__worker = null;
    failLoading(label, `worker failed to start (${e.message || 'unknown error'})`);
  };
  const testMode = new URLSearchParams(location.search).has('test');
  const traceGtaEventvol = new URLSearchParams(location.search).has('traceGtaEventvol');
  const diagProgress = new URLSearchParams(location.search).has('diagProgress');
  const jitMinPrefixValue = new URLSearchParams(location.search).get('jitMinPrefix');
  showLoading('Starting guest program…');
  const message = { type: 'start', wasm, bundle: bundleBytes, assetUrl, assetPackage, testMode, traceGtaEventvol, diagProgress };
  if (jitMinPrefixValue !== null) message.jitMinPrefix = Number(jitMinPrefixValue);
  const transfer = [wasm, bundleBytes.buffer];
  if (off) {
    message.canvas = off;
    transfer.push(off);
  }
  worker.postMessage(message, transfer);
}

function attachInput(canvas) {
  let guestWidth = canvas.width;
  let guestHeight = canvas.height;
  const send = (kind, a = 0, b = 0, c = 0) => worker?.postMessage({ type: 'input', kind, a, b, c });
  const pos = (e) => {
    const r = canvas.getBoundingClientRect();
    const width = canvas.clientWidth || r.width;
    const height = canvas.clientHeight || r.height;
    return [
      Math.round((e.clientX - r.left - canvas.clientLeft) * guestWidth / width),
      Math.round((e.clientY - r.top - canvas.clientTop) * guestHeight / height),
    ];
  };
  canvas.addEventListener('keydown', (e) => { send(0, e.keyCode); if (e.key.length === 1) send(2, e.key.charCodeAt(0)); e.preventDefault(); });
  canvas.addEventListener('keyup', (e) => { send(1, e.keyCode); e.preventDefault(); });
  canvas.addEventListener('mousemove', (e) => { const [x, y] = pos(e); send(3, x, y); });
  canvas.addEventListener('mouseleave', () => send(10));
  canvas.addEventListener('mousedown', (e) => { canvas.focus(); const [x, y] = pos(e); send(4, e.button, x, y); });
  canvas.addEventListener('mouseup', (e) => { const [x, y] = pos(e); send(5, e.button, x, y); });
  canvas.addEventListener('wheel', (e) => { const [x, y] = pos(e); send(6, -Math.sign(e.deltaY) * 120, x, y); e.preventDefault(); }, { passive: false });
  canvas.addEventListener('contextmenu', (e) => e.preventDefault());
  canvas.addEventListener('focus', () => send(8, 1));
  canvas.addEventListener('blur', () => send(8, 0));
  return (width, height) => {
    if (Number.isFinite(width) && width > 0) guestWidth = Math.round(width);
    if (Number.isFinite(height) && height > 0) guestHeight = Math.round(height);
  };
}

async function demo(name) {
  const requestedExe = name === 'gta_sa' && new URLSearchParams(location.search).get('exe') === 'gta_sa.exe'
    ? 'gta_sa.exe'
    : null;
  const bundleName = requestedExe ? 'gta_sa_regular' : name;
  showLoading(`Loading demo bundle ${bundleName}.awb…`);
  try {
    const res = await fetch(new URL(`./demo/${bundleName}.awb`, import.meta.url));
    if (!res.ok) {
      clearLoading();
      stat.textContent = `demo ${bundleName}.awb not found - build it with: anywasm pack`;
      return;
    }
    const assetUrl = name === 'gta_sa' ? new URL('./demo/gta_sa.agp', import.meta.url).href : null;
    const entryName = requestedExe || (name === 'gta_sa' ? 'gta_sa_compact1.0.exe' : `${name}.exe`);
    await run(new Uint8Array(await res.arrayBuffer()), entryName, assetUrl);
  } catch (e) {
    failLoading(`${bundleName}.awb`, `could not load bundle (${e.message})`);
  }
}

capabilities();
$('file').addEventListener('change', async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  showLoading(`Reading bundle ${f.name}…`);
  try {
    await run(new Uint8Array(await f.arrayBuffer()), f.name);
  } catch (error) {
    failLoading(f.name, `could not read bundle (${error.message})`);
  }
});
$('demo-hello').addEventListener('click', () => demo('hello'));
$('demo-crt').addEventListener('click', () => demo('crt_test'));
$('demo-tri').addEventListener('click', () => demo('d3d11_triangle'));
$('demo-tex').addEventListener('click', () => demo('d3d11_textured'));
$('demo-gta').addEventListener('click', () => demo('gta_sa'));
$('stop').addEventListener('click', () => { worker?.postMessage({ type: 'stop' }); });

const packageManager = new PackageManager(new URL('./catalog.json', import.meta.url).href);
const packageList = $('package-list');
const packageStatus = $('package-status');

async function showPackages() {
  packageList.replaceChildren();
  packageStatus.textContent = 'Loading package catalogs…';
  const [entries, installed] = await Promise.all([packageManager.refreshCatalogs(), packageManager.listInstalled()]);
  const installedByKey = new Map(installed.map((record) => [record.key, record]));
  if (packageManager.catalogErrors.length) {
    const error = document.createElement('p');
    error.className = 'bad';
    error.textContent = packageManager.catalogErrors.join('\n');
    packageList.append(error);
  }
  for (const entry of entries) {
    const card = document.createElement('article');
    card.className = 'package-card';
    const info = document.createElement('div');
    const title = document.createElement('h3');
    title.textContent = `${entry.name} · ${entry.version}`;
    const summary = document.createElement('p');
    summary.textContent = entry.summary || entry.id;
    info.append(title, summary);
    const actions = document.createElement('div');
    actions.className = 'package-actions';
    const record = installedByKey.get(`${entry.id}@${entry.version}`);
    if (record) {
      const launch = document.createElement('button');
      launch.type = 'button';
      launch.textContent = 'Launch';
      launch.addEventListener('click', () => {
        packageStatus.textContent = `Starting ${record.name}…`;
        void run(new Uint8Array(record.bundle), `${record.name} ${record.version}`, record.assetPackage?.url || null, record.assetPackage)
          .catch((error) => { packageStatus.textContent = `Launch failed: ${error.message || error}`; });
      });
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.textContent = 'Remove';
      remove.addEventListener('click', async () => {
        remove.disabled = true;
        try {
          await packageManager.remove(record.id, record.version);
          packageStatus.textContent = `Removed ${record.key}.`;
          await showPackages();
        } catch (error) {
          packageStatus.textContent = error.message || String(error);
          remove.disabled = false;
        }
      });
      actions.append(launch, remove);
    } else {
      const install = document.createElement('button');
      install.type = 'button';
      install.textContent = 'Install';
      install.addEventListener('click', async () => {
        install.disabled = true;
        packageStatus.textContent = `Resolving and downloading ${entry.id}@${entry.version}…`;
        try {
          await packageManager.install(entry.id, entry.version);
          packageStatus.textContent = `Installed ${entry.id}@${entry.version}.`;
          await showPackages();
        } catch (error) {
          packageStatus.textContent = `Install failed: ${error.message || error}`;
          install.disabled = false;
        }
      });
      actions.append(install);
    }
    card.append(info, actions);
    packageList.append(card);
  }
  if (!entries.length && !packageManager.catalogErrors.length) {
    const empty = document.createElement('p');
    empty.className = 'dim';
    empty.textContent = 'No packages are listed. Add a catalog URL to browse packages.';
    packageList.append(empty);
  }
  packageStatus.textContent = packageManager.catalogErrors.length
    ? `${entries.length} package version(s) loaded; some catalogs could not be read.`
    : `${entries.length} package version(s) available · ${installed.length} installed.`;
}

$('catalog-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const button = $('add-catalog');
  button.disabled = true;
  try {
    const catalog = await packageManager.addCatalog($('catalog-url').value);
    $('catalog-url').value = '';
    packageStatus.textContent = `Added ${catalog.name}.`;
    await showPackages();
  } catch (error) {
    packageStatus.textContent = `Could not add catalog: ${error.message || error}`;
  } finally {
    button.disabled = false;
  }
});
$('refresh-catalogs').addEventListener('click', () => { void showPackages(); });
window.addEventListener('pagehide', () => packageManager.close(), { once: true });
void showPackages().catch((error) => { packageStatus.textContent = `Package library unavailable: ${error.message || error}`; });

const auto = new URLSearchParams(location.search).get('run');
if (auto) demo(auto);
