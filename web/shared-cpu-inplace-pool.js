import { SharedCpuWorkerPool } from './shared-cpu-pool.js';

// Zero-copy variant of SharedCpuWorkerPool.
//
// The runtime is linked against an imported shared WebAssembly.Memory (see
// scripts/build-shared-runtime.sh), so the guest frame store already lives in memory that CPU
// workers can address. Instead of copying frames into a private region and back, the pool:
//   * points the ABI header's frames region at the runtime's live frame store,
//   * asks the runtime to write the sorted page table with frame offsets into that store, and
//   * learns which frames workers wrote from the worker dirty bitmap, whose bit index is the
//     frame index, so only executable-page writes invalidate translations.
// Only 16 bytes per mapped page plus the CPU contexts are written when the mapping changes.
//
// Fixed low-memory layout (reserved by the link flags of both modules):
//   [0x100000, 0x104000)  ABI header and worker slots (this pool)
//   [0x104000, 0x120000)  CPU worker static data (worker --global-base)
//   [0x120000, ...)       runtime static data, then its heap (contexts, page map, stacks, frames)

const HEADER_BASE = 0x100000;
const WORKER_DATA_END_LIMIT = 0x120000;
const HEADER_BYTES = 128;
const SLOT_BYTES = 64;
const CPU_BYTES = 784;
const PAGE_BYTES = 4096;
const WORKER_STACK_BYTES = 1024 * 1024;
const MAPPING_BYTES = 16;
const ABI_MAGIC = 0x4853_5741;
const ABI_VERSION = 2;

const alignUp = (value, boundary) => Math.ceil(value / boundary) * boundary;

export class InPlaceSharedCpuPool extends SharedCpuWorkerPool {
  static async create({ workerWasm, workerCount, contextCount, anyWasm, workerUrl } = {}) {
    if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer !== 'function') {
      throw new Error('shared CPU workers require cross-origin isolation and SharedArrayBuffer');
    }
    if (typeof Worker !== 'function') throw new Error('Web Workers are unavailable');
    const memory = anyWasm?.x?.memory;
    if (!(memory instanceof WebAssembly.Memory) || !(memory.buffer instanceof SharedArrayBuffer)) {
      throw new Error('in-place shared CPU workers need a runtime built over a shared WebAssembly.Memory');
    }
    if (!Number.isInteger(workerCount) || workerCount < 1 || workerCount > 16) {
      throw new RangeError('workerCount must be between 1 and 16');
    }
    if (HEADER_BYTES + workerCount * SLOT_BYTES > 0x4000) throw new RangeError('too many workers for the header block');
    if (!Number.isInteger(contextCount) || contextCount < 1) throw new RangeError('contextCount must be positive');
    if (!(workerWasm instanceof WebAssembly.Module)) workerWasm = await WebAssembly.compile(workerWasm);
    const pool = new InPlaceSharedCpuPool({
      module: workerWasm,
      memory,
      anyWasm,
      workerCount,
      contextCount,
      pageCount: 0,
      pageCapacity: 0,
      workerUrl: workerUrl || new URL('./shared-cpu-worker.js', import.meta.url),
    });
    try {
      await pool.initialize();
      return pool;
    } catch (error) {
      pool.dispose();
      throw error;
    }
  }

  alloc(bytes, align = 64) {
    const raw = this.anyWasm.x.aw_alloc(bytes + align) >>> 0;
    const aligned = alignUp(raw, align);
    this.allocations.push({ raw, bytes: bytes + align });
    return { raw, address: aligned, bytes };
  }

  release(block) {
    if (!block) return;
    this.anyWasm.x.aw_free(block.raw, block.bytes + 64);
    this.allocations = this.allocations.filter((entry) => entry.raw !== block.raw);
  }

  async initialize() {
    this.allocations = [];
    const ready = [];
    for (let workerIndex = 0; workerIndex < this.workerCount; workerIndex++) {
      const worker = new Worker(this.workerUrl, { type: 'module' });
      this.workers.push(worker);
      this.waiters.push(null);
      this.busy.push(false);
      worker.onmessage = (event) => this.onWorkerMessage(workerIndex, event.data);
      worker.onerror = (event) => this.failWorker(workerIndex, new Error(event.message || 'shared CPU worker failed'));
      ready.push(this.request(workerIndex, 'ready', {
        type: 'init', workerIndex, module: this.module, memory: this.memory,
      }));
    }
    const info = await Promise.all(ready);
    for (const { heapBase } of info) {
      if (heapBase < HEADER_BASE + 0x4000 || heapBase > WORKER_DATA_END_LIMIT) {
        throw new Error(`CPU worker static data ends at ${heapBase.toString(16)}, outside the reserved window; rebuild with scripts/build-shared-runtime.sh`);
      }
    }
    this.contextBlock = this.alloc(this.contextCount * CPU_BYTES, 64);
    const stackBlock = this.alloc(this.workerCount * WORKER_STACK_BYTES, 16);
    this.stackBlock = stackBlock;
    this.mapBlock = null;
    this.dirtyBlock = null;
    this.mapCapacityEntries = 0;
    this.dirtyCapacityBytes = 0;
    this.framesRegion = { address: 0, capacity: 0 };
    this.layout = {
      base: HEADER_BASE,
      totalBytes: 0,
      slots: { offset: HEADER_BYTES, length: this.workerCount * SLOT_BYTES },
      contexts: { offset: this.contextBlock.address - HEADER_BASE, length: this.contextCount * CPU_BYTES },
      pageMap: { offset: 0, length: 0 },
      dirtyMap: { offset: 0, length: 0 },
      frames: { offset: 0, length: 0 },
      stacks: { offset: stackBlock.address - HEADER_BASE, length: this.workerCount * WORKER_STACK_BYTES },
    };
    this.writeHeader();
    const stackReady = [];
    for (let workerIndex = 0; workerIndex < this.workerCount; workerIndex++) {
      const stackTop = stackBlock.address + (workerIndex + 1) * WORKER_STACK_BYTES;
      stackReady.push(this.request(workerIndex, 'stack-ready', { type: 'set-stack', stackTop }));
    }
    await Promise.all(stackReady);
    this.initialized = true;
  }

  /** (Re)write every header field from the current layout. Only called while quiescent. */
  writeHeader() {
    const { base, slots, contexts, pageMap, dirtyMap, frames } = this.layout;
    this.layout.totalBytes = this.memory.buffer.byteLength - base;
    const view = new DataView(this.memory.buffer, base, HEADER_BYTES);
    view.setUint32(0, ABI_MAGIC, true);
    view.setUint32(4, ABI_VERSION, true);
    view.setUint32(8, HEADER_BYTES, true);
    view.setUint32(12, Math.min(this.layout.totalBytes, 0xffff_ffff), true);
    view.setUint32(16, this.workerCount, true);
    view.setUint32(20, this.contextCount, true);
    for (const [offset, region] of [[24, slots], [32, contexts], [40, pageMap], [48, frames]]) {
      view.setUint32(offset, region.offset, true);
      view.setUint32(offset + 4, region.length, true);
    }
    view.setUint32(76, dirtyMap.offset, true);
    view.setUint32(80, dirtyMap.length, true);
    this.header = new Int32Array(this.memory.buffer, base, HEADER_BYTES / 4);
    if (!this.headerInitialized) {
      Atomics.store(this.header, 14, 0);
      Atomics.store(this.header, 15, 1);
      Atomics.store(this.header, 16, 1);
      Atomics.store(this.header, 17, 1);
      Atomics.store(this.header, 18, 0);
      this.headerInitialized = true;
    }
  }

  frameBase() {
    throw new Error('the in-place pool has no separate frame region to export into');
  }

  /** Make room for `pageCount` mapping records and a dirty bitmap covering `frameCapacity` bytes. */
  ensureCapacity(pageCount, frameCapacityBytes) {
    if (pageCount > this.mapCapacityEntries) {
      const entries = Math.max(pageCount + (pageCount >> 2), 4096);
      this.release(this.mapBlock);
      this.mapBlock = this.alloc(entries * MAPPING_BYTES, 64);
      this.mapCapacityEntries = entries;
      this.layout.pageMap = { offset: this.mapBlock.address - HEADER_BASE, length: entries * MAPPING_BYTES };
    }
    // The bitmap is indexed by frame index; the ABI also requires one bit per page-map slot.
    const dirtyBytes = Math.max(Math.ceil(frameCapacityBytes / PAGE_BYTES / 8) + 8, Math.ceil(this.mapCapacityEntries / 8));
    if (dirtyBytes > this.dirtyCapacityBytes) {
      const previous = this.dirtyMap;
      this.release(this.dirtyBlock);
      this.dirtyBlock = this.alloc(dirtyBytes, 64);
      const fresh = new Uint8Array(this.memory.buffer, this.dirtyBlock.address, dirtyBytes);
      fresh.fill(0);
      // Keep worker writes that have not been imported yet.
      if (previous) for (let index = 0; index < previous.length; index++) fresh[index] = Atomics.load(previous, index);
      this.dirtyCapacityBytes = dirtyBytes;
      this.layout.dirtyMap = { offset: this.dirtyBlock.address - HEADER_BASE, length: dirtyBytes };
      this.dirtyMap = new Uint8Array(this.memory.buffer, this.dirtyBlock.address, dirtyBytes);
    }
  }

  /** Publish the quiescent runtime's mappings (by reference) and runnable CPU contexts. */
  publishFrom(anyWasm) {
    this.requireQuiescent();
    const frames = anyWasm.sharedFramesInfo();
    const pageCount = Number(anyWasm.sharedSnapshotPageCount());
    const mappingGeneration = anyWasm.sharedSnapshotMappingGeneration();
    const threadIds = anyWasm.sharedCpuRunnableThreadIds();
    if (threadIds.length > this.contextCount) {
      throw new Error('guest thread count exceeded shared CPU context capacity');
    }
    this.trapRange = anyWasm.sharedCpuTrapRange();

    const regionChanged = frames.address !== this.framesRegion.address || frames.capacity !== this.framesRegion.capacity;
    const mappingChanged = !this.snapshot || regionChanged || pageCount !== this.snapshot.pageCount ||
      mappingGeneration !== this.snapshot.mappingGeneration;
    if (mappingChanged) {
      this.ensureCapacity(pageCount, frames.capacity);
      this.layout.frames = { offset: frames.address - HEADER_BASE, length: frames.capacity };
      this.framesRegion = { address: frames.address, capacity: frames.capacity };
      const written = anyWasm.exportSharedMappingsInPlace(this.mapBlock.address, this.mapBlock.bytes, HEADER_BASE);
      if (written !== pageCount) throw new Error(`in-place mapping export wrote ${written} of ${pageCount} pages`);
      this.writeHeader();
      Atomics.store(this.header, 14, pageCount);
      if (this.snapshot) Atomics.add(this.header, 15, 1);
      this.dirtyMap.fill(0);
    }
    Atomics.add(this.header, 16, 1); // code generation: the coordinator may have written code
    new Uint8Array(this.memory.buffer, this.contextBlock.address, this.contextBlock.bytes).fill(0);
    this.contextIndex.clear();
    threadIds.forEach((threadId, index) => {
      const bytes = anyWasm.exportSharedCpuContext(threadId);
      new Uint8Array(this.memory.buffer, this.contextBlock.address + index * CPU_BYTES, CPU_BYTES).set(bytes);
      this.contextIndex.set(threadId, index);
    });
    this.snapshot = { pageCount, mappingGeneration, frameBase: frames.address, mappings: null };
    this.pageCount = pageCount;
    return threadIds;
  }

  /** Collect frames workers wrote since the last import; no frame bytes move. */
  takeWorkerDirtyFrames() {
    const dirty = [];
    const words = this.dirtyCapacityBytes;
    for (let byteIndex = 0; byteIndex < words; byteIndex++) {
      const value = Atomics.load(this.dirtyMap, byteIndex);
      if (value === 0) continue;
      Atomics.and(this.dirtyMap, byteIndex, 0);
      for (let bit = 0; bit < 8; bit++) if (value & (1 << bit)) dirty.push(byteIndex * 8 + bit);
    }
    return Uint32Array.from(dirty);
  }

  /** Import returned CPU contexts and invalidate translations for worker-written code pages. */
  importTo(anyWasm) {
    this.requireQuiescent();
    const dirty = this.takeWorkerDirtyFrames();
    this.workerDirtyFrames = (this.workerDirtyFrames || 0) + dirty.length;
    anyWasm.noteSharedWorkerWrites(dirty);
    this.importSharedCpuContexts(anyWasm, true);
  }

  dispatchClaimed(workerIndex, dispatch) {
    // Memory may have grown since the header was written; refresh the bound the worker validates.
    this.layout.totalBytes = this.memory.buffer.byteLength - HEADER_BASE;
    new DataView(this.memory.buffer, HEADER_BASE, HEADER_BYTES).setUint32(12, Math.min(this.layout.totalBytes, 0xffff_ffff), true);
    return super.dispatchClaimed(workerIndex, dispatch);
  }

  dispose() {
    super.dispose();
  }
}
