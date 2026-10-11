// Runs one Windows process inside a worker so the page stays responsive.
// Messages in : {type:'start', wasm: ArrayBuffer, bundle: Uint8Array}
//               {type:'stdin', text}   {type:'stop'}
// Messages out: {type:'out', stream, text} {type:'status', status, exitCode, message, instructions, mips}
import { AnyWasm, Status } from './anywasm.js';

let w = null;
let stopped = false;
const enc = new TextEncoder();

async function start({ wasm, bundle }) {
  w = await AnyWasm.create(wasm);
  try {
    w.load(bundle);
  } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  const t0 = performance.now();
  let status = Status.RUNNING;
  while (status === Status.RUNNING && !stopped) {
    const sliceStart = performance.now();
    // ~16 ms of work per slice so input/stop messages are handled promptly.
    let steps = 0;
    while (performance.now() - sliceStart < 16 && status === Status.RUNNING) {
      status = w.run(200_000);
      steps++;
    }
    const o = w.takeStdout();
    if (o.length) postMessage({ type: 'out', stream: 'stdout', text: new TextDecoder().decode(o) });
    const e = w.takeStderr();
    if (e.length) postMessage({ type: 'out', stream: 'stderr', text: new TextDecoder().decode(e) });
    if (status === Status.RUNNING) await new Promise((r) => setTimeout(r, 0));
  }
  const dt = (performance.now() - t0) / 1000;
  const n = w.instructions();
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: w.exitCode(), message: w.message(), instructions: n, mips: n / dt / 1e6 });
}

onmessage = (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m);
  else if (m.type === 'stdin' && w) w.pushStdin(enc.encode(m.text));
  else if (m.type === 'stop') stopped = true;
};
