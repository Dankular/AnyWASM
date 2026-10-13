import assert from 'node:assert/strict';
import { test } from 'node:test';
import { SharedCpuCompletion, SharedCpuWorkerPool } from '../web/shared-cpu-pool.js';

const PAGE_BYTES = 4096;

function makePool({ workerCount = 1, threadIds = [] } = {}) {
  const contextCount = Math.max(1, threadIds.length);
  const pool = new SharedCpuWorkerPool({ workerCount, contextCount, pageCapacity: 2 });
  pool.memory = new WebAssembly.Memory({ initial: 1, maximum: 2, shared: true });
  const contextOffset = Math.ceil((128 + workerCount * 64) / 64) * 64;
  pool.layout = {
    base: 0,
    totalBytes: 12 * PAGE_BYTES,
    slots: { offset: 128, length: workerCount * 64 },
    contexts: { offset: contextOffset, length: contextCount * 784 },
    activity: { offset: 84, length: 8 },
    pageMap: { offset: 2048, length: 32 },
    dirtyMap: { offset: 2080, length: 1 },
    frames: { offset: PAGE_BYTES, length: 2 * PAGE_BYTES },
  };
  pool.dirtyMap = new Uint8Array(pool.memory.buffer, pool.layout.dirtyMap.offset, 1);
  pool.busy = Array(workerCount).fill(false);
  pool.trapRange = { trapStart: 0x1000n, trapEnd: 0x1010n };
  pool.initialized = true;
  pool.publish({
    snapshot: {
      frameBase: pool.layout.frames.offset,
      pageCount: 2,
      mappingGeneration: 1,
      mappings: new Uint8Array(32).fill(0x11),
      frames: new Uint8Array(2 * PAGE_BYTES).fill(0x11),
    },
    contexts: threadIds.map((threadId, index) => ({
      threadId,
      bytes: new Uint8Array(784).fill(0x11 * (index + 1)),
    })),
  });
  return pool;
}

test('dispatches distinct contexts together within one process-wide budget', async () => {
  const pool = makePool({ workerCount: 2, threadIds: [10, 20] });
  const sent = [];
  let inFlight = 0;
  let maximumInFlight = 0;
  const requests = [];
  pool.request = (workerIndex, expectedType, message) => new Promise((resolve) => {
    assert.equal(expectedType, 'complete');
    inFlight++;
    maximumInFlight = Math.max(maximumInFlight, inFlight);
    sent.push({ workerIndex, message });
    requests.push(() => {
      const slot = new Int32Array(pool.memory.buffer, message.slotOffset, 16);
      const reserved = Atomics.load(slot, 4) >>> 0;
      Atomics.store(slot, 9, reserved);
      Atomics.store(slot, 10, 0);
      Atomics.store(slot, 0, SharedCpuCompletion.QUANTUM);
      inFlight--;
      resolve({ sequence: message.sequence });
    });
  });

  const pending = pool.dispatchBatch([
    { threadId: 10, instructionBudget: 80 },
    { threadId: 20, instructionBudget: 60 },
  ], 100);

  assert.equal(sent.length, 2, 'both workers must receive work before completion is awaited');
  assert.equal(maximumInFlight, 2, 'two worker requests must be outstanding at once');
  assert.deepEqual(sent.map(({ workerIndex }) => workerIndex), [0, 1]);
  assert.deepEqual(sent.map(({ message }) => message.slotIndex), [0, 1]);
  assert.deepEqual(sent.map(({ message }) => {
    const slot = new Int32Array(pool.memory.buffer, message.slotOffset, 16);
    return {
      threadId: Atomics.load(slot, 3) >>> 0,
      contextOffset: Atomics.load(slot, 2) >>> 0,
      budget: Atomics.load(slot, 4) >>> 0,
    };
  }), [
    { threadId: 10, contextOffset: pool.layout.contexts.offset, budget: 80 },
    { threadId: 20, contextOffset: pool.layout.contexts.offset + 784, budget: 20 },
  ]);
  for (const complete of requests) complete();
  const completions = await pending;

  assert.deepEqual(completions.map(({ threadId, retiredInstructions }) => [threadId, retiredInstructions]), [
    [10, 80n],
    [20, 20n],
  ]);
  assert.deepEqual(completions.map(({ context }) => context[0]), [0x11, 0x22]);
  assert.equal(inFlight, 0);
  assert.deepEqual(pool.busy, [false, false]);
  assert.deepEqual([...pool.activeThreads], []);
});

test('reports the maximum concurrent worker slices for a completed batch', async () => {
  const pool = makePool({ workerCount: 2, threadIds: [10, 20] });
  const activity = new Int32Array(
    pool.memory.buffer,
    pool.layout.base + pool.layout.activity.offset,
    2,
  );
  const requests = [];
  pool.request = (workerIndex, expectedType, message) => new Promise((resolve) => {
    assert.equal(expectedType, 'complete');
    const active = Atomics.add(activity, 0, 1) + 1;
    let maximum = Atomics.load(activity, 1);
    while (active > maximum) {
      const previous = Atomics.compareExchange(activity, 1, maximum, active);
      if (previous === maximum) break;
      maximum = previous;
    }
    requests.push(() => {
      const slot = new Int32Array(pool.memory.buffer, message.slotOffset, 16);
      Atomics.store(slot, 9, 8);
      Atomics.store(slot, 10, 0);
      Atomics.store(slot, 0, SharedCpuCompletion.QUANTUM);
      Atomics.sub(activity, 0, 1);
      resolve({ sequence: message.sequence });
    });
  });

  Atomics.store(activity, 0, 5);
  Atomics.store(activity, 1, 9);
  const pending = pool.dispatchBatch([
    { threadId: 10, instructionBudget: 80 },
    { threadId: 20, instructionBudget: 80 },
  ], 160);
  requests.forEach((complete) => complete());
  const completions = await pending;

  assert.deepEqual(completions.map(({ maxConcurrentWorkerSlices }) => maxConcurrentWorkerSlices), [2, 2]);
  assert.equal(Atomics.load(activity, 0), 0);
});

test('holds an early HLE completion until every sibling slice reaches a safe point', async () => {
  const pool = makePool({ workerCount: 2, threadIds: [10, 20] });
  const requests = [];
  pool.request = (workerIndex, expectedType, message) => new Promise((resolve) => {
    assert.equal(expectedType, 'complete');
    requests.push({ workerIndex, message, resolve });
  });

  const importedContexts = [];
  let coordinatorMutation = false;
  const coordinator = pool.dispatchBatch([
    { threadId: 10, instructionBudget: 80 },
    { threadId: 20, instructionBudget: 80 },
  ], 160).then((completions) => {
    pool.importTo({
      importSharedCpuContext(threadId) { importedContexts.push(threadId); },
    });
    coordinatorMutation = true;
    return completions;
  });

  const finish = ({ workerIndex, message, resolve }, state, retired) => {
    const slot = new Int32Array(pool.memory.buffer, message.slotOffset, 16);
    Atomics.store(slot, 9, retired);
    Atomics.store(slot, 10, 0);
    Atomics.store(slot, 0, state);
    resolve({ sequence: message.sequence });
  };

  assert.equal(requests.length, 2, 'both sibling slices must be in flight');
  finish(requests[0], SharedCpuCompletion.HLE, 7);
  await Promise.resolve();

  assert.equal(coordinatorMutation, false, 'the coordinator must not import an early HLE result');
  assert.deepEqual(importedContexts, []);
  assert.throws(
    () => pool.importTo({}),
    /shared CPU memory can only be changed at a worker safe point/,
  );

  finish(requests[1], SharedCpuCompletion.QUANTUM, 9);
  const completions = await coordinator;

  assert.equal(coordinatorMutation, true);
  assert.deepEqual(importedContexts, [10, 20]);
  assert.deepEqual(completions.map(({ threadId, state, retiredInstructions }) => [
    threadId, state, retiredInstructions,
  ]), [
    [10, SharedCpuCompletion.HLE, 7n],
    [20, SharedCpuCompletion.QUANTUM, 9n],
  ]);
});

test('rejects duplicate guest contexts before claiming any worker', async () => {
  const pool = makePool({ workerCount: 2, threadIds: [10] });
  let sends = 0;
  pool.request = () => { sends++; return Promise.resolve({}); };

  await assert.rejects(pool.dispatchBatch([
    { threadId: 10, instructionBudget: 80 },
    { threadId: 10, instructionBudget: 20 },
  ], 100), /thread 10 appears more than once in a batch/);

  assert.equal(sends, 0);
  assert.deepEqual(pool.busy, [false, false]);
  assert.deepEqual([...pool.activeThreads], []);
});

test('drains a batch after one worker errors and disables only its slot', async () => {
  const pool = makePool({ workerCount: 2, threadIds: [10, 20] });
  const requests = [];
  pool.request = (workerIndex, expectedType, message) => new Promise((resolve, reject) => {
    assert.equal(expectedType, 'complete');
    requests.push(() => {
      if (workerIndex === 0) {
        reject(new Error('worker crashed during its slice'));
        return;
      }
      const slot = new Int32Array(pool.memory.buffer, message.slotOffset, 16);
      Atomics.store(slot, 9, 7);
      Atomics.store(slot, 10, 0);
      Atomics.store(slot, 0, SharedCpuCompletion.QUANTUM);
      resolve({ sequence: message.sequence });
    });
  });

  const pending = pool.dispatchBatch([
    { threadId: 10, instructionBudget: 80 },
    { threadId: 20, instructionBudget: 80 },
  ], 160);
  requests.forEach((complete) => complete());

  await assert.rejects(pending, /shared CPU batch failed/);
  assert.deepEqual(pool.busy, [true, false], 'only the failed worker slot remains unavailable');
  assert.deepEqual([...pool.activeThreads], [], 'both contexts return to coordinator ownership');
  const failedSlot = new Int32Array(pool.memory.buffer, pool.layout.slots.offset, 16);
  const completedSlot = new Int32Array(pool.memory.buffer, pool.layout.slots.offset + 64, 16);
  assert.equal(Atomics.load(failedSlot, 0), 9, 'the failed slot is disabled');
  assert.equal(Atomics.load(completedSlot, 0), 0, 'the successful sibling slot is reusable');
});

test('rejects shared-worker startup without cross-origin isolation', async () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'crossOriginIsolated');
  Object.defineProperty(globalThis, 'crossOriginIsolated', {
    configurable: true,
    value: false,
  });
  try {
    await assert.rejects(SharedCpuWorkerPool.create({}), /require cross-origin isolation and SharedArrayBuffer/);
  } finally {
    if (previous) Object.defineProperty(globalThis, 'crossOriginIsolated', previous);
    else delete globalThis.crossOriginIsolated;
  }
});

test('terminates every worker when startup fails after a sibling is ready', async () => {
  const previousIsolation = Object.getOwnPropertyDescriptor(globalThis, 'crossOriginIsolated');
  const previousWorker = Object.getOwnPropertyDescriptor(globalThis, 'Worker');
  const terminated = [];
  let nextWorker = 0;
  class FakeWorker {
    constructor() {
      this.index = nextWorker++;
    }
    postMessage(message) {
      if (message.type !== 'init') return;
      queueMicrotask(() => {
        if (this.index === 0) {
          this.onmessage({ data: {
            type: 'ready',
            workerIndex: this.index,
            heapBase: 1024,
            initialStackPointer: 2048,
          } });
        } else {
          this.onmessage({ data: {
            type: 'worker-error',
            workerIndex: this.index,
            message: 'worker startup failed',
          } });
        }
      });
    }
    terminate() {
      terminated.push(this.index);
    }
  }
  Object.defineProperty(globalThis, 'crossOriginIsolated', {
    configurable: true,
    value: true,
  });
  Object.defineProperty(globalThis, 'Worker', {
    configurable: true,
    value: FakeWorker,
  });
  try {
    const module = new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]));
    await assert.rejects(SharedCpuWorkerPool.create({
      workerWasm: module,
      workerCount: 2,
      contextCount: 2,
      pageCount: 1,
    }), /worker startup failed/);
    assert.deepEqual(terminated, [0, 1], 'startup failure must terminate ready and failing workers');
  } finally {
    if (previousIsolation) Object.defineProperty(globalThis, 'crossOriginIsolated', previousIsolation);
    else delete globalThis.crossOriginIsolated;
    if (previousWorker) Object.defineProperty(globalThis, 'Worker', previousWorker);
    else delete globalThis.Worker;
  }
});

test('disposal rejects pending worker requests and terminates every worker', async () => {
  const pool = makePool({ workerCount: 2 });
  let rejectPending;
  const pending = new Promise((_, reject) => { rejectPending = reject; });
  pending.catch(() => {});
  let terminations = 0;
  pool.workers = [
    { terminate() { terminations++; } },
    { terminate() { terminations++; } },
  ];
  pool.waiters = [{ reject: rejectPending }, null];
  pool.busy = [true, false];

  pool.dispose();

  await assert.rejects(pending, /shared CPU worker pool was disposed/);
  assert.equal(terminations, 2);
  assert.deepEqual(pool.busy, [true, true]);
  assert.throws(() => pool.requireInitialized(), /shared CPU worker pool is disposed/);
});

test('imports only worker-dirty pages, including both sides of a page boundary', () => {
  const pool = makePool();
  const sharedFrames = new Uint8Array(pool.memory.buffer, pool.layout.frames.offset, pool.layout.frames.length);
  sharedFrames.fill(0xa1, 0, PAGE_BYTES);
  sharedFrames.fill(0xb2, PAGE_BYTES, 2 * PAGE_BYTES);
  Atomics.or(pool.dirtyMap, 0, 0b11);
  let imported;
  const anyWasm = {
    importSharedSnapshot() { throw new Error('full snapshot import should not run'); },
    importSharedDirtySnapshot(snapshot) { imported = snapshot; },
    importSharedCpuContext() { throw new Error('there are no CPU contexts'); },
  };

  pool.importTo(anyWasm);

  assert.deepEqual([...imported.dirtyPages], [0, 1]);
  assert.equal(imported.mappingGeneration, 1);
  assert.equal('mappings' in imported, false);
  assert.equal(imported.frames.length, 2 * PAGE_BYTES);
  assert.equal(imported.frames[0], 0xa1);
  assert.equal(imported.frames[PAGE_BYTES], 0xb2);
  assert.equal(Atomics.load(pool.dirtyMap, 0), 0);
});

test('publishes only coordinator-dirty pages when mappings are unchanged', () => {
  const pool = makePool();
  const delta = {
    frameBase: pool.layout.frames.offset,
    pageCount: 2,
    mappingGeneration: 1,
    dirtyPages: new Uint32Array([1]),
    frames: new Uint8Array(PAGE_BYTES).fill(0x22),
  };
  let acknowledged;
  const anyWasm = {
    sharedSnapshotPageCount: () => 2n,
    sharedSnapshotMappingGeneration: () => 1,
    exportSharedDelta: () => delta,
    exportSharedSnapshot() { throw new Error('unchanged mappings should use the delta'); },
    sharedCpuRunnableThreadIds: () => [],
    sharedCpuTrapRange: () => ({ trapStart: 0n, trapEnd: 1n }),
    clearSharedSnapshotDirtyPages(pages) { acknowledged = [...pages]; },
    clearAllSharedSnapshotDirty() { throw new Error('full snapshot should not run'); },
  };

  pool.publishFrom(anyWasm);

  const frames = new Uint8Array(pool.memory.buffer, pool.layout.frames.offset, pool.layout.frames.length);
  assert.equal(frames[0], 0x11);
  assert.equal(frames[PAGE_BYTES], 0x22);
  assert.deepEqual(acknowledged, [1]);
});

test('performs a full frame resync when a mapping changes', () => {
  const pool = makePool();
  const mappings = pool.snapshot.mappings.slice();
  const fullFrames = new Uint8Array(2 * PAGE_BYTES);
  fullFrames.fill(0x33, 0, PAGE_BYTES);
  fullFrames.fill(0x44, PAGE_BYTES);
  let clearedAll = false;
  const anyWasm = {
    sharedSnapshotPageCount: () => 2n,
    sharedSnapshotMappingGeneration: () => 2,
    exportSharedDelta: () => ({
      frameBase: pool.layout.frames.offset,
      pageCount: 2,
      mappingGeneration: 2,
      dirtyPages: new Uint32Array(),
      frames: new Uint8Array(),
    }),
    exportSharedSnapshot: () => ({
      frameBase: pool.layout.frames.offset,
      pageCount: 2,
      mappingGeneration: 2,
      mappings,
      frames: fullFrames,
    }),
    sharedCpuRunnableThreadIds: () => [],
    sharedCpuTrapRange: () => ({ trapStart: 0n, trapEnd: 1n }),
    clearSharedSnapshotDirtyPages() { throw new Error('full resync clears all host dirtiness'); },
    clearAllSharedSnapshotDirty() { clearedAll = true; },
  };

  pool.publishFrom(anyWasm);

  const sharedFrames = new Uint8Array(pool.memory.buffer, pool.layout.frames.offset, pool.layout.frames.length);
  const sharedMappings = new Uint8Array(pool.memory.buffer, pool.layout.pageMap.offset, 32);
  assert.equal(sharedFrames[0], 0x33);
  assert.equal(sharedFrames[PAGE_BYTES], 0x44);
  assert.equal(sharedMappings[0], 0x11);
  assert.equal(clearedAll, true);
});
