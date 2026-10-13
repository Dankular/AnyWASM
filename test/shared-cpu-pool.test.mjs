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
