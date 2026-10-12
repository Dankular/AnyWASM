// WASM executor throughput under Node/V8, including generated-block trampoline activity.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasmPath = fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url));
const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const jitMinPrefix = Number(process.env.ANYWASM_JIT_MIN_PREFIX ?? '65');
const rounds = 6;
const wasm = readFileSync(wasmPath);
const bundleDir = mkdtempSync(join(tmpdir(), 'anywasm-bench-'));

try {
  const workloads = [
    ['compute', 'compute.exe'],
    ['call-heavy', 'bench_call.exe'],
    ['memory-heavy', 'bench_memory.exe'],
    ['sse-float', 'bench_sse.exe'],
    ['block-chain', 'jit_state.exe'],
  ];

  console.log(`minimum emitted MOV prefix: ${jitMinPrefix}; rounds: ${rounds}`);
  for (const [name, executable] of workloads) {
    const bundlePath = join(bundleDir, `${name}.awb`);
    const fixturePath = fileURLToPath(new URL(`../fixtures/${executable}`, import.meta.url));
    execFileSync(cliPath, ['pack', fixturePath, '-o', bundlePath], { cwd: repoRoot, stdio: 'ignore' });
    const bundle = readFileSync(bundlePath);
    console.log(`${name}:`);
    for (let round = 0; round < rounds; round++) {
      const w = await AnyWasm.create(wasm, { jitMinPrefix }); w.load(bundle);
      const t0 = performance.now();
      const r = w.runToEnd({ sliceSteps: 5_000_000 });
      const dt = (performance.now() - t0) / 1000;
      const n = w.instructions();
      if (r.status !== Status.EXITED || r.exitCode !== 0) {
        throw new Error(`${name} did not exit cleanly: status=${r.status}, exit=${r.exitCode}, message=${r.message}`);
      }
      const jit = w.jitStats;
      console.log(`  round ${round}: status=${r.status} exit=${r.exitCode} ${n.toLocaleString()} instr in ${dt.toFixed(2)}s = ${(n / dt / 1e6).toFixed(1)} M instr/s; emitted=${jit.executions} modules=${jit.compilations} chainModules=${jit.chainCompilations} chains=${jit.chainTransitions} regreg=${jit.registerRegisterHits} fallback=${jit.fallbacks}`);
    }
  }
} finally {
  rmSync(bundleDir, { recursive: true, force: true });
}
