// Runs real Windows executables through the WebAssembly build under Node.
// Usage: node test/wasm_smoke.mjs   (expects a `jit-test-api` asyncified WASM build and
//                                    target/debug/anywasm for packing)
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';
import { fillCryptoRandom } from '../web/entropy.js';
import { runJitCachePressure } from './jit_cache_pressure.mjs';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasm = readFileSync(fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm', import.meta.url)));
const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-smoke-'));
process.on('exit', () => rmSync(tempDir, { recursive: true, force: true }));
let failures = 0;
const check = (name, ok, detail = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : ' :: ' + detail}`); if (!ok) failures++; };

const entropyMemory = new WebAssembly.Memory({ initial: 2 });
const entropyChunks = [];
const entropyFilled = fillCryptoRandom(entropyMemory, 32, 65_539, {
  getRandomValues(view) {
    entropyChunks.push(view.byteLength);
    view.fill(entropyChunks.length);
    return view;
  },
});
const entropyBytes = new Uint8Array(entropyMemory.buffer, 32, 65_539);
check('Web Crypto helper fills oversized requests in valid chunks',
  entropyFilled === 1 && JSON.stringify(entropyChunks) === '[65536,3]' &&
    entropyBytes[0] === 1 && entropyBytes[65_535] === 1 && entropyBytes[65_536] === 2 && entropyBytes[65_538] === 2,
  JSON.stringify({ entropyChunks, first: entropyBytes[0], boundary: entropyBytes[65_536] }));
check('Web Crypto helper accepts zero length without a provider',
  fillCryptoRandom(null, 0xffff_ffff, 0, null) === 1);
let entropyFailureCalls = 0;
check('Web Crypto helper fails closed for missing entropy and invalid memory',
  fillCryptoRandom(entropyMemory, 0, 8, null) === 0 &&
    fillCryptoRandom(entropyMemory, 131_071, 2, { getRandomValues() { entropyFailureCalls++; } }) === 0 &&
    entropyFailureCalls === 0);

const helloReport = JSON.parse(execFileSync(cliPath, ['analyze', join(repoRoot, 'fixtures/hello.exe'), '--json'], { cwd: repoRoot, encoding: 'utf8' }));
const legacyAnalyzeFields = ['machine', 'subsystem', 'dll', 'dotnet', 'imageBase', 'implemented', 'missing', 'features', 'blockers', 'imports'];
check('analyze --json v1 preserves the established report fields',
  helloReport.schemaVersion === 1 && legacyAnalyzeFields.every((key) => Object.hasOwn(helloReport, key)),
  JSON.stringify(helloReport));
check('analyze --json v1 preserves legacy field types',
  typeof helloReport.machine === 'string' && typeof helloReport.subsystem === 'string' &&
    typeof helloReport.dll === 'boolean' && typeof helloReport.dotnet === 'boolean' &&
    typeof helloReport.imageBase === 'string' && typeof helloReport.implemented === 'number' &&
    typeof helloReport.missing === 'number' && Array.isArray(helloReport.features) &&
    Array.isArray(helloReport.blockers) && Array.isArray(helloReport.imports),
  JSON.stringify(helloReport));
check('analyze preflight with no known blockers does not guarantee runtime success',
  helloReport.preflight?.status === 'no_known_blockers' && helloReport.preflight.staticOnly === true &&
    helloReport.preflight.runtimeSuccessGuaranteed === false && helloReport.preflight.definitiveBlockers.length === 0,
  JSON.stringify(helloReport.preflight));

const unimplReport = JSON.parse(execFileSync(cliPath, ['analyze', join(repoRoot, 'fixtures/unimpl.exe'), '--json'], { cwd: repoRoot, encoding: 'utf8' }));
const potentiallyUnusedGroups = unimplReport.preflight?.potentiallyUnusedMissingImports ?? [];
const missingImportGroup = potentiallyUnusedGroups.find((entry) => entry.symbols?.includes('CreateToolhelp32Snapshot'));
check('analyze preflight separates potentially unused missing imports from known blockers',
  unimplReport.preflight?.status === 'review' && unimplReport.preflight.staticOnly === true &&
    unimplReport.preflight.runtimeSuccessGuaranteed === false && unimplReport.preflight.definitiveBlockers.length === 0 &&
    unimplReport.missing === 1 && typeof missingImportGroup?.dll === 'string' && Array.isArray(missingImportGroup.symbols),
  JSON.stringify(unimplReport.preflight));

const jitPressure = await runJitCachePressure(AnyWasm);
check('JIT cache evicts old modules after 256 entries and recompiles evicted code',
  jitPressure.firstStatus === 0 && jitPressure.probeStatus === 0 &&
    jitPressure.afterPressure.compilations === 344 && jitPressure.afterPressure.executions === 344 &&
    jitPressure.afterPressure.fallbacks === 0 && jitPressure.afterProbe.compilations === 345 &&
    jitPressure.afterProbe.executions === 390 && jitPressure.afterProbe.fallbacks === 0,
  JSON.stringify(jitPressure));

function pack(exe, extra = []) {
  const out = join(tempDir, `${exe.split('/').pop()}.awb`);
  execFileSync(cliPath, ['pack', join(repoRoot, exe), '-o', out, ...extra], { cwd: repoRoot, stdio: 'ignore' });
  return readFileSync(out);
}

async function runBundle(bundle, runtimeOptions = {}, captureState = false) {
  const w = await AnyWasm.create(wasm, runtimeOptions);
  w.load(bundle);
  let out = '', err = '';
  const r = w.runToEnd({ onOutput: (s, t) => (s === 'stdout' ? (out += t) : (err += t)) });
  let cpuSnapshot = null, memorySnapshot = null;
  if (captureState) {
    if (typeof w.x.aw_test_cpu_snapshot !== 'function' || typeof w.x.aw_test_memory_snapshot !== 'function') {
      throw new Error('WASM smoke build is missing the jit-test-api snapshot exports');
    }
    const cpuSize = 776;
    const cpuPtr = w.x.aw_alloc(cpuSize);
    try {
      const written = w.x.aw_test_cpu_snapshot(cpuPtr, cpuSize);
      if (written !== cpuSize) throw new Error(`CPU snapshot wrote ${written} bytes, expected ${cpuSize}`);
      cpuSnapshot = new Uint8Array(w.x.memory.buffer, cpuPtr, cpuSize).slice();
    } finally {
      w.x.aw_free(cpuPtr, cpuSize);
    }
    const memorySize = w.x.aw_test_memory_snapshot(0, 0);
    const memoryPtr = w.x.aw_alloc(memorySize);
    try {
      const written = w.x.aw_test_memory_snapshot(memoryPtr, memorySize);
      if (written !== memorySize) throw new Error(`Memory snapshot wrote ${written} bytes, expected ${memorySize}`);
      memorySnapshot = new Uint8Array(w.x.memory.buffer, memoryPtr, memorySize).slice();
    } finally {
      w.x.aw_free(memoryPtr, memorySize);
    }
  }
  return { ...r, out, err, instr: w.instructions(), retiredInstr: w.retiredInstructions?.(), jitStats: w.jitStats,
    cpuSnapshot, memorySnapshot };
}

const sameCpuSnapshot = (a, b) => a.cpuSnapshot instanceof Uint8Array && b.cpuSnapshot instanceof Uint8Array &&
  a.cpuSnapshot.byteLength === 776 && b.cpuSnapshot.byteLength === 776 &&
  a.cpuSnapshot.every((value, index) => value === b.cpuSnapshot[index]);
const sameMemorySnapshot = (a, b) => a.memorySnapshot instanceof Uint8Array && b.memorySnapshot instanceof Uint8Array &&
  a.memorySnapshot.byteLength === b.memorySnapshot.byteLength &&
  a.memorySnapshot.every((value, index) => value === b.memorySnapshot[index]);

async function runExe(exe, extra = [], runtimeOptions = {}) {
  return runBundle(pack(exe, extra), runtimeOptions);
}

let r = await runExe('fixtures/hello.exe');
check('hello.exe exits 0 with greeting', r.status === Status.EXITED && r.exitCode === 0 && r.out === 'Hello from a Windows PE running inside AnyWASM!\r\n', JSON.stringify(r));

r = await runExe('fixtures/system_random_test.exe');
check('SystemFunction036 resolves and fills through the WASM host', r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

const computeBundle = pack('fixtures/compute.exe');
r = await runBundle(computeBundle, { jitMinPrefix: 1 }, true);
check('compute.exe (heap, SSE2, indirect calls)', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('primes<20000=2262') && r.out.includes('fp=609') && r.out.includes('gpa=ok'), JSON.stringify(r));
check('compute.exe resumes emitted execution after cold-page TLB fallbacks',
  r.jitStats.executions > 0 && r.jitStats.compilations > 0 && r.jitStats.fallbacks === 2, JSON.stringify(r.jitStats));
check('compute.exe executes generated register-to-register MOVs', r.jitStats.registerRegisterHits > 0, JSON.stringify(r.jitStats));

const cachedOnly = await runBundle(computeBundle, {}, true);
check('default emission gate skips short prefixes', cachedOnly.jitStats.executions === 0 && cachedOnly.jitStats.compilations === 0 && cachedOnly.jitStats.fallbacks === 0,
  JSON.stringify(cachedOnly.jitStats));
check('cached-only execution matches emitted compute output and count',
  r.status === cachedOnly.status && r.exitCode === cachedOnly.exitCode && r.out === cachedOnly.out && r.err === cachedOnly.err && r.instr === cachedOnly.instr,
  JSON.stringify({ emitted: { status: r.status, exitCode: r.exitCode, instr: r.instr }, cached: { status: cachedOnly.status, exitCode: cachedOnly.exitCode, instr: cachedOnly.instr } }));

const interpreted = await runBundle(computeBundle, { jit: false, jitMinPrefix: 1 }, true);
check('compute.exe matches generated execution with interpreter fallback',
  r.status === interpreted.status && r.exitCode === interpreted.exitCode && r.out === interpreted.out && r.err === interpreted.err && r.instr === interpreted.instr,
  JSON.stringify({ generated: { status: r.status, exitCode: r.exitCode, out: r.out, err: r.err, instr: r.instr }, interpreter: { status: interpreted.status, exitCode: interpreted.exitCode, out: interpreted.out, err: interpreted.err, instr: interpreted.instr } }));
check('compute CPU snapshot matches cached-only and interpreter execution',
  sameCpuSnapshot(r, cachedOnly) && sameCpuSnapshot(r, interpreted),
  JSON.stringify({ emitted: r.cpuSnapshot?.length, cached: cachedOnly.cpuSnapshot?.length, interpreted: interpreted.cpuSnapshot?.length }));
check('compute mapped guest memory matches cached-only and interpreter execution',
  sameMemorySnapshot(r, cachedOnly) && sameMemorySnapshot(r, interpreted),
  JSON.stringify({ emitted: r.memorySnapshot?.length, cached: cachedOnly.memorySnapshot?.length,
    interpreted: interpreted.memorySnapshot?.length }));
check('JIT opt-out uses interpreter fallback without compiling modules',
  interpreted.jitStats.executions === 0 && interpreted.jitStats.compilations === 0 && interpreted.jitStats.fallbacks > 0,
  JSON.stringify(interpreted.jitStats));

const jitSseBundle = pack('fixtures/jit_sse.exe');
const emittedSse = await runBundle(jitSseBundle, { jitMinPrefix: 1 }, true);
const cachedSse = await runBundle(jitSseBundle, {}, true);
const interpretedSse = await runBundle(jitSseBundle, { jit: false, jitMinPrefix: 1 }, true);
check('JIT emits packed-double arithmetic for finite lanes and falls back on guarded inputs',
  emittedSse.status === Status.EXITED && emittedSse.exitCode === 0 && emittedSse.jitStats.compilations > 0 &&
    emittedSse.jitStats.executions > 0 && emittedSse.jitStats.fallbacks > 0,
  JSON.stringify({ status: emittedSse.status, exitCode: emittedSse.exitCode, jitStats: emittedSse.jitStats }));
check('exceptional packed-double values fall back and match complete CPU/memory state',
  emittedSse.status === cachedSse.status && emittedSse.status === interpretedSse.status &&
    emittedSse.exitCode === cachedSse.exitCode && emittedSse.exitCode === interpretedSse.exitCode &&
    emittedSse.instr === cachedSse.instr && emittedSse.instr === interpretedSse.instr &&
    emittedSse.retiredInstr === cachedSse.retiredInstr && emittedSse.retiredInstr === interpretedSse.retiredInstr,
  JSON.stringify({ emitted: { status: emittedSse.status, exitCode: emittedSse.exitCode, instr: emittedSse.instr },
    cached: { status: cachedSse.status, exitCode: cachedSse.exitCode, instr: cachedSse.instr },
    interpreted: { status: interpretedSse.status, exitCode: interpretedSse.exitCode, instr: interpretedSse.instr } }));
check('packed-double full CPU and mapped-memory snapshots agree across all execution paths',
  sameCpuSnapshot(emittedSse, cachedSse) && sameCpuSnapshot(emittedSse, interpretedSse) &&
    sameMemorySnapshot(emittedSse, cachedSse) && sameMemorySnapshot(emittedSse, interpretedSse));

const jitSseMxcsrBundle = pack('fixtures/jit_sse_mxcsr.exe');
const emittedSseMxcsr = await runBundle(jitSseMxcsrBundle, { jitMinPrefix: 1 }, true);
const cachedSseMxcsr = await runBundle(jitSseMxcsrBundle, {}, true);
const interpretedSseMxcsr = await runBundle(jitSseMxcsrBundle, { jit: false, jitMinPrefix: 1 }, true);
check('non-default MXCSR reaches the emitted packed-double guard and falls back',
  emittedSseMxcsr.status === Status.EXITED && emittedSseMxcsr.exitCode === 0 &&
    emittedSseMxcsr.jitStats.compilations > 0 && emittedSseMxcsr.jitStats.fallbacks > 0,
  JSON.stringify({ status: emittedSseMxcsr.status, exitCode: emittedSseMxcsr.exitCode, jitStats: emittedSseMxcsr.jitStats }));
check('non-default MXCSR fallback matches complete cached-only and interpreter state',
  emittedSseMxcsr.status === cachedSseMxcsr.status && emittedSseMxcsr.status === interpretedSseMxcsr.status &&
    emittedSseMxcsr.exitCode === cachedSseMxcsr.exitCode && emittedSseMxcsr.exitCode === interpretedSseMxcsr.exitCode &&
    emittedSseMxcsr.instr === cachedSseMxcsr.instr && emittedSseMxcsr.instr === interpretedSseMxcsr.instr &&
    sameCpuSnapshot(emittedSseMxcsr, cachedSseMxcsr) && sameCpuSnapshot(emittedSseMxcsr, interpretedSseMxcsr) &&
    sameMemorySnapshot(emittedSseMxcsr, cachedSseMxcsr) && sameMemorySnapshot(emittedSseMxcsr, interpretedSseMxcsr),
  JSON.stringify({ emitted: { status: emittedSseMxcsr.status, exitCode: emittedSseMxcsr.exitCode, instr: emittedSseMxcsr.instr },
    cached: { status: cachedSseMxcsr.status, exitCode: cachedSseMxcsr.exitCode, instr: cachedSseMxcsr.instr },
    interpreted: { status: interpretedSseMxcsr.status, exitCode: interpretedSseMxcsr.exitCode, instr: interpretedSseMxcsr.instr } }));
if (process.platform === 'win32') {
  const nativeSse = spawnSync(join(repoRoot, 'fixtures/jit_sse.exe'), [], { cwd: tempDir });
  check('native host executes the packed-double JIT fixture', nativeSse.status === 0,
    nativeSse.stderr?.toString() ?? 'no stderr');
}

const jitX87Bundle = pack('fixtures/jit_x87.exe');
const emittedX87 = await runBundle(jitX87Bundle, { jitMinPrefix: 1 }, true);
const cachedX87 = await runBundle(jitX87Bundle, {}, true);
const interpretedX87 = await runBundle(jitX87Bundle, { jit: false, jitMinPrefix: 1 }, true);
check('JIT emits f64-model x87 FLD/FXCH/FSTP and register ADD/MUL/SUB/SUBR/DIV/DIVR forms',
  emittedX87.status === Status.EXITED && emittedX87.exitCode === 0 &&
    emittedX87.jitStats.executions > 0 && emittedX87.jitStats.fallbacks === 0,
  JSON.stringify({ status: emittedX87.status, exitCode: emittedX87.exitCode, jitStats: emittedX87.jitStats }));
check('x87 emitted, cached-only, and interpreter paths agree on complete CPU/memory state',
  emittedX87.status === cachedX87.status && emittedX87.status === interpretedX87.status &&
    emittedX87.exitCode === cachedX87.exitCode && emittedX87.exitCode === interpretedX87.exitCode &&
    emittedX87.instr === cachedX87.instr && emittedX87.instr === interpretedX87.instr &&
    emittedX87.retiredInstr === cachedX87.retiredInstr && emittedX87.retiredInstr === interpretedX87.retiredInstr &&
    sameCpuSnapshot(emittedX87, cachedX87) && sameCpuSnapshot(emittedX87, interpretedX87) &&
    sameMemorySnapshot(emittedX87, cachedX87) && sameMemorySnapshot(emittedX87, interpretedX87),
  JSON.stringify({ emitted: { status: emittedX87.status, exitCode: emittedX87.exitCode, instr: emittedX87.instr },
    cached: { status: cachedX87.status, exitCode: cachedX87.exitCode, instr: cachedX87.instr },
    interpreted: { status: interpretedX87.status, exitCode: interpretedX87.exitCode, instr: interpretedX87.instr } }));

const jitX87ExceptionBundle = pack('fixtures/jit_x87_exception.exe');
const emittedX87Exception = await runBundle(jitX87ExceptionBundle, { jitMinPrefix: 1 }, true);
const cachedX87Exception = await runBundle(jitX87ExceptionBundle, {}, true);
const interpretedX87Exception = await runBundle(jitX87ExceptionBundle, { jit: false, jitMinPrefix: 1 }, true);
check('exceptional x87 arithmetic returns to the interpreter without partial emitted state',
  emittedX87Exception.status === Status.EXITED && emittedX87Exception.exitCode === 0 &&
    emittedX87Exception.jitStats.compilations > 0 && emittedX87Exception.jitStats.fallbacks > 0,
  JSON.stringify({ status: emittedX87Exception.status, exitCode: emittedX87Exception.exitCode, jitStats: emittedX87Exception.jitStats }));
check('exceptional x87 fallback matches complete cached-only and interpreter state',
  emittedX87Exception.status === cachedX87Exception.status && emittedX87Exception.status === interpretedX87Exception.status &&
    emittedX87Exception.exitCode === cachedX87Exception.exitCode && emittedX87Exception.exitCode === interpretedX87Exception.exitCode &&
    emittedX87Exception.instr === cachedX87Exception.instr && emittedX87Exception.instr === interpretedX87Exception.instr &&
    sameCpuSnapshot(emittedX87Exception, cachedX87Exception) && sameCpuSnapshot(emittedX87Exception, interpretedX87Exception) &&
    sameMemorySnapshot(emittedX87Exception, cachedX87Exception) && sameMemorySnapshot(emittedX87Exception, interpretedX87Exception),
  JSON.stringify({ emitted: { status: emittedX87Exception.status, exitCode: emittedX87Exception.exitCode, instr: emittedX87Exception.instr },
    cached: { status: cachedX87Exception.status, exitCode: cachedX87Exception.exitCode, instr: cachedX87Exception.instr },
    interpreted: { status: interpretedX87Exception.status, exitCode: interpretedX87Exception.exitCode, instr: interpretedX87Exception.instr } }));

const indirectBundle = pack('fixtures/jit_indirect.exe');
const emittedIndirect = await runBundle(indirectBundle, { jitMinPrefix: 1 }, true);
const cachedIndirect = await runBundle(indirectBundle, {}, true);
const interpretedIndirect = await runBundle(indirectBundle, { jit: false, jitMinPrefix: 1 }, true);
check('register-indirect JMP chains to an already registered emitted block',
  emittedIndirect.status === Status.EXITED && emittedIndirect.exitCode === 0 &&
    emittedIndirect.jitStats.executions > 0 && emittedIndirect.jitStats.chainTransitions > 0 &&
    emittedIndirect.jitStats.fallbacks === 0,
  JSON.stringify({ status: emittedIndirect.status, exitCode: emittedIndirect.exitCode, jitStats: emittedIndirect.jitStats }));
check('register-indirect target state matches cached-only and interpreter execution',
  emittedIndirect.status === cachedIndirect.status && emittedIndirect.status === interpretedIndirect.status &&
    emittedIndirect.exitCode === cachedIndirect.exitCode && emittedIndirect.exitCode === interpretedIndirect.exitCode &&
    emittedIndirect.instr === cachedIndirect.instr && emittedIndirect.instr === interpretedIndirect.instr &&
    sameCpuSnapshot(emittedIndirect, cachedIndirect) && sameCpuSnapshot(emittedIndirect, interpretedIndirect) &&
    sameMemorySnapshot(emittedIndirect, cachedIndirect) && sameMemorySnapshot(emittedIndirect, interpretedIndirect));

const badJmpBundle = pack('fixtures/jit_badjmp.exe');
const emittedBadJmp = await runBundle(badJmpBundle, { jitMinPrefix: 1 }, true);
const cachedBadJmp = await runBundle(badJmpBundle, {}, true);
const interpretedBadJmp = await runBundle(badJmpBundle, { jit: false, jitMinPrefix: 1 }, true);
check('noncanonical indirect target returns to normal guest-fault handling',
  emittedBadJmp.status === Status.CRASHED && emittedBadJmp.status === cachedBadJmp.status &&
    emittedBadJmp.status === interpretedBadJmp.status && emittedBadJmp.message === cachedBadJmp.message &&
    emittedBadJmp.message === interpretedBadJmp.message && emittedBadJmp.instr === cachedBadJmp.instr &&
    emittedBadJmp.instr === interpretedBadJmp.instr &&
    sameCpuSnapshot(emittedBadJmp, cachedBadJmp) && sameCpuSnapshot(emittedBadJmp, interpretedBadJmp) &&
    sameMemorySnapshot(emittedBadJmp, cachedBadJmp) && sameMemorySnapshot(emittedBadJmp, interpretedBadJmp),
  JSON.stringify({ emitted: { status: emittedBadJmp.status, message: emittedBadJmp.message, instr: emittedBadJmp.instr },
    cached: { status: cachedBadJmp.status, message: cachedBadJmp.message, instr: cachedBadJmp.instr },
    interpreted: { status: interpretedBadJmp.status, message: interpretedBadJmp.message, instr: interpretedBadJmp.instr } }));

const hleJmpBundle = pack('fixtures/jit_hle.exe');
const emittedHleJmp = await runBundle(hleJmpBundle, { jitMinPrefix: 1 }, true);
const cachedHleJmp = await runBundle(hleJmpBundle, {}, true);
const interpretedHleJmp = await runBundle(hleJmpBundle, { jit: false, jitMinPrefix: 1 }, true);
check('indirect JMP into an HLE thunk hands off to the runtime dispatcher',
  emittedHleJmp.status === Status.EXITED && emittedHleJmp.exitCode === 0 && emittedHleJmp.jitStats.executions > 0 &&
    emittedHleJmp.status === cachedHleJmp.status && emittedHleJmp.status === interpretedHleJmp.status &&
    emittedHleJmp.instr === cachedHleJmp.instr && emittedHleJmp.instr === interpretedHleJmp.instr &&
    sameCpuSnapshot(emittedHleJmp, cachedHleJmp) && sameCpuSnapshot(emittedHleJmp, interpretedHleJmp) &&
    sameMemorySnapshot(emittedHleJmp, cachedHleJmp) && sameMemorySnapshot(emittedHleJmp, interpretedHleJmp),
  JSON.stringify({ emitted: { status: emittedHleJmp.status, exitCode: emittedHleJmp.exitCode, instr: emittedHleJmp.instr, jitStats: emittedHleJmp.jitStats },
    cached: { status: cachedHleJmp.status, exitCode: cachedHleJmp.exitCode, instr: cachedHleJmp.instr },
    interpreted: { status: interpretedHleJmp.status, exitCode: interpretedHleJmp.exitCode, instr: interpretedHleJmp.instr } }));

const sseLoopBundle = pack('fixtures/bench_sse.exe');
const emittedSseLoop = await runBundle(sseLoopBundle, { jitMinPrefix: 1 });
const cachedSseLoop = await runBundle(sseLoopBundle);
const interpretedSseLoop = await runBundle(sseLoopBundle, { jit: false, jitMinPrefix: 1 });
check('packed-double benchmark emits and chains its ADD/JNE loop with exact lane results',
  emittedSseLoop.status === Status.EXITED && emittedSseLoop.exitCode === 0 &&
    emittedSseLoop.jitStats.executions > 0 && emittedSseLoop.jitStats.chainTransitions > 0,
  JSON.stringify({ status: emittedSseLoop.status, exitCode: emittedSseLoop.exitCode, jitStats: emittedSseLoop.jitStats }));
check('packed-double benchmark matches cached-only and interpreter retirement counts',
  emittedSseLoop.status === cachedSseLoop.status && emittedSseLoop.status === interpretedSseLoop.status &&
    emittedSseLoop.exitCode === cachedSseLoop.exitCode && emittedSseLoop.exitCode === interpretedSseLoop.exitCode &&
    emittedSseLoop.instr === cachedSseLoop.instr && emittedSseLoop.instr === interpretedSseLoop.instr &&
    emittedSseLoop.retiredInstr === cachedSseLoop.retiredInstr &&
    emittedSseLoop.retiredInstr === interpretedSseLoop.retiredInstr,
  JSON.stringify({ emitted: { status: emittedSseLoop.status, exitCode: emittedSseLoop.exitCode, instr: emittedSseLoop.instr },
    cached: { status: cachedSseLoop.status, exitCode: cachedSseLoop.exitCode, instr: cachedSseLoop.instr },
    interpreted: { status: interpretedSseLoop.status, exitCode: interpretedSseLoop.exitCode, instr: interpretedSseLoop.instr } }));

const jitStateBundle = pack('fixtures/jit_state.exe');
const emittedState = await runBundle(jitStateBundle, { jitMinPrefix: 3 }, true);
const cachedState = await runBundle(jitStateBundle, {}, true);
const interpretedState = await runBundle(jitStateBundle, { jit: false, jitMinPrefix: 1 }, true);
const sameJitState = (a, b) => a.status === b.status && a.exitCode === b.exitCode &&
  a.out === b.out && a.err === b.err && a.instr === b.instr && a.retiredInstr === b.retiredInstr;
check('JIT fixture emits warm memory, direct JMP/Jcc exits, and cold-page fallback',
  emittedState.status === Status.EXITED && emittedState.exitCode === 0 && emittedState.jitStats.compilations > 0 &&
    emittedState.jitStats.executions >= 200000 && emittedState.jitStats.registerRegisterHits > 0 &&
    emittedState.jitStats.fallbacks === 1 && emittedState.jitStats.chainTransitions > 0 &&
    emittedState.jitStats.chainCompilations > 0,
  JSON.stringify({ status: emittedState.status, exitCode: emittedState.exitCode, jitStats: emittedState.jitStats }));
check('emitted, cached-only, and interpreter paths agree on JIT register and memory state',
  sameJitState(emittedState, cachedState) && sameJitState(emittedState, interpretedState),
  JSON.stringify({ emitted: { status: emittedState.status, exitCode: emittedState.exitCode, instr: emittedState.instr },
    cached: { status: cachedState.status, exitCode: cachedState.exitCode, instr: cachedState.instr },
    interpreted: { status: interpretedState.status, exitCode: interpretedState.exitCode, instr: interpretedState.instr } }));
check('JIT fixture complete CPU snapshot matches cached-only and interpreter state',
  sameCpuSnapshot(emittedState, cachedState) && sameCpuSnapshot(emittedState, interpretedState),
  JSON.stringify({ emitted: emittedState.cpuSnapshot?.length, cached: cachedState.cpuSnapshot?.length,
    interpreted: interpretedState.cpuSnapshot?.length }));
check('JIT fixture mapped guest memory matches cached-only and interpreter state',
  sameMemorySnapshot(emittedState, cachedState) && sameMemorySnapshot(emittedState, interpretedState),
  JSON.stringify({ emitted: emittedState.memorySnapshot?.length, cached: cachedState.memorySnapshot?.length,
    interpreted: interpretedState.memorySnapshot?.length }));

const memoryHeavyBundle = pack('fixtures/bench_memory.exe');
const emittedMemory = await runBundle(memoryHeavyBundle, { jitMinPrefix: 1 }, true);
const cachedMemory = await runBundle(memoryHeavyBundle, {}, true);
const interpretedMemory = await runBundle(memoryHeavyBundle, { jit: false, jitMinPrefix: 1 }, true);
check('multi-page memory-add emission chains with guarded TLB fallback',
  emittedMemory.status === Status.EXITED && emittedMemory.exitCode === 0 &&
    emittedMemory.jitStats.executions > 0 && emittedMemory.jitStats.chainTransitions > 0 &&
    emittedMemory.jitStats.fallbacks > 0,
  JSON.stringify({ status: emittedMemory.status, exitCode: emittedMemory.exitCode, jitStats: emittedMemory.jitStats }));
check('memory-heavy emitted, cached-only, and interpreted paths match complete state',
  emittedMemory.status === cachedMemory.status && emittedMemory.status === interpretedMemory.status &&
    emittedMemory.exitCode === cachedMemory.exitCode && emittedMemory.exitCode === interpretedMemory.exitCode &&
    emittedMemory.out === cachedMemory.out && emittedMemory.out === interpretedMemory.out &&
    emittedMemory.err === cachedMemory.err && emittedMemory.err === interpretedMemory.err &&
    emittedMemory.instr === cachedMemory.instr && emittedMemory.instr === interpretedMemory.instr &&
    emittedMemory.retiredInstr === cachedMemory.retiredInstr && emittedMemory.retiredInstr === interpretedMemory.retiredInstr &&
    sameCpuSnapshot(emittedMemory, cachedMemory) && sameCpuSnapshot(emittedMemory, interpretedMemory) &&
    sameMemorySnapshot(emittedMemory, cachedMemory) && sameMemorySnapshot(emittedMemory, interpretedMemory),
  JSON.stringify({ emitted: { status: emittedMemory.status, exitCode: emittedMemory.exitCode, instr: emittedMemory.instr },
    cached: { status: cachedMemory.status, exitCode: cachedMemory.exitCode, instr: cachedMemory.instr },
    interpreted: { status: interpretedMemory.status, exitCode: interpretedMemory.exitCode, instr: interpretedMemory.instr } }));

if (process.platform === 'win32') {
  const nativeState = spawnSync(join(repoRoot, 'fixtures/jit_state.exe'), [], { cwd: tempDir });
  check('native host executes the same JIT state fixture', nativeState.status === 0,
    nativeState.stderr?.toString() ?? 'no stderr');
}

const compileFailureWasm = await AnyWasm.create(wasm, { jitMinPrefix: 1 });
compileFailureWasm.load(computeBundle);
const NativeModule = WebAssembly.Module;
let forcedCompileFailures = 0;
WebAssembly.Module = function (bytes) {
  if (forcedCompileFailures === 0) {
    forcedCompileFailures++;
    throw new WebAssembly.CompileError('forced emitted-module compile failure');
  }
  return new NativeModule(bytes);
};
let compileFailureOut = '', compileFailureErr = '', compileFailureRun;
try {
  compileFailureRun = compileFailureWasm.runToEnd({ onOutput: (s, t) => (s === 'stdout' ? (compileFailureOut += t) : (compileFailureErr += t)) });
} finally {
  WebAssembly.Module = NativeModule;
}
check('compile failure and cold-page TLB miss fall back safely before emitted execution resumes',
  forcedCompileFailures === 1 && compileFailureWasm.jitStats.fallbacks === r.jitStats.fallbacks + 1 && compileFailureWasm.jitStats.executions > 0 &&
    compileFailureRun.status === r.status && compileFailureRun.exitCode === r.exitCode &&
    compileFailureOut === r.out && compileFailureErr === r.err && compileFailureWasm.instructions() === r.instr,
  JSON.stringify({ forcedCompileFailures, jitStats: compileFailureWasm.jitStats, status: compileFailureRun.status,
    exitCode: compileFailureRun.exitCode, instr: compileFailureWasm.instructions() }));

const budgetWasm = await AnyWasm.create(wasm, { jitMinPrefix: 1 });
budgetWasm.load(computeBundle);
let budgetOkay = true;
for (let i = 0; i < 50_000 && budgetWasm.jitStats?.executions === 0; i++) {
  const before = budgetWasm.instructions();
  budgetWasm.run(1);
  const retired = budgetWasm.instructions() - before;
  if (retired < 0 || retired > 1) { budgetOkay = false; break; }
}
check('generated MOV blocks honor a one-instruction budget', budgetWasm.jitStats?.executions > 0 && budgetWasm.jitStats?.fallbacks === 0 && budgetOkay,
  JSON.stringify({ jitStats: budgetWasm.jitStats, instructions: budgetWasm.instructions(), budgetOkay }));

r = await runExe('fixtures/crt_test.exe', ['--args', '"C:\\crt_test.exe" first "second arg"']);
// Ground truth: the same C source compiled and run natively (it exits 7 by design).
let nat;
if (process.platform === 'win32') {
  nat = spawnSync(join(repoRoot, 'fixtures/crt_test.exe'), ['first', 'second arg'], { cwd: tempDir });
} else {
  const nativePath = join(tempDir, 'crt_native');
  execFileSync('gcc', ['-O2', '-o', nativePath, 'fixtures/crt_test.c', '-lm'], { cwd: repoRoot });
  nat = spawnSync(nativePath, ['first', 'second arg'], { cwd: tempDir });
}
check('native crt_test baseline exits 7', nat.status === 7, nat.stderr?.toString() ?? 'no stderr');
const native = (nat.stdout?.toString() ?? '').replace('argv0_has_exe=0', 'argv0_has_exe=1');
check('crt_test.exe (mingw msvcrt) exit code 7', r.status === Status.EXITED && r.exitCode === 7, JSON.stringify(r));
const normalizeLines = text => text.replace(/\r\n/g, '\n');
check('crt_test.exe output identical to native build', normalizeLines(r.out) === normalizeLines(native), `\n--- wasm\n${r.out}\n--- native\n${native}`);
check('crt_test.exe stderr captured', r.err === 'to stderr\n', JSON.stringify(r.err));

r = await runExe('fixtures/multibyte_test.exe');
check('MultiByteToWideChar handles sizing, null/short buffers, invalid lengths, and NUL input',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/lcmap_test.exe');
check('LCMapStringEx handles count-only sizing and NUL-terminated case mapping',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/thread_sync_test.exe');
check('cooperative guest threads resume condition waiters under WASM',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));
check('threaded WASM retired count includes worker-thread instructions',
  Number.isFinite(r.retiredInstr) && r.retiredInstr > r.instr,
  JSON.stringify({ activeThread: r.instr, processTotal: r.retiredInstr }));

r = await runExe('fixtures/static_tls_thread_test.exe');
check('static PE TLS initializes and remains isolated across WASM guest threads',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

const cwdDataDir = join(tempDir, 'cwd_data');
mkdirSync(join(cwdDataDir, 'game', 'assets'), { recursive: true });
writeFileSync(join(cwdDataDir, 'game', 'assets', 'config.txt'), 'vfs!!');
r = await runExe('fixtures/cwd_test.exe', ['--dir', cwdDataDir, '--cwd', 'C:\\']);
check('CreateDirectoryA/W create and enter empty directories under WASM',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));
r = await runExe('fixtures/cwd_bundle_test.exe', ['--dir', cwdDataDir, '--cwd', 'C:\\game\\assets']);
check('.awb current directory is restored by the WASM loader and resolves relative paths',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/mapvirtualkey_test.exe');
check('MapVirtualKeyW maps common US-layout scan codes and characters',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/tounicode_test.exe');
check('ToUnicode translates common keys from the supplied state',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/verconditionmask_test.exe');
check('VerSetConditionMask encodes version comparison conditions',
  r.status === Status.EXITED && r.exitCode === 0, JSON.stringify(r));

r = await runExe('fixtures/d3dprobe.exe');
check('d3dprobe.exe (D3D11CreateDevice + MessageBoxA) runs to completion', r.status === Status.EXITED && r.exitCode === 0 && r.err.includes('[MessageBox] probe: ok'), JSON.stringify(r));

r = await runExe('fixtures/unimpl.exe');
check('unimpl.exe reports the unimplemented import by name', r.status === Status.UNIMPLEMENTED && /kernel32!CreateToolhelp32Snapshot/.test(r.message) && r.out === 'before\n', JSON.stringify(r));

r = await runExe('fixtures/d3d11_triangle.exe');
check('d3d11_triangle.exe runs under WASM and emits a GPU stream', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('frames=3'), JSON.stringify(r));

r = await runExe('fixtures/d3d11_textured.exe');
check('d3d11_textured.exe runs under WASM and emits three frames', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('frames=3'), JSON.stringify(r));

try { const w = await AnyWasm.create(wasm); w.load(new Uint8Array([1, 2, 3])); check('bad bundle rejected', false); }
catch (e) { check('bad bundle rejected with message', /corrupt bundle/.test(e.message), e.message); }

console.log(failures ? `\n${failures} FAILED` : '\nall wasm smoke tests passed');
process.exit(failures ? 1 : 0);
