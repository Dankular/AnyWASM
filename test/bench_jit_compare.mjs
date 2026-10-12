// Paired cached-only versus forced-emission throughput check under Node/V8.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasm = readFileSync(fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url)));
const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const rounds = Number(process.env.ANYWASM_JIT_BENCH_ROUNDS ?? '10');
const cachedPrefix = Number(process.env.ANYWASM_JIT_CACHED_PREFIX ?? '65');
const emittedPrefix = Number(process.env.ANYWASM_JIT_EMITTED_PREFIX ?? '1');
const workloadFilter = process.env.ANYWASM_JIT_BENCH_WORKLOADS?.split(',').filter(Boolean);
const warmups = 1;
if (!Number.isInteger(rounds) || rounds < 10) throw new RangeError('ANYWASM_JIT_BENCH_ROUNDS must be an integer of at least 10');
for (const [name, value] of [['ANYWASM_JIT_CACHED_PREFIX', cachedPrefix], ['ANYWASM_JIT_EMITTED_PREFIX', emittedPrefix]]) {
  if (!Number.isInteger(value) || value < 1 || value > 0xffff_ffff) throw new RangeError(`${name} must be an integer from 1 to 4294967295`);
}
const bundleDir = mkdtempSync(join(tmpdir(), 'anywasm-jit-bench-'));

const median = (samples) => {
  const sorted = [...samples].sort((a, b) => a - b);
  const middle = sorted.length >> 1;
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};

async function run(bundle, jitMinPrefix) {
  const runtime = await AnyWasm.create(wasm, { jitMinPrefix });
  runtime.load(bundle);
  let stdout = '', stderr = '';
  const started = performance.now();
  const result = runtime.runToEnd({
    sliceSteps: 5_000_000,
    onOutput(stream, text) {
      if (stream === 'stdout') stdout += text;
      else stderr += text;
    },
  });
  const seconds = (performance.now() - started) / 1000;
  const instructions = runtime.instructions();
  if (result.status !== Status.EXITED || result.exitCode !== 0) {
    throw new Error(`benchmark fixture stopped: status=${result.status}, exit=${result.exitCode}, message=${result.message}`);
  }
  return {
    status: result.status,
    exitCode: result.exitCode,
    stdout,
    stderr,
    instructions,
    seconds,
    mips: instructions / seconds / 1e6,
    jit: runtime.jitStats,
  };
}

const sameResult = (a, b) => a.status === b.status && a.exitCode === b.exitCode &&
  a.stdout === b.stdout && a.stderr === b.stderr && a.instructions === b.instructions;

try {
  const allWorkloads = [
    ['compute', 'compute.exe'],
    ['call-heavy', 'bench_call.exe'],
    ['memory-heavy', 'bench_memory.exe'],
    ['sse-float', 'bench_sse.exe'],
    ['block-chain', 'jit_state.exe'],
  ];
  const workloads = workloadFilter
    ? allWorkloads.filter(([name]) => workloadFilter.includes(name))
    : allWorkloads;
  if (workloads.length === 0) throw new Error(`no requested benchmark workloads matched: ${workloadFilter?.join(', ')}`);
  console.log(`paired cached-only vs emitted benchmark; rounds=${rounds}; warmups=${warmups}; ` +
    `prefixes=${cachedPrefix}/${emittedPrefix}; sliceSteps=5000000`);
  for (const [name, executable] of workloads) {
    const bundlePath = join(bundleDir, `${name}.awb`);
    const fixturePath = fileURLToPath(new URL(`../fixtures/${executable}`, import.meta.url));
    execFileSync(cliPath, ['pack', fixturePath, '-o', bundlePath], { cwd: repoRoot, stdio: 'ignore' });
    const bundle = readFileSync(bundlePath);
    for (let i = 0; i < warmups; i++) {
      const cached = await run(bundle, cachedPrefix);
      const emitted = await run(bundle, emittedPrefix);
      if (!sameResult(cached, emitted)) throw new Error(`${name} warm-up cached/emitted results differ`);
    }
    const samples = { cached: [], emitted: [] };
    let representative = {};
    for (let round = 0; round < rounds; round++) {
      const order = round % 2 === 0 ? ['cached', 'emitted'] : ['emitted', 'cached'];
      const pair = {};
      for (const mode of order) pair[mode] = await run(bundle, mode === 'cached' ? cachedPrefix : emittedPrefix);
      if (!sameResult(pair.cached, pair.emitted)) {
        throw new Error(`${name} round ${round + 1}: cached/emitted status, output, or instruction count differs`);
      }
      samples.cached.push(pair.cached.mips);
      samples.emitted.push(pair.emitted.mips);
      representative = pair;
    }
    const cachedMedian = median(samples.cached);
    const emittedMedian = median(samples.emitted);
    console.log(JSON.stringify({
      workload: name,
      rounds,
      warmups,
      instructions: representative.cached.instructions,
      cachedMedianMips: Number(cachedMedian.toFixed(2)),
      emittedMedianMips: Number(emittedMedian.toFixed(2)),
      gainPercent: Number(((emittedMedian / cachedMedian - 1) * 100).toFixed(1)),
      emittedJit: representative.emitted.jit,
      cachedJit: representative.cached.jit,
    }));
  }
} finally {
  rmSync(bundleDir, { recursive: true, force: true });
}
