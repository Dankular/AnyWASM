// Drives the real web page in headless Chromium and checks the program output on screen.
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';

const server = spawn('node', ['web/serve.mjs', '8099'], { cwd: new URL('../../', import.meta.url).pathname, stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 800));
let failures = 0;
const check = (n, ok, d = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${n}${ok ? '' : ' :: ' + d}`); if (!ok) failures++; };

const dir = '/opt/pw-browsers';
const versioned = readdirSync(dir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const exe = process.env.CHROMIUM || (versioned ? `${dir}/${versioned}/chrome-linux/chrome` : undefined);
const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'] });
try {
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (m) => logs.push(m.text()));
  page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));

  await page.goto('http://localhost:8099/index.html?run=hello');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const text = await page.textContent('#term');
  const stat = await page.textContent('#stat');
  check('hello.exe output shown in the page', text.includes('Hello from a Windows PE running inside AnyWASM!'), text);
  check('status shows clean exit', /exited \(exit code 0\)/.test(stat), stat);

  await page.goto('http://localhost:8099/index.html?run=crt_test');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const t2 = await page.textContent('#term');
  check('crt_test.exe full output in the page', t2.includes('sscanf=3 x+y=46 word=hello') && t2.includes('atexit handler ran') && t2.includes('arg2=second arg'), t2);
  check('crt_test.exe stderr shown', t2.includes('to stderr'), t2);
  check('crt_test.exe exit code 7', (await page.getAttribute('#stat', 'data-exit')) === '7');

  const caps = await page.textContent('#caps');
  console.log('capabilities:', caps.replace(/\s+/g, ' ').trim());
  const gpu = await page.evaluate(async () => {
    if (!('gpu' in navigator)) return 'navigator.gpu missing';
    try { const a = await navigator.gpu.requestAdapter(); return a ? 'adapter: ' + (a.info?.description || a.info?.vendor || 'ok') : 'no adapter'; } catch (e) { return 'error: ' + e.message; }
  });
  console.log('WebGPU in this headless Chromium:', gpu);
  check('no page errors', !logs.some((l) => l.startsWith('PAGEERROR')), logs.join('\n'));
} finally {
  await browser.close();
  server.kill();
}
console.log(failures ? `\n${failures} FAILED` : '\nbrowser tests passed');
process.exit(failures ? 1 : 0);
