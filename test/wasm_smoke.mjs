// Runs real Windows executables through the WebAssembly build under Node.
// Usage: node test/wasm_smoke.mjs   (expects target/wasm32-unknown-unknown/release/anywasm_wasm.wasm
//                                    and target/debug/anywasm for packing)
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasm = readFileSync(fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url)));
const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-smoke-'));
process.on('exit', () => rmSync(tempDir, { recursive: true, force: true }));
let failures = 0;
const check = (name, ok, detail = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : ' :: ' + detail}`); if (!ok) failures++; };

function pack(exe, extra = []) {
  const out = join(tempDir, `${exe.split('/').pop()}.awb`);
  execFileSync(cliPath, ['pack', join(repoRoot, exe), '-o', out, ...extra], { cwd: repoRoot, stdio: 'ignore' });
  return readFileSync(out);
}

async function runExe(exe, extra = []) {
  const w = await AnyWasm.create(wasm);
  w.load(pack(exe, extra));
  let out = '', err = '';
  const r = w.runToEnd({ onOutput: (s, t) => (s === 'stdout' ? (out += t) : (err += t)) });
  return { ...r, out, err, instr: w.instructions() };
}

let r = await runExe('fixtures/hello.exe');
check('hello.exe exits 0 with greeting', r.status === Status.EXITED && r.exitCode === 0 && r.out === 'Hello from a Windows PE running inside AnyWASM!\r\n', JSON.stringify(r));

r = await runExe('fixtures/compute.exe');
check('compute.exe (heap, SSE2, indirect calls)', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('primes<20000=2262') && r.out.includes('fp=609') && r.out.includes('gpa=ok'), JSON.stringify(r));

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
