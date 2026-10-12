// AnyWASM runtime glue: loads the WASM module and drives a Windows x64 process inside it.
// Works unchanged in browsers (main thread or worker) and Node >= 18.
import { fillCryptoRandom } from './entropy.js';
import { AssetRangeCache, fetchAssetIndex } from './assets.mjs';

export const Status = Object.freeze({ RUNNING: 0, EXITED: 1, UNIMPLEMENTED: 2, CRASHED: 3, FRAME: 4, IDLE: 5 });

const MAX_JIT_MODULES = 256;
const DEFAULT_JIT_MIN_PREFIX = 65; // decoded blocks are capped at 64 instructions
const ASYNCIFY_STACK_BYTES = 1024 * 1024;

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
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
  constructor(instance, jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 }, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX, asyncifyData = 0, asyncifyControl = { resumeSliceSteps: 0n }, hostIo = {}) {
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
  static async create(source, { jit = true, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX } = {}) {
    if (!Number.isInteger(jitMinPrefix) || jitMinPrefix < 1 || jitMinPrefix > 0xffff_ffff) {
      throw new RangeError('jitMinPrefix must be an integer from 1 to 4294967295');
    }
    let bytes = source;
    if (typeof source === 'string') bytes = await (await fetch(source)).arrayBuffer();
    else if (typeof Response !== 'undefined' && source instanceof Response) bytes = await source.arrayBuffer();
    let instance;
    let asyncifyData = 0;
    const asyncifyControl = { resumeSliceSteps: 0n, yields: 0, resumes: 0 };
    const hostIo = { assetCache: null, pending: null, assetUrl: null };
    const modules = new Map();
    const modulesByStart = new Map();
    let activeCodeEpoch;
    let registryVersion = 0;
    let chainRunner;
    const jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 };
    const forgetModule = (id) => {
      const old = modules.get(id);
      if (old && modulesByStart.get(old.start) === old) {
        modulesByStart.delete(old.start);
        registryVersion++;
        chainRunner = undefined;
      }
      modules.delete(id);
    };
    const getChainRunner = (memory) => {
      if (chainRunner?.version === registryVersion) return chainRunner;
      const entries = Array.from(modulesByStart.values());
      const module = new WebAssembly.Module(buildChainModule(entries));
      const emitted = new WebAssembly.Instance(module, { anywasm: { memory } });
      chainRunner = { version: registryVersion, run: emitted.exports.run };
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
              registryVersion++;
              chainRunner = undefined;
            }
            const memory = instance.exports.memory;
            const moduleView = new Uint8Array(memory.buffer, modulePtr >>> 0, moduleLen >>> 0);
            let cached = modules.get(id >>> 0);
            if (!cached || !bytesEqual(cached.bytes, moduleView)) {
              if (cached && modulesByStart.get(cached.start) === cached) {
                modulesByStart.delete(cached.start);
                registryVersion++;
                chainRunner = undefined;
              }
              const moduleBytes = moduleView.slice();
              const module = new WebAssembly.Module(moduleBytes);
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
              registryVersion++;
              chainRunner = undefined;
            }

            const remainingBudget = budget >>> 0;
            if (modulesByStart.size === 1) {
              const cpuState = new Uint8Array(memory.buffer, statePtr >>> 0, 160);
              const rollback = cpuState.slice();
              let executed = -1;
              try {
                executed = cached.run(statePtr, remainingBudget);
              } catch {
                // Rust retries the block through the interpreter on a module trap.
              }
              const expected = Math.min(remainingBudget, cached.instructions);
              if (executed !== expected) {
                cpuState.set(rollback);
                jitStats.fallbacks++;
                return -1;
              }
              jitStats.executions++;
              if (cached.firstRegisterRegister !== 0xffff_ffff && executed > cached.firstRegisterRegister) {
                jitStats.registerRegisterHits++;
              }
              return executed;
            }

            const runner = getChainRunner(memory);
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
            if (executed <= 0) {
              if (fallbacks === 0) jitStats.fallbacks++;
              return -1;
            }
            return executed;
          } catch {
            // The Rust side passed a copied CPU state, so the interpreter can retry safely.
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
          if (!hostIo.assetCache) throw new Error('guest requested package bytes before an AGP asset package was attached');
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
            hostIo.pending = hostIo.assetCache.ensureRange(packageOffset, n);
            exports.asyncify_start_unwind(asyncifyData);
            asyncifyControl.yields++;
          }
          return 0;
        },
      },
    };
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
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

  /** Attach the `AGP1` index; payload chunks remain in the HTTP-backed host cache. */
  async attachAssets(url, onProgress = () => {}) {
    if (typeof url !== 'string' || !url) throw new TypeError('asset package URL must be a nonempty string');
    if (typeof this.x.aw_assets_attach !== 'function') throw new Error('WASM runtime is missing aw_assets_attach; rebuild it');
    const index = await fetchAssetIndex(url);
    const ptr = this.x.aw_alloc(index.bytes.byteLength) >>> 0;
    try {
      new Uint8Array(this.x.memory.buffer, ptr, index.bytes.byteLength).set(index.bytes);
      const rc = this.x.aw_assets_attach(ptr, index.bytes.byteLength);
      if (rc !== 0) throw new Error(this.message() || 'failed to attach AGP asset index');
    } finally {
      this.x.aw_free(ptr, index.bytes.byteLength);
    }
    this.hostIo.assetUrl = url;
    this.hostIo.assetCache = new AssetRangeCache(url, index.packageLength, { onProgress });
    return { packageLength: index.packageLength, indexBytes: index.bytes.byteLength };
  }

  get hostWaitPending() { return !!this.hostIo.pending; }

  async waitForHost() {
    const pending = this.hostIo.pending;
    if (!pending) return;
    try {
      await pending;
    } finally {
      if (this.hostIo.pending === pending) this.hostIo.pending = null;
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
  /** Window/GPU command stream produced since the last call (copied out of WASM memory). */
  takeGpu() { return this._take(this.x.aw_gpu_ptr, this.x.aw_gpu_len, this.x.aw_gpu_clear); }
  /** Deliver an input event: see crates/anywasm-rt/src/hle/user32.rs `input_event`. */
  input(kind, a = 0, b = 0, c = 0) {
    const event = () => this.x.aw_input(kind, BigInt(a), BigInt(b), BigInt(c));
    if (this.callbackSuspended) this.pendingHostEvents.push(event);
    else event();
  }
  /** Add host elapsed time in 100 ns ticks to the guest clock after a frame yield. */
  advanceTime100ns(ticks) {
    if (typeof ticks !== 'bigint' || ticks < 0n) throw new RangeError('ticks must be a nonnegative BigInt');
    if (typeof this.x.aw_advance_time_100ns !== 'function') {
      throw new Error('WASM runtime is missing aw_advance_time_100ns; rebuild the runtime');
    }
    this.x.aw_advance_time_100ns(ticks);
  }
  takeStdout() { return this._take(this.x.aw_stdout_ptr, this.x.aw_stdout_len, this.x.aw_stdout_clear); }
  takeStderr() { return this._take(this.x.aw_stderr_ptr, this.x.aw_stderr_len, this.x.aw_stderr_clear); }
  message() {
    const len = this.x.aw_message_len();
    return len ? this.dec.decode(new Uint8Array(this.x.memory.buffer, this.x.aw_message_ptr() >>> 0, len)) : '';
  }
  exitCode() { return this.x.aw_exit_code(); }
  instructions() { return Number(this.x.aw_icount()); }
  retiredInstructions() {
    if (typeof this.x.aw_retired_instructions !== 'function') {
      throw new Error('WASM runtime is missing aw_retired_instructions; rebuild the runtime');
    }
    return Number(this.x.aw_retired_instructions());
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
