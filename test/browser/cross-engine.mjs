// Core runtime smoke tests for installed Firefox and WebKit engines.
// Set FIREFOX / WEBKIT to override Playwright's default browser executables.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { firefox, webkit } from 'playwright-core';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const server = spawn(process.execPath, ['web/serve.mjs', '8119'], { cwd: repoRoot, stdio: 'ignore' });
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const engines = [
  { name: 'Firefox', engine: firefox, executablePath: process.env.FIREFOX },
  { name: 'WebKit', engine: webkit, executablePath: process.env.WEBKIT },
].filter((item) => !process.env.BROWSER || item.name.toLowerCase() === process.env.BROWSER.toLowerCase());
assert.ok(engines.length > 0, `No browser selected by BROWSER=${process.env.BROWSER}`);

try {
  await delay(800);
  for (const item of engines) {
    const browser = await item.engine.launch({ headless: true, ...(item.executablePath ? { executablePath: item.executablePath } : {}) });
    try {
      const page = await browser.newPage();
      const pageErrors = [];
      page.on('pageerror', (error) => pageErrors.push(error.message));
      await page.goto('http://localhost:8119/index.html');
      const capabilities = await page.evaluate(() => ({
        webgpu: !!navigator.gpu,
        offscreenCanvas: typeof HTMLCanvasElement.prototype.transferControlToOffscreen === 'function',
      }));

      await page.locator('#demo-hello').click();
      await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
      const hello = await page.evaluate(() => ({
        status: document.getElementById('stat').dataset.status,
        exitCode: document.getElementById('stat').dataset.exit,
        stat: document.getElementById('stat').textContent,
        output: document.getElementById('term').textContent,
      }));
      assert.equal(hello.status, '1', `${item.name} hello did not exit: ${hello.stat}; page errors: ${pageErrors.join('; ')}`);
      assert.equal(hello.exitCode, '0', `${item.name} hello exited nonzero: ${hello.stat}`);
      assert.match(hello.output, /Hello from a Windows PE running inside AnyWASM!/);

      await page.locator('#demo-tri').click();
      await page.waitForFunction(() => document.getElementById('stat').dataset.status === undefined, null, { timeout: 10000 });
      await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
      const graphics = await page.evaluate(() => ({
        status: document.getElementById('stat').dataset.status,
        exitCode: document.getElementById('stat').dataset.exit,
        stat: document.getElementById('stat').textContent,
      }));
      if (graphics.status === '1') {
        assert.equal(graphics.status, '1', `${item.name} graphics run did not exit: ${graphics.stat}`);
        assert.equal(graphics.exitCode, '0', `${item.name} graphics run exited nonzero: ${graphics.stat}`);
      } else {
        assert.equal(graphics.status, '3', `${item.name} silently completed without graphics: ${graphics.stat}`);
        assert.match(graphics.stat, /Graphics unavailable:/, `${item.name} did not explain its graphics limitation: ${graphics.stat}`);
      }
      assert.deepEqual(pageErrors, [], `${item.name} had uncaught page errors`);
      console.log(`${item.name} ${browser.version()}: ${JSON.stringify({ capabilities, hello, graphics })}`);
    } finally {
      await browser.close();
    }
  }
} finally {
  server.kill();
}
