// Core runtime smoke tests for installed Firefox and WebKit engines.
// Set FIREFOX / WEBKIT to override Playwright's default browser executables.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { chromium, firefox, webkit } from 'playwright-core';
import { fileURLToPath } from 'node:url';
import { decodePng } from './png.mjs';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const indexPath = fileURLToPath(new URL('../../web/index.html', import.meta.url));
const jitPressurePath = fileURLToPath(new URL('../jit_cache_pressure.mjs', import.meta.url));
const jitPressureSource = readFileSync(jitPressurePath);
const csp = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self'; connect-src 'self'";
const cspProbeSource = `
import { AnyWasm } from '/anywasm.js';
import { runJitCachePressure } from '/jit-cache-pressure.mjs';
try {
  const result = await runJitCachePressure(AnyWasm);
  const output = document.createElement('pre');
  output.id = 'csp-jit-results';
  output.textContent = JSON.stringify(result);
  document.body.append(output);
} catch (error) {
  const output = document.createElement('pre');
  output.id = 'csp-jit-results';
  output.textContent = JSON.stringify({ error: error.message });
  document.body.append(output);
}
`;
const server = spawn(process.execPath, ['web/serve.mjs', '8119'], { cwd: repoRoot, stdio: 'ignore' });
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const engines = [
  { name: 'Chromium', engine: chromium, executablePath: process.env.CHROMIUM },
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
      await page.route('**/jit-cache-pressure.mjs', (route) => route.fulfill({ body: jitPressureSource, contentType: 'text/javascript' }));
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
        const near = (actual, expected, tolerance) => actual.every((value, index) => Math.abs(value - expected[index]) <= tolerance);
        const canvasAspect = await page.locator('#screen').evaluate((canvas) => {
          const [width, height] = canvas.style.aspectRatio.split('/').map(Number);
          return { width, height };
        });
        await page.evaluate(() => { window.__pixels = null; window.__worker.postMessage({ type: 'capture' }); });
        await page.waitForFunction(() => window.__pixels !== null, null, { timeout: 20000 });
        const backBuffer = await page.evaluate(() => {
          const pixels = window.__pixels;
          const at = (x, y) => {
            const index = (y * pixels.width + x) * 4;
            return Array.from(pixels.data.slice(index, index + 4));
          };
          const centerY = Math.round((pixels.height - 1) * 0.6);
          return { width: pixels.width, height: pixels.height, centerY, error: pixels.error, clear: at(10, 10), center: at(320, centerY) };
        });
        assert.equal(backBuffer.error, undefined, `${item.name} back-buffer readback error: ${backBuffer.error}`);
        const screenshot = decodePng(await page.locator('#screen').screenshot());
        const pixel = (x, y) => {
          const index = (y * screenshot.w + x) * 4;
          return Array.from(screenshot.data.subarray(index, index + 4));
        };
        const screenshotCenterY = Math.round((screenshot.h - 1) * 0.6);
        const clear = pixel(10, 10), center = pixel(320, screenshotCenterY);
        const gpuStats = await page.evaluate(() => window.__gpuStats);
        assert.ok(gpuStats, `${item.name} missing WebGPU statistics`);
        assert.deepEqual([backBuffer.width, backBuffer.height], [640, 480], `${item.name} triangle back-buffer dimensions`);
        assert.equal(screenshot.w, 640, `${item.name} triangle canvas width`);
        assert.ok(Math.abs(screenshot.h - screenshot.w * canvasAspect.height / canvasAspect.width) < 1,
          `${item.name} canvas dimensions ${screenshot.w}x${screenshot.h} do not match the ${canvasAspect.width}/${canvasAspect.height} guest client ratio`);
        assert.deepEqual(gpuStats.errors, [], `${item.name} emitted WebGPU validation errors: ${JSON.stringify(gpuStats.errors)}`);
        assert.ok(near(backBuffer.clear, [0, 0, 51, 255], 2), `${item.name} back-buffer clear pixel: ${backBuffer.clear}`);
        assert.ok(near(backBuffer.center, [85, 85, 85, 255], 6), `${item.name} back-buffer center pixel: ${backBuffer.center}`);
        assert.ok(near(clear, [0, 0, 51, 255], 2), `${item.name} visible canvas clear pixel: ${clear}`);
        assert.ok(near(center, [85, 85, 85, 255], 6), `${item.name} visible triangle center pixel: ${center}`);
        graphics.rendering = 'pixel-verified';
        graphics.canvasPixels = { size: `${screenshot.w}x${screenshot.h}`, clear, center };
      } else {
        assert.equal(graphics.status, '3', `${item.name} silently completed without graphics: ${graphics.stat}`);
        assert.match(graphics.stat, /Graphics unavailable:/, `${item.name} did not explain its graphics limitation: ${graphics.stat}`);
      }
      const jitPressure = await page.evaluate(async () => {
        const [{ AnyWasm }, { runJitCachePressure }] = await Promise.all([
          import('/anywasm.js'), import('/jit-cache-pressure.mjs'),
        ]);
        return runJitCachePressure(AnyWasm);
      });
      assert.equal(jitPressure.firstStatus, 0, `${item.name} JIT cache pressure run failed`);
      assert.equal(jitPressure.probeStatus, 0, `${item.name} JIT cache eviction probe failed`);
      assert.deepEqual(jitPressure.afterPressure, { compilations: 344, executions: 344, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 });
      assert.deepEqual(jitPressure.afterProbe, { compilations: 345, executions: 390, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 });
      assert.deepEqual(pageErrors, [], `${item.name} had uncaught page errors`);
      console.log(`${item.name} ${browser.version()}: ${JSON.stringify({ capabilities, hello, graphics, jitPressure })}`);

      const cspPage = await browser.newPage();
      try {
        const cspPageErrors = [];
        cspPage.on('pageerror', (error) => cspPageErrors.push(error.message));
        await cspPage.route('**/index.html', (route) => route.fulfill({
          path: indexPath,
          headers: { 'Content-Security-Policy': csp },
        }));
        await cspPage.route('**/jit-cache-pressure.mjs', (route) => route.fulfill({ body: jitPressureSource, contentType: 'text/javascript' }));
        await cspPage.route('**/csp-probe.mjs', (route) => route.fulfill({ body: cspProbeSource, contentType: 'text/javascript' }));
        await cspPage.goto('http://localhost:8119/index.html');
        await cspPage.locator('#demo-hello').click();
        await cspPage.locator('#stat[data-status]').waitFor({ state: 'attached', timeout: 30000 });
        const cspHello = {
          status: await cspPage.locator('#stat').getAttribute('data-status'),
          exitCode: await cspPage.locator('#stat').getAttribute('data-exit'),
          output: await cspPage.locator('#term').textContent(),
        };
        assert.equal(cspHello.status, '1', `${item.name} CSP worker run did not exit`);
        assert.equal(cspHello.exitCode, '0', `${item.name} CSP worker run exited nonzero`);
        assert.match(cspHello.output ?? '', /Hello from a Windows PE running inside AnyWASM!/);

        await cspPage.addScriptTag({ url: '/csp-probe.mjs', type: 'module' });
        await cspPage.locator('#csp-jit-results').waitFor({ state: 'attached', timeout: 30000 });
        const cspJit = JSON.parse((await cspPage.locator('#csp-jit-results').textContent()) ?? '{}');
        assert.equal(cspJit.error, undefined, `${item.name} CSP probe error: ${cspJit.error}`);
        assert.equal(cspJit.firstStatus, 0, `${item.name} CSP emitted run failed`);
        assert.equal(cspJit.probeStatus, 0, `${item.name} CSP eviction probe failed`);
        assert.deepEqual(cspJit.afterPressure, { compilations: 344, executions: 344, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 });
        assert.deepEqual(cspJit.afterProbe, { compilations: 345, executions: 390, fallbacks: 0, registerRegisterHits: 0, chainTransitions: 0, chainCompilations: 0 });
        assert.deepEqual(cspPageErrors, [], `${item.name} CSP page had uncaught errors: ${cspPageErrors.join('; ')}`);
        console.log(`${item.name} CSP: ${JSON.stringify({ hello: cspHello, jitPressure: cspJit })}`);
      } finally {
        await cspPage.close();
      }
    } finally {
      await browser.close();
    }
  }
} finally {
  server.kill();
}
