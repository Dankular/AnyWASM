// AnyWASM runtime glue: loads the WASM module and drives a Windows x64 process inside it.
// Works unchanged in browsers (main thread or worker) and Node >= 18.
import { fillCryptoRandom } from './entropy.js';

export const Status = Object.freeze({ RUNNING: 0, EXITED: 1, UNIMPLEMENTED: 2, CRASHED: 3, FRAME: 4, IDLE: 5 });

const MAX_JIT_MODULES = 256;
const DEFAULT_JIT_MIN_PREFIX = 65; // decoded blocks are capped at 64 instructions
const ASYNCIFY_STACK_BYTES = 1024 * 1024;

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export class AnyWasm {
  /** @param {WebAssembly.Instance} instance */
  constructor(instance, jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0 }, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX, asyncifyData = 0, asyncifyControl = { resumeSliceSteps: 0n }) {
    this.x = instance.exports;
    this.dec = new TextDecoder();
    /** Counts calls routed through the dynamic WebAssembly trampoline. */
    this.jitStats = jitStats;
    this.jitMinPrefix = jitMinPrefix;
    this.asyncifyData = asyncifyData;
    this.asyncifyControl = asyncifyControl;
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
    const modules = new Map();
    const modulesByStart = new Map();
    let activeCodeEpoch;
    const jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0 };
    const forgetModule = (id) => {
      const old = modules.get(id);
      if (old && modulesByStart.get(old.start) === old) modulesByStart.delete(old.start);
      modules.delete(id);
    };
    const imports = {
      anywasm: {
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
            }
            const memory = instance.exports.memory;
            const moduleView = new Uint8Array(memory.buffer, modulePtr >>> 0, moduleLen >>> 0);
            let cached = modules.get(id >>> 0);
            if (!cached || !bytesEqual(cached.bytes, moduleView)) {
              if (cached && modulesByStart.get(cached.start) === cached) modulesByStart.delete(cached.start);
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
            modulesByStart.set(start, cached);

            const remainingBudget = budget >>> 0;
            const state = new DataView(memory.buffer, statePtr >>> 0, 184);
            const cpuState = new Uint8Array(memory.buffer, statePtr >>> 0, 160);
            state.setUint32(176, 0, true);
            let totalExecuted = 0;
            let current = cached;
            while (totalExecuted < remainingBudget) {
              const remaining = remainingBudget - totalExecuted;
              const rollback = cpuState.slice();
              let executed = -1;
              try {
                executed = current.run(statePtr, remaining);
              } catch {
                // The failing module is retried by Rust from this block's untouched CPU state.
              }
              const expected = Math.min(remaining, current.instructions);
              if (executed !== expected) {
                cpuState.set(rollback);
                jitStats.fallbacks++;
                if (totalExecuted === 0) return -1;
                state.setUint32(176, 1, true);
                return totalExecuted;
              }
              totalExecuted += executed;
              jitStats.executions++;
              if (current.firstRegisterRegister !== 0xffff_ffff && executed > current.firstRegisterRegister) {
                jitStats.registerRegisterHits++;
              }
              if (totalExecuted >= remainingBudget) break;
              const nextRip = state.getBigUint64(128, true);
              const next = modulesByStart.get(nextRip);
              if (!next) break;
              state.setUint32(176, 1, true);
              jitStats.chainTransitions++;
              current = next;
            }
            return totalExecuted;
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
      },
    };
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
    const x = instance.exports;
    if (typeof x.asyncify_start_unwind === 'function' && typeof x.asyncify_stop_unwind === 'function' &&
        typeof x.asyncify_start_rewind === 'function' && typeof x.asyncify_stop_rewind === 'function' &&
        typeof x.asyncify_get_state === 'function') {
      asyncifyData = x.aw_alloc(ASYNCIFY_STACK_BYTES + 8);
      const view = new DataView(x.memory.buffer);
      view.setUint32(asyncifyData, asyncifyData + 8, true);
      view.setUint32(asyncifyData + 4, asyncifyData + ASYNCIFY_STACK_BYTES + 8, true);
    }
    return new AnyWasm(instance, jitStats, jitMinPrefix, asyncifyData, asyncifyControl);
  }

  /** Load an .awb bundle. Throws with the loader's message on failure. */
  load(bundle) {
    const n = bundle.byteLength;
    const ptr = this.x.aw_alloc(n);
    new Uint8Array(this.x.memory.buffer, ptr, n).set(bundle);
    const rc = this.x.aw_load(ptr, n);
    this.x.aw_free(ptr, n);
    if (rc !== 0) throw new Error(this.message() || 'failed to load bundle');
    this.x.aw_jit_set_min_prefix?.(this.jitMinPrefix);
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
    const out = new Uint8Array(this.x.memory.buffer, ptrFn(), len).slice();
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
    return len ? this.dec.decode(new Uint8Array(this.x.memory.buffer, this.x.aw_message_ptr(), len)) : '';
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
    const ptr = this.x.aw_alloc(n);
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
      if (yieldFn && status === Status.RUNNING) return Promise.resolve(yieldFn()).then(() => this.runToEnd({ sliceSteps, maxSlices: maxSlices - i - 1, onOutput, yieldFn }));
    }
    return { status, exitCode: this.exitCode(), message: this.message() };
  }
}
