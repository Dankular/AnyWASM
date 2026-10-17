// Checks that the Sims 3 button streams the fixed local install without a folder picker or bundle.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { chromium } from 'playwright-core';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../../', import.meta.url));
const temp = mkdtempSync(join(tmpdir(), 'anywasm-direct-folder-'));
const sims3Install = join(temp, 'sims3-install');
mkdirSync(join(sims3Install, 'Game', 'Bin'), { recursive: true });
mkdirSync(join(sims3Install, 'Game', 'Bin', 'game', 'assets'), { recursive: true });
writeFileSync(join(sims3Install, 'Game', 'Bin', 'TS3W.exe'), readFileSync(join(repo, 'fixtures', 'hello.exe')));
writeFileSync(join(sims3Install, 'Game', 'Bin', 'game', 'assets', 'config.txt'), Buffer.from('vfs!!'));

const portProbe = createServer();
await new Promise((resolveListen) => portProbe.listen(0, '127.0.0.1', resolveListen));
const port = portProbe.address().port;
await new Promise((resolveClose) => portProbe.close(resolveClose));
const origin = `http://127.0.0.1:${port}`;
const server = spawn(process.execPath, [join(repo, 'web', 'serve.mjs'), String(port)], {
  cwd: repo,
  stdio: 'ignore',
  env: { ...process.env, ANYWASM_SIMS3_ROOT: sims3Install },
});
let browser;

try {
  let ready = false;
  for (let attempt = 0; attempt < 50 && !ready; attempt++) {
    try {
      ready = (await fetch(origin)).ok;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 100));
    }
  }
  assert.ok(ready, 'local AnyWASM page server should start');

  const browserDir = process.env.PLAYWRIGHT_BROWSERS_PATH || (process.platform === 'win32'
    ? join(homedir(), 'AppData', 'Local', 'ms-playwright')
    : '/opt/pw-browsers');
  const chromiumVersions = readdirSync(browserDir).filter((name) => /^chromium-\d+$/.test(name)).sort((a, b) => Number(a.slice(9)) - Number(b.slice(9)));
  const latest = chromiumVersions.at(-1);
  const candidates = latest ? [
    join(browserDir, latest, 'chrome-win64', 'chrome.exe'),
    join(browserDir, latest, 'chrome-win', 'chrome.exe'),
    join(browserDir, latest, 'chrome-linux', 'chrome'),
  ] : [];
  const executablePath = process.env.CHROMIUM || candidates.find((path) => existsSync(path));
  browser = await chromium.launch({ ...(executablePath ? { executablePath } : {}), headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  const pageErrors = [];
  const requestedPaths = [];
  const sims3FileRanges = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));
  page.on('request', (request) => {
    const url = new URL(request.url());
    requestedPaths.push(url.pathname);
    if (url.pathname === '/__local/sims3/file') sims3FileRanges.push(request.headers()['range'] || null);
  });
  page.setDefaultTimeout(3000);
  await page.goto(`${origin}?traceImports`);
  const pickerState = await page.evaluate(async () => ({
    folderInputs: document.querySelectorAll('input[webkitdirectory], input[directory]').length,
    folderButtons: [...document.querySelectorAll('button, label')].filter((node) => /run from folder/i.test(node.textContent)).length,
    folderPromptHandler: /window\.prompt|\.prompt\(/.test(await fetch('/main.js').then((response) => response.text())),
  }));
  assert.deepEqual(pickerState, { folderInputs: 0, folderButtons: 0, folderPromptHandler: false });

  await page.locator('#demo-sims3').click();
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30_000 });
  const sims3Result = await page.evaluate(() => ({
    status: document.getElementById('stat').dataset.status,
    exitCode: document.getElementById('stat').dataset.exit,
    stat: document.getElementById('stat').textContent,
    folderInputCount: document.querySelectorAll('input[webkitdirectory], input[directory]').length,
  }));
  assert.equal(sims3Result.status, '1', JSON.stringify(sims3Result));
  assert.equal(sims3Result.exitCode, '0', JSON.stringify(sims3Result));
  assert.equal(sims3Result.folderInputCount, 0, 'the Sims 3 launch page must not expose a folder picker');
  assert.match(await page.locator('#term').innerText(), /\[import(?:32)?\]/, 'the traceImports flag should expose runtime import calls');
  assert.ok(requestedPaths.includes('/__local/sims3/manifest'), 'the Sims 3 button should read the local install manifest');
  assert.ok(sims3FileRanges.includes(null), 'the selected executable should load directly from the local install');
  assert.equal(requestedPaths.some((path) => path.endsWith('/demo/sims3.awb')), false, 'Sims 3 launch must not request a packed bundle');
  const streamedRange = await page.evaluate(async () => {
    const response = await fetch('/__local/sims3/file?path=Game%2FBin%2Fgame%2Fassets%2Fconfig.txt', { headers: { Range: 'bytes=1-3' } });
    return { status: response.status, contentRange: response.headers.get('content-range'), text: await response.text() };
  });
  assert.deepEqual(streamedRange, { status: 206, contentRange: 'bytes 1-3/5', text: 'fs!' });
  assert.ok(sims3FileRanges.includes('bytes=1-3'));
  assert.deepEqual(pageErrors, []);
  console.log('Fixed-path Sims 3 browser smoke passed: no folder picker, direct EXE launch, and streamed local assets.');
} finally {
  await browser?.close();
  server.kill();
  const tempRoot = resolve(tmpdir()) + sep;
  const tempPath = resolve(temp);
  assert.ok(tempPath.startsWith(tempRoot), `refusing to remove test fixture outside temp root: ${tempPath}`);
  rmSync(tempPath, { recursive: true, force: true });
}
