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
    ['SharedArrayBuffer', typeof SharedArrayBuffer !== 'undefined', 'needed for threads (not used yet)'],
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

async function run(bundleBytes, label) {
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
    worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
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
    else if (m.type === 'out') print(m.text, m.stream === 'stderr' ? 'err' : '');
    else if (m.type === 'window') {
      canvas.hidden = false;
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
      clearLoading();
      const names = { 0: 'stopped', 1: 'exited', 2: 'stopped at an unimplemented import', 3: 'crashed' };
      const detail = m.status === 1 ? `exit code ${m.exitCode}` : m.status === 2 ? '' : m.message;
      stat.textContent = `${label}: ${names[m.status]}${detail ? ' (' + detail + ')' : ''} · ${m.instructions.toLocaleString()} instructions · ${m.mips.toFixed(1)} M instr/s${m.frames ? ' · ' + m.frames + ' frames' : ''}`;
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
  showLoading('Starting guest program…');
  const message = { type: 'start', wasm, bundle: bundleBytes, testMode };
  const transfer = [wasm];
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
  showLoading(`Loading demo bundle ${name}.awb…`);
  try {
    const res = await fetch(new URL(`./demo/${name}.awb`, import.meta.url));
    if (!res.ok) {
      clearLoading();
      stat.textContent = `demo ${name}.awb not found - build it with: anywasm pack`;
      return;
    }
    await run(new Uint8Array(await res.arrayBuffer()), `${name}.exe`);
  } catch (e) {
    failLoading(`${name}.awb`, `could not load bundle (${e.message})`);
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

const auto = new URLSearchParams(location.search).get('run');
if (auto) demo(auto);
