// Interpreter throughput under WebAssembly (Node/V8). Prints guest instructions per second.
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { AnyWasm, Status } from '../web/anywasm.js';
const wasm = readFileSync(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url));
execFileSync(new URL('../target/debug/anywasm', import.meta.url).pathname, ['pack', 'fixtures/compute.exe', '-o', '/tmp/bench.awb'], { stdio: 'ignore' });
const bundle = readFileSync('/tmp/bench.awb');
for (let round = 0; round < 3; round++) {
  const w = await AnyWasm.create(wasm); w.load(bundle);
  const t0 = performance.now();
  const r = w.runToEnd({ sliceSteps: 5_000_000 });
  const dt = (performance.now() - t0) / 1000;
  const n = w.instructions();
  console.log(`round ${round}: status=${r.status} ${n.toLocaleString()} instr in ${dt.toFixed(2)}s = ${(n / dt / 1e6).toFixed(1)} M instr/s`);
}
