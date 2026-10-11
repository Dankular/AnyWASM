// Runs real Windows executables through the WebAssembly build under Node.
// Usage: node test/wasm_smoke.mjs   (expects target/wasm32-unknown-unknown/release/anywasm_wasm.wasm
//                                    and target/debug/anywasm for packing)
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';
import { runJitCachePressure } from './jit_cache_pressure.mjs';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasm = readFileSync(fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url)));
const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-smoke-'));
process.on('exit', () => rmSync(tempDir, { recursive: true, force: true }));
let failures = 0;
const check = (name, ok, detail = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : ' :: ' + detail}`); if (!ok) failures++; };

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

async function runBundle(bundle, runtimeOptions = {}) {
  const w = await AnyWasm.create(wasm, runtimeOptions);
  w.load(bundle);
  let out = '', err = '';
  const r = w.runToEnd({ onOutput: (s, t) => (s === 'stdout' ? (out += t) : (err += t)) });
  return { ...r, out, err, instr: w.instructions(), jitStats: w.jitStats };
}

async function runExe(exe, extra = [], runtimeOptions = {}) {
  return runBundle(pack(exe, extra), runtimeOptions);
}

let r = await runExe('fixtures/hello.exe');
check('hello.exe exits 0 with greeting', r.status === Status.EXITED && r.exitCode === 0 && r.out === 'Hello from a Windows PE running inside AnyWASM!\r\n', JSON.stringify(r));

const computeBundle = pack('fixtures/compute.exe');
r = await runBundle(computeBundle, { jitMinPrefix: 1 });
check('compute.exe (heap, SSE2, indirect calls)', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('primes<20000=2262') && r.out.includes('fp=609') && r.out.includes('gpa=ok'), JSON.stringify(r));
check('compute.exe executes generated WebAssembly blocks', r.jitStats.executions > 0 && r.jitStats.compilations > 0 && r.jitStats.fallbacks === 0, JSON.stringify(r.jitStats));
check('compute.exe executes generated register-to-register MOVs', r.jitStats.registerRegisterHits > 0, JSON.stringify(r.jitStats));

const cachedOnly = await runBundle(computeBundle);
check('default emission gate skips short prefixes', cachedOnly.jitStats.executions === 0 && cachedOnly.jitStats.compilations === 0 && cachedOnly.jitStats.fallbacks === 0,
  JSON.stringify(cachedOnly.jitStats));
check('cached-only execution matches emitted compute output and count',
  r.status === cachedOnly.status && r.exitCode === cachedOnly.exitCode && r.out === cachedOnly.out && r.err === cachedOnly.err && r.instr === cachedOnly.instr,
  JSON.stringify({ emitted: { status: r.status, exitCode: r.exitCode, instr: r.instr }, cached: { status: cachedOnly.status, exitCode: cachedOnly.exitCode, instr: cachedOnly.instr } }));

const interpreted = await runBundle(computeBundle, { jit: false, jitMinPrefix: 1 });
check('compute.exe matches generated execution with interpreter fallback',
  r.status === interpreted.status && r.exitCode === interpreted.exitCode && r.out === interpreted.out && r.err === interpreted.err && r.instr === interpreted.instr,
  JSON.stringify({ generated: { status: r.status, exitCode: r.exitCode, out: r.out, err: r.err, instr: r.instr }, interpreter: { status: interpreted.status, exitCode: interpreted.exitCode, out: interpreted.out, err: interpreted.err, instr: interpreted.instr } }));
check('JIT opt-out uses interpreter fallback without compiling modules',
  interpreted.jitStats.executions === 0 && interpreted.jitStats.compilations === 0 && interpreted.jitStats.fallbacks > 0,
  JSON.stringify(interpreted.jitStats));

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
check('host compile failure falls back safely and resumes emitted execution',
  forcedCompileFailures === 1 && compileFailureWasm.jitStats.fallbacks === 1 && compileFailureWasm.jitStats.executions > 0 &&
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
