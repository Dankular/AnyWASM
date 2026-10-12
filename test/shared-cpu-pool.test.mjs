import assert from 'node:assert/strict';
import { test } from 'node:test';
import { SharedCpuWorkerPool } from '../web/shared-cpu-pool.js';

const PAGE_BYTES = 4096;

function makePool() {
  const pool = new SharedCpuWorkerPool({ workerCount: 1, contextCount: 1, pageCapacity: 2 });
  pool.memory = new WebAssembly.Memory({ initial: 1, maximum: 2, shared: true });
  pool.layout = {
    base: 0,
    totalBytes: 12 * PAGE_BYTES,
    slots: { offset: 128, length: 64 },
    contexts: { offset: 192, length: 784 },
    pageMap: { offset: 2048, length: 32 },
    dirtyMap: { offset: 2080, length: 1 },
    frames: { offset: PAGE_BYTES, length: 2 * PAGE_BYTES },
  };
  pool.dirtyMap = new Uint8Array(pool.memory.buffer, pool.layout.dirtyMap.offset, 1);
  pool.initialized = true;
  pool.publish({
    snapshot: {
      frameBase: pool.layout.frames.offset,
      pageCount: 2,
      mappingGeneration: 1,
      mappings: new Uint8Array(32).fill(0x11),
      frames: new Uint8Array(2 * PAGE_BYTES).fill(0x11),
    },
    contexts: [],
  });
  return pool;
}

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
    sharedCpuContextThreadIds: () => [],
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
    sharedCpuContextThreadIds: () => [],
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
