// AnyWASM runtime glue: loads the WASM module and drives a Windows x64 process inside it.
// Works unchanged in browsers (main thread or worker) and Node >= 18.
import { fillCryptoRandom } from './entropy.js';
import { AssetRangeCache, fetchAssetIndex, sha256Hex } from './assets.js';
import { createDirectoryAssetIndex, DirectoryAssetSource, inspectDirectory, normalizeDirectoryPath } from './direct-assets.js';

export const Status = Object.freeze({ RUNNING: 0, EXITED: 1, UNIMPLEMENTED: 2, CRASHED: 3, FRAME: 4, IDLE: 5 });

const MAX_JIT_MODULES = 256;
const MAX_JIT_FAILURE_TEXT = 512;
const CHAIN_EAGER_MODULE_LIMIT = 16;
const CHAIN_REBUILD_MODULE_INTERVAL = 64;
const DEFAULT_JIT_MIN_PREFIX = 65; // decoded blocks are capped at 64 instructions
const ASYNCIFY_STACK_BYTES = 1024 * 1024;
const SHARED_CPU_CONTEXT_BYTES = 784;
const SHARED_CPU_CONTEXT_ALIGN = 16;

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}


const uleb128 = (value) => {
  const out = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    out.push(byte);
  } while (value !== 0);
  return out;
};

/**
 * Emitted modules import `anywasm.memory` as an unshared one-page minimum memory. When the runtime
 * lives in a shared memory the import must be declared shared with a maximum, so rewrite that one
 * import description (flags 0x00 -> 0x03, plus a 65,536-page maximum) and re-encode the import
 * section length. Anything that is not the expected single-memory-import layout is rejected.
 */
function withSharedMemoryImport(bytes) {
  const source = Uint8Array.from(bytes);
  let position = 8;
  while (position < source.length) {
    const id = source[position];
    let size = 0, shift = 0, cursor = position + 1;
    for (;;) {
      const byte = source[cursor++];
      size |= (byte & 0x7f) << shift;
      shift += 7;
      if ((byte & 0x80) === 0) break;
    }
    const end = cursor + size;
    if (id !== 2) { position = end; continue; }
    const payload = source.subarray(cursor, end);
    const tail = [0x02, 0x00, 0x01]; // kind=memory, flags=none, minimum=1 page
    const at = payload.length - tail.length;
    const memoryName = [0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79]; // "memory"
    if (payload[0] !== 1 || at < memoryName.length ||
        !tail.every((byte, index) => payload[at + index] === byte) ||
        !memoryName.every((byte, index) => payload[at - memoryName.length + index] === byte)) {
      throw new Error('unexpected import section in an emitted module');
    }
    const patched = [...payload.subarray(0, at), 0x02, 0x03, 0x01, ...uleb128(65536)];
    return Uint8Array.from([
      ...source.subarray(0, position), 2, ...uleb128(patched.length), ...patched, ...source.subarray(end),
    ]);
  }
  throw new Error('emitted module has no import section');
}

function buildChainModule(entries) {
  entries = [...entries].sort((a, b) => a.start < b.start ? -1 : a.start > b.start ? 1 : 0);
  const uleb = (value) => {
    let n = BigInt(value), bytes = [];
    do {
      let byte = Number(n & 0x7fn);
      n >>= 7n;
      if (n !== 0n) byte |= 0x80;
      bytes.push(byte);
    } while (n !== 0n);
    return bytes;
  };
  const sleb = (value) => {
    let n = BigInt.asIntN(64, BigInt(value)), bytes = [], more;
    do {
      let byte = Number(n & 0x7fn);
      n >>= 7n;
      const sign = (byte & 0x40) !== 0;
      more = !((n === 0n && !sign) || (n === -1n && sign));
      if (more) byte |= 0x80;
      bytes.push(byte);
    } while (more);
    return bytes;
  };
  const name = (value) => {
    const bytes = Array.from(new TextEncoder().encode(value));
    return [...uleb(bytes.length), ...bytes];
  };
  const section = (id, payload) => [id, ...uleb(payload.length), ...payload];
  const readUleb = (bytes, cursor) => {
    let value = 0, shift = 0, byte;
    do {
      byte = bytes[cursor.offset++];
      value |= (byte & 0x7f) << shift;
      shift += 7;
    } while (byte & 0x80);
    return value >>> 0;
  };
  const functionBody = (bytes) => {
    const cursor = { offset: 8 };
    while (cursor.offset < bytes.length) {
      const id = bytes[cursor.offset++];
      const size = readUleb(bytes, cursor);
      const end = cursor.offset + size;
      if (id === 10) {
        const count = readUleb(bytes, cursor);
        if (count !== 1) throw new Error('unexpected emitted module function count');
        const bodySize = readUleb(bytes, cursor);
        if (cursor.offset + bodySize > end) throw new Error('truncated emitted module function');
        return bytes.slice(cursor.offset, cursor.offset + bodySize);
      }
      cursor.offset = end;
    }
    throw new Error('emitted module has no code body');
  };
  const localGet = (index, code) => code.push(0x20, ...uleb(index));
  const localSet = (index, code) => code.push(0x21, ...uleb(index));
  const i32const = (value, code) => code.push(0x41, ...sleb(value));
  const incrementState = (offset, code) => {
    localGet(0, code);
    localGet(0, code);
    code.push(0x28, 0x02, ...uleb(offset)); // i32.load
    i32const(1, code);
    code.push(0x6a, 0x36, 0x02, ...uleb(offset)); // i32.add; i32.store
  };

  const body = [0x01, 0x04, 0x7f]; // four i32 locals: total, result, remaining, expected
  i32const(0, body);
  localSet(3, body);
  for (const offset of [176, 180, 184]) {
    localGet(0, body);
    i32const(0, body);
    body.push(0x36, 0x02, ...uleb(offset));
  }
  body.push(0x02, 0x40, 0x03, 0x40); // block $exit; loop $dispatch
  localGet(3, body);
  localGet(1, body);
  body.push(0x4f, 0x0d, 0x01); // total >= budget -> exit
  emitDispatchTree(entries, 0, entries.length, 0, body, {
    uleb, sleb, localGet, localSet, i32const, incrementState,
  });
  body.push(0x0b, 0x0b); // end loop and exit block
  localGet(3, body);
  body.push(0x0b);

  const bytes = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  bytes.push(...section(1, [
    2,
    0x60, 2, 0x7f, 0x7f, 1, 0x7f,
    0x60, 3, 0x7f, 0x7f, 0x7f, 1, 0x7f,
  ]));
  bytes.push(...section(2, [1, ...name('anywasm'), ...name('memory'), 0x02, 0x00, 0x01]));
  bytes.push(...section(3, [...uleb(entries.length + 1), ...Array(entries.length).fill(0), 1]));
  bytes.push(...section(7, [1, ...name('run'), 0x00, ...uleb(entries.length)]));
  const code = [...uleb(entries.length + 1)];
  for (const entry of entries) {
    const emittedBody = functionBody(entry.bytes);
    code.push(...uleb(emittedBody.length), ...emittedBody);
  }
  code.push(...uleb(body.length), ...body);
  bytes.push(...section(10, code));
  return new Uint8Array(bytes);
}

function emitDispatchTree(entries, first, end, depth, body, wasm) {
  const { uleb, sleb, localGet, localSet, i32const, incrementState } = wasm;
  if (first === end) {
    body.push(0x0c, ...uleb(depth * 2 + 1)); // unknown RIP exits the dispatcher
    return;
  }

  const middle = (first + end) >>> 1;
  const entry = entries[middle];
  localGet(0, body);
  body.push(0x29, 0x03, 0x80, 0x01); // i64.load state.rip at offset 128
  body.push(0x42, ...sleb(entry.start), 0x51, 0x04, 0x40); // if RIP equals this entry

  localGet(3, body);
  body.push(0x45, 0x04, 0x40); // if total == 0
  body.push(0x05); // else: count a module transition
  incrementState(180, body);
  body.push(0x0b);

  localGet(2, body);
  localGet(0, body);
  i32const(160, body);
  body.push(0xfc, 0x0a, 0x00, 0x00); // memory.copy CPU snapshot to scratch

  localGet(1, body);
  localGet(3, body);
  body.push(0x6b); // remaining = budget - total
  localSet(5, body);
  localGet(5, body);
  i32const(entry.instructions, body);
  body.push(0x49, 0x04, 0x7f); // if remaining < block length, result i32
  localGet(5, body);
  body.push(0x05);
  i32const(entry.instructions, body);
  body.push(0x0b);
  localSet(6, body);

  localGet(0, body);
  localGet(5, body);
  body.push(0x10, ...uleb(middle)); // call this emitted block function
  localSet(4, body);
  localGet(4, body);
  localGet(6, body);
  body.push(0x47, 0x04, 0x40); // if returned count is invalid or the module fell back
  localGet(0, body);
  localGet(2, body);
  i32const(160, body);
  body.push(0xfc, 0x0a, 0x00, 0x00); // restore CPU snapshot
  incrementState(176, body); // fallback count
  body.push(0x0c, ...uleb(depth * 2 + 3)); // exit the dispatcher
  body.push(0x0b);

  if (entry.firstRegisterRegister !== 0xffff_ffff) {
    localGet(4, body);
    i32const(entry.firstRegisterRegister, body);
    body.push(0x4b, 0x04, 0x40); // executed > first register-register MOV
    incrementState(184, body);
    body.push(0x0b);
  }
  localGet(3, body);
  localGet(4, body);
  body.push(0x6a);
  localSet(3, body);
  body.push(0x0c, ...uleb(depth * 2 + 1)); // redispatch using the new guest RIP
  body.push(0x05); // else: select a subtree by the sorted start address

  localGet(0, body);
  body.push(0x29, 0x03, 0x80, 0x01); // i64.load state.rip at offset 128
  body.push(0x42, ...sleb(entry.start), 0x54, 0x04, 0x40); // if RIP is below this entry
  emitDispatchTree(entries, first, middle, depth + 1, body, wasm);
  body.push(0x05);
  emitDispatchTree(entries, middle + 1, end, depth + 1, body, wasm);
  body.push(0x0b, 0x0b); // end less-than and equality branches
}

export class AnyWasm {
  /** @param {WebAssembly.Instance} instance */
  constructor(instance, jitStats = { compilations: 0, executions: 0, emittedGuestInstructions: 0, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0, lastFailure: null, lastError: null }, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX, asyncifyData = 0, asyncifyControl = { resumeSliceSteps: 0n }, hostIo = {}) {
    this.x = instance.exports;
    this.dec = new TextDecoder();
    /** Counts calls routed through the dynamic WebAssembly trampoline. */
    this.jitStats = jitStats;
    this.jitMinPrefix = jitMinPrefix;
    this.asyncifyData = asyncifyData;
    this.asyncifyControl = asyncifyControl;
    this.hostIo = hostIo;
    this.callbackSuspended = false;
    this.pendingHostEvents = [];
  }

  /**
   * Instantiate from raw WASM bytes (Uint8Array/ArrayBuffer) or a Response/URL string.
   * Set `{ jit: false }` to run emitted candidates through the interpreter fallback. Use
   * `{ jitMinPrefix: 1 }` to opt in to dynamic emission; the default (65) disables it.
   */
  static async create(source, { jit = true, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX, memory: importedMemory } = {}) {
    if (!Number.isInteger(jitMinPrefix) || jitMinPrefix < 1 || jitMinPrefix > 0xffff_ffff) {
      throw new RangeError('jitMinPrefix must be an integer from 1 to 4294967295');
    }
    let bytes = source;
    if (typeof source === 'string') bytes = await (await fetch(source)).arrayBuffer();
    else if (typeof Response !== 'undefined' && source instanceof Response) bytes = await source.arrayBuffer();
    let instance;
    const sharedMemory = typeof SharedArrayBuffer !== 'undefined' && importedMemory?.buffer instanceof SharedArrayBuffer;
    let asyncifyData = 0;
    const asyncifyControl = { resumeSliceSteps: 0n, yields: 0, resumes: 0 };
    const hostIo = {
      assetCache: null, pending: null, pendingRange: null, assetUrl: null,
    };
    const modules = new Map();
    const modulesByStart = new Map();
    let activeCodeEpoch;
    let chainRunner;
    let chainModulesSinceBuild = 0;
    const jitStats = { compilations: 0, executions: 0, emittedGuestInstructions: 0, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0, lastFailure: null, lastError: null };
    const recordJitFailure = (error) => {
      const message = typeof error?.message === 'string' ? error.message : String(error);
      const bounded = message.slice(0, MAX_JIT_FAILURE_TEXT);
      jitStats.lastFailure = bounded;
      jitStats.lastError = bounded;
    };
    const forgetModule = (id) => {
      const old = modules.get(id);
      if (old && modulesByStart.get(old.start) === old) {
        modulesByStart.delete(old.start);
      }
      modules.delete(id);
    };
    const getChainRunner = (memory) => {
      if (chainRunner) return chainRunner;
      const entries = Array.from(modulesByStart.values());
      const chainBytes = buildChainModule(entries);
      const module = new WebAssembly.Module(sharedMemory ? withSharedMemoryImport(chainBytes) : chainBytes);
      const emitted = new WebAssembly.Instance(module, { anywasm: { memory } });
      chainRunner = {
        run: emitted.exports.run,
        entries: new Map(entries.map((entry) => [entry.start, entry])),
      };
      chainModulesSinceBuild = 0;
      jitStats.chainCompilations++;
      return chainRunner;
    };
    const gpuReadback = (commandsPtr, commandsLength, resourceId, subresource, destination, length) => {
      const exports = instance?.exports;
      const memory = exports?.memory;
      const state = exports?.asyncify_get_state ? exports.asyncify_get_state() : 0;
      const n = Number(length) >>> 0;
      const dst = Number(destination) >>> 0;
      if (state === 2) {
        exports.asyncify_stop_rewind();
        asyncifyControl.resumes++;
        const result = hostIo.gpuReadbackResult;
        hostIo.gpuReadbackResult = null;
        if (!result?.bytes || result.bytes.byteLength !== n) return -1;
        new Uint8Array(memory.buffer, dst, n).set(result.bytes);
        return 1;
      }
      if (state !== 0 || !exports?.asyncify_get_state || !asyncifyData || !hostIo.gpuExecutor) return 0;
      if (hostIo.pending) return 0;
      try {
        const pendingLength = exports.aw_gpu_len();
        if (pendingLength) {
          const pending = new Uint8Array(memory.buffer, exports.aw_gpu_ptr() >>> 0, pendingLength);
          try {
            const pendingCommands = typeof SharedArrayBuffer !== 'undefined' && memory.buffer instanceof SharedArrayBuffer
              ? pending.slice()
              : pending;
            hostIo.gpuExecutor.execute(pendingCommands);
          } finally {
            exports.aw_gpu_clear();
          }
        }
        const commands = new Uint8Array(memory.buffer, Number(commandsPtr) >>> 0, Number(commandsLength) >>> 0);
        if (commands.length) hostIo.gpuExecutor.execute(commands);
        const readback = hostIo.gpuExecutor.readBackResource(Number(resourceId) >>> 0, Number(subresource) >>> 0);
        hostIo.pending = Promise.resolve(readback).then(
          (bytes) => { hostIo.gpuReadbackResult = { bytes }; },
          (error) => { hostIo.gpuReadbackResult = { error: error?.message || String(error) }; },
        );
        exports.asyncify_start_unwind(asyncifyData);
        asyncifyControl.yields++;
        return 0;
      } catch (error) {
        hostIo.gpuReadbackResult = { error: error?.message || String(error) };
        return -1;
      }
    };
    const imports = {
      anywasm: {
        gpu_readback: gpuReadback,
        random_fill(pointer, length) {
          try {
            return fillCryptoRandom(instance.exports.memory, pointer, length);
          } catch {
            return 0;
          }
        },
        run_emitted_block(id, modulePtr, moduleLen, statePtr, budget, emittedInstructions, firstRegisterRegister, start, codeEpoch) {
          try {
            if (!jit) {
              jitStats.fallbacks++;
              return -1;
            }
            if (activeCodeEpoch !== codeEpoch) {
              modulesByStart.clear();
              activeCodeEpoch = codeEpoch;
              chainRunner = undefined;
              chainModulesSinceBuild = 0;
            }
            const memory = instance.exports.memory;
            const moduleView = new Uint8Array(memory.buffer, modulePtr >>> 0, moduleLen >>> 0);
            let cached = modules.get(id >>> 0);
            if (!cached || !bytesEqual(cached.bytes, moduleView)) {
              if (cached && modulesByStart.get(cached.start) === cached) {
                modulesByStart.delete(cached.start);
              }
              const moduleBytes = moduleView.slice();
              const module = new WebAssembly.Module(sharedMemory ? withSharedMemoryImport(moduleBytes) : moduleBytes);
              const emitted = new WebAssembly.Instance(module, { anywasm: { memory } });
              cached = {
                bytes: moduleBytes,
                run: emitted.exports.run,
                instructions: emittedInstructions >>> 0,
                firstRegisterRegister: firstRegisterRegister >>> 0,
                start,
              };
              modules.set(id >>> 0, cached);
              if (modules.size > MAX_JIT_MODULES) forgetModule(modules.keys().next().value);
              jitStats.compilations++;
            }
            cached.instructions = emittedInstructions >>> 0;
            cached.firstRegisterRegister = firstRegisterRegister >>> 0;
            cached.start = start;
            if (modulesByStart.get(start) !== cached) {
              modulesByStart.set(start, cached);
              chainModulesSinceBuild++;
              if (modulesByStart.size <= CHAIN_EAGER_MODULE_LIMIT ||
                chainModulesSinceBuild >= CHAIN_REBUILD_MODULE_INTERVAL) chainRunner = undefined;
            }

            const remainingBudget = budget >>> 0;
            const executeDirect = () => {
              const cpuState = new Uint8Array(memory.buffer, statePtr >>> 0, 160);
              const rollback = cpuState.slice();
              let executed = -1;
              try {
                executed = cached.run(statePtr, remainingBudget);
              } catch (error) {
                // Rust retries the block through the interpreter on a module trap.
                recordJitFailure(error);
              }
              const expected = Math.min(remainingBudget, cached.instructions);
              if (executed !== expected) {
                cpuState.set(rollback);
                jitStats.fallbacks++;
                return -1;
              }
              jitStats.executions++;
              jitStats.emittedGuestInstructions += executed;
              if (cached.firstRegisterRegister !== 0xffff_ffff && executed > cached.firstRegisterRegister) {
                jitStats.registerRegisterHits++;
              }
              return executed;
            };
            if (modulesByStart.size === 1 ||
              (chainRunner && chainRunner.entries.get(start) !== cached)) {
              return executeDirect();
            }

            const runner = getChainRunner(memory);
            if (runner.entries.get(start) !== cached) return executeDirect();
            const scratch = ((statePtr >>> 0) + 192) >>> 0;
            const executed = runner.run(statePtr, remainingBudget, scratch);
            const state = new DataView(memory.buffer, statePtr >>> 0, 352);
            const fallbacks = state.getUint32(176, true);
            const transitions = state.getUint32(180, true);
            const registerHits = state.getUint32(184, true);
            jitStats.fallbacks += fallbacks;
            jitStats.chainTransitions += transitions;
            jitStats.registerRegisterHits += registerHits;
            jitStats.executions += Math.max(0, transitions + 1 - fallbacks);
            if (executed > 0) jitStats.emittedGuestInstructions += executed;
            if (executed <= 0) {
              if (fallbacks === 0) jitStats.fallbacks++;
              return -1;
            }
            return executed;
          } catch (error) {
            // The Rust side passed a copied CPU state, so the interpreter can retry safely.
            if (globalThis.ANYWASM_DEBUG_JIT) console.error('[jit] fallback:', error?.message || error);
            recordJitFailure(error);
            jitStats.fallbacks++;
            return -1;
          }
        },
        yield_slice() {
          const exports = instance?.exports;
          if (!exports?.asyncify_get_state || !asyncifyData) {
            throw new Error('guest callback exhausted its slice but the runtime was not Asyncify transformed');
          }
          const state = exports.asyncify_get_state();
          if (state === 2) {
            exports.asyncify_stop_rewind();
            const steps = asyncifyControl.resumeSliceSteps;
            asyncifyControl.resumeSliceSteps = 0n;
            asyncifyControl.resumes++;
            return steps;
          }
          if (state !== 0) throw new Error(`cannot suspend guest callback from Asyncify state ${state}`);
          exports.asyncify_start_unwind(asyncifyData);
          asyncifyControl.yields++;
          return 0n;
        },
        read_asset_range(packageOffset, length, destination) {
          if (!hostIo.assetCache) throw new Error('guest requested asset bytes before an asset source was attached');
          const n = Number(length);
          const dst = Number(destination) >>> 0;
          const output = new Uint8Array(instance.exports.memory.buffer, dst, n);
          const exports = instance?.exports;
          const state = exports?.asyncify_get_state ? exports.asyncify_get_state() : 0;
          if (state === 2) {
            exports.asyncify_stop_rewind();
            asyncifyControl.resumes++;
            if (hostIo.assetCache.copyRange(packageOffset, n, output)) return n;
            throw new Error('asset range was still missing when the guest resumed');
          }
          if (state === 0 && hostIo.assetCache.copyRange(packageOffset, n, output)) return n;
          if (!exports?.asyncify_get_state || !asyncifyData) {
            throw new Error('asset range miss requires the Asyncify-enabled browser runtime');
          }
          if (state !== 0) throw new Error(`cannot suspend asset read from Asyncify state ${state}`);
          if (!hostIo.pending) {
            hostIo.pendingRange = { offset: String(packageOffset), length: n };
            hostIo.pending = hostIo.assetCache.ensureRange(packageOffset, n);
            exports.asyncify_start_unwind(asyncifyData);
            asyncifyControl.yields++;
          }
          return 0;
        },
      },
    };
    // A runtime built for shared memory imports its linear memory (env.memory) instead of exporting
    // one. Present it as `exports.memory` so every caller works unchanged.
    if (importedMemory) imports.env = { memory: importedMemory };
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
    if (importedMemory) instance = { exports: Object.assign(Object.create(null), instance.exports, { memory: importedMemory }) };
    const x = instance.exports;
    if (typeof x.asyncify_start_unwind === 'function' && typeof x.asyncify_stop_unwind === 'function' &&
        typeof x.asyncify_start_rewind === 'function' && typeof x.asyncify_stop_rewind === 'function' &&
        typeof x.asyncify_get_state === 'function') {
      asyncifyData = x.aw_alloc(ASYNCIFY_STACK_BYTES + 8) >>> 0;
      const view = new DataView(x.memory.buffer);
      view.setUint32(asyncifyData, (asyncifyData + 8) >>> 0, true);
      view.setUint32((asyncifyData + 4) >>> 0, (asyncifyData + ASYNCIFY_STACK_BYTES + 8) >>> 0, true);
    }
    return new AnyWasm(instance, jitStats, jitMinPrefix, asyncifyData, asyncifyControl, hostIo);
  }

  /** Load an .awb bundle. Throws with the loader's message on failure. */
  load(bundle) {
    const n = bundle.byteLength;
    const ptr = this.x.aw_alloc(n) >>> 0;
    new Uint8Array(this.x.memory.buffer, ptr, n).set(bundle);
    const rc = this.x.aw_load(ptr, n);
    this.x.aw_free(ptr, n);
    if (rc !== 0) throw new Error(this.message() || 'failed to load bundle');
    this.x.aw_jit_set_min_prefix?.(this.jitMinPrefix);
  }

  /** Stage and load an executable directly from a browser-selected folder. */
  async loadDirectory(entryPath, files, onProgress = () => {}) {
    for (const name of ['aw_load_begin', 'aw_load_add_file', 'aw_load_finish']) {
      if (typeof this.x[name] !== 'function') throw new Error(`WASM runtime is missing ${name}; rebuild it`);
    }
    const inventory = inspectDirectory(files);
    const entry = normalizeDirectoryPath(entryPath);
    if (!inventory.executables.includes(entry)) {
      throw new Error(`selected entry point is not an executable in this folder: ${entry}`);
    }
    const dllPaths = [...inventory.filesByPath.keys()]
      .filter((path) => !inventory.filesByPath.get(path)?.isDirectory && path.toLowerCase().endsWith('.dll'))
      .sort();
    const bootPaths = [entry, ...dllPaths];
    const index = createDirectoryAssetIndex(files, bootPaths);
    const source = new DirectoryAssetSource(index);
    this.hostIo.assetUrl = null;
    this.hostIo.assetCache = new AssetRangeCache(null, index.packageLength, {
      onProgress,
      fileRanges: index.fileRanges,
      readRange: (start, length) => source.readRange(start, length),
    });

    const entryBytes = new TextEncoder().encode(entry);
    const indexPtr = this.x.aw_alloc(index.bytes.byteLength) >>> 0;
    let entryPtr = null;
    try {
      entryPtr = this.x.aw_alloc(entryBytes.byteLength) >>> 0;
      new Uint8Array(this.x.memory.buffer, indexPtr, index.bytes.byteLength).set(index.bytes);
      new Uint8Array(this.x.memory.buffer, entryPtr, entryBytes.byteLength).set(entryBytes);
      const rc = this.x.aw_load_begin(entryPtr, entryBytes.byteLength, indexPtr, index.bytes.byteLength);
      if (rc !== 0) throw new Error(this.message() || 'failed to begin direct folder load');
    } finally {
      if (entryPtr !== null) this.x.aw_free(entryPtr, entryBytes.byteLength);
      this.x.aw_free(indexPtr, index.bytes.byteLength);
    }

    const bootFiles = bootPaths.map((path) => ({ path, file: index.filesByPath.get(path) }));
    const totalBytes = bootFiles.reduce((total, item) => total + item.file.size, 0);
    let loadedBytes = 0;
    for (let i = 0; i < bootFiles.length; i++) {
      const { path, file } = bootFiles[i];
      onProgress({ phase: 'boot', path, loadedBytes, totalBytes, fileIndex: i, totalFiles: bootFiles.length });
      let data;
      try {
        data = new Uint8Array(await file.arrayBuffer());
      } catch (error) {
        throw new Error(`failed to read boot file ${path}: ${error?.message || error}`);
      }
      if (data.byteLength !== file.size) {
        throw new Error(`short read for boot file ${path}: received ${data.byteLength} of ${file.size} bytes`);
      }
      const pathBytes = new TextEncoder().encode(path);
      const pathPtr = this.x.aw_alloc(pathBytes.byteLength) >>> 0;
      let dataPtr = null;
      try {
        dataPtr = this.x.aw_alloc(data.byteLength) >>> 0;
        new Uint8Array(this.x.memory.buffer, pathPtr, pathBytes.byteLength).set(pathBytes);
        new Uint8Array(this.x.memory.buffer, dataPtr, data.byteLength).set(data);
        const rc = this.x.aw_load_add_file(pathPtr, pathBytes.byteLength, dataPtr, data.byteLength);
        if (rc !== 0) throw new Error(this.message() || `failed to load boot file ${path}`);
      } finally {
        if (dataPtr !== null) this.x.aw_free(dataPtr, data.byteLength);
        this.x.aw_free(pathPtr, pathBytes.byteLength);
      }
      loadedBytes += data.byteLength;
      onProgress({ phase: 'boot', path, loadedBytes, totalBytes, fileIndex: i + 1, totalFiles: bootFiles.length });
    }

    const finishRc = this.x.aw_load_finish();
    if (finishRc !== 0) throw new Error(this.message() || 'failed to finish direct folder load');
    this.x.aw_jit_set_min_prefix?.(this.jitMinPrefix);
    return {
      packageLength: index.packageLength,
      indexBytes: index.bytes.byteLength,
      bootFiles: bootFiles.length,
      assetFiles: index.fileRanges.length,
    };
  }

  setVirtualRoot(rootPath) {
    if (typeof this.x.aw_set_vfs_root_alias !== 'function') {
      throw new Error('WASM runtime is missing aw_set_vfs_root_alias; rebuild it');
    }
    const bytes = new TextEncoder().encode(rootPath);
    const ptr = this.x.aw_alloc(bytes.byteLength) >>> 0;
    try {
      new Uint8Array(this.x.memory.buffer, ptr, bytes.byteLength).set(bytes);
      const rc = this.x.aw_set_vfs_root_alias(ptr, bytes.byteLength);
      if (rc !== 0) throw new Error(this.message() || `could not mount direct folder root ${rootPath}`);
    } finally {
      this.x.aw_free(ptr, bytes.byteLength);
    }
  }

  /** Number of guest pages in the current quiescent process snapshot. */
  sharedSnapshotPageCount() {
    if (typeof this.x.aw_shared_snapshot_page_count !== 'function') {
      throw new Error('WASM runtime is missing the shared-memory snapshot ABI; rebuild the runtime');
    }
    const count = this.x.aw_shared_snapshot_page_count();
    if (count === 0xffff_ffff_ffff_ffffn) throw new Error('no process is loaded');
    return count;
  }

  sharedSnapshotMappingGeneration() {
    if (typeof this.x.aw_shared_snapshot_mapping_generation !== 'function') {
      throw new Error('WASM runtime is missing the shared snapshot generation ABI; rebuild the runtime');
    }
    const generation = this.x.aw_shared_snapshot_mapping_generation() >>> 0;
    if (generation === 0xffff_ffff) throw new Error('no process is loaded');
    return generation;
  }

  /**
   * Copy the process page map and packed guest frames into host-owned byte arrays. Call only
   * between guest run slices. `frameBase` is the aligned offset where the caller will publish the
   * packed frames in shared Wasm memory; this method does not create or mutate that memory.
   */
  exportSharedSnapshot(frameBase) {
    if (!Number.isInteger(frameBase) || frameBase < 0 || frameBase > 0xffff_ffff || frameBase % 4096 !== 0) {
      throw new RangeError('frameBase must be a 4096-byte-aligned u32');
    }
    const pageCount = this.sharedSnapshotPageCount();
    const mappingBytesBig = pageCount * 16n;
    const frameBytesBig = pageCount * 4096n;
    const totalBytesBig = mappingBytesBig + frameBytesBig;
    if (totalBytesBig > 0x7fff_ffffn) throw new RangeError('shared snapshot is too large for the Wasm32 bridge');
    const mappingBytes = Number(mappingBytesBig);
    const frameBytes = Number(frameBytesBig);
    const totalBytes = Number(totalBytesBig);
    const ptr = this.x.aw_alloc(totalBytes) >>> 0;
    try {
      const rc = this.x.aw_shared_snapshot_export(
        frameBase >>> 0,
        ptr,
        mappingBytes,
        (ptr + mappingBytes) >>> 0,
        frameBytes,
      );
      if (rc !== 0) throw new Error(this.message() || `shared snapshot export failed (${rc})`);
      const memory = this.x.memory.buffer;
      return {
        frameBase: frameBase >>> 0,
        pageCount,
        mappingGeneration: this.sharedSnapshotMappingGeneration(),
        mappings: new Uint8Array(memory, ptr, mappingBytes).slice(),
        frames: new Uint8Array(memory, (ptr + mappingBytes) >>> 0, frameBytes).slice(),
      };
    } finally {
      this.x.aw_free(ptr, totalBytes);
    }
  }

  /** Export only coordinator-dirty frames and their mapping-row indices. */
  exportSharedDelta(frameBase) {
    if (!Number.isInteger(frameBase) || frameBase < 0 || frameBase > 0xffff_ffff || frameBase % 4096 !== 0) {
      throw new RangeError('frameBase must be a 4096-byte-aligned u32');
    }
    if (typeof this.x.aw_shared_snapshot_export_delta !== 'function') {
      throw new Error('WASM runtime is missing the dirty-page snapshot ABI; rebuild the runtime');
    }
    const pageCount = Number(this.sharedSnapshotPageCount());
    const indexBytes = pageCount * 4;
    const frameCapacity = pageCount * 4096;
    const indexOffset = 8;
    const framesOffset = indexOffset + indexBytes;
    const totalBytes = framesOffset + frameCapacity;
    if (!Number.isSafeInteger(totalBytes) || totalBytes > 0x7fff_ffff) {
      throw new RangeError('shared snapshot delta is too large for the Wasm32 bridge');
    }
    const ptr = this.x.aw_alloc(Math.max(totalBytes, 8)) >>> 0;
    try {
      const rc = this.x.aw_shared_snapshot_export_delta(
        frameBase >>> 0,
        (ptr + indexOffset) >>> 0,
        indexBytes,
        (ptr + framesOffset) >>> 0,
        frameCapacity,
        ptr,
      );
      if (rc !== 0) throw new Error(this.message() || `shared snapshot delta export failed (${rc})`);
      const memory = this.x.memory.buffer;
      const dirtyCount = new DataView(memory, ptr, 4).getUint32(0, true);
      if (dirtyCount > pageCount) throw new Error('WASM returned an invalid dirty-page count');
      return {
        frameBase: frameBase >>> 0,
        pageCount,
        mappingGeneration: this.sharedSnapshotMappingGeneration(),
        dirtyPages: new Uint32Array(memory, (ptr + indexOffset) >>> 0, dirtyCount).slice(),
        frames: new Uint8Array(memory, (ptr + framesOffset) >>> 0, dirtyCount * 4096).slice(),
      };
    } finally {
      this.x.aw_free(ptr, Math.max(totalBytes, 8));
    }
  }

  /**
   * Describe the guest frame store of a runtime linked against an imported shared memory:
   * `{ address, used, capacity, mappingGeneration }` in bytes of that memory. The address changes
   * only when the store grows and moves, which also changes the mapping generation.
   */
  sharedFramesInfo() {
    if (typeof this.x.aw_shared_frames_info !== 'function') {
      throw new Error('WASM runtime is missing the in-place shared-memory ABI; rebuild the runtime');
    }
    const ptr = this.x.aw_alloc(16) >>> 0;
    try {
      const rc = this.x.aw_shared_frames_info(ptr);
      if (rc !== 0) throw new Error(this.message() || `frame store query failed (${rc})`);
      const [address, used, capacity, mappingGeneration] = new Uint32Array(this.x.memory.buffer.slice(ptr, ptr + 16));
      return { address, used, capacity, mappingGeneration };
    } finally {
      this.x.aw_free(ptr, 16);
    }
  }

  /**
   * Write the sorted guest page table, pointing at the live frames, into `byteOffset` of the shared
   * memory (8-byte aligned, `byteLength` bytes). Frame offsets are relative to `headerBase`, the
   * address of the worker ABI header. Returns the number of 16-byte records written.
   */
  exportSharedMappingsInPlace(byteOffset, byteLength, headerBase) {
    const count = this.x.aw_shared_export_mappings_in_place(byteOffset >>> 0, byteLength >>> 0, headerBase >>> 0);
    if (count < 0n && typeof count === 'bigint') throw new Error(this.message() || `in-place mapping export failed (${count})`);
    return Number(count);
  }

  /** Report frames that CPU workers wrote in place; returns how many back executable pages. */
  noteSharedWorkerWrites(frames) {
    if (!(frames instanceof Uint32Array)) throw new TypeError('frames must be a Uint32Array');
    if (frames.length === 0) return 0;
    const bytes = frames.length * 4;
    const ptr = this.x.aw_alloc(bytes) >>> 0;
    try {
      new Uint32Array(this.x.memory.buffer, ptr, frames.length).set(frames);
      return this.x.aw_shared_note_worker_writes(ptr, frames.length);
    } finally {
      this.x.aw_free(ptr, bytes);
    }
  }

  clearSharedSnapshotDirtyPages(dirtyPages) {
    if (!(dirtyPages instanceof Uint32Array)) throw new TypeError('dirtyPages must be a Uint32Array');
    if (dirtyPages.length === 0) return;
    if (typeof this.x.aw_shared_snapshot_clear_dirty_pages !== 'function') {
      throw new Error('WASM runtime is missing the dirty-page acknowledgement ABI; rebuild the runtime');
    }
    const byteLength = dirtyPages.length * 4;
    const ptr = this.x.aw_alloc(byteLength) >>> 0;
    try {
      new Uint32Array(this.x.memory.buffer, ptr, dirtyPages.length).set(dirtyPages);
      const rc = this.x.aw_shared_snapshot_clear_dirty_pages(ptr, dirtyPages.length);
      if (rc !== 0) throw new Error(this.message() || `shared dirty-page acknowledgement failed (${rc})`);
    } finally {
      this.x.aw_free(ptr, byteLength);
    }
  }

  clearAllSharedSnapshotDirty() {
    if (typeof this.x.aw_shared_snapshot_clear_all_dirty !== 'function') {
      throw new Error('WASM runtime is missing the dirty-page acknowledgement ABI; rebuild the runtime');
    }
    const rc = this.x.aw_shared_snapshot_clear_all_dirty();
    if (rc !== 0) throw new Error(this.message() || `shared dirty-page acknowledgement failed (${rc})`);
  }

  /**
   * Copy worker-written frames back into the coordinator after all workers are quiescent. The
   * mapping bytes must come from the matching `exportSharedSnapshot` call; the frame bytes may be
   * copied from a SharedArrayBuffer view before this method is called.
   */
  importSharedSnapshot(snapshot) {
    if (!snapshot || !Number.isInteger(snapshot.frameBase) || snapshot.frameBase < 0 ||
        snapshot.frameBase > 0xffff_ffff || snapshot.frameBase % 4096 !== 0 ||
        !(snapshot.mappings instanceof Uint8Array) || !(snapshot.frames instanceof Uint8Array)) {
      throw new TypeError('invalid shared snapshot');
    }
    const pageCount = this.sharedSnapshotPageCount();
    const mappingBytesBig = pageCount * 16n;
    const frameBytesBig = pageCount * 4096n;
    const totalBytesBig = mappingBytesBig + frameBytesBig;
    if (totalBytesBig > 0x7fff_ffffn) throw new RangeError('shared snapshot is too large for the Wasm32 bridge');
    const mappingBytes = Number(mappingBytesBig);
    const frameBytes = Number(frameBytesBig);
    const totalBytes = Number(totalBytesBig);
    if (BigInt(snapshot.mappings.byteLength) !== mappingBytesBig || snapshot.frames.byteLength !== frameBytes) {
      throw new RangeError('shared snapshot buffers do not match the current process mapping');
    }

    const ptr = this.x.aw_alloc(totalBytes) >>> 0;
    try {
      const memory = new Uint8Array(this.x.memory.buffer, ptr, totalBytes);
      memory.set(snapshot.mappings, 0);
      memory.set(snapshot.frames, mappingBytes);
      const rc = this.x.aw_shared_snapshot_import(
        snapshot.frameBase >>> 0,
        ptr,
        mappingBytes,
        (ptr + mappingBytes) >>> 0,
        frameBytes,
      );
      if (rc !== 0) throw new Error(this.message() || `shared snapshot import failed (${rc})`);
    } finally {
      this.x.aw_free(ptr, totalBytes);
    }
  }

  /** Import only worker-dirty frames after all shared CPU slices have completed. */
  importSharedDirtySnapshot(snapshot) {
    if (!snapshot || !Number.isInteger(snapshot.frameBase) || snapshot.frameBase < 0 ||
        snapshot.frameBase > 0xffff_ffff || snapshot.frameBase % 4096 !== 0 ||
        !Number.isInteger(snapshot.mappingGeneration) || snapshot.mappingGeneration < 0 ||
        snapshot.mappingGeneration > 0xffff_ffff || !(snapshot.dirtyPages instanceof Uint32Array) ||
        !(snapshot.frames instanceof Uint8Array)) {
      throw new TypeError('invalid dirty shared snapshot');
    }
    if (snapshot.dirtyPages.length === 0) return;
    if (typeof this.x.aw_shared_snapshot_import_dirty !== 'function') {
      throw new Error('WASM runtime is missing the dirty-page import ABI; rebuild the runtime');
    }
    const pageCount = Number(this.sharedSnapshotPageCount());
    const indexBytes = snapshot.dirtyPages.length * 4;
    const frameBytes = snapshot.dirtyPages.length * 4096;
    if (snapshot.pageCount !== pageCount ||
        snapshot.mappingGeneration !== this.sharedSnapshotMappingGeneration() ||
        snapshot.frames.byteLength !== frameBytes || snapshot.dirtyPages.length > pageCount) {
      throw new RangeError('dirty shared snapshot buffers do not match the current process mapping');
    }
    const totalBytes = indexBytes + frameBytes;
    if (!Number.isSafeInteger(totalBytes) || totalBytes > 0x7fff_ffff) {
      throw new RangeError('dirty shared snapshot is too large for the Wasm32 bridge');
    }
    const ptr = this.x.aw_alloc(Math.max(totalBytes, 1)) >>> 0;
    try {
      const indexPtr = ptr;
      const framePtr = (indexPtr + indexBytes) >>> 0;
      new Uint32Array(this.x.memory.buffer, indexPtr, snapshot.dirtyPages.length).set(snapshot.dirtyPages);
      new Uint8Array(this.x.memory.buffer, framePtr, frameBytes).set(snapshot.frames);
      const rc = this.x.aw_shared_snapshot_import_dirty(
        snapshot.mappingGeneration >>> 0,
        snapshot.frameBase >>> 0,
        indexPtr,
        snapshot.dirtyPages.length,
        framePtr,
        frameBytes,
      );
      if (rc !== 0) throw new Error(this.message() || `dirty shared snapshot import failed (${rc})`);
    } finally {
      this.x.aw_free(ptr, Math.max(totalBytes, 1));
    }
  }

  /** Stable thread IDs for every active or saved CPU context in the loaded process. */
  sharedCpuContextThreadIds() {
    if (typeof this.x.aw_shared_context_count !== 'function' ||
        typeof this.x.aw_shared_context_thread_id !== 'function') {
      throw new Error('WASM runtime is missing shared CPU context enumeration; rebuild the runtime');
    }
    const count = this.x.aw_shared_context_count() >>> 0;
    return Array.from({ length: count }, (_, index) => this.x.aw_shared_context_thread_id(index) >>> 0);
  }

  /** Runnable contexts in coordinator order; blocked and exited threads are omitted. */
  sharedCpuRunnableThreadIds() {
    if (typeof this.x.aw_shared_runnable_context_count !== 'function' ||
        typeof this.x.aw_shared_runnable_context_thread_id !== 'function') {
      throw new Error('WASM runtime is missing runnable shared CPU contexts; rebuild the runtime');
    }
    const count = this.x.aw_shared_runnable_context_count() >>> 0;
    const threadIds = Array.from(
      { length: count },
      (_, index) => this.x.aw_shared_runnable_context_thread_id(index) >>> 0,
    );
    if (threadIds.some((threadId) => threadId === 0) || new Set(threadIds).size !== threadIds.length) {
      throw new Error('WASM returned an invalid shared CPU runnable-context list');
    }
    return threadIds;
  }

  /** Remaining worker quantum for a runnable thread, or zero when it is ineligible. */
  sharedCpuThreadQuantumRemaining(threadId) {
    if (!Number.isInteger(threadId) || threadId < 0 || threadId > 0xffff_ffff) {
      throw new RangeError('threadId must be a u32');
    }
    if (typeof this.x.aw_shared_context_quantum_remaining_for !== 'function') {
      throw new Error('WASM runtime is missing per-thread shared CPU quantum state; rebuild the runtime');
    }
    return this.x.aw_shared_context_quantum_remaining_for(threadId >>> 0) >>> 0;
  }

  /** Cumulative retired instruction count for an active or saved thread. */
  sharedCpuThreadInstructionCount(threadId) {
    if (!Number.isInteger(threadId) || threadId < 0 || threadId > 0xffff_ffff) {
      throw new RangeError('threadId must be a u32');
    }
    if (typeof this.x.aw_shared_context_instruction_count !== 'function') {
      throw new Error('WASM runtime is missing per-thread shared CPU accounting; rebuild the runtime');
    }
    const count = this.x.aw_shared_context_instruction_count(threadId >>> 0);
    if (typeof count !== 'bigint') {
      throw new Error('WASM runtime returned an invalid per-thread instruction count');
    }
    return count;
  }

  /** Activate a completed saved context for serialized coordinator work. */
  beginSharedCpuCoordinatorTurn(threadId) {
    if (!Number.isInteger(threadId) || threadId < 0 || threadId > 0xffff_ffff) {
      throw new RangeError('threadId must be a u32');
    }
    if (typeof this.x.aw_shared_context_begin_handoff !== 'function') {
      throw new Error('WASM runtime is missing shared CPU coordinator handoff; rebuild the runtime');
    }
    const rc = this.x.aw_shared_context_begin_handoff(threadId >>> 0);
    if (rc === 0) return true;
    if (rc === 1) return false;
    throw new Error(this.message() || `shared CPU coordinator handoff failed (${rc})`);
  }

  /** Restore the previously active context after coordinator work on a saved context. */
  finishSharedCpuCoordinatorTurn() {
    if (typeof this.x.aw_shared_context_finish_handoff !== 'function') {
      throw new Error('WASM runtime is missing shared CPU coordinator handoff; rebuild the runtime');
    }
    const rc = this.x.aw_shared_context_finish_handoff();
    if (rc !== 0) throw new Error(this.message() || `shared CPU coordinator handoff restore failed (${rc})`);
  }

  /** Active context eligible for dispatch, or zero when the coordinator must schedule first. */
  sharedCpuActiveThreadId() {
    if (typeof this.x.aw_shared_context_active_thread_id !== 'function') {
      throw new Error('WASM runtime is missing active shared CPU context state; rebuild the runtime');
    }
    return this.x.aw_shared_context_active_thread_id() >>> 0;
  }

  /** Remaining instructions before the coordinator's current thread quantum must yield. */
  sharedCpuQuantumRemaining() {
    if (typeof this.x.aw_shared_context_quantum_remaining !== 'function') {
      throw new Error('WASM runtime is missing the shared CPU quantum ABI; rebuild the runtime');
    }
    return this.x.aw_shared_context_quantum_remaining() >>> 0;
  }

  /** HLE thunk range at which shared CPU workers must return to the coordinator. */
  sharedCpuTrapRange() {
    if (typeof this.x.aw_shared_hle_range_start !== 'function' ||
        typeof this.x.aw_shared_hle_range_end !== 'function' ||
        typeof this.x.aw_shared_process_exit_magic !== 'function' ||
        typeof this.x.aw_shared_thread_return_magic !== 'function') {
      throw new Error('WASM runtime is missing the shared CPU trap ABI; rebuild the runtime');
    }
    return {
      trapStart: this.x.aw_shared_hle_range_start(),
      trapEnd: this.x.aw_shared_hle_range_end(),
      processExit: this.x.aw_shared_process_exit_magic(),
      threadExit: this.x.aw_shared_thread_return_magic(),
    };
  }

  /** Copy one quiescent guest thread's architectural CPU state for a shared worker slot. */
  exportSharedCpuContext(threadId) {
    if (!Number.isInteger(threadId) || threadId < 0 || threadId > 0xffff_ffff) {
      throw new RangeError('threadId must be a u32');
    }
    if (typeof this.x.aw_shared_context_export !== 'function') {
      throw new Error('WASM runtime is missing the shared CPU context ABI; rebuild the runtime');
    }
    const allocationBytes = SHARED_CPU_CONTEXT_BYTES + SHARED_CPU_CONTEXT_ALIGN - 1;
    const allocation = this.x.aw_alloc(allocationBytes) >>> 0;
    const ptr = ((allocation + SHARED_CPU_CONTEXT_ALIGN - 1) & ~(SHARED_CPU_CONTEXT_ALIGN - 1)) >>> 0;
    try {
      const size = this.x.aw_shared_context_export(threadId >>> 0, ptr, SHARED_CPU_CONTEXT_BYTES);
      if (size !== SHARED_CPU_CONTEXT_BYTES) {
        throw new Error(this.message() || `shared CPU context export failed for thread ${threadId}`);
      }
      return new Uint8Array(this.x.memory.buffer, ptr, size).slice();
    } finally {
      this.x.aw_free(allocation, allocationBytes);
    }
  }

  /** Import a worker-completed CPU context after that thread has reached a safe point. */
  importSharedCpuContext(threadId, context, { syncTebAlias = false } = {}) {
    if (!Number.isInteger(threadId) || threadId < 0 || threadId > 0xffff_ffff) {
      throw new RangeError('threadId must be a u32');
    }
    if (!(context instanceof Uint8Array) || context.byteLength !== SHARED_CPU_CONTEXT_BYTES) {
      throw new TypeError(`context must be a ${SHARED_CPU_CONTEXT_BYTES}-byte Uint8Array`);
    }
    if (typeof this.x.aw_shared_context_import !== 'function') {
      throw new Error('WASM runtime is missing the shared CPU context ABI; rebuild the runtime');
    }
    const allocationBytes = SHARED_CPU_CONTEXT_BYTES + SHARED_CPU_CONTEXT_ALIGN - 1;
    const allocation = this.x.aw_alloc(allocationBytes) >>> 0;
    const ptr = ((allocation + SHARED_CPU_CONTEXT_ALIGN - 1) & ~(SHARED_CPU_CONTEXT_ALIGN - 1)) >>> 0;
    try {
      new Uint8Array(this.x.memory.buffer, ptr, SHARED_CPU_CONTEXT_BYTES).set(context);
      const rc = this.x.aw_shared_context_import(
        threadId >>> 0,
        ptr,
        SHARED_CPU_CONTEXT_BYTES,
        syncTebAlias ? 1 : 0,
      );
      if (rc !== 0) throw new Error(this.message() || `shared CPU context import failed (${rc})`);
    } finally {
      this.x.aw_free(allocation, allocationBytes);
    }
  }

  /** Attach the `AGP1` index; payload chunks remain in the HTTP-backed host cache. */
  async attachAssets(url, onProgress = () => {}, expected = null) {
    if (typeof url !== 'string' || !url) throw new TypeError('asset package URL must be a nonempty string');
    if (typeof this.x.aw_assets_attach !== 'function') throw new Error('WASM runtime is missing aw_assets_attach; rebuild it');
    const index = await fetchAssetIndex(url);
    if (expected?.size !== undefined && BigInt(expected.size) !== index.packageLength) {
      throw new Error(`asset package size mismatch: expected ${expected.size}, received ${index.packageLength}`);
    }
    if (expected?.indexSha256 && await sha256Hex(index.bytes) !== expected.indexSha256.toLowerCase()) {
      throw new Error('asset package header/index SHA-256 mismatch');
    }
    const ptr = this.x.aw_alloc(index.bytes.byteLength) >>> 0;
    try {
      new Uint8Array(this.x.memory.buffer, ptr, index.bytes.byteLength).set(index.bytes);
      const rc = this.x.aw_assets_attach(ptr, index.bytes.byteLength);
      if (rc !== 0) throw new Error(this.message() || 'failed to attach AGP asset index');
    } finally {
      this.x.aw_free(ptr, index.bytes.byteLength);
    }
    this.hostIo.assetUrl = url;
    this.hostIo.assetCache = new AssetRangeCache(url, index.packageLength, {
      onProgress,
      fileRanges: index.fileRanges,
    });
    return { packageLength: index.packageLength, indexBytes: index.bytes.byteLength };
  }

  get hostWaitPending() { return !!this.hostIo.pending; }

  async waitForHost() {
    const pending = this.hostIo.pending;
    if (!pending) return;
    try {
      await pending;
    } finally {
      if (this.hostIo.pending === pending) {
        this.hostIo.pending = null;
        this.hostIo.pendingRange = null;
      }
    }
  }

  /** Execute up to `steps` guest instructions. Returns a Status value. */
  run(steps = 1_000_000) {
    if (this.callbackSuspended) {
      this.asyncifyControl.resumeSliceSteps = BigInt(steps);
      this.x.asyncify_start_rewind(this.asyncifyData);
    }
    const status = this.x.aw_run(BigInt(steps));
    if (this.asyncifyData) {
      const state = this.x.asyncify_get_state();
      if (state === 1) {
        this.x.asyncify_stop_unwind();
        this.callbackSuspended = true;
        return Status.RUNNING;
      }
      if (state !== 0) throw new Error(`guest run returned in unexpected Asyncify state ${state}`);
      this.callbackSuspended = false;
      this.flushPendingHostEvents();
    }
    return status;
  }

  get asyncifyStats() {
    return { yields: this.asyncifyControl.yields, resumes: this.asyncifyControl.resumes };
  }

  _take(ptrFn, lenFn, clearFn) {
    if (this.callbackSuspended) return new Uint8Array(0);
    const len = lenFn();
    if (!len) return new Uint8Array(0);
    const out = new Uint8Array(this.x.memory.buffer, ptrFn() >>> 0, len).slice();
    clearFn();
    return out;
  }
  /**
   * Run `fn` on the pending GPU command stream as a view of WASM memory (no copy), then clear it.
   * `fn` must be synchronous and must not keep the view: it is invalid once the runtime runs
   * again. Falls back to a copy when the memory is shared (some Web APIs reject such views).
   * Returns false when there was nothing to run.
   */
  consumeGpu(fn) {
    if (this.callbackSuspended) return false;
    const len = this.x.aw_gpu_len();
    if (!len) return false;
    const buffer = this.x.memory.buffer;
    const view = new Uint8Array(buffer, this.x.aw_gpu_ptr() >>> 0, len);
    try {
      fn(typeof SharedArrayBuffer !== 'undefined' && buffer instanceof SharedArrayBuffer ? view.slice() : view);
    } finally {
      this.x.aw_gpu_clear();
    }
    return true;
  }
  /** Bytes of GPU command stream waiting to be taken. */
  gpuPendingBytes() { return this.callbackSuspended ? 0 : this.x.aw_gpu_len(); }
  /** Window/GPU command stream produced since the last call (copied out of WASM memory). */
  takeGpu() { return this._take(this.x.aw_gpu_ptr, this.x.aw_gpu_len, this.x.aw_gpu_clear); }
  /** Deliver an input event: see crates/anywasm-rt/src/hle/user32.rs `input_event`. */
  input(kind, a = 0, b = 0, c = 0) {
    const event = () => this.x.aw_input(kind, BigInt(a), BigInt(b), BigInt(c));
    if (this.callbackSuspended) this.pendingHostEvents.push(event);
    else event();
  }
  /** Add host elapsed time in 100 ns ticks to the guest clock after a frame or idle wait. */
  advanceTime100ns(ticks) {
    if (typeof ticks !== 'bigint' || ticks < 0n) throw new RangeError('ticks must be a nonnegative BigInt');
    if (typeof this.x.aw_advance_time_100ns !== 'function') {
      throw new Error('WASM runtime is missing aw_advance_time_100ns; rebuild the runtime');
    }
    this.x.aw_advance_time_100ns(ticks);
  }
  /** Delay to the next guest user32 timer, or null when no active timer exists. */
  nextTimerDelayMs() {
    if (typeof this.x.aw_next_timer_delay_ms !== 'function') {
      throw new Error('WASM runtime is missing aw_next_timer_delay_ms; rebuild the runtime');
    }
    const delay = this.x.aw_next_timer_delay_ms() >>> 0;
    return delay === 0xffff_ffff ? null : delay;
  }
  takeStdout() { return this._take(this.x.aw_stdout_ptr, this.x.aw_stdout_len, this.x.aw_stdout_clear); }
  takeStderr() { return this._take(this.x.aw_stderr_ptr, this.x.aw_stderr_len, this.x.aw_stderr_clear); }
  message() {
    const len = this.x.aw_message_len();
    return len ? this.dec.decode(new Uint8Array(this.x.memory.buffer, this.x.aw_message_ptr() >>> 0, len).slice()) : '';
  }
  exitCode() { return this.x.aw_exit_code(); }
  instructions() { return Number(this.x.aw_icount()); }
  retiredInstructions() {
    if (typeof this.x.aw_retired_instructions !== 'function') {
      throw new Error('WASM runtime is missing aw_retired_instructions; rebuild the runtime');
    }
    return Number(this.x.aw_retired_instructions());
  }
  retiredInstructionsExact() {
    if (typeof this.x.aw_retired_instructions !== 'function') {
      throw new Error('WASM runtime is missing aw_retired_instructions; rebuild the runtime');
    }
    return this.x.aw_retired_instructions();
  }

  pushStdin(bytes) {
    const copy = this.callbackSuspended ? Uint8Array.from(bytes) : bytes;
    const push = () => this._pushStdin(copy);
    if (this.callbackSuspended) {
      this.pendingHostEvents.push(push);
      return;
    }
    push();
  }

  _pushStdin(bytes) {
    const n = bytes.byteLength;
    const ptr = this.x.aw_alloc(n) >>> 0;
    new Uint8Array(this.x.memory.buffer, ptr, n).set(bytes);
    this.x.aw_stdin_push(ptr, n);
    this.x.aw_free(ptr, n);
  }

  flushPendingHostEvents() {
    if (this.callbackSuspended) return;
    while (this.pendingHostEvents.length) this.pendingHostEvents.shift()();
  }

  /** Run to completion, calling `onOutput(stream, text)` between slices. Returns {status, exitCode, message}. */
  runToEnd({ sliceSteps = 2_000_000, maxSlices = Infinity, onOutput = () => {}, yieldFn = null } = {}) {
    const textDec = new TextDecoder();
    let status = Status.RUNNING;
    for (let i = 0; i < maxSlices && (status === Status.RUNNING || status === Status.FRAME); i++) {
      status = this.run(sliceSteps);
      const o = this.takeStdout(); if (o.length) onOutput('stdout', textDec.decode(o));
      const e = this.takeStderr(); if (e.length) onOutput('stderr', textDec.decode(e));
      if (status === Status.FRAME) { this.takeGpu(); status = Status.RUNNING; }
      if (this.hostWaitPending) {
        return Promise.resolve(this.waitForHost()).then(() => this.runToEnd({ sliceSteps, maxSlices: maxSlices - i - 1, onOutput, yieldFn }));
      }
      if (yieldFn && status === Status.RUNNING) return Promise.resolve(yieldFn()).then(() => this.runToEnd({ sliceSteps, maxSlices: maxSlices - i - 1, onOutput, yieldFn }));
    }
    return { status, exitCode: this.exitCode(), message: this.message() };
  }
}
