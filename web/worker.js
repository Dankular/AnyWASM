// Runs one Windows process inside a worker so the page stays responsive, and (if a canvas is
// supplied) replays the program's window/Direct3D command stream with WebGPU.
//
// In : {type:'start', wasm, bundle, canvas?, jitMinPrefix?}
//      {type:'input', kind, a, b, c}  {type:'stop'}  {type:'capture'}  {type:'readback-texture', id}
// Out: {type:'ready'}  {type:'out', stream, text}  {type:'window', width, height, title}  {type:'status', ...}
//      {type:'pixels', width, height, data}  {type:'texture-readback', ...}  {type:'gpu', draws, presents, pipelines, errors}
//      Terminal status includes JIT counters for runtime diagnostics.
import { AnyWasm, Status } from './anywasm.js';
import { ShaderTool } from './shaders.js';
import { GpuExecutor } from './gpu.js';

let w = null, exec = null, stopped = false, wake = null, testMode = false;
let graphicsUnavailableReason = '';
const dec = new TextDecoder();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const nextFrame = () => (typeof requestAnimationFrame === 'function' ? new Promise((r) => requestAnimationFrame(() => r())) : sleep(16));
const waitForInput = () => new Promise((r) => { wake = r; });
const log = (m) => postMessage({ type: 'out', stream: 'stderr', text: `[gpu] ${m}\n` });

async function start({ wasm, bundle, canvas, testMode: requestedTestMode, jitMinPrefix }) {
  stopped = false;
  wake = null;
  testMode = !!requestedTestMode;
  exec?.device?.destroy();
  exec = null;
  graphicsUnavailableReason = canvas ? '' : 'OffscreenCanvas worker transfer is unsupported';
  const options = jitMinPrefix === undefined ? {} : { jitMinPrefix };
  w = await AnyWasm.create(wasm, options);
  try { w.load(bundle); } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  if (canvas) {
    try {
      const tool = new ShaderTool(new URL('./shaders/', import.meta.url));
      await tool.init();
      exec = new GpuExecutor(canvas, tool, log, { noPresent: testMode });
      exec.onWindow = (win) => postMessage({ type: 'window', ...win });
      await exec.init();
      postMessage({ type: 'gpuinfo', info: { ...exec.adapterInfo }, features: [...exec.features] });
    } catch (e) {
      graphicsUnavailableReason = `WebGPU unavailable: ${e.message}`;
      log(graphicsUnavailableReason);
      exec = null;
    }
  }
  postMessage({ type: 'ready' });
  const t0 = performance.now();
  let status = Status.RUNNING;
  let terminalMessage = '';
  let frames = 0;
  while (!stopped && (status === Status.RUNNING || status === Status.FRAME || status === Status.IDLE)) {
    const sliceStart = performance.now();
    status = Status.RUNNING;
    while (status === Status.RUNNING && performance.now() - sliceStart < 12) status = w.run(250_000);
    const g = w.takeGpu();
    if (g.length) {
      if (exec) exec.execute(g);
      else {
        status = Status.CRASHED;
        terminalMessage = `Graphics unavailable: ${graphicsUnavailableReason || 'no renderer is available'}`;
      }
    }
    const o = w.takeStdout(); if (o.length) postMessage({ type: 'out', stream: 'stdout', text: dec.decode(o) });
    const e = w.takeStderr(); if (e.length) postMessage({ type: 'out', stream: 'stderr', text: dec.decode(e) });
    if (status === Status.FRAME) {
      frames++;
      if (exec && frames % 30 === 0) postMessage({ type: 'gpu', ...exec.stats, asyncify: w.asyncifyStats });
      const waitStart = performance.now();
      await nextFrame();
      const elapsed100ns = Math.max(0, Math.round((performance.now() - waitStart) * 10_000));
      w.advanceTime100ns(BigInt(elapsed100ns));
    } else if (status === Status.IDLE) await waitForInput();
    else if (status === Status.RUNNING) await sleep(0);
  }
  const dt = (performance.now() - t0) / 1000;
  const callbackSuspended = !!w?.callbackSuspended;
  const n = callbackSuspended ? 0 : w.instructions();
  if (exec) postMessage({ type: 'gpu', ...exec.stats, asyncify: w?.asyncifyStats });
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: callbackSuspended || status === Status.CRASHED && terminalMessage ? -1 : w.exitCode(), message: terminalMessage || (callbackSuspended ? 'stopped with a guest callback suspended' : w.message()), instructions: n, mips: n / dt / 1e6, frames, jitStats: w.jitStats });
}

onmessage = async (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m);
  else if (m.type === 'dispose') { stopped = true; wake?.(); close(); }
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
  } else if (m.type === 'readback-texture') {
    try {
      if (!testMode) throw new Error('texture readback is available only in test mode');
      if (!exec) throw new Error('WebGPU executor is unavailable');
      const result = await exec.readBackTexture(m.id);
      postMessage({ type: 'texture-readback', ...result });
    } catch (e) {
      postMessage({ type: 'texture-readback', id: m.id, error: e.message });
    }
  }
};
