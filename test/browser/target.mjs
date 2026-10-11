// Replay one target frame through the browser WebGPU path. This remains a diagnostic smoke test;
// a non-empty frame does not establish correct gameplay or full application acceptance.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const bundlePath = resolve(process.argv[2] || '');
const options = process.argv.slice(3);
const outputPath = resolve(options.find((argument) => !argument.startsWith('--')) || 'target-first-frame.png');
const checkMovement = options.includes('--check-movement');
const checkMapReset = options.includes('--check-map-reset');
const checkUav = options.includes('--check-uav');
if (!process.argv[2] || !existsSync(bundlePath)) {
  throw new Error('usage: CHROMIUM=<browser> node test/browser/target.mjs <target.awb> [frame.png]');
}
if (!process.env.CHROMIUM) throw new Error('set CHROMIUM to a local Chromium executable');

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const server = spawn(process.execPath, ['web/serve.mjs', '8099'], { cwd: repoRoot, stdio: 'ignore' });
let browser;
try {
  await new Promise((r) => setTimeout(r, 800));
  browser = await chromium.launch({ executablePath: process.env.CHROMIUM, args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'] });
  const page = await browser.newPage();
  const workerDiagnostics = [];
  page.on('console', (message) => {
    if (message.text().startsWith('[target-probe]')) workerDiagnostics.push(message.text());
  });
  await page.route('**/worker.js', async (route) => {
    const response = await route.fetch();
    const original = await response.text();
    const inputBranch = "else if (m.type === 'input' && w) { w.input(m.kind, m.a, m.b, m.c); wake?.(); wake = null; }";
    const source = original
      .replace('w.run(250_000)', 'w.run(1_000_000_000)')
      .replace('frames % 30 === 0', 'frames % 1 === 0')
      .replace(inputBranch, "else if (m.type === 'input' && w) { console.log('[target-probe] input ' + m.kind + ':' + m.a); w.input(m.kind, m.a, m.b, m.c); wake?.(); wake = null; }");
    assert.notEqual(source, original, 'diagnostic worker overrides should match the current worker');
    await route.fulfill({ response, body: source, contentType: 'text/javascript' });
  });

  await page.goto('http://localhost:8099/index.html?test=1');
  await page.locator('#file').setInputFiles(bundlePath);
  await page.waitForFunction(
    () => window.__gpuStats || document.querySelector('#stat')?.dataset.status !== undefined,
    null,
    { timeout: 240_000 },
  );
  const stats = await page.evaluate(() => window.__gpuStats);
  assert.ok(stats, 'the browser should report a GPU frame before terminating');
  assert.ok(stats.dispatches > 0, 'the target should execute its compute dispatch');
  assert.ok(stats.draws > 0, 'the target should submit graphics draws');
  assert.deepEqual(stats.errors, [], 'the target frame should have no WebGPU validation errors');

  let uavReadback;
  if (checkUav) {
    const viewId = stats.lastComputeUavs?.[0];
    assert.ok(viewId, 'the last compute dispatch should report a bound UAV view');
    await page.evaluate((id) => {
      window.__textureReadback = null;
      window.__worker?.postMessage({ type: 'readback-texture', id });
    }, viewId);
    await page.waitForFunction(() => window.__textureReadback, null, { timeout: 30_000 });
    uavReadback = await page.evaluate(() => {
      const result = window.__textureReadback;
      if (result.error) return result;
      const values = result.data;
      let finiteValues = 0, negativeValues = 0, positiveValues = 0;
      let min = Infinity, max = -Infinity;
      for (const value of values) {
        if (Number.isFinite(value)) finiteValues++;
        if (value < 0) negativeValues++;
        if (value > 0) positiveValues++;
        min = Math.min(min, value);
        max = Math.max(max, value);
      }
      const at = (x, y) => values[y * result.width + x];
      return {
        id: result.id,
        resourceId: result.resourceId,
        width: result.width,
        height: result.height,
        format: result.format,
        valueCount: values.length,
        finiteValues,
        negativeValues,
        positiveValues,
        min,
        max,
        samples: { topLeft: at(0, 0), center: at(result.width >> 1, result.height >> 1), bottomRight: at(result.width - 1, result.height - 1) },
      };
    });
    assert.equal(uavReadback.error, undefined, `target UAV readback should succeed: ${JSON.stringify(uavReadback)}`);
    assert.equal(uavReadback.format, 'r32float', 'the target SDF UAV should use R32_FLOAT');
    assert.equal(uavReadback.width, 128, 'the target SDF should be 128 texels wide');
    assert.equal(uavReadback.height, 128, 'the target SDF should be 128 texels high');
    assert.equal(uavReadback.finiteValues, 128 * 128, 'every target SDF texel should be finite');
    assert.ok(uavReadback.negativeValues > 0 && uavReadback.positiveValues > 0, 'the SDF should contain wall and empty-space distances');
    assert.ok(uavReadback.min >= -4.0001 && uavReadback.max <= 4.0001, 'SDF values should respect the shader radius bound');
    writeFileSync(outputPath.replace(/\.png$/i, '.uav.json'), `${JSON.stringify(uavReadback, null, 2)}\n`);
  }

  await page.evaluate(() => window.__worker?.postMessage({ type: 'capture' }));
  await page.waitForFunction(() => window.__pixels, null, { timeout: 30_000 });
  const result = await page.evaluate(() => {
    const pixels = window.__pixels;
    const data = pixels.data;
    let nonTransparent = 0, nonBlack = 0;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i + 3]) nonTransparent++;
      if (data[i] || data[i + 1] || data[i + 2]) nonBlack++;
    }
    const canvas = document.createElement('canvas');
    canvas.width = pixels.width;
    canvas.height = pixels.height;
    canvas.getContext('2d').putImageData(
      new ImageData(new Uint8ClampedArray(data), pixels.width, pixels.height), 0, 0,
    );
    const png = canvas.toDataURL('image/png').split(',')[1];
    const at = (x, y) => Array.from(data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
    let greenX = 0, greenY = 0, greenCount = 0;
    for (let y = 0; y < pixels.height; y++) for (let x = 0; x < pixels.width; x++) {
      const i = (y * pixels.width + x) * 4;
      if (data[i + 1] > 180 && data[i] < 70 && data[i + 2] < 120 && data[i + 3]) {
        greenX += x; greenY += y; greenCount++;
      }
    }
    return {
      width: pixels.width,
      height: pixels.height,
      nonTransparent,
      nonBlack,
      center: at(pixels.width >> 1, pixels.height >> 1),
      greenMarker: greenCount ? { x: greenX / greenCount, y: greenY / greenCount, pixels: greenCount } : null,
      png,
    };
  });
  assert.ok(result.nonTransparent > 0, 'the target should write pixels to its back buffer');
  assert.ok(result.nonBlack > 0, 'the target frame should contain visible colour');
  writeFileSync(outputPath, Buffer.from(result.png, 'base64'));
  delete result.png;
  const capturePixels = async () => {
    await page.evaluate(() => { window.__pixels = null; window.__worker?.postMessage({ type: 'capture' }); });
    await page.waitForFunction(() => window.__pixels, null, { timeout: 30_000 });
    return page.evaluate(() => {
      const p = window.__pixels, d = p.data;
      let xTotal = 0, yTotal = 0, count = 0;
      for (let y = 0; y < p.height; y++) for (let x = 0; x < p.width; x++) {
        const i = (y * p.width + x) * 4;
        if (d[i + 1] > 180 && d[i] < 70 && d[i + 2] < 120 && d[i + 3]) { xTotal += x; yTotal += y; count++; }
      }
      return { width: p.width, height: p.height, greenMarker: count ? { x: xTotal / count, y: yTotal / count, pixels: count } : null };
    });
  };
  let movement;
  let mapReset;
  let inputDiagnostics;
  if (checkMovement || checkMapReset) {
    assert.ok(result.greenMarker, 'the initial frame should show the green player marker');
    await page.evaluate(() => {
      window.__targetKeyEvents = [];
      const screen = document.querySelector('#screen');
      screen.addEventListener('keydown', (event) => window.__targetKeyEvents.push(`${event.type}:${event.key}`));
      screen.addEventListener('keyup', (event) => window.__targetKeyEvents.push(`${event.type}:${event.key}`));
    });
    await page.locator('#screen').focus();
    if (checkMovement) {
      const attempts = [];
      movement = { before: result.greenMarker, attempts };
      for (const key of ['w', 's', 'a', 'd']) {
        await page.keyboard.down(key);
        await page.waitForTimeout(500);
        await page.keyboard.up(key);
        await page.waitForTimeout(100);
        const captured = await capturePixels();
        const after = captured.greenMarker;
        const distance = after ? Math.hypot(after.x - result.greenMarker.x, after.y - result.greenMarker.y) : 0;
        attempts.push({ key: key.toUpperCase(), after, distance });
        if (distance > 1.5) {
          movement = { key: key.toUpperCase(), before: result.greenMarker, after, distance, attempts };
          const png = await page.evaluate(() => {
            const p = window.__pixels, canvas = document.createElement('canvas');
            canvas.width = p.width; canvas.height = p.height;
            canvas.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(p.data), p.width, p.height), 0, 0);
            return canvas.toDataURL('image/png').split(',')[1];
          });
          writeFileSync(outputPath.replace(/\.png$/i, `.after-${key.toLowerCase()}.png`), Buffer.from(png, 'base64'));
          break;
        }
      }
      if (process.env.REQUIRE_TARGET_MOVEMENT === '1') {
        assert.ok(movement.key, `no WASD key moved the player marker: ${JSON.stringify(attempts)}`);
      }
    }
    if (checkMapReset) {
      await capturePixels();
      await page.evaluate(() => { window.__mapBefore = new Uint8ClampedArray(window.__pixels.data); });
      await page.keyboard.press('Tab');
      await page.waitForTimeout(350);
      await capturePixels();
      mapReset = await page.evaluate(() => {
        const a = window.__mapBefore, b = window.__pixels.data;
        let changedPixels = 0;
        for (let i = 0; i < a.length; i += 4) {
          if (a[i] !== b[i] || a[i + 1] !== b[i + 1] || a[i + 2] !== b[i + 2]) changedPixels++;
        }
        return { changedPixels, pixelCount: window.__pixels.width * window.__pixels.height, fraction: changedPixels / (window.__pixels.width * window.__pixels.height) };
      });
      if (process.env.REQUIRE_TARGET_MAP_RESET === '1') {
        const diagnostics = await page.evaluate(() => ({ keys: window.__targetKeyEvents, status: document.querySelector('#stat')?.textContent, import: document.querySelector('#diagnostic code')?.textContent, gpuStats: window.__gpuStats }));
        assert.ok(mapReset.fraction > 0.03, `Tab should regenerate the visible map: ${JSON.stringify({ mapReset, diagnostics, workerDiagnostics })}`);
      }
      const png = await page.evaluate(() => {
        const p = window.__pixels, canvas = document.createElement('canvas');
        canvas.width = p.width; canvas.height = p.height;
        canvas.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(p.data), p.width, p.height), 0, 0);
        return canvas.toDataURL('image/png').split(',')[1];
      });
      writeFileSync(outputPath.replace(/\.png$/i, '.after-tab.png'), Buffer.from(png, 'base64'));
    }
    inputDiagnostics = await page.evaluate(() => ({ keys: window.__targetKeyEvents, status: document.querySelector('#stat')?.textContent, gpuStats: window.__gpuStats }));
  }
  console.log(JSON.stringify({ bundle: bundlePath, frame: outputPath, stats, uavReadback, movement, mapReset, inputDiagnostics, workerDiagnostics, ...result }, null, 2));
} finally {
  await browser?.close();
  server.kill();
}
