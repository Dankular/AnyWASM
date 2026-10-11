// Runs real Windows executables through the WebAssembly build under Node.
// Usage: node test/wasm_smoke.mjs   (expects target/wasm32-unknown-unknown/release/anywasm_wasm.wasm
//                                    and target/debug/anywasm for packing)
import { readFileSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { AnyWasm, Status } from '../web/anywasm.js';

const wasm = readFileSync(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url));
let failures = 0;
const check = (name, ok, detail = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : ' :: ' + detail}`); if (!ok) failures++; };

function pack(exe, extra = []) {
  const out = `/tmp/${exe.split('/').pop()}.awb`;
  execFileSync(new URL('../target/debug/anywasm', import.meta.url).pathname, ['pack', exe, '-o', out, ...extra], { stdio: 'ignore' });
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
const tmp = mkdtempSync(`${tmpdir()}/aw-`);
execFileSync('gcc', ['-O2', '-o', `${tmp}/crt_native`, 'fixtures/crt_test.c', '-lm']);
const nat = spawnSync(`${tmp}/crt_native`, ['first', 'second arg'], { cwd: tmp });
const native = nat.stdout.toString().replace('argv0_has_exe=0', 'argv0_has_exe=1');
check('crt_test.exe (mingw msvcrt) exit code 7', r.status === Status.EXITED && r.exitCode === 7, JSON.stringify(r));
check('crt_test.exe output identical to native build', r.out.replace(/\r\n/g, '\n') === native, `\n--- wasm\n${r.out}\n--- native\n${native}`);
check('crt_test.exe stderr captured', r.err === 'to stderr\n', JSON.stringify(r.err));

r = await runExe('fixtures/d3dprobe.exe');
check('d3dprobe.exe reports the unimplemented import by name', r.status === Status.UNIMPLEMENTED && /D3D11CreateDevice|MessageBoxA/.test(r.message), JSON.stringify(r));

try { const w = await AnyWasm.create(wasm); w.load(new Uint8Array([1, 2, 3])); check('bad bundle rejected', false); }
catch (e) { check('bad bundle rejected with message', /corrupt bundle/.test(e.message), e.message); }

console.log(failures ? `\n${failures} FAILED` : '\nall wasm smoke tests passed');
process.exit(failures ? 1 : 0);
