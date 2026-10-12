// Replay one target frame through the browser WebGPU path. This remains a diagnostic smoke test;
// a non-empty frame does not establish correct gameplay or full application acceptance.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { buildPlaythroughPlan } from './target-playthrough.mjs';
import {
  mapFlashlightOrigin,
  measureFlashlightRayResponse,
  selectClearFlashlightDirection,
} from './target-flashlight.mjs';

const bundlePath = resolve(process.argv[2] || '');
const options = process.argv.slice(3);
const outputPath = resolve(options.find((argument) => !argument.startsWith('--')) || 'target-first-frame.png');
const checkMovement = options.includes('--check-movement');
const checkDirections = options.includes('--check-directions');
const checkAim = options.includes('--check-aim');
const checkFlashlight = options.includes('--check-flashlight');
const checkMapReset = options.includes('--check-map-reset');
const checkUav = options.includes('--check-uav');
const normalWorkerSlices = options.includes('--normal-slices');
const playthroughArgument = options.find((argument) => argument.startsWith('--playthrough='));
const pulseMsArgument = options.find((argument) => argument.startsWith('--pulse-ms='));
const pulseMs = pulseMsArgument ? Number(pulseMsArgument.slice('--pulse-ms='.length)) : 10;
const playthroughRecord = playthroughArgument
  ? JSON.parse(readFileSync(playthroughArgument.slice('--playthrough='.length), 'utf8'))
  : null;
if (playthroughRecord && (checkMovement || checkDirections || checkAim || checkFlashlight || checkMapReset)) {
  throw new Error('--playthrough cannot be combined with the independent input probes');
}
if (!process.argv[2] || !existsSync(bundlePath)) {
  throw new Error('usage: CHROMIUM=<browser> node test/browser/target.mjs <target.awb> [frame.png] [--playthrough=<record.json>]');
}
if (!process.env.CHROMIUM) throw new Error('set CHROMIUM to a local Chromium executable');

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const wasmPath = resolve(process.env.TARGET_WASM || resolve(repoRoot, 'web/anywasm_wasm.wasm'));
if (!existsSync(wasmPath)) throw new Error(`runtime WASM does not exist: ${wasmPath}`);
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
  await page.route('**/anywasm_wasm.wasm', async (route) => {
    await route.fulfill({ status: 200, contentType: 'application/wasm', body: readFileSync(wasmPath) });
  });
  await page.route('**/worker.js', async (route) => {
    const response = await route.fetch();
    const original = await response.text();
    const inputBranch = "else if (m.type === 'input' && w) { w.input(m.kind, m.a, m.b, m.c); wake?.(); wake = null; }";
    const source = (normalWorkerSlices ? original : original.replace('w.run(250_000)', 'w.run(1_000_000_000)'))
      .replace('frames % 30 === 0', 'frames % 1 === 0')
      .replace(inputBranch, "else if (m.type === 'input' && w) { console.log('[target-probe] input ' + m.kind + ':' + m.a + ':' + m.b); w.input(m.kind, m.a, m.b, m.c); wake?.(); wake = null; }");
    assert.notEqual(source, original, 'diagnostic worker overrides should match the current worker');
    if (normalWorkerSlices) assert.match(source, /w\.run\(250_000\)/, 'normal-slice runs must keep the production worker quota');
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
  const startupDiagnostics = await page.evaluate(() => ({
    status: document.querySelector('#stat')?.textContent,
    terminal: document.querySelector('#term')?.textContent,
    workerStatus: document.querySelector('#stat')?.dataset.status,
  }));
  assert.ok(stats.dispatches > 0,
    `the target should execute its compute dispatch: ${JSON.stringify({ stats, startupDiagnostics, workerDiagnostics })}`);
  assert.ok(stats.draws > 0,
    `the target should submit graphics draws: ${JSON.stringify({ stats, startupDiagnostics, workerDiagnostics })}`);
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
  const capturePixels = async (includePixels = false) => {
    await page.evaluate(() => { window.__pixels = null; window.__worker?.postMessage({ type: 'capture' }); });
    await page.waitForFunction(() => window.__pixels, null, { timeout: 30_000 });
    return page.evaluate((includePixels) => {
      const p = window.__pixels, d = p.data;
      let xTotal = 0, yTotal = 0, count = 0, luminance = 0, brightPixels = 0;
      for (let y = 0; y < p.height; y++) for (let x = 0; x < p.width; x++) {
        const i = (y * p.width + x) * 4;
        if (d[i + 1] > 180 && d[i] < 70 && d[i + 2] < 120 && d[i + 3]) { xTotal += x; yTotal += y; count++; }
        const luma = 0.2126 * d[i] + 0.7152 * d[i + 1] + 0.0722 * d[i + 2];
        luminance += luma;
        if (luma > 16) brightPixels++;
      }
      return {
        width: p.width,
        height: p.height,
        greenMarker: count ? { x: xTotal / count, y: yTotal / count, pixels: count } : null,
        meanLuminance: luminance / (p.width * p.height),
        brightPixels,
        pixels: includePixels ? Array.from(d) : undefined,
      };
    }, includePixels);
  };
  let movement;
  let mapReset;
  let playthroughReplay;
  let inputDiagnostics;
  const playthroughPlan = playthroughRecord
    ? buildPlaythroughPlan(playthroughRecord, { pulseMs, targetWidth: result.width, targetHeight: result.height })
    : null;
  if (playthroughPlan) {
    await page.locator('#screen').focus();
    const progress = [];
    let before = result.greenMarker;
    for (const [index, step] of playthroughPlan.steps.entries()) {
      const alignment = [];
      for (const axis of ['x', 'y']) {
        for (let attempt = 0; attempt < 2; attempt++) {
          const error = step.expectedBefore[axis] - before[axis];
          if (Math.abs(error) <= 1.25) break;
          const key = axis === 'x' ? (error > 0 ? 'd' : 'a') : (error > 0 ? 's' : 'w');
          const durationMs = Math.min(100, Math.max(16, Math.round(Math.abs(error) * 21.8)));
          await page.keyboard.down(key);
          await page.waitForTimeout(durationMs);
          await page.keyboard.up(key);
          await page.waitForTimeout(100);
          const afterCorrection = (await capturePixels()).greenMarker;
          assert.ok(afterCorrection, `playthrough alignment ${index}:${axis} should retain the green player marker`);
          alignment.push({ axis, key, requestedError: error, durationMs, before, after: afterCorrection });
          before = afterCorrection;
        }
      }
      const segmentBefore = before;
      for (const key of step.keys) await page.keyboard.down(key);
      await page.waitForTimeout(step.durationMs);
      for (const key of [...step.keys].reverse()) await page.keyboard.up(key);
      await page.waitForTimeout(100);

      const after = (await capturePixels()).greenMarker;
      assert.ok(after, `playthrough step ${index} should retain the green player marker`);
      const dx = after.x - segmentBefore.x;
      const dy = after.y - segmentBefore.y;
      const expectedDx = step.expectedAfter.x - step.expectedBefore.x;
      const expectedDy = step.expectedAfter.y - step.expectedBefore.y;
      const expectedLength = Math.hypot(expectedDx, expectedDy) || 1;
      progress.push({
        index,
        keys: step.keys,
        pulses: step.pulseCount,
        durationMs: step.durationMs,
        alignment,
        before: segmentBefore,
        after,
        delta: { x: dx, y: dy },
        projectedPixels: (dx * expectedDx + dy * expectedDy) / expectedLength,
        distanceFromRecordedEnd: Math.hypot(after.x - step.expectedAfter.x, after.y - step.expectedAfter.y),
      });
      before = after;
    }

    const finalDistanceFromStart = Math.hypot(before.x - result.greenMarker.x, before.y - result.greenMarker.y);
    inputDiagnostics = await page.evaluate(() => ({
      status: document.querySelector('#stat')?.textContent,
      gpuStats: window.__gpuStats,
    }));
    playthroughReplay = {
      record: playthroughArgument.slice('--playthrough='.length),
      pulseMs: playthroughPlan.pulseMs,
      keyPulses: playthroughPlan.keyPulses,
      steps: progress.length,
      totalInputMs: playthroughPlan.steps.reduce((sum, step) => sum + step.durationMs, 0),
      expectedTeleport: playthroughPlan.teleported,
      recordedCaptureSize: playthroughRecord.captureSize,
      replayCaptureSize: { width: result.width, height: result.height },
      callbackSuspensions: inputDiagnostics.gpuStats?.asyncify,
      initialMarker: result.greenMarker,
      finalMarker: before,
      finalDistanceFromStart,
      actualTravelPixels: progress.reduce((sum, step) => sum + Math.hypot(step.delta.x, step.delta.y), 0),
      alignmentTravelPixels: progress.flatMap((step) => step.alignment).reduce((sum, correction) => sum + Math.hypot(correction.after.x - correction.before.x, correction.after.y - correction.before.y), 0),
      finalSegmentTravelPixels: Math.hypot(progress.at(-1).delta.x, progress.at(-1).delta.y),
      progress,
    };
    const png = await page.evaluate(() => {
      const p = window.__pixels, canvas = document.createElement('canvas');
      canvas.width = p.width; canvas.height = p.height;
      canvas.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(p.data), p.width, p.height), 0, 0);
      return canvas.toDataURL('image/png').split(',')[1];
    });
    writeFileSync(outputPath.replace(/\.png$/i, '.after-playthrough.png'), Buffer.from(png, 'base64'));
    writeFileSync(outputPath.replace(/\.png$/i, '.playthrough.json'), `${JSON.stringify(playthroughReplay, null, 2)}\n`);
    if (process.env.REQUIRE_TARGET_PLAYTHROUGH === '1') {
      assert.ok(playthroughPlan.teleported, 'the stored route should describe a teleporting run');
      assert.ok(playthroughReplay.actualTravelPixels > 100,
        `the stored route should move the player through the maze: ${JSON.stringify(playthroughReplay)}`);
      assert.ok(playthroughReplay.finalSegmentTravelPixels > 100,
        `the stored route's final step should cross the teleporter: ${JSON.stringify(playthroughReplay)}`);
      assert.ok(finalDistanceFromStart <= 8,
        `the stored route should reach the teleport and return the player to its start: ${JSON.stringify(playthroughReplay)}`);
    }
    if (process.env.REQUIRE_TARGET_CALLBACK_SUSPENSIONS === '1') {
      assert.ok(inputDiagnostics.gpuStats?.asyncify?.yields > 0,
        `the target should suspend guest callbacks at worker slice boundaries: ${JSON.stringify(inputDiagnostics)}`);
      assert.ok(inputDiagnostics.gpuStats.asyncify.resumes > 0,
        `the target should resume suspended callbacks on later worker slices: ${JSON.stringify(inputDiagnostics)}`);
    }
  }
  if (checkMovement || checkDirections || checkMapReset) {
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
    if (checkDirections) {
      const directions = [
        { key: 'w', name: 'W', dx: 0, dy: -1 },
        { key: 's', name: 'S', dx: 0, dy: 1 },
        { key: 'a', name: 'A', dx: -1, dy: 0 },
        { key: 'd', name: 'D', dx: 1, dy: 0 },
      ];
      let before = result.greenMarker;
      movement = { before, directions: [] };
      for (const direction of directions) {
        await page.keyboard.down(direction.key);
        await page.waitForTimeout(500);
        await page.keyboard.up(direction.key);
        await page.waitForTimeout(100);
        const after = (await capturePixels()).greenMarker;
        const dx = after && before ? after.x - before.x : 0;
        const dy = after && before ? after.y - before.y : 0;
        const projected = dx * direction.dx + dy * direction.dy;
        movement.directions.push({ key: direction.name, before, after, dx, dy, projected });
        assert.ok(projected > 1, `${direction.name} should move the player in the expected screen direction: ${JSON.stringify(movement)}`);
        before = after;
      }
      writeFileSync(outputPath.replace(/\.png$/i, '.directions.json'), `${JSON.stringify(movement, null, 2)}\n`);
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
  let aim;
  if (checkAim) {
    await page.locator('#screen').focus();
    const bounds = await page.locator('#screen').boundingBox();
    assert.ok(bounds, 'the target canvas should be visible for mouse aiming');
    let before = (await capturePixels()).greenMarker;
    aim = { before, directions: [] };
    for (const direction of [
      { name: 'up', dx: 0, dy: -1 },
      { name: 'down', dx: 0, dy: 1 },
      { name: 'left', dx: -1, dy: 0 },
      { name: 'right', dx: 1, dy: 0 },
    ]) {
      const playerX = bounds.x + before.x * bounds.width / result.width;
      const playerY = bounds.y + before.y * bounds.height / result.height;
      await page.mouse.move(playerX + direction.dx * bounds.width * 0.2, playerY + direction.dy * bounds.height * 0.2);
      await page.waitForTimeout(100);
      await page.keyboard.down('Space');
      await page.waitForTimeout(500);
      await page.keyboard.up('Space');
      await page.waitForTimeout(100);
      const after = (await capturePixels()).greenMarker;
      const dx = after && before ? after.x - before.x : 0;
      const dy = after && before ? after.y - before.y : 0;
      const projected = dx * direction.dx + dy * direction.dy;
      aim.directions.push({ direction: direction.name, before, after, dx, dy, projected });
      assert.ok(projected > 1, `mouse aim ${direction.name} should move the player in that direction: ${JSON.stringify({ aim, workerDiagnostics })}`);
      before = after;
    }
    writeFileSync(outputPath.replace(/\.png$/i, '.aim.json'), `${JSON.stringify(aim, null, 2)}\n`);
  }
  let flashlight;
  if (checkFlashlight) {
    await page.locator('#screen').focus();
    const bounds = await page.locator('#screen').boundingBox();
    assert.ok(bounds, 'the target canvas should be visible for flashlight input');
    const liveGpuStats = await page.evaluate(() => window.__gpuStats);
    const sdfViewId = liveGpuStats?.lastComputeUavs?.[0];
    assert.ok(sdfViewId, 'the latest compute dispatch should report the SDF UAV');
    await page.evaluate((id) => {
      window.__textureReadback = null;
      window.__worker?.postMessage({ type: 'readback-texture', id });
    }, sdfViewId);
    await page.waitForFunction(() => window.__textureReadback, null, { timeout: 30_000 });
    const sdf = await page.evaluate(() => {
      const result = window.__textureReadback;
      return { error: result.error, width: result.width, height: result.height, format: result.format, values: Array.from(result.data || []) };
    });
    assert.equal(sdf.error, undefined, `target SDF readback should succeed: ${JSON.stringify({ ...sdf, values: undefined })}`);
    assert.equal(sdf.format, 'r32float', 'the target SDF should use R32_FLOAT');
    const before = await capturePixels();
    const marker = before.greenMarker || result.greenMarker;
    assert.ok(marker, 'the current frame should show the player marker for SDF ray selection');
    const mapAspect = 1;
    const mapWidth = 64;
    const flashlightRange = 25;
    const origin = mapFlashlightOrigin(marker, result.width, result.height, sdf.width, sdf.height, { mapAspect });
    const maxDistanceTexels = flashlightRange * sdf.width / mapWidth;
    const direction = selectClearFlashlightDirection(sdf.values, sdf.width, sdf.height, {
      ...origin,
      maxDistanceTexels,
    });
    const offset = Math.min(bounds.width, bounds.height) * 0.2;
    const x = bounds.x + marker.x * bounds.width / result.width + direction.dx * offset;
    const y = bounds.y + marker.y * bounds.height / result.height + direction.dy * offset;
    await page.mouse.move(x, y);
    await page.waitForTimeout(100);
    const dark = await capturePixels(true);
    await page.mouse.down({ button: 'left' });
    await page.waitForTimeout(1000);
    const lit = await capturePixels(true);
    await page.mouse.up({ button: 'left' });
    const rayResponse = measureFlashlightRayResponse(
      dark.pixels, lit.pixels, result.width, result.height, sdf.width, sdf.height,
      origin, direction,
    );
    flashlight = {
      direction: direction.direction,
      sdfOrigin: origin,
      playerMarker: marker,
      clearTexels: direction.clearTexels,
      wallDistanceTexels: direction.wallDistanceTexels,
      maxDistanceTexels,
      minimumClearance: direction.minimumClearance,
      before: { meanLuminance: dark.meanLuminance, brightPixels: dark.brightPixels },
      after: { meanLuminance: lit.meanLuminance, brightPixels: lit.brightPixels },
      meanLuminanceDelta: lit.meanLuminance - dark.meanLuminance,
      brightPixelDelta: lit.brightPixels - dark.brightPixels,
      rayResponse,
    };
    writeFileSync(outputPath.replace(/\.png$/i, '.flashlight.json'), `${JSON.stringify(flashlight, null, 2)}\n`);
    assert.ok(rayResponse.longestIlluminatedRun >= 3,
      `holding the left mouse button should brighten at least three adjacent patches along the selected clear ray: ${JSON.stringify(flashlight)}`);
  }
  console.log(JSON.stringify({ bundle: bundlePath, frame: outputPath, stats, uavReadback, movement, aim, flashlight, mapReset, playthrough: playthroughReplay, inputDiagnostics, workerDiagnostics, ...result }, null, 2));
} finally {
  await browser?.close();
  server.kill();
}
