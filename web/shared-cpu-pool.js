const ABI_MAGIC = 0x4853_5741; // little-endian "AWSH"
const ABI_VERSION = 2;
const HEADER_BYTES = 128;
const SLOT_BYTES = 64;
const CPU_BYTES = 784;
const PAGE_BYTES = 4096;
const WASM_PAGE_BYTES = 65536;
const INITIAL_WASM_PAGES = 512;
const MAX_WASM_PAGES = 16384;
const WORKER_STACK_BYTES = 1024 * 1024;

const SLOT_IDLE = 0;
const SLOT_WRITING = 1;
const SLOT_READY = 2;
const SLOT_QUANTUM = 4;
const SLOT_HLE = 5;
const SLOT_FAULT = 6;
const SLOT_EXITED = 7;
const SLOT_FAILED = 8;
const SLOT_DISABLED = 9;

export const SharedCpuCompletion = Object.freeze({
  QUANTUM: SLOT_QUANTUM,
  HLE: SLOT_HLE,
  FAULT: SLOT_FAULT,
  EXITED: SLOT_EXITED,
  FAILED: SLOT_FAILED,
});

const align = (value, boundary) => Math.ceil(value / boundary) * boundary;
const u32 = (value, name) => {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new RangeError(`${name} must be a u32`);
  }
  return value >>> 0;
};

function layoutFor({ workerCount, contextCount, pageCount, base }) {
  const activity = { offset: 84, length: 8 };
  let cursor = HEADER_BYTES;
  const slots = { offset: cursor, length: workerCount * SLOT_BYTES };
  cursor = align(cursor + slots.length, 64);
  const contexts = { offset: cursor, length: contextCount * CPU_BYTES };
  cursor = align(cursor + contexts.length, 8);
  const pageMap = { offset: cursor, length: pageCount * 16 };
  cursor = align(cursor + pageMap.length, 8);
  const dirtyMap = { offset: cursor, length: Math.ceil(pageCount / 8) };
  cursor = align(cursor + dirtyMap.length, PAGE_BYTES);
  const frames = { offset: cursor, length: pageCount * PAGE_BYTES };
  cursor = align(cursor + frames.length, 16);
  const stacks = { offset: cursor, length: workerCount * WORKER_STACK_BYTES };
  cursor += stacks.length;
  if (cursor > 0xffff_ffff || base + cursor > MAX_WASM_PAGES * WASM_PAGE_BYTES) {
    throw new RangeError('shared CPU worker memory exceeds its 1 GiB maximum');
  }
  return { base, totalBytes: cursor, slots, contexts, pageMap, dirtyMap, frames, stacks, activity };
}

/**
 * Host-side owner of the shared guest-frame memory and CPU worker slots.
 *
 * This pool is a building block for the process coordinator. It does not schedule guest threads or
 * perform HLE itself: callers publish quiescent AnyWasm snapshots, dispatch a context, then return
 * completed contexts and worker-written frames to the coordinator.
 */
export class SharedCpuWorkerPool {
  static async create({ workerWasm, workerCount, contextCount, pageCount, pageCapacity = pageCount, workerUrl } = {}) {
    if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer !== 'function') {
      throw new Error('shared CPU workers require cross-origin isolation and SharedArrayBuffer');
    }
    if (typeof Worker !== 'function') throw new Error('Web Workers are unavailable');
    if (!Number.isInteger(workerCount) || workerCount < 1 || workerCount > 64) {
      throw new RangeError('workerCount must be between 1 and 64');
    }
    if (!Number.isInteger(contextCount) || contextCount < 1 || contextCount > 0xffff_ffff) {
      throw new RangeError('contextCount must be a positive u32');
    }
    if (!Number.isInteger(pageCount) || pageCount < 1 || pageCount > 0xffff_ffff) {
      throw new RangeError('pageCount must be a positive u32');
    }
    if (!Number.isInteger(pageCapacity) || pageCapacity < pageCount || pageCapacity > 0xffff_ffff) {
      throw new RangeError('pageCapacity must be a u32 no smaller than pageCount');
    }
    if (!(workerWasm instanceof WebAssembly.Module)) {
      if (!(workerWasm instanceof ArrayBuffer) && !ArrayBuffer.isView(workerWasm)) {
        throw new TypeError('workerWasm must be a WebAssembly.Module or byte buffer');
      }
      workerWasm = await WebAssembly.compile(workerWasm);
    }
    const memory = new WebAssembly.Memory({
      initial: INITIAL_WASM_PAGES,
      maximum: MAX_WASM_PAGES,
      shared: true,
    });
    const pool = new SharedCpuWorkerPool({
      module: workerWasm,
      memory,
      workerCount,
      contextCount,
      pageCount,
      pageCapacity,
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

  constructor(options) {
    Object.assign(this, options);
    this.workers = [];
    this.waiters = [];
    this.busy = [];
    this.contextIndex = new Map();
    this.activeThreads = new Set();
    this.initialized = false;
    this.disposed = false;
  }

  async initialize() {
    const ready = [];
    for (let workerIndex = 0; workerIndex < this.workerCount; workerIndex++) {
      const worker = new Worker(this.workerUrl, { type: 'module' });
      this.workers.push(worker);
      this.waiters.push(null);
      this.busy.push(false);
      worker.onmessage = (event) => this.onWorkerMessage(workerIndex, event.data);
      worker.onerror = (event) => this.failWorker(workerIndex, new Error(event.message || 'shared CPU worker failed'));
      ready.push(this.request(workerIndex, 'ready', {
        type: 'init',
        workerIndex,
        module: this.module,
        memory: this.memory,
      }));
    }
    const workerInfo = await Promise.all(ready);
    const firstBase = Math.max(...workerInfo.map((info) => Math.max(info.heapBase, info.initialStackPointer)));
    const base = align(firstBase, PAGE_BYTES);
    this.layout = layoutFor({
      workerCount: this.workerCount,
      contextCount: this.contextCount,
      pageCount: this.pageCapacity,
      base,
    });
    const requiredPages = Math.ceil((base + this.layout.totalBytes) / WASM_PAGE_BYTES);
    const currentPages = this.memory.buffer.byteLength / WASM_PAGE_BYTES;
    if (requiredPages > currentPages) this.memory.grow(requiredPages - currentPages);
    this.initializeHeader();
    const stackReady = [];
    for (let workerIndex = 0; workerIndex < this.workerCount; workerIndex++) {
      const stackTop = this.layout.base + this.layout.stacks.offset + (workerIndex + 1) * WORKER_STACK_BYTES;
      stackReady.push(this.request(workerIndex, 'stack-ready', { type: 'set-stack', stackTop }));
    }
    await Promise.all(stackReady);
    this.initialized = true;
  }

  initializeHeader() {
    const { base, totalBytes, slots, contexts, pageMap, dirtyMap, frames } = this.layout;
    const bytes = new Uint8Array(this.memory.buffer, base, totalBytes);
    bytes.fill(0);
    const view = new DataView(this.memory.buffer, base, HEADER_BYTES);
    view.setUint32(0, ABI_MAGIC, true);
    view.setUint32(4, ABI_VERSION, true);
    view.setUint32(8, HEADER_BYTES, true);
    view.setUint32(12, totalBytes, true);
    view.setUint32(16, this.workerCount, true);
    view.setUint32(20, this.contextCount, true);
    for (const [offset, region] of [[24, slots], [32, contexts], [40, pageMap], [48, frames]]) {
      view.setUint32(offset, region.offset, true);
      view.setUint32(offset + 4, region.length, true);
    }
    view.setUint32(76, dirtyMap.offset, true);
    view.setUint32(80, dirtyMap.length, true);
    const header = new Int32Array(this.memory.buffer, base, HEADER_BYTES / 4);
    Atomics.store(header, 14, this.pageCount);
    Atomics.store(header, 15, 1); // mapping generation
    Atomics.store(header, 16, 1); // code generation
    Atomics.store(header, 17, 1); // dispatch generation
    Atomics.store(header, 18, 0); // flags
    this.dirtyMap = new Uint8Array(this.memory.buffer, base + dirtyMap.offset, dirtyMap.length);
  }

  /** Return the offset to pass as `frameBase` to AnyWasm.exportSharedSnapshot. */
  frameBase() {
    this.requireInitialized();
    return this.layout.frames.offset;
  }

  /** Publish matching mappings, frames, and per-thread contexts before starting workers. */
  publish({ snapshot, contexts }) {
    this.requireQuiescent();
    const snapshotPageCount = Number(snapshot?.pageCount);
    if (!snapshot || snapshot.frameBase !== this.layout.frames.offset ||
        !Number.isSafeInteger(snapshotPageCount) || snapshotPageCount < 0 || snapshotPageCount > this.pageCapacity ||
        !(snapshot.mappings instanceof Uint8Array) || !(snapshot.frames instanceof Uint8Array) ||
        snapshot.mappings.byteLength !== snapshotPageCount * 16 ||
        snapshot.frames.byteLength !== snapshotPageCount * PAGE_BYTES) {
      throw new RangeError('guest-memory snapshot does not match the shared worker layout');
    }
    if (!Array.isArray(contexts) || contexts.length > this.contextCount) {
      throw new RangeError('guest CPU contexts exceed the shared context capacity');
    }
    const threadIds = new Set();
    for (const { threadId, bytes } of contexts) {
      u32(threadId, 'threadId');
      if (!(bytes instanceof Uint8Array) || bytes.byteLength !== CPU_BYTES || threadIds.has(threadId)) {
        throw new TypeError('each shared CPU context must have a unique thread ID and 784 bytes');
      }
      threadIds.add(threadId);
    }
    const mappingsView = new Uint8Array(
      this.memory.buffer,
      this.layout.base + this.layout.pageMap.offset,
      this.layout.pageMap.length,
    );
    const framesView = new Uint8Array(
      this.memory.buffer,
      this.layout.base + this.layout.frames.offset,
      this.layout.frames.length,
    );
    const firstPublish = !this.snapshot;
    const mappingGeneration = snapshot.mappingGeneration ?? this.snapshot?.mappingGeneration ?? 1;
    const mappingChanged = firstPublish || this.snapshot.pageCount !== snapshotPageCount ||
      mappingGeneration !== this.snapshot.mappingGeneration ||
      snapshot.mappings.some((byte, index) => byte !== this.snapshot.mappings[index]);
    const previousMapBytes = this.snapshot ? this.snapshot.pageCount * 16 : 0;
    const previousFrameBytes = this.snapshot ? this.snapshot.pageCount * PAGE_BYTES : 0;
    const nextMapBytes = snapshotPageCount * 16;
    const nextFrameBytes = snapshotPageCount * PAGE_BYTES;
    if (nextMapBytes < previousMapBytes) mappingsView.fill(0, nextMapBytes, previousMapBytes);
    if (nextFrameBytes < previousFrameBytes) framesView.fill(0, nextFrameBytes, previousFrameBytes);
    mappingsView.set(snapshot.mappings);
    framesView.set(snapshot.frames);
    this.dirtyMap.fill(0);
    const header = new Int32Array(this.memory.buffer, this.layout.base, HEADER_BYTES / 4);
    if (!firstPublish && mappingChanged) Atomics.add(header, 15, 1);
    if (!firstPublish) Atomics.add(header, 16, 1);
    Atomics.store(header, 14, snapshotPageCount);
    new Uint8Array(this.memory.buffer, this.layout.base + this.layout.contexts.offset, this.layout.contexts.length).fill(0);
    this.contextIndex.clear();
    for (let index = 0; index < contexts.length; index++) {
      const { threadId, bytes } = contexts[index];
      const offset = this.layout.base + this.layout.contexts.offset + index * CPU_BYTES;
      new Uint8Array(this.memory.buffer, offset, CPU_BYTES).set(bytes);
      this.contextIndex.set(threadId, index);
    }
    this.snapshot = {
      frameBase: snapshot.frameBase,
      pageCount: snapshotPageCount,
      mappingGeneration,
      mappings: snapshot.mappings.slice(),
    };
  }

  /** Export the current quiescent AnyWasm process into this pool's fixed layout. */
  publishFrom(anyWasm) {
    this.requireQuiescent();
    const pageCount = Number(anyWasm.sharedSnapshotPageCount());
    const mappingGeneration = anyWasm.sharedSnapshotMappingGeneration();
    if (pageCount > this.pageCapacity) throw new Error('guest mapping size exceeded shared CPU page capacity');
    const threadIds = anyWasm.sharedCpuRunnableThreadIds();
    if (threadIds.length > this.contextCount) {
      throw new Error('guest thread count exceeded shared CPU context capacity');
    }
    const contexts = threadIds.map((threadId) => ({
      threadId,
      bytes: anyWasm.exportSharedCpuContext(threadId),
    }));
    this.trapRange = anyWasm.sharedCpuTrapRange();
    const mappingChanged = !this.snapshot || this.snapshot.pageCount !== pageCount ||
      mappingGeneration !== this.snapshot.mappingGeneration;
    if (mappingChanged) {
      const snapshot = anyWasm.exportSharedSnapshot(this.layout.frames.offset);
      this.publish({ snapshot, contexts });
      anyWasm.clearAllSharedSnapshotDirty();
    } else {
      const delta = anyWasm.exportSharedDelta(this.layout.frames.offset);
      if (delta.pageCount !== pageCount || delta.frameBase !== this.layout.frames.offset ||
          delta.mappingGeneration !== mappingGeneration ||
          !(delta.dirtyPages instanceof Uint32Array) || !(delta.frames instanceof Uint8Array) ||
          delta.frames.byteLength !== delta.dirtyPages.length * PAGE_BYTES) {
        throw new RangeError('guest-memory delta does not match the shared worker layout');
      }
      this.publishDelta({ delta, contexts });
      if (delta.dirtyPages.length) anyWasm.clearSharedSnapshotDirtyPages(delta.dirtyPages);
    }
    return threadIds;
  }

  /** Publish only coordinator-written frames when the mapping generation is unchanged. */
  publishDelta({ delta, contexts }) {
    this.requireQuiescent();
    if (!this.snapshot || delta.pageCount !== this.snapshot.pageCount ||
        delta.mappingGeneration !== this.snapshot.mappingGeneration) {
      throw new Error('shared worker mapping changed during a dirty-page publication');
    }
    if (!Array.isArray(contexts) || contexts.length > this.contextCount) {
      throw new RangeError('guest CPU contexts exceed the shared context capacity');
    }
    let previous = -1;
    const framesView = new Uint8Array(
      this.memory.buffer,
      this.layout.base + this.layout.frames.offset,
      this.layout.frames.length,
    );
    for (let index = 0; index < delta.dirtyPages.length; index++) {
      const page = delta.dirtyPages[index];
      if (page <= previous || page >= delta.pageCount) {
        throw new RangeError('dirty mapping indices must be sorted, unique, and in range');
      }
      previous = page;
      const sourceStart = index * PAGE_BYTES;
      const targetStart = page * PAGE_BYTES;
      framesView.set(delta.frames.subarray(sourceStart, sourceStart + PAGE_BYTES), targetStart);
    }
    const header = new Int32Array(this.memory.buffer, this.layout.base, HEADER_BYTES / 4);
    Atomics.add(header, 16, 1);
    new Uint8Array(this.memory.buffer, this.layout.base + this.layout.contexts.offset, this.layout.contexts.length).fill(0);
    this.contextIndex.clear();
    for (let index = 0; index < contexts.length; index++) {
      const { threadId, bytes } = contexts[index];
      u32(threadId, 'threadId');
      if (!(bytes instanceof Uint8Array) || bytes.byteLength !== CPU_BYTES || this.contextIndex.has(threadId)) {
        throw new TypeError('each shared CPU context must have a unique thread ID and 784 bytes');
      }
      const offset = this.layout.base + this.layout.contexts.offset + index * CPU_BYTES;
      new Uint8Array(this.memory.buffer, offset, CPU_BYTES).set(bytes);
      this.contextIndex.set(threadId, index);
    }
  }

  /** Copy worker-written frames and their immutable mapping snapshot for coordinator import. */
  readGuestSnapshot() {
    this.requireQuiescent();
    if (!this.snapshot) throw new Error('shared CPU pool has no published guest snapshot');
    return {
      ...this.snapshot,
      frames: new Uint8Array(
        this.memory.buffer,
        this.layout.base + this.layout.frames.offset,
        this.snapshot.pageCount * PAGE_BYTES,
      ).slice(),
    };
  }

  /** Copy only worker-dirty pages after all active slices have completed. */
  readDirtyGuestSnapshot() {
    this.requireQuiescent();
    if (!this.snapshot) throw new Error('shared CPU pool has no published guest snapshot');
    const dirtyPages = [];
    for (let page = 0; page < this.snapshot.pageCount; page++) {
      const byteIndex = page >>> 3;
      const bit = 1 << (page & 7);
      if (Atomics.load(this.dirtyMap, byteIndex) & bit) dirtyPages.push(page);
    }
    const frames = new Uint8Array(dirtyPages.length * PAGE_BYTES);
    const sharedFrames = new Uint8Array(
      this.memory.buffer,
      this.layout.base + this.layout.frames.offset,
      this.layout.frames.length,
    );
    for (let index = 0; index < dirtyPages.length; index++) {
      const page = dirtyPages[index];
      frames.set(sharedFrames.subarray(page * PAGE_BYTES, (page + 1) * PAGE_BYTES), index * PAGE_BYTES);
    }
    return {
      frameBase: this.snapshot.frameBase,
      pageCount: this.snapshot.pageCount,
      mappingGeneration: this.snapshot.mappingGeneration,
      dirtyPages: Uint32Array.from(dirtyPages),
      frames,
    };
  }

  /** Import worker-written pages and contexts after every active slice has completed. */
  importTo(anyWasm) {
    this.requireQuiescent();
    const snapshot = this.readDirtyGuestSnapshot();
    if (snapshot.dirtyPages.length) {
      anyWasm.importSharedDirtySnapshot(snapshot);
      for (const page of snapshot.dirtyPages) {
        const byteIndex = page >>> 3;
        const bit = 1 << (page & 7);
        Atomics.and(this.dirtyMap, byteIndex, ~bit);
      }
    }
    this.importSharedCpuContexts(anyWasm, true);
  }

  /** Import returned CPU registers and counters without copying the guest frame store. */
  importSharedCpuContexts(anyWasm, syncTebAlias = false) {
    this.requireQuiescent();
    for (const [threadId, index] of this.contextIndex) {
      const context = new Uint8Array(this.memory.buffer, this.contextOffset(index), CPU_BYTES).slice();
      anyWasm.importSharedCpuContext(threadId, context, { syncTebAlias });
    }
  }

  /** Replace one quiescent context snapshot before its next dispatch. */
  publishContext(threadId, bytes) {
    this.requireQuiescentThread(threadId);
    if (!(bytes instanceof Uint8Array) || bytes.byteLength !== CPU_BYTES) {
      throw new TypeError(`CPU context must be exactly ${CPU_BYTES} bytes`);
    }
    const index = this.contextIndex.get(threadId);
    if (index === undefined) throw new RangeError(`unknown shared CPU context for thread ${threadId}`);
    new Uint8Array(this.memory.buffer, this.contextOffset(index), CPU_BYTES).set(bytes);
  }

  /** Dispatch one guest thread and resolve with its completion and returned CPU context. */
  dispatch(threadId, instructionBudget, options = {}) {
    const dispatch = this.prepareDispatch(threadId, instructionBudget, options);
    const workerIndex = this.busy.findIndex((isBusy) => !isBusy);
    if (workerIndex < 0) throw new Error('no shared CPU worker is available');
    this.claimWorkerSlot(workerIndex);
    this.advanceDispatchGeneration();
    this.busy[workerIndex] = true;
    this.activeThreads.add(dispatch.threadId);
    try {
      return this.dispatchClaimed(workerIndex, dispatch);
    } catch (error) {
      const slot = new Int32Array(
        this.memory.buffer,
        this.layout.base + this.layout.slots.offset + workerIndex * SLOT_BYTES,
        SLOT_BYTES / 4,
      );
      Atomics.compareExchange(slot, 0, SLOT_WRITING, SLOT_IDLE);
      this.busy[workerIndex] = false;
      this.activeThreads.delete(dispatch.threadId);
      throw error;
    }
  }

  prepareDispatch(threadId, instructionBudget, options = {}) {
    this.requireInitialized();
    u32(threadId, 'threadId');
    const budget = BigInt(instructionBudget);
    if (budget <= 0n || budget > 0xffff_ffff_ffff_ffffn) throw new RangeError('instructionBudget must be a positive u64');
    const firstTrapValue = options.trapStart ?? this.trapRange?.trapStart;
    const endTrapValue = options.trapEnd ?? this.trapRange?.trapEnd;
    if (firstTrapValue === undefined || endTrapValue === undefined) {
      throw new Error('an HLE trap range must be published before dispatch');
    }
    const firstTrap = BigInt(firstTrapValue);
    const endTrap = BigInt(endTrapValue);
    if (firstTrap < 0n || endTrap <= firstTrap || endTrap > 0xffff_ffff_ffff_ffffn) {
      throw new RangeError('trap range must be a nonempty u64 interval');
    }
    const processExit = BigInt(options.processExit ?? this.trapRange?.processExit ?? 0);
    const threadExit = BigInt(options.threadExit ?? this.trapRange?.threadExit ?? 0);
    for (const [name, address] of [['processExit', processExit], ['threadExit', threadExit]]) {
      if (address < 0n || address > 0xffff_ffff_ffff_ffffn ||
          (address !== 0n && (address < firstTrap || address >= endTrap))) {
        throw new RangeError(`${name} must be zero or an address inside the trap range`);
      }
    }
    if (processExit !== 0n && processExit === threadExit) {
      throw new RangeError('processExit and threadExit must be different addresses');
    }
    const contextIndex = this.contextIndex.get(threadId);
    if (contextIndex === undefined) throw new RangeError(`unknown shared CPU context for thread ${threadId}`);
    if (this.activeThreads.has(threadId)) throw new Error(`thread ${threadId} already has an active CPU slice`);
    return { threadId, budget, firstTrap, endTrap, processExit, threadExit, contextIndex };
  }

  claimWorkerSlot(workerIndex) {
    const slotOffset = this.layout.slots.offset + workerIndex * SLOT_BYTES;
    const slot = new Int32Array(this.memory.buffer, this.layout.base + slotOffset, SLOT_BYTES / 4);
    if (Atomics.compareExchange(slot, 0, SLOT_IDLE, SLOT_WRITING) !== SLOT_IDLE) {
      throw new Error(`shared CPU worker slot ${workerIndex} is unavailable`);
    }
  }

  advanceDispatchGeneration() {
    const header = new Int32Array(this.memory.buffer, this.layout.base, HEADER_BYTES / 4);
    Atomics.add(header, 17, 1);
  }

  dispatchClaimed(workerIndex, dispatch) {
    const { threadId, budget, firstTrap, endTrap, processExit, threadExit, contextIndex } = dispatch;
    const slotOffset = this.layout.slots.offset + workerIndex * SLOT_BYTES;
    const slot = new Int32Array(this.memory.buffer, this.layout.base + slotOffset, SLOT_BYTES / 4);
    const sequence = (Atomics.add(slot, 1, 1) + 1) >>> 0;
    const contextOffset = this.layout.contexts.offset + contextIndex * CPU_BYTES;
    const header = new Int32Array(this.memory.buffer, this.layout.base, HEADER_BYTES / 4);
    const mappingGeneration = Atomics.load(header, 15) >>> 0;
    const codeGeneration = Atomics.load(header, 16) >>> 0;
    const dispatchGeneration = Atomics.load(header, 17) >>> 0;
    Atomics.store(slot, 2, contextOffset);
    Atomics.store(slot, 3, threadId);
    Atomics.store(slot, 4, Number(budget & 0xffff_ffffn));
    Atomics.store(slot, 5, Number((budget >> 32n) & 0xffff_ffffn));
    Atomics.store(slot, 6, mappingGeneration);
    Atomics.store(slot, 7, codeGeneration);
    Atomics.store(slot, 8, dispatchGeneration);
    for (let index = 9; index < SLOT_BYTES / 4; index++) Atomics.store(slot, index, 0);
    Atomics.store(slot, 0, SLOT_READY);
    const result = this.request(workerIndex, 'complete', {
      type: 'run',
      sharedBase: this.layout.base,
      availableBytes: this.layout.totalBytes,
      slotIndex: workerIndex,
      slotOffset,
      activityOffset: this.layout.base + this.layout.activity.offset,
      sequence,
      trapStart: firstTrap,
      trapEnd: endTrap,
      processExit,
      threadExit,
    });
    return result.then((message) => ({
      ...this.takeCompletion(workerIndex, threadId, sequence, message),
      threadId,
      reservedInstructions: budget,
    })).catch((error) => {
      this.activeThreads.delete(threadId);
      const activeSlot = new Int32Array(
        this.memory.buffer,
        this.layout.base + slotOffset,
        SLOT_BYTES / 4,
      );
      Atomics.store(activeSlot, 0, SLOT_DISABLED);
      this.busy[workerIndex] = true;
      throw error;
    });
  }

  /** Dispatch distinct runnable contexts concurrently within one process-wide instruction budget. */
  async dispatchBatch(contexts, processBudget) {
    this.requireInitialized();
    if (!Array.isArray(contexts)) throw new TypeError('contexts must be an array');
    let remaining = BigInt(processBudget);
    if (remaining < 0n || remaining > 0xffff_ffff_ffff_ffffn) {
      throw new RangeError('processBudget must be a u64');
    }
    if (remaining === 0n || contexts.length === 0) return [];

    const idleWorkers = this.busy.reduce((count, isBusy) => count + Number(!isBusy), 0);
    if (contexts.length > idleWorkers) throw new Error('batch exceeds the available shared CPU workers');
    const threadIds = new Set();
    const dispatches = [];
    for (const context of contexts) {
      const threadId = u32(context?.threadId, 'threadId');
      if (threadIds.has(threadId)) throw new Error(`thread ${threadId} appears more than once in a batch`);
      const item = this.prepareDispatch(threadId, context.instructionBudget);
      const quantum = item.budget;
      if (quantum <= 0n || quantum > 0xffff_ffff_ffff_ffffn) {
        throw new RangeError(`instructionBudget for thread ${threadId} must be a positive u64`);
      }
      threadIds.add(threadId);
      if (remaining === 0n) break;
      const budget = quantum < remaining ? quantum : remaining;
      dispatches.push({ ...item, budget });
      remaining -= budget;
    }
    if (dispatches.length === 0) return [];

    const activity = new Int32Array(
      this.memory.buffer,
      this.layout.base + this.layout.activity.offset,
      2,
    );
    Atomics.store(activity, 0, 0);
    Atomics.store(activity, 1, 0);

    const workerIndices = this.busy
      .map((isBusy, index) => isBusy ? -1 : index)
      .filter((index) => index >= 0)
      .slice(0, dispatches.length);
    if (workerIndices.length !== dispatches.length) {
      throw new Error('batch exceeds the available shared CPU workers');
    }
    const claimed = [];
    try {
      for (const workerIndex of workerIndices) {
        this.claimWorkerSlot(workerIndex);
        claimed.push(workerIndex);
      }
    } catch (error) {
      for (const workerIndex of claimed) {
        const slot = new Int32Array(
          this.memory.buffer,
          this.layout.base + this.layout.slots.offset + workerIndex * SLOT_BYTES,
          SLOT_BYTES / 4,
        );
        Atomics.compareExchange(slot, 0, SLOT_WRITING, SLOT_IDLE);
      }
      throw error;
    }

    this.advanceDispatchGeneration();
    for (let index = 0; index < dispatches.length; index++) {
      this.busy[workerIndices[index]] = true;
      this.activeThreads.add(dispatches[index].threadId);
    }
    const pending = [];
    let synchronousFailure = null;
    for (let index = 0; index < dispatches.length; index++) {
      try {
        pending.push(this.dispatchClaimed(workerIndices[index], dispatches[index]));
      } catch (error) {
        synchronousFailure = error;
        for (let unsent = index; unsent < dispatches.length; unsent++) {
          const workerIndex = workerIndices[unsent];
          const slot = new Int32Array(
            this.memory.buffer,
            this.layout.base + this.layout.slots.offset + workerIndex * SLOT_BYTES,
            SLOT_BYTES / 4,
          );
          if (Atomics.compareExchange(slot, 0, SLOT_WRITING, SLOT_IDLE) === SLOT_WRITING) {
            this.busy[workerIndex] = false;
            this.activeThreads.delete(dispatches[unsent].threadId);
          }
        }
        break;
      }
    }

    const outcomes = await Promise.allSettled(pending);
    const failures = outcomes.filter((outcome) => outcome.status === 'rejected');
    if (failures.length || synchronousFailure) {
      throw new AggregateError([
        ...failures.map((outcome) => outcome.reason),
        ...(synchronousFailure ? [synchronousFailure] : []),
      ], 'shared CPU batch failed');
    }
    const maxConcurrentWorkerSlices = Atomics.load(activity, 1) >>> 0;
    return outcomes.map((outcome) => ({
      ...outcome.value,
      maxConcurrentWorkerSlices,
    }));
  }

  takeCompletion(workerIndex, threadId, sequence, message) {
    this.activeThreads.delete(threadId);
    const slotOffset = this.layout.base + this.layout.slots.offset + workerIndex * SLOT_BYTES;
    const slot = new Int32Array(this.memory.buffer, slotOffset, SLOT_BYTES / 4);
    const state = Atomics.load(slot, 0) >>> 0;
    const actualSequence = Atomics.load(slot, 1) >>> 0;
    if (message.sequence !== sequence || actualSequence !== sequence ||
        ![SLOT_QUANTUM, SLOT_HLE, SLOT_FAULT, SLOT_EXITED, SLOT_FAILED].includes(state)) {
      Atomics.store(slot, 0, SLOT_DISABLED);
      this.busy[workerIndex] = true;
      throw new Error(`shared CPU worker ${workerIndex} returned an invalid completion`);
    }
    const retired = BigInt(Atomics.load(slot, 9) >>> 0) | (BigInt(Atomics.load(slot, 10) >>> 0) << 32n);
    const payloadAddress = BigInt(Atomics.load(slot, 12) >>> 0) | (BigInt(Atomics.load(slot, 13) >>> 0) << 32n);
    const payloadValue = BigInt(Atomics.load(slot, 14) >>> 0) | (BigInt(Atomics.load(slot, 15) >>> 0) << 32n);
    const completion = {
      state,
      sequence,
      retiredInstructions: retired,
      stopReason: Atomics.load(slot, 11) >>> 0,
      payloadAddress,
      payloadValue,
      context: new Uint8Array(this.memory.buffer, this.contextOffsetForThread(threadId), CPU_BYTES).slice(),
    };
    const nextState = state === SLOT_FAILED ? SLOT_DISABLED : SLOT_IDLE;
    if (Atomics.compareExchange(slot, 0, state, nextState) !== state) {
      this.busy[workerIndex] = true;
      throw new Error(`shared CPU worker ${workerIndex} completion was consumed concurrently`);
    }
    this.busy[workerIndex] = nextState !== SLOT_IDLE;
    return completion;
  }

  request(workerIndex, expectedType, message) {
    return new Promise((resolve, reject) => {
      if (this.disposed) return reject(new Error('shared CPU worker pool is disposed'));
      if (this.waiters[workerIndex]) return reject(new Error(`worker ${workerIndex} already has a pending request`));
      this.waiters[workerIndex] = { expectedType, resolve, reject };
      this.workers[workerIndex].postMessage(message);
    });
  }

  onWorkerMessage(workerIndex, message) {
    const waiter = this.waiters[workerIndex];
    if (message.type === 'worker-error') {
      this.failWorker(workerIndex, new Error(message.message));
      return;
    }
    if (!waiter || message.type !== waiter.expectedType) return;
    this.waiters[workerIndex] = null;
    waiter.resolve(message);
  }

  failWorker(workerIndex, error) {
    const waiter = this.waiters[workerIndex];
    this.waiters[workerIndex] = null;
    this.busy[workerIndex] = true;
    if (waiter) waiter.reject(error);
  }

  contextOffset(index) {
    return this.layout.base + this.layout.contexts.offset + index * CPU_BYTES;
  }

  contextOffsetForThread(threadId) {
    return this.contextOffset(this.contextIndex.get(threadId));
  }

  requireInitialized() {
    if (this.disposed) throw new Error('shared CPU worker pool is disposed');
    if (!this.initialized) throw new Error('shared CPU worker pool is not initialized');
  }

  requireQuiescent() {
    this.requireInitialized();
    if (this.busy.some(Boolean)) throw new Error('shared CPU memory can only be changed at a worker safe point');
  }

  requireQuiescentThread(threadId) {
    this.requireQuiescent();
    if (this.activeThreads.has(threadId)) throw new Error(`thread ${threadId} has an active CPU slice`);
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    for (let index = 0; index < this.workers.length; index++) {
      this.waiters[index]?.reject(new Error('shared CPU worker pool was disposed'));
      this.waiters[index] = null;
      this.workers[index].terminate();
    }
    this.busy.fill(true);
  }
}
