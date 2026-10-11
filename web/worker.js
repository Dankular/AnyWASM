// Runs one Windows process inside a worker so the page stays responsive, and (if a canvas is
// supplied) replays the program's window/Direct3D command stream with WebGPU.
//
// In : {type:'start', wasm, bundle, canvas?}  {type:'input', kind, a, b, c}  {type:'stop'}  {type:'capture'}
// Out: {type:'out', stream, text}  {type:'window', width, height, title}  {type:'status', ...}
//      {type:'pixels', width, height, data}  {type:'gpu', draws, presents, pipelines, errors}
import { AnyWasm, Status } from './anywasm.js';
import { ShaderTool } from './shaders.js';
import { GpuExecutor } from './gpu.js';

let w = null, exec = null, stopped = false, wake = null;
const dec = new TextDecoder();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const nextFrame = () => (typeof requestAnimationFrame === 'function' ? new Promise((r) => requestAnimationFrame(() => r())) : sleep(16));
const waitForInput = () => new Promise((r) => { wake = r; });
const log = (m) => postMessage({ type: 'out', stream: 'stderr', text: `[gpu] ${m}\n` });

async function start({ wasm, bundle, canvas, testMode }) {
  w = await AnyWasm.create(wasm);
  try { w.load(bundle); } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  if (canvas) {
    try {
      const tool = new ShaderTool(new URL('./shaders/', import.meta.url));
      await tool.init();
      exec = new GpuExecutor(canvas, tool, log, { noPresent: !!testMode });
      exec.onWindow = (win) => postMessage({ type: 'window', ...win });
      await exec.init();
      postMessage({ type: 'gpuinfo', info: { ...exec.adapterInfo }, features: [...exec.features] });
    } catch (e) {
      log('WebGPU unavailable: ' + e.message);
      exec = null;
    }
  }
  const t0 = performance.now();
  let status = Status.RUNNING;
  let frames = 0;
  while (!stopped && (status === Status.RUNNING || status === Status.FRAME || status === Status.IDLE)) {
    const sliceStart = performance.now();
    status = Status.RUNNING;
    while (status === Status.RUNNING && performance.now() - sliceStart < 12) status = w.run(250_000);
    const g = w.takeGpu();
    if (g.length && exec) exec.execute(g);
    const o = w.takeStdout(); if (o.length) postMessage({ type: 'out', stream: 'stdout', text: dec.decode(o) });
    const e = w.takeStderr(); if (e.length) postMessage({ type: 'out', stream: 'stderr', text: dec.decode(e) });
    if (status === Status.FRAME) {
      frames++;
      if (exec && frames % 30 === 0) postMessage({ type: 'gpu', ...exec.stats });
      await nextFrame();
    } else if (status === Status.IDLE) await waitForInput();
    else if (status === Status.RUNNING) await sleep(0);
  }
  const dt = (performance.now() - t0) / 1000;
  const n = w.instructions();
  if (exec) postMessage({ type: 'gpu', ...exec.stats });
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: w.exitCode(), message: w.message(), instructions: n, mips: n / dt / 1e6, frames });
}

onmessage = async (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m);
  else if (m.type === 'input' && w) { w.input(m.kind, m.a, m.b, m.c); wake?.(); wake = null; }
  else if (m.type === 'stdin' && w) { w.pushStdin(new TextEncoder().encode(m.text)); wake?.(); wake = null; }
  else if (m.type === 'stop') { stopped = true; wake?.(); }
  else if (m.type === 'capture') {
    try {
      const px = exec ? await exec.readBackbuffer() : null;
      postMessage({ type: 'pixels', ...(px || { width: 0, height: 0, data: new Uint8Array(0) }) });
    } catch (e) {
      log('capture failed: ' + (e.stack || e.message));
      postMessage({ type: 'pixels', width: 0, height: 0, data: new Uint8Array(0), error: e.message });
    }
  }
};
