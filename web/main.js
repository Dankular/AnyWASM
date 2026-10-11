const $ = (id) => document.getElementById(id);
const term = $('term');
const stat = $('stat');
let worker = null;

function capabilities() {
  const caps = [
    ['WebAssembly', typeof WebAssembly === 'object', 'required'],
    ['Module workers', typeof Worker !== 'undefined', 'required'],
    ['WebGPU', 'gpu' in navigator, 'needed for graphics programs'],
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

async function run(bundleBytes, label) {
  if (worker) worker.terminate();
  term.textContent = '';
  stat.textContent = `Starting ${label}…`;
  delete stat.dataset.status;
  $('stop').disabled = false;
  const wasm = await (await fetch(new URL('./anywasm_wasm.wasm', import.meta.url))).arrayBuffer();
  worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  window.__worker = worker; // exposed for automated tests

  // A fresh canvas per run: a canvas can only be transferred to a worker once.
  const old = $('screen');
  const canvas = document.createElement('canvas');
  canvas.id = 'screen'; canvas.width = 640; canvas.height = 480; canvas.hidden = true; canvas.tabIndex = 0;
  old.replaceWith(canvas);
  attachInput(canvas);
  const off = canvas.transferControlToOffscreen();

  worker.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'out') print(m.text, m.stream === 'stderr' ? 'err' : '');
    else if (m.type === 'window') {
      canvas.hidden = false;
      canvas.style.aspectRatio = `${m.width} / ${m.height}`;
      if (m.title) document.title = `${m.title} - AnyWASM`;
    } else if (m.type === 'gpuinfo') {
      window.__gpuInfo = m;
    } else if (m.type === 'gpu') {
      window.__gpuStats = m;
    } else if (m.type === 'pixels') {
      window.__pixels = m;
    } else if (m.type === 'status') {
      const names = { 0: 'stopped', 1: 'exited', 2: 'stopped at an unimplemented import', 3: 'crashed' };
      const detail = m.status === 1 ? `exit code ${m.exitCode}` : m.message;
      stat.textContent = `${label}: ${names[m.status]}${detail ? ' (' + detail + ')' : ''} · ${m.instructions.toLocaleString()} instructions · ${m.mips.toFixed(1)} M instr/s${m.frames ? ' · ' + m.frames + ' frames' : ''}`;
      stat.dataset.status = String(m.status);
      stat.dataset.exit = String(m.exitCode);
      $('stop').disabled = true;
    }
  };
  const testMode = new URLSearchParams(location.search).has('test');
  worker.postMessage({ type: 'start', wasm, bundle: bundleBytes, canvas: off, testMode }, [wasm, off]);
}

function attachInput(canvas) {
  const send = (kind, a = 0, b = 0, c = 0) => worker?.postMessage({ type: 'input', kind, a, b, c });
  const pos = (e) => { const r = canvas.getBoundingClientRect(); return [Math.round((e.clientX - r.left) * canvas.width / r.width), Math.round((e.clientY - r.top) * canvas.height / r.height)]; };
  canvas.addEventListener('keydown', (e) => { send(0, e.keyCode); if (e.key.length === 1) send(2, e.key.charCodeAt(0)); e.preventDefault(); });
  canvas.addEventListener('keyup', (e) => { send(1, e.keyCode); e.preventDefault(); });
  canvas.addEventListener('mousemove', (e) => { const [x, y] = pos(e); send(3, x, y); });
  canvas.addEventListener('mousedown', (e) => { canvas.focus(); const [x, y] = pos(e); send(4, e.button, x, y); });
  canvas.addEventListener('mouseup', (e) => { const [x, y] = pos(e); send(5, e.button, x, y); });
  canvas.addEventListener('wheel', (e) => { const [x, y] = pos(e); send(6, -Math.sign(e.deltaY) * 120, x, y); e.preventDefault(); }, { passive: false });
  canvas.addEventListener('contextmenu', (e) => e.preventDefault());
  canvas.addEventListener('focus', () => send(8, 1));
  canvas.addEventListener('blur', () => send(8, 0));
}

async function demo(name) {
  const res = await fetch(new URL(`./demo/${name}.awb`, import.meta.url));
  if (!res.ok) { stat.textContent = `demo ${name}.awb not found - build it with: anywasm pack`; return; }
  await run(new Uint8Array(await res.arrayBuffer()), `${name}.exe`);
}

capabilities();
$('file').addEventListener('change', async (e) => {
  const f = e.target.files[0];
  if (f) await run(new Uint8Array(await f.arrayBuffer()), f.name);
});
$('demo-hello').addEventListener('click', () => demo('hello'));
$('demo-crt').addEventListener('click', () => demo('crt_test'));
$('demo-tri').addEventListener('click', () => demo('d3d11_triangle'));
$('stop').addEventListener('click', () => { worker?.postMessage({ type: 'stop' }); });

const auto = new URLSearchParams(location.search).get('run');
if (auto) demo(auto);
