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
import { waitForPresentInterval } from './frame-pacing.js';
import { SharedCpuCompletion, SharedCpuWorkerPool } from './shared-cpu-pool.js';

let w = null, exec = null, stopped = false, wake = null, testMode = false, sharedCpuPool = null, sharedCpuRequested = false;
let sharedNeedsCoordinatorTurn = false;
const pendingSharedInputs = [];
let graphicsUnavailableReason = '';
const dec = new TextDecoder();
const sharedCpuWorkersEnabled = new URL(self.location.href).searchParams.has('sharedCpuWorkers');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitForInput = () => new Promise((r) => { wake = r; });
const log = (m) => postMessage({ type: 'out', stream: 'stderr', text: `[gpu] ${m}\n` });

async function start({ wasm, bundle, assetUrl, canvas, testMode: requestedTestMode, jitMinPrefix }) {
  stopped = false;
  wake = null;
  testMode = !!requestedTestMode;
  sharedCpuRequested = sharedCpuWorkersEnabled;
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedNeedsCoordinatorTurn = false;
  pendingSharedInputs.length = 0;
  exec?.device?.destroy();
  exec = null;
  graphicsUnavailableReason = canvas ? '' : 'OffscreenCanvas worker transfer is unsupported';
  const options = jitMinPrefix === undefined ? {} : { jitMinPrefix };
  w = await AnyWasm.create(wasm, options);
  try { w.load(bundle); } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  bundle = null;
  if (assetUrl) {
    postMessage({ type: 'load-progress', message: 'Loading game data index…' });
    let lastAssetProgress = 0;
    await w.attachAssets(assetUrl, ({ fetchedBytes, packageLength }) => {
      if (fetchedBytes - lastAssetProgress >= 4 * 1024 * 1024 || fetchedBytes === packageLength) {
        lastAssetProgress = fetchedBytes;
        postMessage({ type: 'asset-progress', fetchedBytes, packageLength: Number(packageLength) });
      }
    });
  }
  if (sharedCpuRequested) {
    postMessage({ type: 'load-progress', message: 'Starting shared CPU worker…' });
    try {
      const response = await fetch(new URL('./anywasm_cpu_worker.wasm', import.meta.url));
      if (!response.ok) throw new Error(`worker module fetch returned HTTP ${response.status}`);
      const workerModule = await WebAssembly.compile(await response.arrayBuffer());
      const threadIds = w.sharedCpuContextThreadIds();
      if (!threadIds.length) throw new Error('the loaded process has no CPU contexts');
      const pageCount = Number(w.sharedSnapshotPageCount());
      sharedCpuPool = await SharedCpuWorkerPool.create({
        workerWasm: workerModule,
        workerCount: 1,
        contextCount: Math.max(threadIds.length, 64),
        pageCount,
        pageCapacity: pageCount + Math.max(4096, Math.ceil(pageCount / 4)),
      });
      sharedCpuPool.publishFrom(w);
      postMessage({ type: 'out', stream: 'stderr', text: '[runtime] shared CPU worker mode enabled\n' });
    } catch (error) {
      sharedCpuPool?.dispose();
      sharedCpuPool = null;
      sharedCpuRequested = false;
      for (const input of pendingSharedInputs.splice(0)) {
        if (input.type === 'input') w.input(input.kind, input.a, input.b, input.c);
        else w.pushStdin(new TextEncoder().encode(input.text));
      }
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[runtime] shared CPU worker unavailable; using cooperative execution: ${error.message || error}\n`,
      });
    }
  }
  const publishSharedCoordinatorState = () => {
    if (!sharedCpuPool) return;
    try {
      sharedCpuPool.publishFrom(w);
    } catch (error) {
      sharedCpuPool.dispose();
      sharedCpuPool = null;
      sharedCpuRequested = false;
      sharedNeedsCoordinatorTurn = false;
      for (const input of pendingSharedInputs.splice(0)) {
        if (input.type === 'input') w.input(input.kind, input.a, input.b, input.c);
        else w.pushStdin(new TextEncoder().encode(input.text));
      }
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[runtime] shared CPU layout changed; continuing cooperatively: ${error.message || error}\n`,
      });
    }
  };
  if (canvas) {
    try {
      const tool = new ShaderTool(new URL('./shaders/', import.meta.url));
      await tool.init();
      exec = new GpuExecutor(canvas, tool, log, { noPresent: testMode });
      exec.onWindow = (win) => postMessage({ type: 'window', ...win });
      await exec.init();
      w.hostIo.gpuExecutor = exec;
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
    while (status === Status.RUNNING && performance.now() - sliceStart < 12 && !w.hostWaitPending) {
      if (!sharedCpuPool) {
        status = w.run(250_000);
        continue;
      }
      if (sharedNeedsCoordinatorTurn) {
        sharedCpuPool.importTo(w);
        for (const input of pendingSharedInputs.splice(0)) {
          if (input.type === 'input') w.input(input.kind, input.a, input.b, input.c);
          else w.pushStdin(new TextEncoder().encode(input.text));
        }
        sharedNeedsCoordinatorTurn = false;
        status = w.run(1);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
        continue;
      }
      const activeThreadId = w.sharedCpuActiveThreadId();
      if (!activeThreadId) {
        sharedCpuPool.importTo(w);
        status = w.run(1);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
        continue;
      }
      const instructionBudget = w.sharedCpuQuantumRemaining();
      if (!instructionBudget) {
        sharedCpuPool.importTo(w);
        status = w.run(1);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
        continue;
      }
      const completion = await sharedCpuPool.dispatch(activeThreadId, BigInt(instructionBudget));
      if (completion.state === SharedCpuCompletion.FAILED) {
        throw new Error(`shared CPU worker failed at thread ${activeThreadId}`);
      }
      if (completion.state === SharedCpuCompletion.QUANTUM) {
        sharedCpuPool.importSharedCpuContexts(w);
      } else {
        sharedCpuPool.importTo(w);
        status = w.run(1);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
      }
    }
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
    if (w.hostWaitPending) {
      postMessage({ type: 'load-progress', message: 'Fetching requested game data…' });
      try {
        await w.waitForHost();
        publishSharedCoordinatorState();
      } catch (e) {
        status = Status.CRASHED;
        terminalMessage = `Game data range request failed: ${e.message}`;
      }
      continue;
    } else if (status === Status.FRAME) {
      frames++;
      if (exec && frames % 30 === 0) postMessage({ type: 'gpu', ...exec.stats, asyncify: w.asyncifyStats });
      const waitStart = performance.now();
      await waitForPresentInterval(exec?.lastPresentSyncInterval ?? 1);
      const elapsed100ns = Math.max(0, Math.round((performance.now() - waitStart) * 10_000));
      w.advanceTime100ns(BigInt(elapsed100ns));
      publishSharedCoordinatorState();
    } else if (status === Status.IDLE) await waitForInput();
    else if (status === Status.RUNNING) await sleep(0);
  }
  const dt = (performance.now() - t0) / 1000;
  const callbackSuspended = !!w?.callbackSuspended;
  const n = callbackSuspended ? 0 : w.instructions();
  if (exec) postMessage({ type: 'gpu', ...exec.stats, asyncify: w?.asyncifyStats });
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedCpuRequested = false;
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: callbackSuspended || status === Status.CRASHED && terminalMessage ? -1 : w.exitCode(), message: terminalMessage || (callbackSuspended ? 'stopped with a guest callback suspended' : w.message()), instructions: n, mips: n / dt / 1e6, frames, jitStats: w.jitStats });
}

onmessage = async (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m).catch((e) => {
    sharedCpuPool?.dispose();
    sharedCpuPool = null;
    sharedCpuRequested = false;
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message || String(e), instructions: 0, mips: 0 });
  });
  else if (m.type === 'dispose') { stopped = true; sharedCpuPool?.dispose(); sharedCpuPool = null; sharedCpuRequested = false; wake?.(); close(); }
  else if (m.type === 'input' && w) {
    if (sharedCpuRequested) {
      pendingSharedInputs.push({ type: 'input', kind: m.kind, a: m.a, b: m.b, c: m.c });
      sharedNeedsCoordinatorTurn = true;
    } else w.input(m.kind, m.a, m.b, m.c);
    wake?.(); wake = null;
  }
  else if (m.type === 'stdin' && w) {
    if (sharedCpuRequested) {
      pendingSharedInputs.push({ type: 'stdin', text: m.text });
      sharedNeedsCoordinatorTurn = true;
    } else w.pushStdin(new TextEncoder().encode(m.text));
    wake?.(); wake = null;
  }
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
