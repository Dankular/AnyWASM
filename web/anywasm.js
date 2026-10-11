// AnyWASM runtime glue: loads the WASM module and drives a Windows x64 process inside it.
// Works unchanged in browsers (main thread or worker) and Node >= 18.
import { fillCryptoRandom } from './entropy.js';

export const Status = Object.freeze({ RUNNING: 0, EXITED: 1, UNIMPLEMENTED: 2, CRASHED: 3, FRAME: 4, IDLE: 5 });

const MAX_JIT_MODULES = 256;
const DEFAULT_JIT_MIN_PREFIX = 65; // decoded blocks are capped at 64 instructions

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export class AnyWasm {
  /** @param {WebAssembly.Instance} instance */
  constructor(instance, jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0 }, jitMinPrefix = DEFAULT_JIT_MIN_PREFIX) {
    this.x = instance.exports;
    this.dec = new TextDecoder();
    /** Counts calls routed through the dynamic WebAssembly trampoline. */
    this.jitStats = jitStats;
    this.jitMinPrefix = jitMinPrefix;
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
    const modules = new Map();
    const jitStats = { compilations: 0, executions: 0, fallbacks: 0, registerRegisterHits: 0 };
    const imports = {
      anywasm: {
        random_fill(pointer, length) {
          try {
            return fillCryptoRandom(instance.exports.memory, pointer, length);
          } catch {
            return 0;
          }
        },
        run_emitted_block(id, modulePtr, moduleLen, statePtr, budget, emittedInstructions, firstRegisterRegister) {
          try {
            if (!jit) {
              jitStats.fallbacks++;
              return -1;
            }
            const memory = instance.exports.memory;
            const moduleView = new Uint8Array(memory.buffer, modulePtr >>> 0, moduleLen >>> 0);
            let cached = modules.get(id >>> 0);
            if (!cached || !bytesEqual(cached.bytes, moduleView)) {
              const moduleBytes = moduleView.slice();
              const module = new WebAssembly.Module(moduleBytes);
              const emitted = new WebAssembly.Instance(module, { anywasm: { memory } });
              cached = { bytes: moduleBytes, run: emitted.exports.run };
              modules.set(id >>> 0, cached);
              if (modules.size > MAX_JIT_MODULES) modules.delete(modules.keys().next().value);
              jitStats.compilations++;
            }
            const executed = cached.run(statePtr, budget);
            const expected = Math.min(budget >>> 0, emittedInstructions >>> 0);
            if (executed !== expected) {
              jitStats.fallbacks++;
              return -1;
            }
            jitStats.executions++;
            const firstRegisterMove = firstRegisterRegister >>> 0;
            if (firstRegisterMove !== 0xffff_ffff && executed > firstRegisterMove) {
              jitStats.registerRegisterHits++;
            }
            return executed;
          } catch {
            // The Rust side passed a copied CPU state, so the interpreter can retry safely.
            jitStats.fallbacks++;
            return -1;
          }
        },
      },
    };
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
    return new AnyWasm(instance, jitStats, jitMinPrefix);
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
    return this.x.aw_run(BigInt(steps));
  }

  _take(ptrFn, lenFn, clearFn) {
    const len = lenFn();
    if (!len) return new Uint8Array(0);
    const out = new Uint8Array(this.x.memory.buffer, ptrFn(), len).slice();
    clearFn();
    return out;
  }
  /** Window/GPU command stream produced since the last call (copied out of WASM memory). */
  takeGpu() { return this._take(this.x.aw_gpu_ptr, this.x.aw_gpu_len, this.x.aw_gpu_clear); }
  /** Deliver an input event: see crates/anywasm-rt/src/hle/user32.rs `input_event`. */
  input(kind, a = 0, b = 0, c = 0) { this.x.aw_input(kind, BigInt(a), BigInt(b), BigInt(c)); }
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
    const n = bytes.byteLength;
    const ptr = this.x.aw_alloc(n);
    new Uint8Array(this.x.memory.buffer, ptr, n).set(bytes);
    this.x.aw_stdin_push(ptr, n);
    this.x.aw_free(ptr, n);
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
