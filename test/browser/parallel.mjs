// Runs the shared CPU guest fixtures in headless Chromium. MODE=fail-startup verifies cooperative
// fallback, and MODE=fault verifies that a worker exception reaches the coordinator's VEH path.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const faultMode = process.env.MODE === 'fault';
const fixtureName = faultMode ? 'shared_fault_test' : 'parallel_test';
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-shared-cpu-'));
const bundlePath = join(tempDir, `${fixtureName}.awb`);
const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', `${fixtureName}.exe`), '-o', bundlePath], {
  cwd: repoRoot,
  stdio: 'ignore',
});
const server = spawn(process.execPath, ['web/serve.mjs', '8131'], { cwd: repoRoot, stdio: 'ignore' });
await new Promise((resolve) => setTimeout(resolve, 800));
const browserDir = process.env.PLAYWRIGHT_BROWSERS_PATH || (process.platform === 'win32'
  ? join(homedir(), 'AppData', 'Local', 'ms-playwright')
  : '/opt/pw-browsers');
const versioned = process.env.CHROMIUM ? undefined
  : readdirSync(browserDir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const defaultExecutable = versioned && (process.platform === 'win32'
  ? join(browserDir, versioned, 'chrome-win', 'chrome.exe')
  : join(browserDir, versioned, 'chrome-linux', 'chrome'));
const executablePath = process.env.CHROMIUM || defaultExecutable;
const browser = await chromium.launch({ executablePath, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (m) => logs.push(m.text()));
  page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));
  const inPlace = process.env.MODE === 'in-place';
  const failStartup = process.env.MODE === 'fail-startup';
  if (failStartup) {
    await page.route('**/anywasm_cpu_worker.wasm', (route) => route.fulfill({
      status: 503,
      body: 'simulated shared CPU worker startup failure',
    }));
  }
  await page.goto(`http://localhost:8131/index.html?${inPlace ? 'inPlaceCpu' : 'sharedCpuWorkers'}`);
  await page.locator('#file').setInputFiles(bundlePath);
  try {
    await page.waitForFunction(() => document.getElementById('stat')?.dataset.status !== undefined, null, { timeout: 120000 });
  } catch (error) {
    const diagnostic = await page.evaluate(() => ({
      status: document.getElementById('stat')?.textContent,
      output: document.getElementById('term')?.textContent,
      executionMode: window.__executionMode,
    }));
    throw new Error(`${error.message}\n${JSON.stringify({ diagnostic, logs })}`);
  }
  const output = await page.textContent('#term');
  const stat = await page.textContent('#stat');
  const mode = await page.evaluate(() => window.__executionMode);
  console.log(`mode=${mode?.mode} workers=${mode?.workerCount} workerRetired=${mode?.workerRetired} parallelBatches=${mode?.parallelBatches} maxActiveSlices=${mode?.maxConcurrentWorkerSlices || 0} chrome=${browser.version()} reason=${mode?.reason || '-'}\nstat: ${stat}`);
  const expectedMode = failStartup ? 'cooperative'
    : inPlace ? 'shared-cpu-workers-in-place' : 'shared-cpu-workers';
  assert.equal(mode?.mode, expectedMode, `unexpected execution mode: ${mode?.reason}\n${output}`);
  if (failStartup) {
    assert.equal(mode.workerCount, 0, 'failed startup must leave no worker pool active');
    assert.equal(mode.workerRetired, '0', 'cooperative fallback must not report shared-worker retirements');
    assert.equal(mode.parallelBatches, 0, 'cooperative fallback must not dispatch parallel batches');
    assert.match(mode.reason, /HTTP 503/, `missing fallback reason: ${JSON.stringify(mode)}`);
    assert.match(output, /using cooperative execution/, `missing fallback status message: ${output}`);
  } else if (!faultMode) {
    assert.ok(BigInt(mode.workerRetired) > 500_000n, `the fixture needs ~1.1M guest instructions; workers retired only ${mode.workerRetired} guest instructions`);
    assert.ok(mode.parallelBatches > 0, 'no batch dispatched two guest threads to separate workers together');
    if (!inPlace) {
      assert.ok(mode.maxConcurrentWorkerSlices >= 2,
        `guest worker slices never overlapped: ${JSON.stringify(mode)}`);
      assert.match(output, /rendezvous=2 stack=2/,
        `guest threads did not rendezvous through shared memory with distinct stack state: ${output}`);
    }
  }
  if (faultMode) {
    assert.ok(BigInt(mode.workerRetired) > 0n, 'the CPU worker did not retire guest instructions before the exception');
    assert.match(output, /fault handled/, `vectored exception handler did not resume the guest: ${output}`);
  } else {
    assert.match(output,
      /t0=5133 t1=4459 total=9592 tls0=0x1111 tls1=0x2222 last0=0x3333 last1=0x4444 mainTls=0x5555 mainLast=0x6666/,
      `wrong thread, TLS, or TEB results: ${output}`);
  }
  assert.match(stat, /exit code 0/, stat);
  console.log(`shared CPU ${fixtureName} execution: ok`);
} finally {
  await browser.close();
  server.kill();
  rmSync(tempDir, { recursive: true, force: true });
}
