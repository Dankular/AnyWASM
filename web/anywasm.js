// AnyWASM runtime glue: loads the WASM module and drives a Windows x64 process inside it.
// Works unchanged in browsers (main thread or worker) and Node >= 18.

export const Status = Object.freeze({ RUNNING: 0, EXITED: 1, UNIMPLEMENTED: 2, CRASHED: 3 });

export class AnyWasm {
  /** @param {WebAssembly.Instance} instance */
  constructor(instance) {
    this.x = instance.exports;
    this.dec = new TextDecoder();
  }

  /** Instantiate from raw WASM bytes (Uint8Array/ArrayBuffer) or a Response/URL string. */
  static async create(source) {
    let bytes = source;
    if (typeof source === 'string') bytes = await (await fetch(source)).arrayBuffer();
    else if (typeof Response !== 'undefined' && source instanceof Response) bytes = await source.arrayBuffer();
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new AnyWasm(instance);
  }

  /** Load an .awb bundle. Throws with the loader's message on failure. */
  load(bundle) {
    const n = bundle.byteLength;
    const ptr = this.x.aw_alloc(n);
    new Uint8Array(this.x.memory.buffer, ptr, n).set(bundle);
    const rc = this.x.aw_load(ptr, n);
    this.x.aw_free(ptr, n);
    if (rc !== 0) throw new Error(this.message() || 'failed to load bundle');
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
  takeStdout() { return this._take(this.x.aw_stdout_ptr, this.x.aw_stdout_len, this.x.aw_stdout_clear); }
  takeStderr() { return this._take(this.x.aw_stderr_ptr, this.x.aw_stderr_len, this.x.aw_stderr_clear); }
  message() {
    const len = this.x.aw_message_len();
    return len ? this.dec.decode(new Uint8Array(this.x.memory.buffer, this.x.aw_message_ptr(), len)) : '';
  }
  exitCode() { return this.x.aw_exit_code(); }
  instructions() { return Number(this.x.aw_icount()); }

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
    for (let i = 0; i < maxSlices && status === Status.RUNNING; i++) {
      status = this.run(sliceSteps);
      const o = this.takeStdout(); if (o.length) onOutput('stdout', textDec.decode(o));
      const e = this.takeStderr(); if (e.length) onOutput('stderr', textDec.decode(e));
      if (yieldFn && status === Status.RUNNING) return Promise.resolve(yieldFn()).then(() => this.runToEnd({ sliceSteps, maxSlices: maxSlices - i - 1, onOutput, yieldFn }));
    }
    return { status, exitCode: this.exitCode(), message: this.message() };
  }
}
