import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const probeSource = join(repoRoot, 'test', 'fixtures', 'wasm_shared_memory_probe.rs');
const runtimeSharedMemoryBytes = 32 * 1024 * 1024;
const runtimeMaximumBytes = 1024 * 1024 * 1024;
const runtimeInitialPages = runtimeSharedMemoryBytes / 65536;
const runtimeMaximumPages = runtimeMaximumBytes / 65536;
const iterationsPerWorker = 100_000;
const activeWorkers = new Set();

function failCommand(label, result) {
  if (result.status === 0) return;
  throw new Error(`${label} failed (${result.status ?? result.signal})\n${result.stdout ?? ''}${result.stderr ?? ''}`);
}

function compile(source, output, { env = process.env, extraLinkArgs = [] } = {}) {
  const result = spawnSync('rustc', [
    source,
    '--crate-type=cdylib',
    '--target=wasm32-unknown-unknown',
    '-Copt-level=2',
    '-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals',
    '-Clink-arg=--import-memory',
    '-Clink-arg=--shared-memory',
    `-Clink-arg=--initial-memory=${runtimeSharedMemoryBytes}`,
    `-Clink-arg=--max-memory=${runtimeMaximumBytes}`,
    ...extraLinkArgs.map((arg) => `-Clink-arg=${arg}`),
    '-o', output,
  ], { cwd: repoRoot, encoding: 'utf8', env });
  failCommand('rustc shared-memory probe build', result);
}

function instantiate(module, memory) {
  const imports = Object.create(null);
  for (const entry of WebAssembly.Module.imports(module)) {
    imports[entry.module] ??= Object.create(null);
    if (entry.kind === 'memory') {
      imports[entry.module][entry.name] = memory;
    } else if (entry.kind === 'function') {
      imports[entry.module][entry.name] = () => 0;
    } else {
      throw new Error(`unexpected probe import: ${entry.module}.${entry.name} (${entry.kind})`);
    }
  }
  return new WebAssembly.Instance(module, imports);
}

function startWorker(data) {
  const worker = new Worker(new URL(import.meta.url), {
    workerData: data,
  });
  activeWorkers.add(worker);
  let readySeen = false;
  let doneSeen = false;
  let readyResolve;
  let readyReject;
  let doneResolve;
  let doneReject;
  let exitResolve;
  let exitReject;
  const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
  const done = new Promise((resolve, reject) => { doneResolve = resolve; doneReject = reject; });
  const exited = new Promise((resolve, reject) => { exitResolve = resolve; exitReject = reject; });
  ready.catch(() => {});
  done.catch(() => {});
  exited.catch(() => {});
  const rejectWorker = (error) => {
    readyReject(error);
    doneReject(error);
    exitReject(error);
  };
  const timeout = setTimeout(() => {
    const error = new Error('shared-memory worker timed out');
    rejectWorker(error);
    void worker.terminate();
  }, 30_000);
  timeout.unref();
  worker.on('message', (message) => {
    if (message.type === 'ready' && !readySeen) {
      readySeen = true;
      readyResolve(message);
    } else if (message.type === 'done' && !doneSeen) {
      doneSeen = true;
      doneResolve(message);
    } else {
      const error = new Error(`unexpected worker message: ${JSON.stringify(message)}`);
      rejectWorker(error);
    }
  });
  worker.once('error', (error) => {
    rejectWorker(error);
  });
  worker.once('exit', (code) => {
    activeWorkers.delete(worker);
    clearTimeout(timeout);
    if (code !== 0) {
      rejectWorker(new Error(`shared-memory worker exited with code ${code}`));
    } else if (!readySeen || !doneSeen) {
      rejectWorker(new Error(`shared-memory worker exited before completing its protocol (ready=${readySeen}, done=${doneSeen})`));
    } else {
      exitResolve(code);
    }
  });
  return { worker, ready, done, exited };
}

async function runProbe() {
  const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-sab-probe-'));
  try {
    const probeWasm = join(tempDir, 'probe.wasm');
    compile(probeSource, probeWasm);
    const module = new WebAssembly.Module(readFileSync(probeWasm));
    const imports = WebAssembly.Module.imports(module);
    const memoryImports = imports.filter((entry) => entry.kind === 'memory');
    assert.equal(memoryImports.length, 1, 'probe must import exactly one memory');
    const memory = new WebAssembly.Memory({
      initial: runtimeInitialPages,
      maximum: runtimeMaximumPages,
      shared: true,
    });
    assert.ok(memory.buffer instanceof SharedArrayBuffer, 'host memory must be shared');

    const first = instantiate(module, memory);
    const second = instantiate(module, memory);
    first.exports.set_runtime_state(0x1234_5678);
    const runtimeStateShared = second.exports.runtime_state() === 0x1234_5678;
    const firstCursor = first.exports.stand_in_cursor_probe();
    const secondCursor = second.exports.stand_in_cursor_probe();
    const standInCursorShared = secondCursor === firstCursor + 64;
    assert.equal(runtimeStateShared, true, 'mutable Rust statics must alias through shared linear memory');
    assert.equal(standInCursorShared, true, 'the stand-in cursor must advance across instances');

    const barrier = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
    const workers = [0, 1].map(() => startWorker({
      mode: 'alias', module, memory, barrier, iterations: iterationsPerWorker,
    }));
    await Promise.all(workers.map((worker) => worker.ready));

    Atomics.store(new Int32Array(barrier), 0, 1);
    Atomics.notify(new Int32Array(barrier), 0, workers.length);
    const completion = await Promise.all(workers.map((worker) => worker.done));
    const atomicCount = first.exports.atomic_value();
    assert.equal(atomicCount, iterationsPerWorker * workers.length,
      'concurrent atomic increments must not be lost');

    const workerWasm = join(tempDir, 'worker.wasm');
    const workerSource = join(repoRoot, 'test', 'fixtures', 'wasm_shared_worker_probe.rs');
    compile(workerSource, workerWasm, {
      extraLinkArgs: ['--export=__stack_pointer', '--export=__data_end'],
    });
    const workerModule = new WebAssembly.Module(readFileSync(workerWasm));
    const workerMemoryImports = WebAssembly.Module.imports(workerModule).filter((entry) => entry.kind === 'memory');
    assert.equal(workerMemoryImports.length, 1, 'worker module must import one shared memory');
    const workerInstance = instantiate(workerModule, memory);
    const stackPointer = workerInstance.exports.__stack_pointer;
    assert.ok(stackPointer instanceof WebAssembly.Global, 'worker stack pointer must be exported');
    const workerDataEnd = Number(workerInstance.exports.__data_end.value);
    const counterAddress = 2 * 1024 * 1024;
    const activeAddress = counterAddress + 4;
    const maximumActiveAddress = counterAddress + 8;
    const contextAddresses = [4 * 1024 * 1024, 4 * 1024 * 1024 + 64];
    const stackTops = [8 * 1024 * 1024, 10 * 1024 * 1024];
    const stackBytes = 1024 * 1024;
    const stackBases = stackTops.map((top) => top - stackBytes);
    const stackGuardAddresses = stackBases.map((base) => base - Uint32Array.BYTES_PER_ELEMENT);
    const stackGuardValues = [0x51ac_0000, 0x51ac_0001];
    const sentinelAddress = 12 * 1024 * 1024;
    const bytes = new Uint8Array(memory.buffer);
    assert.ok(workerDataEnd < counterAddress, 'worker static image must end before guest synchronization words');
    bytes[sentinelAddress] = 0xa5;
    const stackGuardView = new DataView(memory.buffer);
    for (let index = 0; index < stackGuardAddresses.length; index++) {
      assert.ok(stackGuardAddresses[index] >= workerDataEnd, 'stack guards must be outside the worker static image');
      stackGuardView.setUint32(stackGuardAddresses[index], stackGuardValues[index], true);
    }
    const workerSync = new Int32Array(memory.buffer, counterAddress, 3);
    Atomics.store(workerSync, 0, 0);
    Atomics.store(workerSync, 1, 0);
    Atomics.store(workerSync, 2, 0);
    const workerBarrier = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
    const workerIterations = 20_000;
    for (let left = 0; left < stackBases.length; left++) {
      for (let right = left + 1; right < stackBases.length; right++) {
        assert.ok(stackBases[left] + stackBytes <= stackBases[right]
          || stackBases[right] + stackBytes <= stackBases[left], 'worker stack regions must not overlap');
      }
    }
    const isolatedWorkers = stackTops.map((stackTop, index) => startWorker({
      mode: 'isolated',
      module: workerModule,
      memory,
      barrier: workerBarrier,
      iterations: workerIterations,
      workerIndex: index,
      stackTop,
      contextAddress: contextAddresses[index],
      counterAddress,
      activeAddress,
      maximumActiveAddress,
    }));
    const readyInfo = await Promise.all(isolatedWorkers.map((worker) => worker.ready));
    assert.deepEqual(readyInfo.map((info) => info.stackPointer), stackTops,
      'each worker must install its own stack pointer before execution');
    Atomics.store(new Int32Array(workerBarrier), 0, 1);
    Atomics.notify(new Int32Array(workerBarrier), 0, isolatedWorkers.length);
    const isolatedCompletion = await Promise.all(isolatedWorkers.map((worker) => worker.done));
    await Promise.all(isolatedWorkers.map((worker) => worker.exited));
    await Promise.all(workers.map((worker) => worker.exited));
    const workerCounter = Atomics.load(new Int32Array(memory.buffer, counterAddress, 1), 0);
    const maximumConcurrentWorkers = Atomics.load(new Int32Array(memory.buffer, maximumActiveAddress, 1), 0);
    assert.equal(workerCounter, workerIterations * isolatedWorkers.length,
      'CPU worker modules must share an atomic guest counter');
    assert.ok(maximumConcurrentWorkers > 1, 'CPU worker slices must overlap in execution');
    assert.equal(bytes[sentinelAddress], 0xa5, 'worker module initialization must not overwrite guest data');
    const contextView = new DataView(memory.buffer);
    const workerContexts = contextAddresses.map((address) => ({
      seed: contextView.getUint32(address, true),
      checksum: contextView.getUint32(address + 4, true),
      stackMarker: contextView.getUint32(address + 8, true),
    }));
    const workerSeeds = [0x101, 0x202];
    assert.deepEqual(workerContexts.map((context) => context.seed), workerSeeds);
    assert.deepEqual(workerContexts.map((context) => context.checksum),
      workerSeeds.map((seed) => (256 * seed + (255 * 256) / 2) >>> 0),
      'each worker must write the expected checksum into its own context');
    assert.deepEqual(workerContexts.map((context) => context.stackMarker), [0x200, 0x301]);
    const stackGuardsPreserved = stackGuardAddresses.every((address, index) =>
      stackGuardView.getUint32(address, true) === stackGuardValues[index]);
    assert.equal(stackGuardsPreserved, true, 'worker stacks must not overwrite their low-boundary canaries');

    const result = {
      sharedMemoryImport: `${memoryImports[0].module}.${memoryImports[0].name}`,
      workerCount: workers.length,
      atomicCount,
      expectedAtomicCount: iterationsPerWorker * workers.length,
      runtimeStateShared,
      standInCursorShared,
      fullRuntimeInstanceGate: {
        status: 'blocked',
        mutableRuntimeStateShared: runtimeStateShared,
        standInCursorShared,
      },
      workers: completion.map(({ atomicCount: count }) => count),
      noStdWorkerModule: {
        sharedMemoryImport: `${workerMemoryImports[0].module}.${workerMemoryImports[0].name}`,
        exportedPerInstanceStackPointer: true,
        workerCount: isolatedWorkers.length,
        atomicCount: workerCounter,
        expectedAtomicCount: workerIterations * isolatedWorkers.length,
        maximumConcurrentWorkers,
        stackPointers: readyInfo.map((info) => info.stackPointer),
        dataEnd: workerDataEnd,
        contexts: workerContexts,
        workerResults: isolatedCompletion.map(({ result: workerResult }) => workerResult),
        guestSentinelPreserved: bytes[sentinelAddress] === 0xa5,
        stackGuardsPreserved,
      },
    };
    console.log(JSON.stringify(result, null, 2));
    return result;
  } finally {
    await Promise.allSettled([...activeWorkers].map((worker) => worker.terminate()));
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function runProductionBuild() {
  const targetDir = join(repoRoot, 'target', 'sab-feasibility');
  const input = join(targetDir, 'wasm32-unknown-unknown', 'release', 'anywasm_wasm.wasm');
  const output = join(targetDir, 'wasm32-unknown-unknown', 'release', 'anywasm_wasm.asyncified.wasm');
  const env = {
    ...process.env,
    CARGO_TARGET_DIR: targetDir,
    RUSTFLAGS: [
      '-C target-feature=+atomics,+bulk-memory,+mutable-globals',
      '-C link-arg=--import-memory',
      '-C link-arg=--shared-memory',
      `-C link-arg=--initial-memory=${runtimeSharedMemoryBytes}`,
      `-C link-arg=--max-memory=${runtimeMaximumBytes}`,
    ].join(' '),
  };
  const build = spawnSync('cargo', [
    'build', '-p', 'anywasm-wasm', '--target', 'wasm32-unknown-unknown', '--release',
  ], { cwd: repoRoot, encoding: 'utf8', env, maxBuffer: 16 * 1024 * 1024 });
  if (build.stdout) process.stdout.write(build.stdout);
  if (build.stderr) process.stderr.write(build.stderr);
  if (build.status !== 0) {
    const output = `${build.stdout ?? ''}\n${build.stderr ?? ''}`;
    const linkerReason = output.split(/\r?\n/).find((line) => line.includes('rust-lld: error:'));
    console.log(JSON.stringify({
      productionSharedMemoryBuild: 'blocked',
      reason: linkerReason ?? `cargo exited ${build.status ?? build.signal}`,
      asyncifyChecked: false,
    }, null, 2));
    return false;
  }

  const asyncify = spawnSync('node', ['scripts/asyncify-wasm.mjs', input, output], {
    cwd: repoRoot,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });
  if (asyncify.stdout) process.stdout.write(asyncify.stdout);
  if (asyncify.stderr) process.stderr.write(asyncify.stderr);
  if (asyncify.status !== 0) {
    console.log(JSON.stringify({
      productionSharedMemoryBuild: 'built',
      asyncify: 'blocked',
      reason: `Asyncify exited ${asyncify.status ?? asyncify.signal}`,
    }, null, 2));
    return false;
  }

  const module = new WebAssembly.Module(readFileSync(output));
  const memoryImports = WebAssembly.Module.imports(module).filter((entry) => entry.kind === 'memory');
  assert.equal(memoryImports.length, 1, 'asyncified production module must retain one imported memory');
  const memory = new WebAssembly.Memory({
    initial: runtimeInitialPages,
    maximum: runtimeMaximumPages,
    shared: true,
  });
  const instance = instantiate(module, memory);
  assert.ok(instance.exports.asyncify_start_unwind, 'asyncify exports must survive shared-memory build');
  console.log(JSON.stringify({
    productionSharedMemoryImport: `${memoryImports[0].module}.${memoryImports[0].name}`,
    asyncifyExportsPresent: true,
    memoryBytes: memory.buffer.byteLength,
  }, null, 2));
  return true;
}

if (!isMainThread) {
  const { mode, module, memory, barrier, iterations } = workerData;
  const instance = instantiate(module, memory);
  if (mode === 'alias') {
    parentPort.postMessage({ type: 'ready' });
    Atomics.wait(new Int32Array(barrier), 0, 0);
    for (let i = 0; i < iterations; i++) instance.exports.atomic_increment();
    parentPort.postMessage({ type: 'done', atomicCount: iterations });
  } else {
    instance.exports.__stack_pointer.value = workerData.stackTop;
    parentPort.postMessage({ type: 'ready', stackPointer: instance.exports.__stack_pointer.value });
    Atomics.wait(new Int32Array(barrier), 0, 0);
    let result = 0;
    for (let i = 0; i < iterations; i++) {
      result = instance.exports.worker_slice(
        workerData.contextAddress,
        workerData.counterAddress,
        workerData.activeAddress,
        workerData.maximumActiveAddress,
        workerData.workerIndex === 0 ? 0x101 : 0x202,
      );
    }
    parentPort.postMessage({ type: 'done', result });
  }
} else {
  await runProbe();
  if (process.argv.includes('--runtime')) runProductionBuild();
}
