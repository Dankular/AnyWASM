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
  $('stop').disabled = false;
  const wasm = await (await fetch(new URL('./anywasm_wasm.wasm', import.meta.url))).arrayBuffer();
  worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  worker.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'out') print(m.text, m.stream === 'stderr' ? 'err' : '');
    else if (m.type === 'status') {
      const names = { 0: 'stopped', 1: 'exited', 2: 'stopped at an unimplemented import', 3: 'crashed' };
      const detail = m.status === 1 ? `exit code ${m.exitCode}` : m.message;
      stat.textContent = `${label}: ${names[m.status]}${detail ? ' (' + detail + ')' : ''} · ${m.instructions.toLocaleString()} instructions · ${m.mips.toFixed(1)} M instr/s`;
      stat.dataset.status = String(m.status);
      stat.dataset.exit = String(m.exitCode);
      $('stop').disabled = true;
    }
  };
  worker.postMessage({ type: 'start', wasm, bundle: bundleBytes }, [wasm]);
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
$('stop').addEventListener('click', () => { worker?.postMessage({ type: 'stop' }); });

const auto = new URLSearchParams(location.search).get('run');
if (auto) demo(auto);
