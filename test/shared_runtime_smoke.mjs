// Runs real Windows executables on the runtime built against an imported SHARED WebAssembly.Memory
// (scripts/build-shared-runtime.sh). The coordinator and (later) CPU workers then address the
// same linear memory, so guest frames never need to be copied between them.
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { AnyWasm, Status } from '../web/anywasm.js';

const wasm = readFileSync(new URL('../web/anywasm_wasm_shared.wasm', import.meta.url));
let failures = 0;
const check = (name, ok, detail = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : ' :: ' + detail}`); if (!ok) failures++; };

async function run(exe, extra = [], options = {}) {
  execFileSync(new URL('../target/debug/anywasm', import.meta.url).pathname, ['pack', `fixtures/${exe}`, '-o', `/tmp/${exe}.awb`, ...extra], { stdio: 'ignore' });
  const memory = new WebAssembly.Memory({ initial: 4096, maximum: 32768, shared: true });
  const w = await AnyWasm.create(wasm, { memory, ...options });
  w.load(readFileSync(`/tmp/${exe}.awb`));
  let out = '';
  const r = w.runToEnd({ onOutput: (stream, text) => { if (stream === 'stdout') out += text; } });
  const status = r.status;
  return { status, exitCode: w.exitCode(), out, shared: memory.buffer instanceof SharedArrayBuffer, jit: { ...w.jitStats } };
}

let r = await run('hello.exe');
check('memory is a SharedArrayBuffer', r.shared);
check('hello.exe runs on shared-memory runtime', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('Hello from a Windows PE'), JSON.stringify(r));
r = await run('crt_test.exe', ['--args', '"C:\\crt_test.exe" first "second arg"']);
check('crt_test.exe (real msvcrt imports) matches on shared memory', r.status === Status.EXITED && r.exitCode === 7 && r.out.includes('sscanf=3 x+y=46 word=hello'), JSON.stringify(r).slice(0, 400));
r = await run('parallel_test.exe');
check('parallel_test.exe cooperative threads on shared memory', r.status === Status.EXITED && r.exitCode === 0 && r.out.includes('t0=5133 t1=4459 total=9592'), JSON.stringify(r));
// NOTE: emitted-block execution on shared memory is not covered yet: the import rewrite in
// web/anywasm.js (withSharedMemoryImport) is unverified because forced emission produced no
// compilations in this harness for the plain build either (cause not yet found).
console.log(failures ? `\n${failures} FAILED` : '\nshared runtime smoke passed');
process.exit(failures ? 1 : 0);
