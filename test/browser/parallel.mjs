// Runs the two-thread prime-counting fixture on the shared CPU workers in headless Chromium and
// checks the result. Fails if the page fell back to cooperative execution.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
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
  await page.goto(`http://localhost:8131/index.html?run=parallel_test&${inPlace ? 'inPlaceCpu' : 'sharedCpuWorkers'}`);
  await page.waitForFunction(() => document.getElementById('stat')?.dataset.status !== undefined, null, { timeout: 120000 });
  const output = await page.textContent('#term');
  const stat = await page.textContent('#stat');
  const mode = await page.evaluate(() => window.__executionMode);
  console.log(`mode=${mode?.mode} workers=${mode?.workerCount} workerRetired=${mode?.workerRetired} parallelBatches=${mode?.parallelBatches} reason=${mode?.reason || '-'}\nstat: ${stat}`);
  assert.equal(mode?.mode, inPlace ? 'shared-cpu-workers-in-place' : 'shared-cpu-workers', `fell back to cooperative: ${mode?.reason}\n${output}`);
  assert.ok(BigInt(mode.workerRetired) > 500_000n, `the fixture needs ~1.1M guest instructions; workers retired only ${mode.workerRetired} guest instructions`);
  assert.ok(mode.parallelBatches > 0, 'no batch dispatched two guest threads to separate workers together');
  assert.match(output, /t0=5133 t1=4459 total=9592/, `wrong thread results: ${output}`);
  assert.match(stat, /exit code 0/, stat);
  console.log('parallel guest threads: ok');
} finally {
  await browser.close();
  server.kill();
}
