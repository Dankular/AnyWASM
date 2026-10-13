import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';
import test from 'node:test';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const bundlePath = join(repoRoot, 'web', 'demo', 'hello.awb');
const port = '8134';
const browserDir = process.env.PLAYWRIGHT_BROWSERS_PATH || (process.platform === 'win32'
  ? join(homedir(), 'AppData', 'Local', 'ms-playwright')
  : '/opt/pw-browsers');
const versioned = process.env.CHROMIUM
  ? undefined
  : readdirSync(browserDir).filter((dir) => /^chromium-\d+$/.test(dir)).sort().pop();
const defaultExecutable = versioned && (process.platform === 'win32'
  ? join(browserDir, versioned, 'chrome-win64', 'chrome.exe')
  : join(browserDir, versioned, 'chrome-linux', 'chrome'));
const executablePath = process.env.CHROMIUM || defaultExecutable;

test('shared CPU request falls back cooperatively when SharedArrayBuffer is unavailable', async () => {
  const server = spawn(process.execPath, ['web/serve.mjs', port], {
    cwd: repoRoot,
    stdio: 'ignore',
    windowsHide: true,
  });
  let browser;
  try {
    await new Promise((resolve) => setTimeout(resolve, 800));
    browser = await chromium.launch({ executablePath, args: ['--no-sandbox'] });
    const context = await browser.newContext();
    await context.route('**/*', async (route) => {
      const response = await route.fetch();
      const headers = { ...response.headers() };
      delete headers['cross-origin-opener-policy'];
      delete headers['cross-origin-embedder-policy'];
      await route.fulfill({ response, headers });
    });
    const page = await context.newPage();
    await page.goto(`http://localhost:${port}/index.html?sharedCpuWorkers`);
    await page.locator('#file').setInputFiles(bundlePath);
    await page.waitForFunction(
      () => document.querySelector('#stat')?.dataset.status !== undefined,
      null,
      { timeout: 30_000 },
    );

    const result = await page.evaluate(() => ({
      crossOriginIsolated: globalThis.crossOriginIsolated,
      sharedArrayBuffer: typeof SharedArrayBuffer,
      mode: window.__executionMode?.mode,
      workerCount: window.__executionMode?.workerCount,
      reason: window.__executionMode?.reason,
      status: document.querySelector('#stat')?.dataset.status,
      exitCode: document.querySelector('#stat')?.dataset.exit,
    }));
    assert.equal(result.crossOriginIsolated, false, `the page must run without cross-origin isolation: ${JSON.stringify(result)}`);
    assert.equal(result.sharedArrayBuffer, 'undefined', `SharedArrayBuffer must be unavailable: ${JSON.stringify(result)}`);
    assert.equal(result.mode, 'cooperative', `an unavailable shared memory must use cooperative execution: ${JSON.stringify(result)}`);
    assert.equal(result.workerCount, 0, `cooperative fallback must not start shared CPU workers: ${JSON.stringify(result)}`);
    assert.equal(result.status, '1', `the fixture should exit normally: ${JSON.stringify(result)}`);
    assert.equal(result.exitCode, '0', `the fixture should return zero: ${JSON.stringify(result)}`);
    assert.match(result.reason, /cross-origin isolation and SharedArrayBuffer/, `the fallback reason must be reported: ${JSON.stringify(result)}`);
  } finally {
    await browser?.close();
    server.kill();
  }
});
