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

let w = null, exec = null, stopped = false, disposeRequested = false, wake = null, testMode = false, sharedCpuPool = null, sharedCpuRequested = false;
let sharedNeedsCoordinatorTurn = false;
const pendingSharedInputs = [];
const pendingSharedCompletions = [];
let sharedHandoffPendingFinish = false;
let sharedExecutionMode = 'cooperative';
let sharedExecutionReason = '';
let sharedExecutionWorkerCount = 0;
let sharedWorkerRetired = 0n;
let sharedParallelBatches = 0;
let graphicsUnavailableReason = '';
const dec = new TextDecoder();
const sharedCpuWorkersEnabled = new URL(self.location.href).searchParams.has('sharedCpuWorkers');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitForInput = () => new Promise((r) => { wake = r; });
const log = (m) => postMessage({ type: 'out', stream: 'stderr', text: `[gpu] ${m}\n` });

async function start({ wasm, bundle, assetUrl, assetPackage, canvas, testMode: requestedTestMode, jitMinPrefix, traceGtaEventvol, diagProgress }) {
  stopped = false;
  disposeRequested = false;
  wake = null;
  testMode = !!requestedTestMode;
  sharedCpuRequested = sharedCpuWorkersEnabled;
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedNeedsCoordinatorTurn = false;
  pendingSharedInputs.length = 0;
  pendingSharedCompletions.length = 0;
  sharedHandoffPendingFinish = false;
  sharedExecutionMode = 'cooperative';
  sharedExecutionReason = '';
  sharedExecutionWorkerCount = 0;
  exec?.device?.destroy();
  exec = null;
  graphicsUnavailableReason = canvas ? '' : 'OffscreenCanvas worker transfer is unsupported';
  const options = jitMinPrefix === undefined ? {} : { jitMinPrefix };
  w = await AnyWasm.create(wasm, options);
  try { w.load(bundle); } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  if (traceGtaEventvol) w.x.aw_set_gta_eventvol_trace?.(1);
  bundle = null;
  if (assetUrl) {
    postMessage({ type: 'load-progress', message: 'Loading game data index…' });
    let lastAssetProgress = 0;
    await w.attachAssets(assetUrl, ({ fetchedBytes, packageLength }) => {
      if (fetchedBytes - lastAssetProgress >= 4 * 1024 * 1024 || fetchedBytes === packageLength) {
        lastAssetProgress = fetchedBytes;
        postMessage({ type: 'asset-progress', fetchedBytes, packageLength: Number(packageLength) });
      }
    }, assetPackage || null);
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
        workerCount: 2,
        contextCount: Math.max(threadIds.length, 64),
        pageCount,
        pageCapacity: pageCount + Math.max(4096, Math.ceil(pageCount / 4)),
      });
      sharedCpuPool.publishFrom(w);
      sharedExecutionMode = 'shared-cpu-workers';
      sharedExecutionWorkerCount = sharedCpuPool.workerCount;
      postMessage({ type: 'out', stream: 'stderr', text: '[runtime] shared CPU worker mode enabled\n' });
    } catch (error) {
      sharedCpuPool?.dispose();
      sharedCpuPool = null;
      sharedCpuRequested = false;
      sharedExecutionReason = error.message || String(error);
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
      sharedExecutionMode = 'cooperative';
      sharedExecutionReason = error.message || String(error);
      sharedExecutionWorkerCount = 0;
      pendingSharedCompletions.length = 0;
      sharedHandoffPendingFinish = false;
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
  const traceGuestThreads = () => {
    const count = Math.min(Number(w.x.aw_debug_thread_count?.() || 0), 64);
    const threads = [];
    for (let i = 0; i < count; i++) {
      const id = Number(w.x.aw_debug_thread_at?.(i) || 0);
      if (!id) continue;
      const waitHandle = Number(w.x.aw_debug_thread_wait_handle?.(id) || 0);
      threads.push({
        id: `0x${id.toString(16)}`,
        rip: `0x${Number(w.x.aw_debug_thread_rip?.(id) || 0).toString(16)}`,
        state: Number(w.x.aw_debug_thread_state?.(id) || 0),
        waitHandle: waitHandle ? `0x${waitHandle.toString(16)}` : null,
        handleState: waitHandle ? Number(w.x.aw_debug_handle_state?.(BigInt(waitHandle)) || 0) : 0,
        sp: `0x${Number(w.x.aw_debug_thread_sp?.(id) || 0).toString(16)}`,
        stack: Array.from({ length: 8 }, (_, word) =>
          `0x${Number(w.x.aw_debug_thread_stack_word?.(id, word) || 0).toString(16)}`),
      });
    }
    const readGuestU16 = (address) => Number(w.x.aw_debug_guest_u16?.(address) || 0);
    const readGuestU32 = (address) => Number(w.x.aw_debug_guest_u32?.(address) || 0);
    const currentRip = Number(w.x.aw_debug_rip?.() || 0) >>> 0;
    const currentRegs = Object.fromEntries(['eax', 'ecx', 'edx', 'ebx', 'esp', 'ebp', 'esi', 'edi']
      .map((name, index) => [name, Number(w.x.aw_debug_gpr?.(index) || 0) >>> 0]));
    const readGuestString = (address, limit = 48) => {
      if (!address || address < 0x10000 || address > 0xfffffff0) return null;
      let result = '';
      for (let offset = 0; offset < limit; offset++) {
        const word = readGuestU32((address + (offset & ~3)) >>> 0) >>> 0;
        const byte = (word >>> ((offset & 3) * 8)) & 0xff;
        if (byte === 0) break;
        result += byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : `\\x${byte.toString(16).padStart(2, '0')}`;
      }
      return result;
    };
    const callFrames = [];
    const framePointerValid = (candidate) =>
      candidate >= currentRegs.esp && candidate <= (currentRegs.esp + 0x10000) >>> 0 && (candidate & 3) === 0;
    const framePointerSource = framePointerValid(currentRegs.ebp) ? 'ebp' : 'saved-ebp';
    let frame = framePointerSource === 'ebp'
      ? currentRegs.ebp
      : readGuestU32((currentRegs.esp + 8) >>> 0) >>> 0;
    for (let depth = 0; depth < 8 && frame >= 0x10000 && frame <= 0xfffffff0; depth++) {
      const previous = readGuestU32(frame) >>> 0;
      const returnAddress = readGuestU32((frame + 4) >>> 0) >>> 0;
      const args = Array.from({ length: 4 }, (_, index) => {
        const value = readGuestU32((frame + 8 + index * 4) >>> 0) >>> 0;
        return { value: `0x${value.toString(16)}`, text: readGuestString(value, depth === 0 ? 48 : 16) };
      });
      callFrames.push({ frame: `0x${frame.toString(16)}`, previous: `0x${previous.toString(16)}`, returnAddress: `0x${returnAddress.toString(16)}`, args });
      if (previous <= frame || previous > 0xfffffff0) break;
      frame = previous;
    }
    const stack32 = Array.from({ length: 128 }, (_, index) =>
      `0x${(readGuestU32((currentRegs.esp + index * 4) >>> 0) >>> 0).toString(16)}`);
    const stackStrings = Array.from({ length: 32 }, (_, index) => {
      const value = readGuestU32((currentRegs.esp + index * 4) >>> 0) >>> 0;
      const text = readGuestString(value);
      return text ? { offset: index * 4, value: `0x${value.toString(16)}`, text } : null;
    }).filter(Boolean);
    const memoryWindow = (address, words) => ({
      base: `0x${(address >>> 0).toString(16)}`,
      words: Array.from({ length: words }, (_, index) =>
        `0x${(readGuestU32((address + index * 4) >>> 0) >>> 0).toString(16)}`),
    });
    const queueCount = Math.min(Number(w.x.aw_debug_window_queue_len?.() || 0), 8);
    const factoryRoot = readGuestU32(0xc97b24) >>> 0;
    const factoryOffset = readGuestU32(0xc97b4c) >>> 0;
    const factoryMethods = (factoryRoot + factoryOffset) >>> 0;
    const txdPoolBase = readGuestU32(0xc88018) >>> 0;
    const txdStorage = readGuestU32(0xc8800c) >>> 0;
    const txdRecords = readGuestU32(txdStorage) >>> 0;
    const txdIndex = readGuestU32(0xc88014) >>> 0;
    return {
      threadId: `0x${Number(w.x.aw_debug_thread_id?.() || 0).toString(16)}`,
      threads,
      regs: Object.fromEntries(Object.entries(currentRegs).map(([name, value]) => [name, `0x${value.toString(16)}`])),
      framePointerSource,
      callFrames,
      stack32,
      stackStrings,
      faultMemory: {
        object: memoryWindow((currentRegs.ebp - 0x10) >>> 0, 48),
        lockedBuffer: memoryWindow((currentRegs.edx - 0x10) >>> 0, 24),
        source: memoryWindow((currentRegs.esi - 0x10) >>> 0, 8),
        copyInput: memoryWindow(0x31b07318, 48),
        copiedRecord: memoryWindow(0x30c04090, 16),
      },
      resourceGlobals: {
        factoryBase: memoryWindow(0xc97b24, 12),
        rasterCreateCallback: `0x${readGuestU32((factoryRoot + 0x58) >>> 0).toString(16)}`,
        rasterCreateCallbackSlots: memoryWindow((factoryRoot + 0x58) >>> 0, 8),
        factoryAllocatorSlots: memoryWindow((factoryRoot + 0x130) >>> 0, 8),
        factoryAllocateCallback: `0x${readGuestU32((factoryRoot + 0x144) >>> 0).toString(16)}`,
        factoryVtable: memoryWindow(0xc97b4c, 16),
        factoryObject: memoryWindow(factoryMethods, 24),
        failedParticleRaster: memoryWindow(0x3005cc68, 16),
        factoryLookupCallback: `0x${readGuestU32((factoryMethods + 0x18) >>> 0).toString(16)}`,
        factoryCreateCallback: `0x${readGuestU32((factoryMethods + 0x14) >>> 0).toString(16)}`,
        txdTable: memoryWindow(0xc8800c, 12),
        txdIndices: memoryWindow(0xc88014, 8),
        txdStorage: memoryWindow(txdStorage, 8),
        txdRecords: memoryWindow(txdRecords, 36),
        currentTxdRecord: memoryWindow((txdRecords + txdIndex * 12) >>> 0, 6),
        txdPoolBase: `0x${txdPoolBase.toString(16)}`,
        txdPoolFirstWords: memoryWindow(txdPoolBase, 8),
        errorState: memoryWindow(0xc9a6f0, 4),
      },
      flags: `0x${Number(w.x.aw_debug_rflags?.() || 0).toString(16)}`,
      tickMs: Number(w.x.aw_debug_tick_ms?.() || 0),
      keyEvents: Array.from({ length: 12 }, (_, i) => readGuestU16(0xb731a8 + i * 2)),
      customEvents: Array.from({ length: 12 }, (_, i) => readGuestU16(0xb73190 + i * 2)),
      gameState: {
        startupPhase: readGuestU32(0xc8d4c0),
        splashFlag: readGuestU32(0xba67a4),
        windowFlag: readGuestU32(0xba67a8),
        activationFlag: readGuestU32(0xc87ffc),
        shutdownFlag: readGuestU32(0xc17050),
        returnSeed: readGuestU32(0xc17044),
        startupInputFlag: readGuestU32(0xba67a5),
      },
      windowQueue: Array.from({ length: queueCount }, (_, i) => ({
        hwnd: `0x${Number(w.x.aw_debug_window_message?.(i, 0) || 0).toString(16)}`,
        msg: `0x${Number(w.x.aw_debug_window_message?.(i, 1) || 0).toString(16)}`,
        wparam: `0x${Number(w.x.aw_debug_window_message?.(i, 2) || 0).toString(16)}`,
        lparam: `0x${Number(w.x.aw_debug_window_message?.(i, 3) || 0).toString(16)}`,
      })),
    };
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
  let lastTraceProgress = t0;
  let status = Status.RUNNING;
  let terminalMessage = '';
  let frames = 0;
  const SHARED_PUMP_INSTRUCTION_BUDGET = 250_000n;
  let sharedInstructionBudgetRemaining = SHARED_PUMP_INSTRUCTION_BUDGET;
  const chargeSharedInstructionBudget = (before) => {
    const retired = w.retiredInstructionsExact() - before;
    if (retired < 0n) throw new Error('shared CPU retired-instruction counter moved backwards');
    if (retired > sharedInstructionBudgetRemaining) {
      sharedInstructionBudgetRemaining = 0n;
      throw new Error('shared CPU coordinator exceeded its pump instruction budget');
    }
    sharedInstructionBudgetRemaining -= retired;
    return retired;
  };
  while (!stopped && (status === Status.RUNNING || status === Status.FRAME || status === Status.IDLE)) {
    const sliceStart = performance.now();
    sharedInstructionBudgetRemaining = SHARED_PUMP_INSTRUCTION_BUDGET;
    status = Status.RUNNING;
    while (!stopped && status === Status.RUNNING && performance.now() - sliceStart < 12 && !w.hostWaitPending) {
      if (!sharedCpuPool) {
        status = w.run(250_000);
        continue;
      }
      if (sharedInstructionBudgetRemaining === 0n) break;
      if (w.callbackSuspended) {
        const before = w.retiredInstructionsExact();
        status = w.run(Number(sharedInstructionBudgetRemaining < 1n ? sharedInstructionBudgetRemaining : 1n));
        chargeSharedInstructionBudget(before);
        if (!w.hostWaitPending && !w.callbackSuspended) {
          if (sharedHandoffPendingFinish) {
            w.finishSharedCpuCoordinatorTurn();
            sharedHandoffPendingFinish = false;
          }
          publishSharedCoordinatorState();
        }
        continue;
      }
      if (sharedNeedsCoordinatorTurn) {
        sharedCpuPool.importTo(w);
        for (const input of pendingSharedInputs.splice(0)) {
          if (input.type === 'input') w.input(input.kind, input.a, input.b, input.c);
          else w.pushStdin(new TextEncoder().encode(input.text));
        }
        sharedNeedsCoordinatorTurn = false;
        if (pendingSharedCompletions.length) {
          const completion = pendingSharedCompletions.shift();
          if (completion.state === SharedCpuCompletion.FAILED) {
            throw new Error(`shared CPU worker failed at thread ${completion.threadId}`);
          }
          const before = w.retiredInstructionsExact();
          const switched = w.beginSharedCpuCoordinatorTurn(completion.threadId);
          try {
            status = w.run(1);
          } finally {
            if (switched && !w.hostWaitPending) w.finishSharedCpuCoordinatorTurn();
            else if (switched) sharedHandoffPendingFinish = true;
          }
          chargeSharedInstructionBudget(before);
          sharedNeedsCoordinatorTurn = pendingSharedCompletions.length > 0;
        } else {
          const before = w.retiredInstructionsExact();
          status = w.run(1);
          chargeSharedInstructionBudget(before);
        }
        if (!w.hostWaitPending && !sharedHandoffPendingFinish) publishSharedCoordinatorState();
        continue;
      }
      const runnableThreadIds = w.sharedCpuRunnableThreadIds();
      if (!runnableThreadIds.length) {
        sharedCpuPool.importTo(w);
        const coordinatorBudget = Number(sharedInstructionBudgetRemaining);
        const before = w.retiredInstructionsExact();
        status = w.run(coordinatorBudget);
        chargeSharedInstructionBudget(before);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
        continue;
      }
      const contexts = runnableThreadIds
        .slice(0, sharedCpuPool.workerCount)
        .map((threadId) => ({
          threadId,
          instructionBudget: w.sharedCpuThreadQuantumRemaining(threadId),
        }))
        .filter((context) => context.instructionBudget > 0);
      const coordinatorReserve = BigInt(contexts.length);
      const workerBudget = sharedInstructionBudgetRemaining > coordinatorReserve
        ? sharedInstructionBudgetRemaining - coordinatorReserve
        : 0n;
      if (workerBudget === 0n || contexts.length === 0) break;

      const before = w.retiredInstructionsExact();
      const completions = await sharedCpuPool.dispatchBatch(contexts, workerBudget);
      if (completions.some((completion) => completion.state === SharedCpuCompletion.FAILED)) {
        const failed = completions.find((completion) => completion.state === SharedCpuCompletion.FAILED);
        throw new Error(`shared CPU worker failed at thread ${failed.threadId}`);
      }
      const workerRetired = completions.reduce(
        (total, completion) => total + completion.retiredInstructions,
        0n,
      );
      sharedWorkerRetired += workerRetired;
      if (contexts.length >= 2) sharedParallelBatches++;
      if (workerRetired > workerBudget) {
        throw new Error('shared CPU workers exceeded their combined instruction reservation');
      }
      sharedCpuPool.importTo(w);
      const importedRetired = chargeSharedInstructionBudget(before);
      if (importedRetired !== workerRetired) {
        throw new Error('shared CPU completion counts do not match coordinator accounting');
      }
      pendingSharedCompletions.push(
        ...completions.filter((completion) => completion.state !== SharedCpuCompletion.QUANTUM),
      );
      if (pendingSharedCompletions.length) sharedNeedsCoordinatorTurn = true;
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
    const e = w.takeStderr();
    if (e.length) {
      const text = dec.decode(e);
      const diagnostic = traceGtaEventvol
        ? text.split('\n').filter((line) => line.startsWith('[process-exit]') || line.startsWith('[gta-eventvol]') || line.startsWith('[gta-startup-return]') || line.startsWith('[gta-copy-trace]') || line.startsWith('[gta-copy-watch]') || line.startsWith('[gta-txd-trace]') || line.startsWith('[gta-file-open]') || line.startsWith('[gta-file-read]') || line.startsWith('[gta-file-read-result]') || line.startsWith('[gta-file-size]') || line.startsWith('[gta-file-seek]') || line.startsWith('[gta-map-view]') || /PostQuitMessage|PostThreadMessage|PostMessage|DestroyWindow|ExitProcess|TerminateProcess/i.test(line)).join('\n')
        : text;
      if (diagnostic) postMessage({ type: 'out', stream: 'stderr', text: diagnostic });
    }
    if ((traceGtaEventvol || diagProgress) && performance.now() - lastTraceProgress >= (diagProgress ? 1_000 : 5_000)) {
      lastTraceProgress = performance.now();
      const trace = traceGtaEventvol ? {
        rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
        instructions: w.retiredInstructions(),
        jitStats: w.jitStats,
        asyncify: w.asyncifyStats,
        ...traceGuestThreads(),
      } : {
        rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
        instructions: w.retiredInstructions(),
        jitStats: w.jitStats,
        asyncify: w.asyncifyStats,
        ...traceGuestThreads(),
        crtFile: Array.from({ length: 8 }, (_, index) =>
          `0x${(Number(w.x.aw_debug_guest_u32?.(0x8e2c90 + index * 4) || 0) >>> 0).toString(16)}`),
      };
      postMessage({
        type: 'trace-progress',
        trace,
      });
      if (diagProgress) console.info(`[gta-worker-progress]${JSON.stringify({
        rip: trace.rip,
        instructions: trace.instructions,
        jitStats: trace.jitStats,
        asyncify: trace.asyncify,
        frames,
        draws: exec?.stats?.draws ?? 0,
        presents: exec?.stats?.presents ?? 0,
        hostWaitPending: w.hostWaitPending,
        assetRange: w.hostIo.pendingRange,
        regs: trace.regs,
        callFrames: trace.callFrames?.map(({ frame, returnAddress, args }) => ({ frame, returnAddress, args: args.slice(0, 2) })),
        gameState: trace.gameState,
        keyEvents: trace.keyEvents,
        customEvents: trace.customEvents,
      })}`);
    }
    if (w.hostWaitPending) {
      postMessage({
        type: 'load-progress',
        message: 'Fetching requested game data…',
        trace: traceGtaEventvol || diagProgress ? {
          rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
          instructions: w.retiredInstructions(),
          asyncify: w.asyncifyStats,
          assetRange: w.hostIo.pendingRange,
          ...(traceGtaEventvol ? traceGuestThreads() : {}),
        } : undefined,
      });
      try {
        await w.waitForHost();
        if (!w.callbackSuspended) publishSharedCoordinatorState();
      } catch (e) {
        status = Status.CRASHED;
        terminalMessage = `Game data range request failed: ${e.message}`;
      }
      continue;
    } else if (status === Status.FRAME) {
      frames++;
      if (exec && frames % 30 === 0) postMessage({
        type: 'gpu',
        ...exec.stats,
        asyncify: w.asyncifyStats,
        trace: traceGtaEventvol ? { rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`, instructions: w.retiredInstructions(), ...traceGuestThreads() } : undefined,
      });
      const syncInterval = exec?.lastPresentSyncInterval ?? 1;
      const waitStart = performance.now();
      const vblanksWaited = await waitForPresentInterval(syncInterval);
      const elapsed100ns = Math.max(0, Math.round((performance.now() - waitStart) * 10_000));
      w.advanceTime100ns(BigInt(elapsed100ns));
      if (testMode) postMessage({ type: 'frame-pacing', syncInterval, vblanksWaited, elapsed100ns, virtualTicksAdded: elapsed100ns * 10 });
      publishSharedCoordinatorState();
    } else if (status === Status.IDLE) {
      if (traceGtaEventvol) {
        postMessage({
          type: 'load-progress',
          message: 'Guest waiting for input',
          trace: { rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`, instructions: w.retiredInstructions(), ...traceGuestThreads() },
        });
      }
      await waitForInput();
    }
    else if (status === Status.RUNNING) await sleep(0);
  }
  const dt = (performance.now() - t0) / 1000;
  const callbackSuspended = !!w?.callbackSuspended;
  const n = callbackSuspended ? 0 : w.instructions();
  if (exec) postMessage({ type: 'gpu', ...exec.stats, asyncify: w?.asyncifyStats });
  const finalMessage = terminalMessage || (callbackSuspended ? 'stopped with a guest callback suspended' : w.message());
  const exceptionSp = Number(finalMessage.match(/\besp=0x([0-9a-f]+)/i)?.[1] || 0) >>> 0;
  const exceptionStack = diagProgress && exceptionSp
    ? Array.from({ length: 64 }, (_, index) =>
        `0x${(Number(w.x.aw_debug_guest_u32?.((exceptionSp + index * 4) >>> 0) || 0) >>> 0).toString(16)}`)
    : undefined;
  const finalTrace = traceGtaEventvol || diagProgress ? {
    rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
    instructions: w.retiredInstructions(),
    ...traceGuestThreads(),
    ...(exceptionStack ? { exceptionSp: `0x${exceptionSp.toString(16)}`, exceptionStack } : {}),
  } : undefined;
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedCpuRequested = false;
  if (disposeRequested) {
    close();
    return;
  }
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: callbackSuspended || status === Status.CRASHED && terminalMessage ? -1 : w.exitCode(), message: finalMessage, instructions: n, mips: n / dt / 1e6, frames, jitStats: w.jitStats, trace: finalTrace, executionMode: sharedExecutionMode, executionWorkerCount: sharedExecutionWorkerCount, executionModeReason: sharedExecutionReason, sharedWorkerRetired: String(sharedWorkerRetired), sharedParallelBatches });
}

onmessage = async (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m).catch((e) => {
    sharedCpuPool?.dispose();
    sharedCpuPool = null;
    sharedCpuRequested = false;
    if (disposeRequested) {
      close();
      return;
    }
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message || String(e), instructions: 0, mips: 0, executionMode: sharedExecutionMode, executionWorkerCount: sharedExecutionWorkerCount, executionModeReason: sharedExecutionReason });
  });
  else if (m.type === 'dispose') {
    disposeRequested = true;
    stopped = true;
    wake?.();
    if (!w) {
      sharedCpuPool?.dispose();
      sharedCpuPool = null;
      close();
    }
  }
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
