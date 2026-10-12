// Drives the real web page in headless Chromium and checks the program output on screen.
import { chromium } from 'playwright-core';
import { decodePng } from './png.mjs';

import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-browser-'));
const unimplBundle = join(tempDir, 'unimpl.awb');
const computeBundle = join(tempDir, 'compute.awb');
const jitStateBundle = join(tempDir, 'jit_state.awb');
const systemRandomBundle = join(tempDir, 'system_random.awb');
const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'unimpl.exe'), '-o', unimplBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'compute.exe'), '-o', computeBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'jit_state.exe'), '-o', jitStateBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'system_random_test.exe'), '-o', systemRandomBundle], { cwd: repoRoot, stdio: 'ignore' });

const server = spawn(process.execPath, ['web/serve.mjs', '8099'], { cwd: repoRoot, stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 800));
let failures = 0;
const check = (n, ok, d = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${n}${ok ? '' : ' :: ' + d}`); if (!ok) failures++; };

const dir = '/opt/pw-browsers';
const versioned = process.env.CHROMIUM ? undefined : readdirSync(dir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const exe = process.env.CHROMIUM || (versioned ? `${dir}/${versioned}/chrome-linux/chrome` : undefined);
const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'] });
try {
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (m) => logs.push(m.text()));
  page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));

  let releaseRuntimeFetch;
  let runtimeFetchStartedResolve;
  let runtimeFetchTimeout;
  const runtimeFetchStarted = new Promise((resolve) => { runtimeFetchStartedResolve = resolve; });
  const runtimeFetchGate = new Promise((resolve) => { releaseRuntimeFetch = resolve; });
  await page.route('**/anywasm_wasm.wasm', async (route) => {
    runtimeFetchStartedResolve();
    await runtimeFetchGate;
    await route.continue();
  });
  await page.goto('http://localhost:8099/index.html');
  await page.locator('#demo-hello').click();
  const reachedRuntimeFetch = await Promise.race([
    runtimeFetchStarted.then(() => true),
    new Promise((resolve) => { runtimeFetchTimeout = setTimeout(() => resolve(false), 8000); }),
  ]);
  clearTimeout(runtimeFetchTimeout);
  const loading = page.locator('#loading');
  const loadingCount = await loading.count();
  const loadingText = loadingCount ? await loading.textContent() : null;
  const loadingRole = loadingCount ? await loading.getAttribute('role') : null;
  const progressVisible = loadingCount ? await loading.locator('progress').isVisible() : false;
  check('browser shows accessible load progress while the runtime download is pending',
    reachedRuntimeFetch && loadingCount === 1 && loadingRole === 'status' && await loading.isVisible() && progressVisible && /Loading WebAssembly runtime/.test(loadingText || ''),
    JSON.stringify({ loadingText, loadingRole, progressVisible }));
  releaseRuntimeFetch();
  await page.unroute('**/anywasm_wasm.wasm');
  if (reachedRuntimeFetch) await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const text = await page.textContent('#term');
  const stat = await page.textContent('#stat');
  check('hello.exe output shown in the page', text.includes('Hello from a Windows PE running inside AnyWASM!'), text);
  check('status shows clean exit', /exited \(exit code 0\)/.test(stat), stat);
  check('load progress clears after the guest reports its status', loadingCount === 1 && await loading.isHidden());

  const systemRandomBrowser = await page.evaluate(async (bundle) => {
    const { AnyWasm, Status } = await import('/anywasm.js');
    const wasm = await (await fetch('/anywasm_wasm.wasm')).arrayBuffer();
    const runtime = await AnyWasm.create(wasm);
    runtime.load(new Uint8Array(bundle));
    return { ...runtime.runToEnd(), exited: Status.EXITED };
  }, Array.from(readFileSync(systemRandomBundle)));
  check('Chrome Web Crypto supplies SystemFunction036 to the guest',
    systemRandomBrowser.status === systemRandomBrowser.exited && systemRandomBrowser.exitCode === 0,
    JSON.stringify(systemRandomBrowser));

  const computeBytes = Array.from(readFileSync(computeBundle));
  const jitBrowser = await page.evaluate(async (bundle) => {
    const { AnyWasm, Status } = await import('/anywasm.js');
    const wasm = await (await fetch('/anywasm_wasm.wasm')).arrayBuffer();
    const execute = async (options) => {
      const runtime = await AnyWasm.create(wasm, options);
      runtime.load(new Uint8Array(bundle));
      let out = '', err = '';
      const result = runtime.runToEnd({
        onOutput(stream, text) { if (stream === 'stdout') out += text; else err += text; },
      });
      return { ...result, instr: runtime.instructions(), out, err, jitStats: runtime.jitStats };
    };
    const executeAfterGrowth = async () => {
      const runtime = await AnyWasm.create(wasm, { jitMinPrefix: 1 });
      runtime.load(new Uint8Array(bundle));
      const decoder = new TextDecoder();
      let out = '', err = '', status = Status.RUNNING;
      for (let i = 0; i < 50_000 && runtime.jitStats.executions === 0 && status === Status.RUNNING; i++) {
        status = runtime.run(1);
        out += decoder.decode(runtime.takeStdout());
        err += decoder.decode(runtime.takeStderr());
      }
      const executionsBeforeGrowth = runtime.jitStats.executions;
      const bytesBefore = runtime.x.memory.buffer.byteLength;
      let bytesAfter = bytesBefore, growthError = '';
      try {
        runtime.x.memory.grow(1);
        bytesAfter = runtime.x.memory.buffer.byteLength;
      } catch (error) {
        growthError = error.message;
      }
      const result = runtime.runToEnd({
        onOutput(stream, text) { if (stream === 'stdout') out += text; else err += text; },
      });
      return {
        ...result,
        instr: runtime.instructions(), out, err, bytesBefore, bytesAfter, growthError,
        executionsBeforeGrowth, jitStats: runtime.jitStats,
      };
    };
    return {
      emitted: await execute({ jitMinPrefix: 1 }),
      cached: await execute({}),
      afterGrowth: await executeAfterGrowth(),
      exited: Status.EXITED,
    };
  }, computeBytes);
  check('Chrome executes opt-in emitted blocks including register-register MOVs',
    jitBrowser.emitted.status === jitBrowser.exited && jitBrowser.emitted.exitCode === 0 &&
      jitBrowser.emitted.jitStats.executions > 0 && jitBrowser.emitted.jitStats.registerRegisterHits > 0 &&
      jitBrowser.emitted.jitStats.fallbacks === 2 && jitBrowser.emitted.out.includes('primes<20000=2262') &&
      jitBrowser.emitted.out.includes('fp=609') && jitBrowser.emitted.out.includes('gpa=ok'),
    JSON.stringify(jitBrowser.emitted));
  check('Chrome cached-only run matches emitted output and retired count',
    jitBrowser.cached.status === jitBrowser.exited && jitBrowser.cached.exitCode === jitBrowser.emitted.exitCode &&
      jitBrowser.cached.out === jitBrowser.emitted.out && jitBrowser.cached.err === jitBrowser.emitted.err &&
      jitBrowser.cached.instr === jitBrowser.emitted.instr && jitBrowser.cached.jitStats.executions === 0 &&
      jitBrowser.cached.jitStats.compilations === 0 && jitBrowser.cached.jitStats.fallbacks === 0,
    JSON.stringify({ emitted: jitBrowser.emitted, cached: jitBrowser.cached }));
  check('Chrome resumes cached emitted modules after WASM memory growth',
    jitBrowser.afterGrowth.status === jitBrowser.exited && jitBrowser.afterGrowth.exitCode === 0 &&
      jitBrowser.afterGrowth.bytesAfter === jitBrowser.afterGrowth.bytesBefore + 65_536 &&
      jitBrowser.afterGrowth.executionsBeforeGrowth > 0 &&
      jitBrowser.afterGrowth.jitStats.executions > jitBrowser.afterGrowth.executionsBeforeGrowth &&
      jitBrowser.afterGrowth.jitStats.fallbacks === jitBrowser.emitted.jitStats.fallbacks &&
      jitBrowser.afterGrowth.out === jitBrowser.emitted.out &&
      jitBrowser.afterGrowth.err === jitBrowser.emitted.err && jitBrowser.afterGrowth.instr === jitBrowser.emitted.instr,
    JSON.stringify(jitBrowser.afterGrowth));

  const workerJit = await page.evaluate(async (bundle) => {
    const wasm = await (await fetch('/anywasm_wasm.wasm')).arrayBuffer();
    const guest = new Uint8Array(bundle);
    const worker = new Worker('/worker.js', { type: 'module' });
    try {
      return await new Promise((resolve, reject) => {
        let out = '', err = '';
        const timeout = setTimeout(() => reject(new Error('emitted worker timed out')), 30_000);
        worker.onerror = (event) => { clearTimeout(timeout); reject(new Error(event.message)); };
        worker.onmessage = ({ data }) => {
          if (data.type === 'out') {
            if (data.stream === 'stdout') out += data.text;
            else err += data.text;
          } else if (data.type === 'status') {
            clearTimeout(timeout);
            resolve({ ...data, out, err });
          }
        };
        worker.postMessage({ type: 'start', wasm, bundle: guest, jitMinPrefix: 1 }, [wasm, guest.buffer]);
      });
    } finally {
      worker.terminate();
    }
  }, computeBytes);
  check('Chrome worker executes opt-in emitted blocks without an additional fallback',
    workerJit.status === jitBrowser.exited && workerJit.exitCode === 0 &&
      workerJit.jitStats?.executions > 0 && workerJit.jitStats?.registerRegisterHits > 0 &&
      workerJit.jitStats?.fallbacks === jitBrowser.emitted.jitStats.fallbacks &&
      workerJit.out.includes('primes<20000=2262') &&
      workerJit.out.includes('fp=609') && workerJit.out.includes('gpa=ok'),
    JSON.stringify(workerJit));
  check('Chrome worker output and retired count match page-realm emitted execution',
    workerJit.out === jitBrowser.emitted.out && workerJit.err === jitBrowser.emitted.err &&
      workerJit.instructions === jitBrowser.emitted.instr,
    JSON.stringify({ worker: workerJit, page: jitBrowser.emitted }));

  const jitStateBrowser = await page.evaluate(async (bundle) => {
    const { AnyWasm, Status } = await import('/anywasm.js');
    const wasm = await (await fetch('/anywasm_wasm.wasm')).arrayBuffer();
    const run = async (options) => {
      const runtime = await AnyWasm.create(wasm, options);
      runtime.load(new Uint8Array(bundle));
      const result = runtime.runToEnd();
      return { ...result, instructions: runtime.instructions(), retired: runtime.retiredInstructions(), jitStats: runtime.jitStats };
    };
    return {
      emitted: await run({ jitMinPrefix: 3 }),
      cached: await run({}),
      exited: Status.EXITED,
    };
  }, Array.from(readFileSync(jitStateBundle)));
  check('Chrome emits warmed memory and direct JMP/Jcc exits, then falls back on a cold TLB page',
    jitStateBrowser.emitted.status === jitStateBrowser.exited && jitStateBrowser.emitted.exitCode === 0 &&
      jitStateBrowser.emitted.jitStats.compilations > 0 && jitStateBrowser.emitted.jitStats.executions >= 200000 &&
      jitStateBrowser.emitted.jitStats.registerRegisterHits > 0 && jitStateBrowser.emitted.jitStats.fallbacks === 1 &&
      jitStateBrowser.emitted.jitStats.chainTransitions > 0 && jitStateBrowser.emitted.jitStats.chainCompilations > 0,
    JSON.stringify(jitStateBrowser.emitted));
  check('Chrome NOP/MOV prefix matches cached-only status and retired count',
    jitStateBrowser.cached.status === jitStateBrowser.emitted.status &&
      jitStateBrowser.cached.exitCode === jitStateBrowser.emitted.exitCode &&
      jitStateBrowser.cached.instructions === jitStateBrowser.emitted.instructions &&
      jitStateBrowser.cached.retired === jitStateBrowser.emitted.retired &&
      jitStateBrowser.cached.jitStats.executions === 0,
    JSON.stringify(jitStateBrowser));

  await page.goto('http://localhost:8099/index.html?run=crt_test');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const t2 = await page.textContent('#term');
  check('crt_test.exe full output in the page', t2.includes('sscanf=3 x+y=46 word=hello') && t2.includes('atexit handler ran') && t2.includes('arg2=second arg'), t2);
  check('crt_test.exe stderr shown', t2.includes('to stderr'), t2);
  check('crt_test.exe exit code 7', (await page.getAttribute('#stat', 'data-exit')) === '7');

  // A user-supplied .awb with an unsupported import should show an accessible, actionable error.
  await page.goto('http://localhost:8099/index.html');
  await page.locator('#file').setInputFiles(unimplBundle);
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const diagnostic = page.locator('#diagnostic');
  const diagnosticText = await diagnostic.textContent().catch(() => null);
  const diagnosticRole = await diagnostic.getAttribute('role').catch(() => null);
  const diagnosticVisible = await diagnostic.isVisible().catch(() => false);
  check('unsupported import has an accessible alert with the exact runtime message',
    diagnosticVisible && diagnosticRole === 'alert' && diagnosticText?.includes('unimplemented import kernel32!CreateToolhelp32Snapshot'),
    JSON.stringify({ diagnosticText, diagnosticRole, diagnosticVisible }));
  check('unsupported import alert points to the analyzer for the full import list',
    diagnosticText?.includes('anywasm analyze <program.exe>'), diagnosticText || 'no diagnostic');
  check('unsupported import remains a stopped run', (await page.getAttribute('#stat', 'data-status')) === '2');

  // ── Browser key + mouse events must reach the guest window procedure ──
  await page.goto('http://localhost:8099/index.html?run=input_test');
  await page.waitForFunction(() => {
    const term = document.querySelector('#term')?.textContent || '';
    return term.includes('guest input ready') || document.getElementById('stat').dataset.status !== undefined;
  }, null, { timeout: 20000 });
  const inputReady = await page.evaluate(() => document.querySelector('#term').textContent.includes('guest input ready'));
  const inputReadyDiag = await page.evaluate(() => ({ term: document.querySelector('#term').textContent, stat: document.getElementById('stat').textContent, caps: document.getElementById('caps').textContent }));
  check('input fixture reached its message loop', inputReady, JSON.stringify(inputReadyDiag));
  const idleLoadIndicator = page.locator('#loading');
  check('load progress clears while a running guest waits for input',
    await idleLoadIndicator.count() === 1 && await idleLoadIndicator.isHidden());
  if (inputReady) {
    await page.evaluate(() => {
      window.__inputTrace = [];
      const worker = window.__worker;
      const post = worker.postMessage.bind(worker);
      worker.postMessage = (message, transfer) => {
        if (message?.type === 'input') window.__inputTrace.push(message);
        return post(message, transfer);
      };
    });
    // Input delivery is independent of WebGPU; expose the canvas in browsers without a GPU adapter.
    await page.locator('#screen').evaluate((canvas) => {
      canvas.hidden = false;
      // Keep a 256x192 content box so integer browser pointer coordinates map exactly at 4x scale.
      canvas.style.width = '258px';
      canvas.style.height = '194px';
    });
    await page.locator('#screen').focus();
    await page.keyboard.press('a');
    const inputCenter = await page.locator('#screen').evaluate((canvas) => {
      const rect = canvas.getBoundingClientRect();
      return {
        x: rect.left + canvas.clientLeft + canvas.clientWidth / 2,
        y: rect.top + canvas.clientTop + canvas.clientHeight / 2,
      };
    });
    await page.mouse.click(inputCenter.x, inputCenter.y);
    const inputMove = await page.locator('#screen').evaluate((canvas) => {
      const rect = canvas.getBoundingClientRect();
      return { x: rect.left + canvas.clientLeft + 130, y: rect.top + canvas.clientTop + 101 };
    });
    await page.mouse.move(inputMove.x, inputMove.y);
    await page.mouse.move(inputCenter.x + 500, inputCenter.y + 300);
    await page.waitForFunction(() => {
      const term = document.querySelector('#term')?.textContent || '';
      return term.includes('guest input complete') || document.getElementById('stat').dataset.status !== undefined;
    }, null, { timeout: 10000 }).catch(() => {});
  }
  const inputText = await page.textContent('#term');
  const inputStat = await page.textContent('#stat');
  const inputTrace = await page.evaluate(() => ({ trace: window.__inputTrace, aspectRatio: document.querySelector('#screen')?.style.aspectRatio }));
  check('guest observed WM_KEYDOWN(A)', inputText.includes('guest saw keydown A'), inputText);
  check('guest mouse coordinates scale from the canvas to the 1024x768 Win32 client area',
    inputText.includes('guest saw scaled mousemove (520,404)') &&
      inputTrace.trace.some((event) => event.kind === 3 && event.a === 520 && event.b === 404) &&
      inputTrace.aspectRatio === '1024 / 768', JSON.stringify({ inputText, inputTrace }));
  check('guest TrackMouseEvent(TME_LEAVE) succeeds', inputText.includes('guest TrackMouseEvent(TME_LEAVE) succeeded'), inputText + inputStat);
  check('guest receives WM_MOUSELEAVE after the pointer leaves the canvas', inputText.includes('guest saw WM_MOUSELEAVE'), inputText + inputStat);
  check('guest observed WM_LBUTTONDOWN', inputText.includes('guest saw left-down'), inputText);
  check('guest observed both input messages', inputText.includes('guest input complete'), inputText);
  check('guest input fixture exited zero', /exited \(exit code 0\)/.test(inputStat), inputStat);

  // ── Direct3D 11 triangle: replay the program's GPU stream with WebGPU and read the pixels back ──
  await page.goto('http://localhost:8099/index.html?run=d3d11_triangle&test=1');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const triText = await page.textContent('#term');
  const triStat = await page.textContent('#stat');
  console.log('triangle status:', triStat);
  check('d3d11 program ran to completion', /exited \(exit code 0\)/.test(triStat) && triText.includes('frames=3'), triText + triStat);
  const gpuStats = await page.evaluate(() => window.__gpuStats);
  console.log('gpu stats:', JSON.stringify(gpuStats));
  check('WebGPU executor drew and presented', gpuStats && gpuStats.draws >= 3 && gpuStats.presents >= 3, JSON.stringify(gpuStats));
  check('no WebGPU validation errors', gpuStats && gpuStats.errors.length === 0, JSON.stringify(gpuStats?.errors));
  // Test mode skips swap-chain presentation so this readback checks the D3D back buffer directly.
  await page.evaluate(() => { window.__pixels = null; window.__worker.postMessage({ type: 'capture' }); });
  await page.waitForFunction(() => window.__pixels, null, { timeout: 20000 });
  const px = await page.evaluate(() => { const p = window.__pixels; return { w: p.width, h: p.height, error: p.error, data: Array.from(p.data) }; });
  console.log('captured:', px.w, 'x', px.h, 'bytes', px.data.length, px.error || '');
  const at = (x, y) => { const i = (y * px.w + x) * 4; return px.data.slice(i, i + 4); };
  const near = (a, b, tol = 4) => a.length === b.length && a.every((v, k) => Math.abs(v - b[k]) <= tol);
  check('backbuffer is 640x480', px.w === 640 && px.h === 480, `${px.w}x${px.h}`);
  check('clear colour (0,0,51,255) in the corner', near(at(10, 10), [0, 0, 51, 255]), JSON.stringify(at(10, 10)));
  check('triangle centroid is mid-grey (barycentric blend 85,85,85)', near(at(320, 288), [85, 85, 85, 255], 6), JSON.stringify(at(320, 288)));
  // Exact expected colours: barycentric interpolation of red/green/blue over the NDC triangle.
  const V = [[320, 96, [255, 0, 0]], [512, 384, [0, 255, 0]], [128, 384, [0, 0, 255]]];
  const bary = (x, y) => {
    const [a, b, c] = V, d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    const w0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / d, w1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
    return [w0, w1, 1 - w0 - w1];
  };
  let worst = 0, checked = 0;
  for (let y = 100; y < 380; y += 17) for (let x = 140; x < 500; x += 13) {
    const w = bary(x + 0.5, y + 0.5);
    if (w.some((v) => v < 0.03)) continue; // skip pixels too close to an edge (rasterisation rule differences)
    const exp = [0, 1, 2].map((k) => Math.round(255 * w[0] * V[0][2][k] / 255 + 255 * w[1] * V[1][2][k] / 255 + 255 * w[2] * V[2][2][k] / 255));
    const got = at(x, y);
    worst = Math.max(worst, ...exp.map((v, k) => Math.abs(v - got[k])));
    checked++;
  }
  check(`interpolated colours match barycentric maths at ${checked} pixels (worst error ${worst}/255)`, checked > 50 && worst <= 3, `worst ${worst}`);
  check('near the bottom-right vertex it is green', at(500, 380)[1] > 200 && at(500, 380)[0] < 50, JSON.stringify(at(500, 380)));
  check('near the bottom-left vertex it is blue', at(140, 380)[2] > 200 && at(140, 380)[0] < 50, JSON.stringify(at(140, 380)));
  check('outside the triangle (left of the bottom-left vertex) is still the clear colour', near(at(100, 400), [0, 0, 51, 255]), JSON.stringify(at(100, 400)));

  // Run without test mode to verify the actual swap-chain-to-canvas blit in a browser screenshot.
  await page.goto('http://localhost:8099/index.html?run=d3d11_triangle');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const canvasStats = await page.evaluate(() => window.__gpuStats);
  check('canvas presentation completed without WebGPU validation errors', canvasStats && canvasStats.presents === 3 && canvasStats.errors.length === 0, JSON.stringify(canvasStats));
  const canvasLayout = await page.locator('#screen').evaluate((canvas) => {
    const [width, height] = canvas.style.aspectRatio.split('/').map(Number);
    return { aspectWidth: width, aspectHeight: height };
  });
  const canvasPixels = decodePng(await page.locator('#screen').screenshot());
  const atCanvas = (x, y) => { const i = (y * canvasPixels.w + x) * 4; return Array.from(canvasPixels.data.slice(i, i + 4)); };
  check('canvas screenshot preserves the guest window aspect ratio',
    canvasPixels.w === 640 && Math.abs(canvasPixels.h - canvasPixels.w * canvasLayout.aspectHeight / canvasLayout.aspectWidth) < 1,
    `${canvasPixels.w}x${canvasPixels.h} at ${canvasLayout.aspectWidth}/${canvasLayout.aspectHeight}`);
  check('canvas blit preserves the clear colour', near(atCanvas(10, 10), [0, 0, 51, 255], 2), JSON.stringify(atCanvas(10, 10)));
  const centroidY = Math.round(1 + 288.5 / 480 * (canvasPixels.h - 2));
  check('canvas blit presents the interpolated triangle', near(atCanvas(320, centroidY), [85, 85, 85, 255], 6), JSON.stringify(atCanvas(320, centroidY)));
  await page.locator('#screen').evaluate((canvas) => { canvas.style.width = '320px'; });
  const scaledCanvasPixels = decodePng(await page.locator('#screen').screenshot());
  const atScaledCanvas = (x, y) => { const i = (y * scaledCanvasPixels.w + x) * 4; return Array.from(scaledCanvasPixels.data.slice(i, i + 4)); };
  check('scaled canvas screenshot preserves the guest window aspect ratio',
    scaledCanvasPixels.w === 320 && Math.abs(scaledCanvasPixels.h - scaledCanvasPixels.w * canvasLayout.aspectHeight / canvasLayout.aspectWidth) < 1,
    `${scaledCanvasPixels.w}x${scaledCanvasPixels.h}`);
  const scaledCentroidY = Math.round(1 + 288.5 / 480 * (scaledCanvasPixels.h - 2));
  check('scaled canvas blit preserves the interpolated triangle', near(atScaledCanvas(160, scaledCentroidY), [85, 85, 85, 255], 6), JSON.stringify(atScaledCanvas(160, scaledCentroidY)));

  // ── Direct3D 11 textured quads + depth buffer + indexed draws ──
  // Verify multisampled D3D11 render targets and ResolveSubresource in the WebGPU executor.
  const msaaResolveResult = await page.evaluate(async ({ drawMapVertexBytes, drawMapPixelBytes }) => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 16; canvas.height = 16;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const logs = [];
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const drawMapVertex = tool.translate(new Uint8Array(drawMapVertexBytes));
      const drawMapPixel = tool.translate(new Uint8Array(drawMapPixelBytes));
      const drawMapVertexModule = gpu.device.createShaderModule({ code: drawMapVertex.wgsl });
      const drawMapPixelModule = gpu.device.createShaderModule({ code: drawMapPixel.wgsl });
      const drawMapInfo = await Promise.all([drawMapVertexModule.getCompilationInfo(), drawMapPixelModule.getCompilationInfo()]);
      const drawMapShaderErrors = drawMapInfo.flatMap((info) => info.messages.filter((message) => message.type === 'error').map((message) => message.message));
      let drawMapPipeline = null, drawMapPipelineError = '';
      gpu.device.pushErrorScope('validation');
      try {
        drawMapPipeline = await gpu.device.createRenderPipelineAsync({
          layout: 'auto',
          vertex: { module: drawMapVertexModule, entryPoint: 'main' },
          fragment: { module: drawMapPixelModule, entryPoint: 'main', targets: [{ format: 'rgba8unorm' }] },
          primitive: { topology: 'triangle-strip' },
        });
      } catch (error) { drawMapPipelineError = String(error); }
      const drawMapValidation = await gpu.device.popErrorScope();
      let drawMapClearPixel = null, drawMapWallPixel = null, drawMapRenderError = '';
      if (drawMapPipeline) try {
        const mapConstants = gpu.device.createBuffer({ size: 48, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
        const playerConstants = gpu.device.createBuffer({ size: 32, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
        const drawParameters = gpu.device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
        gpu.device.queue.writeBuffer(mapConstants, 0, new Float32Array([
          -1, 1, 1, -1, // screen rectangle
          4, 4, 0.25, 0.25, // map-to-screen scale
          100, 100, 1, 0.2, // teleport outside the test view
        ]));
        gpu.device.queue.writeBuffer(playerConstants, 0, new Float32Array([
          0, 0, 0.25, 4, // player position/radius and ambient-light radius
          1, 0, 0, 10, // flashlight direction/power/range
        ]));
        gpu.device.queue.writeBuffer(drawParameters, 0, new Int32Array(4));
        const sdfTexture = gpu.device.createTexture({
          size: { width: 1, height: 1 }, format: 'rgba8unorm',
          usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
        });
        const sampler = gpu.device.createSampler({ magFilter: 'nearest', minFilter: 'nearest', mipmapFilter: 'nearest' });
        const groups = [
          gpu.device.createBindGroup({ layout: drawMapPipeline.getBindGroupLayout(0), entries: [
            { binding: 0, resource: { buffer: mapConstants } },
            { binding: 1, resource: { buffer: playerConstants } },
          ] }),
          gpu.device.createBindGroup({ layout: drawMapPipeline.getBindGroupLayout(1), entries: [
            { binding: 0, resource: sdfTexture.createView() },
          ] }),
          gpu.device.createBindGroup({ layout: drawMapPipeline.getBindGroupLayout(2), entries: [
            { binding: 0, resource: sampler },
          ] }),
          gpu.device.createBindGroup({ layout: drawMapPipeline.getBindGroupLayout(3), entries: [
            { binding: 0, resource: { buffer: drawParameters } },
          ] }),
        ];
        const target = gpu.device.createTexture({
          size: { width: 16, height: 16 }, format: 'rgba8unorm',
          usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
        });
        const readback = gpu.device.createBuffer({ size: 16 * 256, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
        const renderSdfSample = async (red) => {
          gpu.device.queue.writeTexture({ texture: sdfTexture }, new Uint8Array([red, 0, 0, 255]),
            { bytesPerRow: 4, rowsPerImage: 1 }, { width: 1, height: 1, depthOrArrayLayers: 1 });
          const encoder = gpu.device.createCommandEncoder();
          const pass = encoder.beginRenderPass({ colorAttachments: [{
            view: target.createView(), clearValue: { r: 0, g: 0, b: 0, a: 1 }, loadOp: 'clear', storeOp: 'store',
          }] });
          pass.setPipeline(drawMapPipeline);
          groups.forEach((group, index) => pass.setBindGroup(index, group));
          pass.draw(4, 1, 0, 0);
          pass.end();
          encoder.copyTextureToBuffer({ texture: target }, { buffer: readback, bytesPerRow: 256, rowsPerImage: 16 }, { width: 16, height: 16, depthOrArrayLayers: 1 });
          gpu.device.queue.submit([encoder.finish()]);
          await readback.mapAsync(GPUMapMode.READ);
          const pixels = new Uint8Array(readback.getMappedRange());
          const centerPixel = Array.from(pixels.slice(8 * 256 + 8 * 4, 8 * 256 + 8 * 4 + 4));
          readback.unmap();
          return centerPixel;
        };
        drawMapClearPixel = await renderSdfSample(255);
        drawMapWallPixel = await renderSdfSample(0);
        for (const resource of [mapConstants, playerConstants, drawParameters, sdfTexture, target, readback]) resource.destroy();
      } catch (error) { drawMapRenderError = String(error); }
      const vertexShader = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixelShader = tool.compileHlsl(`
        float4 main() : SV_Target { return float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(4, words(1, 8, 8, 1, 1, 28, 0x20, 0, 4, 0)); // 4x RGBA8 render target
      command(12, words(2, 0, 1, 28, 4, 0, 1, 0, 1));
      command(27, words(2, 0, 0, 0, 0x3f800000)); // clear to black
      shader(10, 0, vertexShader);
      shader(11, 1, pixelShader);
      command(17, words(0, 10));
      command(17, words(1, 11));
      command(21, words(1, 2, 0));
      command(29, words(3, 0));
      command(4, words(4, 8, 8, 1, 1, 87, 0x20, 0, 4, 0)); // 4x BGRA8 render target
      command(12, words(5, 0, 4, 87, 4, 0, 1, 0, 1));
      command(27, words(5, 0, 0, 0, 0x3f800000));
      command(21, words(1, 5, 0));
      command(29, words(3, 0));
      command(4, words(3, 8, 8, 1, 1, 28, 0x20, 0, 1, 0)); // single-sample resolve target
      command(4, words(6, 8, 8, 1, 1, 87, 0x20, 0, 1, 0));
      command(47, words(3, 0, 1, 0, 28)); // ResolveSubresource(dst, 0, src, 0, RGBA8_UNORM)
      command(47, words(6, 0, 4, 0, 87)); // ResolveSubresource(dst, 0, src, 0, BGRA8_UNORM)
      command(4, words(7, 8, 8, 1, 1, 10, 0x20, 0, 4, 0)); // 4x RGBA16_FLOAT render target
      command(12, words(8, 0, 7, 10, 4, 0, 1, 0, 1));
      command(27, words(8, 0, 0, 0, 0x3f800000));
      command(21, words(1, 8, 0));
      command(29, words(3, 0));
      command(4, words(9, 8, 8, 1, 1, 10, 0x20, 0, 1, 0)); // single-sample RGBA16_FLOAT resolve target
      command(47, words(9, 0, 7, 0, 10)); // ResolveSubresource(dst, 0, src, 0, R16G16B16A16_FLOAT)
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackTexture(3);
      const bgraPixels = await gpu.readBackTexture(6);
      const float16Pixels = await gpu.readBackTexture(9);
      const result = {
        samples: gpu.res.get(1)?.samples,
        bgraSamples: gpu.res.get(4)?.samples,
        float16Samples: gpu.res.get(7)?.samples,
        width: pixels.width,
        height: pixels.height,
        format: pixels.format,
        allPixelsMatch: pixels.data.every((value, i) => Math.abs(value - [64, 128, 191, 255][i % 4]) <= 1),
        drawMapPipelineValidated: !!drawMapPipeline && !drawMapValidation && !drawMapPipelineError && drawMapShaderErrors.length === 0,
        drawMapPipelineError: drawMapPipelineError || drawMapValidation?.message || drawMapShaderErrors.join('\n'),
        drawMapClearPixel,
        drawMapWallPixel,
        drawMapRenderError,
        bgraFormat: bgraPixels.format,
        bgraPixelsMatch: bgraPixels.data.every((value, i) => Math.abs(value - [64, 128, 191, 255][i % 4]) <= 1),
        float16Format: float16Pixels.format,
        float16PixelsMatch: float16Pixels.data.every((value, i) => Math.abs(value - [0.25, 0.5, 0.75, 1.0][i % 4]) <= 0.001),
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
        logs,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: String(error) };
    }
  }, {
    drawMapVertexBytes: Array.from(readFileSync(join(repoRoot, 'fixtures', 'diligent_glfwdemo_drawmap_vs.dxbc'))),
    drawMapPixelBytes: Array.from(readFileSync(join(repoRoot, 'fixtures', 'diligent_glfwdemo_drawmap_ps.dxbc'))),
  });
  check('4x multisampled render target draws and resolves into a single-sample texture',
    msaaResolveResult.samples === 4 && msaaResolveResult.width === 8 && msaaResolveResult.height === 8
      && msaaResolveResult.format === 'rgba8unorm' && msaaResolveResult.allPixelsMatch
      && msaaResolveResult.bgraSamples === 4 && msaaResolveResult.bgraFormat === 'bgra8unorm'
      && msaaResolveResult.bgraPixelsMatch && msaaResolveResult.float16Samples === 4
      && msaaResolveResult.float16Format === 'rgba16float' && msaaResolveResult.float16PixelsMatch
      && msaaResolveResult.draws === 3 && msaaResolveResult.errors?.length === 0,
    JSON.stringify(msaaResolveResult));
  check('Diligent DrawMap vertex and pixel shaders validate as a WebGPU render pipeline',
    msaaResolveResult.drawMapPipelineValidated, msaaResolveResult.drawMapPipelineError || JSON.stringify(msaaResolveResult));
  const drawMapClearLuminance = msaaResolveResult.drawMapClearPixel?.slice(0, 3).reduce((sum, channel) => sum + channel, 0) ?? 0;
  const drawMapWallLuminance = msaaResolveResult.drawMapWallPixel?.slice(0, 3).reduce((sum, channel) => sum + channel, 0) ?? 0;
  check('Diligent DrawMap ray marching darkens its center for a zero-distance wall sample',
    msaaResolveResult.drawMapClearPixel?.[3] === 255 && msaaResolveResult.drawMapWallPixel?.[3] === 255
      && drawMapClearLuminance > drawMapWallLuminance,
    msaaResolveResult.drawMapRenderError || JSON.stringify({ clearSdf: msaaResolveResult.drawMapClearPixel, wallSdf: msaaResolveResult.drawMapWallPixel }));

  await page.goto('http://localhost:8099/index.html?run=d3d11_textured&test=1');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const texText = await page.textContent('#term');
  check('textured program ran to completion', texText.includes('frames=3') && /exited \(exit code 0\)/.test(await page.textContent('#stat')), texText);
  const g2 = await page.evaluate(() => window.__gpuStats);
  check('textured: 18 indexed draws, no validation errors', g2 && g2.draws === 18 && g2.errors.length === 0, JSON.stringify(g2));
  await page.evaluate(() => { window.__pixels = null; window.__worker.postMessage({ type: 'capture' }); });
  await page.waitForFunction(() => window.__pixels, null, { timeout: 20000 });
  const p2 = await page.evaluate(() => { const p = window.__pixels; return { w: p.width, h: p.height, data: Array.from(p.data) }; });
  const at2 = (x, y) => { const i = (y * p2.w + x) * 4; return p2.data.slice(i, i + 4); };
  const RED = [255, 0, 0, 255], GREEN = [0, 255, 0, 255], BLUE = [0, 0, 255, 255], WHITE = [255, 255, 255, 255], BG = [0, 0, 51, 255];
  // Quad A: x 64..384, y 72..264, half-blended red.  Quad B: x 256..576, y 72..264, green, behind A.
  check('near quad A blends half red over the clear blue', near(at2(100, 150), [128, 0, 26, 255], 1), JSON.stringify(at2(100, 150)));
  check('far quad B only region is green', near(at2(500, 150), GREEN, 1), JSON.stringify(at2(500, 150)));
  check('overlap keeps the blended NEAR quad (depth test; B drawn after A)', near(at2(320, 150), [128, 0, 26, 255], 1), JSON.stringify(at2(320, 150)));
  check('overlap edge: last column of A is blended, first column right of A is green', near(at2(382, 150), [128, 0, 26, 255], 1) && near(at2(387, 150), GREEN, 1), JSON.stringify([at2(382, 150), at2(387, 150)]));
  // Quad C: x 128..512, y 312..456, whole 2x2 texture with point sampling: red green / blue white.
  check('quad C top-left texel red', near(at2(200, 340), RED, 1), JSON.stringify(at2(200, 340)));
  check('quad C top-right texel green', near(at2(440, 340), GREEN, 1), JSON.stringify(at2(440, 340)));
  check('quad C bottom-left texel blue', near(at2(200, 430), BLUE, 1), JSON.stringify(at2(200, 430)));
  check('SRC_ALPHA blends half-white over the known blue region', near(at2(270, 420), [128, 128, 255, 128], 1), JSON.stringify(at2(270, 420)));
  check('quad C bottom-right texel white', near(at2(440, 430), WHITE, 1), JSON.stringify(at2(440, 430)));
  check('background untouched', near(at2(10, 10), BG, 1) && near(at2(320, 290), BG, 1), JSON.stringify([at2(10, 10), at2(320, 290)]));
  check('point sampling gives hard texel edge (no blend at the seam)', [at2(319, 340), at2(320, 340)].every((c) => near(c, RED, 1) || near(c, GREEN, 1)), JSON.stringify([at2(319, 340), at2(320, 340)]));
  check('CopyResource exposes the source red texel in quad D', near(at2(32, 150), RED, 1), JSON.stringify(at2(32, 150)));
  check('buffer CopyResource exposes the red quad at its copied vertex-buffer position', near(at2(604, 150), RED, 1), JSON.stringify(at2(604, 150)));

  await page.goto('http://localhost:8099/index.html?run=d3d11_textured');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const visibleTextureStats = await page.evaluate(() => window.__gpuStats);
  const visibleTextureLayout = await page.locator('#screen').evaluate((canvas) => {
    const [width, height] = canvas.style.aspectRatio.split('/').map(Number);
    return { aspectWidth: width, aspectHeight: height };
  });
  const visibleTexturePixels = decodePng(await page.locator('#screen').screenshot());
  const visibleTextureY = (sourceY) => Math.round(1 + (sourceY + 0.5) / 480 * (visibleTexturePixels.h - 2));
  const atVisibleTexture = (x, y) => {
    const i = (y * visibleTexturePixels.w + x) * 4;
    return Array.from(visibleTexturePixels.data.slice(i, i + 4));
  };
  check('textured D3D11 canvas presents all frames without WebGPU validation errors',
    visibleTextureStats?.draws === 18 && visibleTextureStats.presents === 3 && visibleTextureStats.errors.length === 0,
    JSON.stringify(visibleTextureStats));
  check('textured canvas blit preserves the blended red quad and sampled texture pixels',
    visibleTexturePixels.w === 640
      && Math.abs(visibleTexturePixels.h - visibleTexturePixels.w * visibleTextureLayout.aspectHeight / visibleTextureLayout.aspectWidth) < 1
      && near(atVisibleTexture(100, visibleTextureY(150)), [128, 0, 26, 255], 2)
      && near(atVisibleTexture(200, visibleTextureY(340)), RED, 2)
      && near(atVisibleTexture(440, visibleTextureY(430)), WHITE, 2),
    JSON.stringify({ size: [visibleTexturePixels.w, visibleTexturePixels.h], layout: visibleTextureLayout,
      blended: atVisibleTexture(100, visibleTextureY(150)),
      texture: [atVisibleTexture(200, visibleTextureY(340)), atVisibleTexture(440, visibleTextureY(430))] }));

  const indexCopyResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        float4 main() : SV_Target { return float4(0.25, 0.5, 1.0, 1.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(2, words(11, 12, 0, 0x40, 0, 0, 0)); // source R32_UINT index buffer
      command(2, words(12, 12, 0, 0x40, 0, 0, 0)); // destination R32_UINT index buffer
      const indices = new Uint8Array(12), indexData = new DataView(indices.buffer);
      [0, 1, 2].forEach((value, i) => indexData.setUint32(i * 4, value, true));
      const write = new Uint8Array(12 + indices.length);
      write.set(words(11, 0, indices.length)); write.set(indices, 12); command(3, write);
      command(33, words(12, 11)); // CopyResource destination <- source
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(21, words(1, 2, 0)); command(16, words(4));
      command(14, words(12, 42, 0)); // R32_UINT index buffer
      command(30, words(3, 0, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('index-buffer CopyResource preserves a drawable R32_UINT triangle',
    indexCopyResult.draws === 1 && indexCopyResult.errors.length === 0
      && near(indexCopyResult.pixel, [64, 128, 255, 255], 1),
    JSON.stringify(indexCopyResult));

  const copyRegionResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28)); // RGBA8 back buffer
      command(4, words(3, 4, 4, 3, 1, 28, 0x28, 0, 1, 1)); // RGBA8 source texture with mips
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1)); // back-buffer RTV
      command(12, words(4, 2, 3, 28, 4, 0, 3, 0, 1)); // source SRV, all mip levels
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 0, true);
      clearView.setFloat32(12, 0.2, true); clearView.setFloat32(16, 1, true);
      command(27, clear);
      const rowPitch = 20, textureBytes = new Uint8Array(rowPitch * 4);
      for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) {
        textureBytes.set([x * 40, y * 40, 0, 255], y * rowPitch + x * 4);
      }
      const upload = new Uint8Array(36 + textureBytes.length);
      upload.set(words(3, 0, 0, 0, 0, 4, 4, rowPitch, textureBytes.length));
      upload.set(textureBytes, 36);
      command(5, upload); // UpdateSubresource-style padded texture upload
      command(39, words(4)); // D3D11 GenerateMips on the all-level SRV
      command(40, words(9, 0, 10, 10, 0, 3, 0, 1, 1, 1, 0, 3, 3, 1));
      command(40, words(9, 0, 20, 20, 0, 3, 1, 0, 0, 0, 0, 0, 0, 0)); // copy mip 1 (2x2)
      command(40, words(9, 0, 24, 24, 0, 3, 2, 0, 0, 0, 0, 0, 0, 0)); // copy mip 2 (1x1)
      gpu.execute(append());
      const copiedPixels = await gpu.readBackbuffer(1);
      const atCopied = (x, y) => {
        const i = (y * copiedPixels.width + x) * 4;
        return Array.from(copiedPixels.data.slice(i, i + 4));
      };
      const result = {
        width: copiedPixels.width, height: copiedPixels.height,
        first: atCopied(10, 10), last: atCopied(11, 11),
        outside: [atCopied(9, 10), atCopied(12, 11)], errors: gpu.stats.errors,
        mip1: [atCopied(20, 20), atCopied(21, 20), atCopied(20, 21), atCopied(21, 21)], mip2: atCopied(24, 24),
      };

      stream.length = 0;
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture2D<float4> source : register(t0);
        SamplerState linearSampler : register(s0);
        float4 main() : SV_Target { return source.SampleLevel(linearSampler, float2(0.5, 0.5), 0.5); }
      `, 'main', 'ps_5_0');
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(4, words(5, 4, 4, 3, 1, 28, 0x28, 0, 1, 1));
      command(12, words(6, 2, 5, 28, 4, 0, 3, 0, 1));
      const pattern = new Uint8Array(64);
      for (let i = 0; i < 16; i++) pattern.set([0, 0, 0, 255], i * 4);
      for (const [x, y] of [[1, 1], [2, 1], [1, 2], [2, 2]]) {
        pattern.set([255, 0, 0, 255], (y * 4 + x) * 4);
      }
      const patternUpload = new Uint8Array(36 + pattern.length);
      patternUpload.set(words(5, 0, 0, 0, 0, 4, 4, 16, pattern.length));
      patternUpload.set(pattern, 36); command(5, patternUpload);
      command(39, words(6));
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 7, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(20, 0, true); samplerView.setUint32(24, 1, true); samplerView.setUint32(28, 8, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true);
      command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(19, words(1, 0, 1, 6)); command(20, words(1, 0, 1, 7));
      command(16, words(4)); command(21, words(1, 2, 0)); command(29, words(3, 0));
      gpu.execute(append());
      const filteredPixels = await gpu.readBackbuffer(1);
      const filteredAt = (x, y) => {
        const i = (y * filteredPixels.width + x) * 4;
        return Array.from(filteredPixels.data.slice(i, i + 4));
      };
      result.trilinear = filteredAt(32, 32);
      result.errors = gpu.stats.errors;
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('UpdateSubresource-style texture upload and CopySubresourceRegion validate without WebGPU errors',
    copyRegionResult.width === 64 && copyRegionResult.height === 64 && copyRegionResult.errors?.length === 0,
    JSON.stringify(copyRegionResult));
  check('CopySubresourceRegion copies both corners from the requested source box',
    near(copyRegionResult.first, [40, 40, 0, 255], 1) && near(copyRegionResult.last, [80, 80, 0, 255], 1),
    JSON.stringify(copyRegionResult));
  check('CopySubresourceRegion leaves pixels outside the destination box untouched',
    copyRegionResult.outside?.every((pixel) => near(pixel, [0, 0, 51, 255], 1)),
    JSON.stringify(copyRegionResult.outside));
  check('GenerateMips box-filters the 4x4 source into four 2x2 colour averages',
    copyRegionResult.mip1?.every((pixel, i) => near(pixel, [[20, 20, 0, 255], [100, 20, 0, 255], [20, 100, 0, 255], [100, 100, 0, 255]][i], 1)),
    JSON.stringify(copyRegionResult.mip1));
  check('GenerateMips reaches the final 1x1 level with the full-image average',
    near(copyRegionResult.mip2, [60, 60, 0, 255], 1), JSON.stringify(copyRegionResult.mip2));
  check('linear mip filtering blends LOD 0 and 1 at fractional LOD 0.5',
    near(copyRegionResult.trilinear, [159, 0, 0, 255], 2) && copyRegionResult.errors.length === 0,
    JSON.stringify({ pixel: copyRegionResult.trilinear, errors: copyRegionResult.errors }));

  const anisotropicSamplerResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const desc = new DataView(new ArrayBuffer(52));
      desc.setUint32(0, 0x55, true); // D3D11_FILTER_ANISOTROPIC
      desc.setUint32(4, 3, true); desc.setUint32(8, 3, true); desc.setUint32(12, 3, true);
      desc.setFloat32(16, 0, true); desc.setUint32(20, 8, true); desc.setUint32(24, 8, true);
      desc.setFloat32(44, 0, true); desc.setFloat32(48, 8, true);
      gpu.device.pushErrorScope('validation');
      const sampler = gpu.makeSampler(desc);
      const validation = await gpu.device.popErrorScope();
      const result = {
        maxAnisotropy: sampler.maxAnisotropy,
        validation: validation?.message || null,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 anisotropic sampler requests the configured 8x filtering without validation errors',
    anisotropicSamplerResult.maxAnisotropy === 8 && !anisotropicSamplerResult.validation
      && anisotropicSamplerResult.errors?.length === 0,
    JSON.stringify(anisotropicSamplerResult));

  const samplerAddressResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState wrapSampler : register(s0);
        SamplerState clampSampler : register(s1);
        float4 main(float4 position : SV_Position) : SV_Target {
          return position.x < 32.0
            ? image.SampleLevel(wrapSampler, float2(1.25, 0.5), 0.0)
            : image.SampleLevel(clampSampler, float2(1.25, 0.5), 0.0);
        }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(13, 2, 1, 1, 1, 28, 8, 0, 1, 0));
      const texels = new Uint8Array(44);
      texels.set(words(13, 0, 0, 0, 0, 2, 1, 8, 8));
      texels.set([255, 0, 0, 255, 0, 255, 0, 255], 36);
      command(5, texels);
      command(12, words(14, 2, 13, 28, 4, 0, 1, 0, 1));
      const sampler = (id, addressU) => {
        const data = new Uint8Array(56), view = new DataView(data.buffer);
        view.setUint32(0, id, true); view.setUint32(4, 0, true); // point min/mag/mip
        view.setUint32(8, addressU, true); view.setUint32(12, 3, true); view.setUint32(16, 3, true);
        view.setUint32(24, 1, true); view.setUint32(28, 1, true);
        view.setFloat32(48, 0, true); view.setFloat32(52, 0, true);
        return data;
      };
      command(8, sampler(15, 1)); // wrap U
      command(8, sampler(16, 3)); // clamp U
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(21, words(1, 2, 0)); command(16, words(4));
      command(19, words(1, 0, 1, 14)); command(20, words(1, 0, 2, 15, 16));
      command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const at = (x) => Array.from(pixels.data.slice((32 * pixels.width + x) * 4, (32 * pixels.width + x) * 4 + 4));
      const result = { wrap: at(16), clamp: at(48), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 sampler repeat and clamp address modes select the wrapped and edge texels',
    samplerAddressResult.draws === 1 && samplerAddressResult.errors.length === 0
      && near(samplerAddressResult.wrap, [255, 0, 0, 255], 1)
      && near(samplerAddressResult.clamp, [0, 255, 0, 255], 1),
    JSON.stringify(samplerAddressResult));

  const d3d9FixedResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(12, 64, 64, 1, 1, 40, 0x40, 0, 1, 0)); // D3D9 depth surface
      command(12, words(3, 1, 12, 40, 3, 0, 1, 0, 1)); // DSV
      command(21, words(1, 2, 3));
      command(28, words(3, 1, 0, 0)); // Clear depth to zero so default LESS rejects the fan.
      const viewport = new Uint8Array(28), viewportView = new DataView(viewport.buffer);
      viewportView.setUint32(0, 1, true);
      [0, 0, 64, 64, 0, 1].forEach((value, i) => viewportView.setFloat32(4 + i * 4, value, true));
      command(22, viewport);
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 0, true);
      clearView.setFloat32(12, 0, true); clearView.setFloat32(16, 1, true);
      command(27, clear);
      command(45, words(0x44, 64, 64)); // D3DFVF_XYZRHW | D3DFVF_DIFFUSE
      command(2, words(11, 80, 1, 0, 0, 20));
      const vertexData = new Uint8Array(80), vertexView = new DataView(vertexData.buffer);
      [[8, 8], [56, 8], [56, 56], [8, 56]].forEach(([x, y], i) => {
        const offset = i * 20;
        vertexView.setFloat32(offset, x, true);
        vertexView.setFloat32(offset + 4, y, true);
        vertexView.setFloat32(offset + 8, 0.5, true);
        vertexView.setFloat32(offset + 12, 1, true);
        vertexView.setUint32(offset + 16, 0xffff0000, true); // D3DCOLOR red, stored BGRA
      });
      const write = new Uint8Array(12 + vertexData.length);
      write.set(words(11, 0, vertexData.length)); write.set(vertexData, 12); command(3, write);
      command(13, words(0, 1, 11, 20, 0));
      command(16, words(6)); // D3D9 triangle fan
      command(48, words(7, 0)); // D3DRS_ZENABLE = FALSE
      command(48, words(22, 1)); // D3DRS_CULLMODE = D3DCULL_NONE
      command(48, words(15, 1)); // D3DRS_ALPHATESTENABLE
      command(48, words(25, 5)); // D3DRS_ALPHAFUNC = D3DCMP_GREATER
      command(48, words(24, 255)); // D3DRS_ALPHAREF
      command(29, words(4, 0));
      gpu.execute(append());
      stream.length = 0;
      const rejectedPixels = await gpu.readBackbuffer(1);
      command(48, words(24, 254)); // Alpha 1.0 is now greater than the reference.
      command(29, words(4, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const alphaFunctionPixels = {};
      for (const alphaFunc of [1, 2, 3, 4, 6, 7, 8]) {
        stream.length = 0;
        command(27, clear);
        command(48, words(25, alphaFunc)); // D3DRS_ALPHAFUNC
        command(48, words(24, 255)); // Match the vertex alpha exactly.
        command(29, words(4, 0));
        gpu.execute(append());
        const comparisonPixels = await gpu.readBackbuffer(1);
        const center = (20 * comparisonPixels.width + 45) * 4;
        alphaFunctionPixels[alphaFunc] = Array.from(comparisonPixels.data.slice(center, center + 4));
      }
      stream.length = 0;
      const translucentVertices = vertexData.slice();
      const translucentView = new DataView(translucentVertices.buffer);
      for (let i = 0; i < 4; i++) translucentView.setUint32(i * 20 + 16, 0x80ff0000, true);
      const translucentWrite = new Uint8Array(12 + translucentVertices.length);
      translucentWrite.set(words(11, 0, translucentVertices.length));
      translucentWrite.set(translucentVertices, 12); command(3, translucentWrite);
      const blueClear = new Uint8Array(20), blueClearView = new DataView(blueClear.buffer);
      blueClearView.setUint32(0, 2, true);
      blueClearView.setFloat32(12, 1, true); blueClearView.setFloat32(16, 1, true);
      command(27, blueClear);
      command(48, words(15, 0)); // disable alpha test for blending
      command(48, words(27, 1)); // D3DRS_ALPHABLENDENABLE
      command(48, words(19, 5)); // D3DBLEND_SRCALPHA
      command(48, words(20, 6)); // D3DBLEND_INVSRCALPHA
      command(29, words(4, 0));
      gpu.execute(append());
      const blendedPixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const rejectedAt = (x, y) => {
        const i = (y * rejectedPixels.width + x) * 4;
        return Array.from(rejectedPixels.data.slice(i, i + 4));
      };
      const result = {
        rejected: [rejectedAt(45, 20), rejectedAt(20, 45)],
        upperRight: at(45, 20), lowerLeft: at(20, 45), background: at(1, 1), stats: gpu.stats,
        alphaFunctionPixels,
        blendedPixel: Array.from(blendedPixels.data.slice((20 * blendedPixels.width + 45) * 4, (20 * blendedPixels.width + 45) * 4 + 4)),
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D9 FVF fixed-function triangle fan draws with no WebGPU validation errors',
    d3d9FixedResult.stats?.draws === 10 && d3d9FixedResult.stats.errors.length === 0,
    JSON.stringify(d3d9FixedResult));
  check('D3D9 alpha test discards fragments whose alpha equals the GREATER reference',
    d3d9FixedResult.rejected?.every((pixel) => near(pixel, [0, 0, 0, 255], 1)),
    JSON.stringify(d3d9FixedResult.rejected));
  check('D3D9 alpha test allows fragments above the GREATER reference across the triangle fan',
    near(d3d9FixedResult.upperRight, [255, 0, 0, 255], 1)
      && near(d3d9FixedResult.lowerLeft, [255, 0, 0, 255], 1)
      && near(d3d9FixedResult.background, [0, 0, 0, 255], 1),
    JSON.stringify(d3d9FixedResult));
  const alphaFunctionPasses = { 1: false, 2: false, 3: true, 4: true, 6: false, 7: true, 8: true };
  check('D3D9 alpha compare functions preserve their equality boundary',
    Object.entries(alphaFunctionPasses).every(([alphaFunc, passes]) =>
      near(d3d9FixedResult.alphaFunctionPixels?.[alphaFunc], passes ? [255, 0, 0, 255] : [0, 0, 0, 255], 1)),
    JSON.stringify(d3d9FixedResult.alphaFunctionPixels));
  check('D3D9 SRCALPHA/INVSRCALPHA blends half-alpha red over blue',
    near(d3d9FixedResult.blendedPixel, [128, 0, 127, 191], 1), JSON.stringify(d3d9FixedResult.blendedPixel));

  const d3d9TextureResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(21, words(1, 2, 0));
      command(4, words(13, 2, 2, 1, 1, 28, 8, 0, 1, 0)); // 2x2 RGBA8 D3D9 texture
      const texel = new Uint8Array(52);
      texel.set(words(13, 0, 0, 0, 0, 2, 2, 8, 16));
      texel.set([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255], 36);
      command(5, texel);
      command(12, words(14, 2, 13, 28, 4, 0, 1, 0, 1)); // texture SRV
      command(19, words(1, 0, 1, 14));
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 15, true); samplerView.setUint32(4, 0x15, true); // linear min/mag/mip
      samplerView.setUint32(8, 1, true); samplerView.setUint32(12, 1, true); samplerView.setUint32(16, 1, true);
      samplerView.setUint32(24, 1, true); samplerView.setUint32(28, 8, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 0, true);
      command(8, sampler); command(20, words(1, 0, 1, 15));
      command(45, words(0x144, 64, 64, 2, 2, 1, 2, 2, 1)); // stage 0 selects texture color and alpha
      command(2, words(11, 84, 1, 0, 0, 28));
      const vertices = new Uint8Array(84), view = new DataView(vertices.buffer);
      [[8, 8, 0.5, 0.5], [56, 8, 0.5, 0.5], [32, 56, 0.5, 0.5]].forEach(([x, y, u, v], i) => {
        const offset = i * 28;
        view.setFloat32(offset, x, true); view.setFloat32(offset + 4, y, true);
        view.setFloat32(offset + 8, 0.5, true); view.setFloat32(offset + 12, 1, true);
        view.setUint32(offset + 16, 0x80ff0000, true); // half-alpha red D3DCOLOR
        view.setFloat32(offset + 20, u, true); view.setFloat32(offset + 24, v, true);
      });
      const upload = new Uint8Array(12 + vertices.length);
      upload.set(words(11, 0, vertices.length)); upload.set(vertices, 12); command(3, upload);
      command(13, words(0, 1, 11, 28, 0));
      command(16, words(4)); command(29, words(3, 0));
      gpu.execute(append());
      const selectedPixels = await gpu.readBackbuffer(1);
      const selectedAt = (20 * selectedPixels.width + 32) * 4;

      stream.length = 0;
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 0, true);
      clearView.setFloat32(12, 0, true); clearView.setFloat32(16, 1, true);
      command(27, clear);
      command(45, words(0x144, 64, 64, 4, 2, 1, 2, 2, 1)); // MODULATE texture RGB with diffuse color
      command(16, words(4)); command(29, words(3, 0));
      gpu.execute(append());
      const modulatedPixels = await gpu.readBackbuffer(1);
      const modulatedAt = (20 * modulatedPixels.width + 32) * 4;
      const result = {
        selectedPixel: Array.from(selectedPixels.data.slice(selectedAt, selectedAt + 4)),
        modulatedPixel: Array.from(modulatedPixels.data.slice(modulatedAt, modulatedAt + 4)),
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D9 stage-0 SELECTARG1 samples texture RGB and texture alpha',
    d3d9TextureResult.draws === 2 && d3d9TextureResult.errors.length === 0
      && near(d3d9TextureResult.selectedPixel, [127, 127, 127, 255], 2),
    JSON.stringify(d3d9TextureResult));
  check('D3D9 stage-0 MODULATE multiplies texture RGB by diffuse vertex color',
    near(d3d9TextureResult.modulatedPixel, [127, 0, 0, 255], 2), JSON.stringify(d3d9TextureResult));

  const discardResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        float4 main(float4 position : SV_Position) : SV_Target {
          clip(32.0 - position.x);
          return float4(1.0, 0.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(7, words(10, 0)); // vertex shader uses SV_VertexID only
      shader(6, 0, vertex); shader(7, 1, pixel);
      command(15, words(10));
      command(17, words(0, 6)); command(17, words(1, 7));
      command(16, words(4)); command(21, words(1, 2, 0));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 0, true);
      clearView.setFloat32(12, 0, true); clearView.setFloat32(16, 1, true);
      command(27, clear);
      command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { kept: at(16, 32), discarded: at(48, 32), stats: gpu.stats };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('DXBC clip/discard pipeline validates and draws without WebGPU errors',
    discardResult.stats?.draws === 1 && discardResult.stats.errors.length === 0,
    JSON.stringify(discardResult));
  check('DXBC clip preserves fragments below the threshold and discards the rest',
    near(discardResult.kept, [255, 0, 0, 255], 1) && near(discardResult.discarded, [0, 0, 0, 255], 1),
    JSON.stringify(discardResult));

  const computeResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const messages = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (m) => messages.push(m), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        RWTexture2D<float> dst : register(u0);
        [numthreads(1, 1, 1)] void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = 0.25; }
      `, 'main', 'cs_5_0');
      const vertex = tool.compileHlsl(`
        struct V { float2 position : POSITION; };
        float4 main(V v) : SV_Position { return float4(v.position, 0.0, 1.0); }
      `, 'main', 'vs_5_0');
      const pixelShader = tool.compileHlsl(`
        Texture2D<float> src : register(t0);
        float4 main(float4 position : SV_Position) : SV_Target {
          return float4(src.Load(int3(0, 0, 0)), 0.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');

      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28)); // swap chain and back buffer
      command(4, words(3, 4, 4, 1, 1, 41, 0x88, 0, 1, 0)); // R32_FLOAT SRV + UAV texture
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1)); // back-buffer RTV
      command(12, words(4, 3, 3, 41, 4, 0, 1, 0, 1)); // UAV
      command(12, words(5, 2, 3, 41, 4, 0, 1, 0, 1)); // SRV
      command(2, words(11, 24, 1, 0, 0, 8));
      const vertexBytes = new Uint8Array(24), vertexData = new DataView(vertexBytes.buffer);
      [-1, -1, -1, 3, 3, -1].forEach((value, i) => vertexData.setFloat32(i * 4, value, true));
      const write = new Uint8Array(12 + vertexBytes.length);
      write.set(words(11, 0, vertexBytes.length)); write.set(vertexBytes, 12); command(3, write);
      shader(6, 2, compute); shader(7, 0, vertex); shader(8, 1, pixelShader);
      command(7, words(10, 1, 0, 16, 0, 0, 0, 0)); // POSITION -> R32G32_FLOAT
      command(17, words(2, 6)); command(17, words(0, 7)); command(17, words(1, 8));
      command(43, words(0, 1, 4, 0xffffffff)); // CS UAV binding
      command(42, words(4, 4, 1)); // compute writes all 16 texels
      command(21, words(1, 2, 0)); command(15, words(10)); command(16, words(4));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 1, true);
      clearView.setFloat32(12, 0, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(13, words(0, 1, 11, 8, 0));
      command(19, words(1, 0, 1, 5));
      command(29, words(3, 0)); command(35, words(1, 9));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const observedPixel = Array.from(pixels.data.slice(i, i + 4));
      const readback = await gpu.readBackTexture(4); // read the texture through its D3D11 UAV view
      const uavValues = Array.from(readback.data);
      const uavCenter = uavValues[2 * readback.width + 2];
      const result = {
        pixel: observedPixel,
        uavCenter,
        uav: { width: readback.width, height: readback.height, format: readback.format, values: uavValues },
        stats: gpu.stats,
        errors: messages,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message, errors: messages };
    }
  });
  check('WebGPU executes one R32_FLOAT compute dispatch with no validation errors',
    computeResult.stats?.dispatches === 1 && computeResult.stats.errors.length === 0,
    JSON.stringify(computeResult));
  check('compute-written UAV texture is visible to the following pixel shader',
    computeResult.pixel?.every((value, i) => Math.abs(value - [64, 0, 0, 255][i]) <= 1),
    JSON.stringify({ pixel: computeResult.pixel, uavCenter: computeResult.uavCenter, error: computeResult.error }));
  check('R32_FLOAT UAV contains the compute shader value', Math.abs(computeResult.uavCenter - 0.25) < 0.0001,
    JSON.stringify(computeResult.uavCenter));
  check('R32_FLOAT texture readback returns all 16 UAV values',
    computeResult.uav?.width === 4 && computeResult.uav.height === 4 && computeResult.uav.format === 'r32float'
      && computeResult.uav.values.length === 16 && computeResult.uav.values.every((value) => Math.abs(value - 0.25) < 0.0001),
    JSON.stringify(computeResult.uav));

  const typedTextureUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const logs = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        RWTexture2D<uint> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = id.x + id.y * 4; }
      `, 'main', 'cs_5_0');
      const signedCompute = tool.compileHlsl(`
        RWTexture2D<int> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = int(id.x + id.y * 4) - 8; }
      `, 'main', 'cs_5_0');
      const rgbaCompute = tool.compileHlsl(`
        RWTexture2D<float4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'cs_5_0');
      const rgba32Compute = tool.compileHlsl(`
        RWTexture2D<float4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'cs_5_0');
      const rgba16Compute = tool.compileHlsl(`
        RWTexture2D<float4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'cs_5_0');
      const rgbaUintCompute = tool.compileHlsl(`
        RWTexture2D<uint4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = uint4(1, 2, 3, 255); }
      `, 'main', 'cs_5_0');
      const rgbaSintCompute = tool.compileHlsl(`
        RWTexture2D<int4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = int4(-1, -2, 3, 4); }
      `, 'main', 'cs_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, dxbc) => {
        const head = words(id, 2, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(4, words(3, 4, 4, 1, 1, 42, 0x88, 0, 1, 0)); // R32_UINT SRV + UAV texture
      command(12, words(4, 3, 3, 42, 4, 0, 1, 0, 1)); // R32_UINT UAV
      shader(6, compute);
      command(17, words(2, 6));
      command(43, words(0, 1, 4, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(13, 4, 4, 1, 1, 43, 0x88, 0, 1, 0)); // R32_SINT SRV + UAV texture
      command(12, words(14, 3, 13, 43, 4, 0, 1, 0, 1)); // R32_SINT UAV
      shader(7, signedCompute);
      command(17, words(2, 7));
      command(43, words(0, 1, 14, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(23, 4, 4, 1, 1, 28, 0x88, 0, 1, 0)); // RGBA8_UNORM SRV + UAV texture
      command(12, words(24, 3, 23, 28, 4, 0, 1, 0, 1)); // RGBA8_UNORM UAV
      shader(8, rgbaCompute);
      command(17, words(2, 8));
      command(43, words(0, 1, 24, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(33, 4, 4, 1, 1, 2, 0x88, 0, 1, 0)); // RGBA32_FLOAT SRV + UAV texture
      command(12, words(34, 3, 33, 2, 4, 0, 1, 0, 1)); // RGBA32_FLOAT UAV
      shader(9, rgba32Compute);
      command(17, words(2, 9));
      command(43, words(0, 1, 34, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(43, 4, 4, 1, 1, 10, 0x88, 0, 1, 0)); // RGBA16_FLOAT SRV + UAV texture
      command(12, words(44, 3, 43, 10, 4, 0, 1, 0, 1)); // RGBA16_FLOAT UAV
      shader(10, rgba16Compute);
      command(17, words(2, 10));
      command(43, words(0, 1, 44, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(53, 4, 4, 1, 1, 30, 0x88, 0, 1, 0)); // RGBA8_UINT SRV + UAV texture
      command(12, words(54, 3, 53, 30, 4, 0, 1, 0, 1)); // RGBA8_UINT UAV
      shader(11, rgbaUintCompute);
      command(17, words(2, 11));
      command(43, words(0, 1, 54, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(63, 4, 4, 1, 1, 32, 0x88, 0, 1, 0)); // RGBA8_SINT SRV + UAV texture
      command(12, words(64, 3, 63, 32, 4, 0, 1, 0, 1)); // RGBA8_SINT UAV
      shader(12, rgbaSintCompute);
      command(17, words(2, 12));
      command(43, words(0, 1, 64, 0xffffffff));
      command(42, words(4, 4, 1));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);
      const readback = await gpu.readBackTexture(4);
      const signedReadback = await gpu.readBackTexture(14);
      const rgbaReadback = await gpu.readBackTexture(24);
      const rgba32Readback = await gpu.readBackTexture(34);
      const rgba16Readback = await gpu.readBackTexture(44);
      const rgbaUintReadback = await gpu.readBackTexture(54);
      const rgbaSintReadback = await gpu.readBackTexture(64);
      const result = {
        width: readback.width,
        height: readback.height,
        format: readback.format,
        values: Array.from(readback.data),
        signedFormat: signedReadback.format,
        signedValues: Array.from(signedReadback.data),
        rgbaFormat: rgbaReadback.format,
        rgbaValues: Array.from(rgbaReadback.data),
        rgba32Format: rgba32Readback.format,
        rgba32Values: Array.from(rgba32Readback.data),
        rgba16Format: rgba16Readback.format,
        rgba16Values: Array.from(rgba16Readback.data),
        rgbaUintFormat: rgbaUintReadback.format,
        rgbaUintValues: Array.from(rgbaUintReadback.data),
        rgbaSintFormat: rgbaSintReadback.format,
        rgbaSintValues: Array.from(rgbaSintReadback.data),
        dispatches: gpu.stats.dispatches,
        errors: gpu.stats.errors,
        logs,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: String(error), logs };
    }
  });
  check('typed R32 and RGBA UAV dispatches and readback preserve values',
    typedTextureUavResult.width === 4 && typedTextureUavResult.height === 4
      && typedTextureUavResult.format === 'r32uint'
      && typedTextureUavResult.dispatches === 7 && typedTextureUavResult.errors.length === 0
      && typedTextureUavResult.values.every((value, i) => value === i)
      && typedTextureUavResult.signedFormat === 'r32sint'
      && typedTextureUavResult.signedValues.every((value, i) => value === i - 8)
      && typedTextureUavResult.rgbaFormat === 'rgba8unorm'
      && typedTextureUavResult.rgbaValues.every((value, i) => Math.abs(value - [64, 128, 191, 255][i % 4]) <= 1)
      && typedTextureUavResult.rgba32Format === 'rgba32float'
      && typedTextureUavResult.rgba32Values.length === 64
      && typedTextureUavResult.rgba32Values.every((value, i) => value === [0.25, 0.5, 0.75, 1.0][i % 4])
      && typedTextureUavResult.rgba16Format === 'rgba16float'
      && typedTextureUavResult.rgba16Values.length === 64
      && typedTextureUavResult.rgba16Values.every((value, i) => value === [0.25, 0.5, 0.75, 1.0][i % 4])
      && typedTextureUavResult.rgbaUintFormat === 'rgba8uint'
      && typedTextureUavResult.rgbaUintValues.every((value, i) => value === [1, 2, 3, 255][i % 4])
      && typedTextureUavResult.rgbaSintFormat === 'rgba8sint'
      && typedTextureUavResult.rgbaSintValues.every((value, i) => value === [-1, -2, 3, 4][i % 4]),
    JSON.stringify(typedTextureUavResult));

  const structuredBufferUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const logs = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        StructuredBuffer<uint> inputData : register(t0);
        RWStructuredBuffer<uint> output : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { output[id.x] = inputData[id.x] + 1; }
      `, 'main', 'cs_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, dxbc) => {
        const head = words(id, 2, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const input = Uint32Array.from({ length: 16 }, (_, i) => 100 + i);
      const initial = new Uint32Array(16).fill(0xfeed1234);
      command(2, words(11, input.byteLength, 0x8, 0, 0x40, 4)); // structured SRV buffer
      const inputWrite = new Uint8Array(12 + input.byteLength);
      inputWrite.set(words(11, 0, input.byteLength)); inputWrite.set(new Uint8Array(input.buffer), 12); command(3, inputWrite);
      const copied = new Uint32Array(16).fill(0xdeadbeef);
      command(2, words(17, copied.byteLength, 0x8, 0, 0x40, 4)); // structured CopyResource destination
      const copiedWrite = new Uint8Array(12 + copied.byteLength);
      copiedWrite.set(words(17, 0, copied.byteLength)); copiedWrite.set(new Uint8Array(copied.buffer), 12); command(3, copiedWrite);
      command(33, words(17, 11)); // structured-buffer CopyResource destination <- source
      command(12, words(12, 2, 17, 0, 1, 1, 3, 0, 0)); // copied buffer SRV, elements [1, 4)
      command(2, words(13, initial.byteLength, 0x80, 0, 0x40, 4)); // structured UAV buffer
      const outputWrite = new Uint8Array(12 + initial.byteLength);
      outputWrite.set(words(13, 0, initial.byteLength)); outputWrite.set(new Uint8Array(initial.buffer), 12); command(3, outputWrite);
      command(12, words(14, 3, 13, 0, 1, 2, 3, 0, 0)); // buffer UAV, elements [2, 5)
      shader(15, compute);
      command(19, words(2, 0, 1, 12));
      command(17, words(2, 15));
      command(43, words(0, 1, 14, 0xffffffff));
      command(42, words(3, 1, 1));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);

      const resource = gpu.res.get(13);
      const readback = gpu.device.createBuffer({ size: initial.byteLength, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
      const encoder = gpu.device.createCommandEncoder();
      encoder.copyBufferToBuffer(resource.gpu, 0, readback, 0, initial.byteLength);
      gpu.device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);
      const values = Array.from(new Uint32Array(readback.getMappedRange().slice(0)));
      readback.unmap(); readback.destroy(); gpu.device.destroy();
      return { values, expected: [input[1] + 1, input[2] + 1, input[3] + 1], dispatches: gpu.stats.dispatches, errors: gpu.stats.errors, logs };
    } catch (error) {
      return { error: String(error), logs };
    }
  });
  const structuredValuesMatchViews = structuredBufferUavResult.values?.every((value, i) =>
    (i < 2 || i >= 5) ? value === 0xfeed1234 : value === structuredBufferUavResult.expected?.[i - 2]);
  check('D3D11 structured-buffer CopyResource feeds an SRV/UAV dispatch with FirstElement and NumElements',
    structuredBufferUavResult.dispatches === 1 && structuredBufferUavResult.errors.length === 0
      && structuredValuesMatchViews,
    JSON.stringify(structuredBufferUavResult));

  const rawBufferUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const logs = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        RWByteAddressBuffer output : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { output.Store(id.x * 4, 123); }
      `, 'main', 'cs_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, dxbc) => {
        const head = words(id, 2, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const initial = new Uint32Array(16).fill(0xfeed1234);
      command(2, words(23, initial.byteLength, 0x80, 0, 0x20, 0)); // raw UAV buffer
      const write = new Uint8Array(12 + initial.byteLength);
      write.set(words(23, 0, initial.byteLength)); write.set(new Uint8Array(initial.buffer), 12); command(3, write);
      command(12, words(24, 3, 23, 39, 1, 2, 3, 1, 0)); // R32_TYPELESS raw view, elements [2, 5)
      shader(25, compute);
      command(17, words(2, 25));
      command(43, words(0, 1, 24, 0xffffffff));
      command(42, words(3, 1, 1));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);

      const resource = gpu.res.get(23);
      const readback = gpu.device.createBuffer({ size: initial.byteLength, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
      const encoder = gpu.device.createCommandEncoder();
      encoder.copyBufferToBuffer(resource.gpu, 0, readback, 0, initial.byteLength);
      gpu.device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);
      const values = Array.from(new Uint32Array(readback.getMappedRange().slice(0)));
      readback.unmap(); readback.destroy(); gpu.device.destroy();
      return { values, dispatches: gpu.stats.dispatches, errors: gpu.stats.errors, logs };
    } catch (error) {
      return { error: String(error), logs };
    }
  });
  const sentinelsOutsideRawView = rawBufferUavResult.values?.every((value, i) =>
    (i < 2 || i >= 5) ? value === 0xfeed1234 : value === 123);
  check('D3D11 raw-buffer UAV Store honors FirstElement and NumElements',
    rawBufferUavResult.dispatches === 1 && rawBufferUavResult.errors.length === 0 && sentinelsOutsideRawView,
    JSON.stringify(rawBufferUavResult));

  const typedBufferResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const logs = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        Buffer<uint> inputData : register(t0);
        RWBuffer<uint> outputData : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { outputData[id.x] = inputData[id.x] + 1; }
      `, 'main', 'cs_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, dxbc) => {
        const head = words(id, 2, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const input = Uint32Array.from({ length: 16 }, (_, i) => 200 + i);
      const initial = new Uint32Array(16).fill(0xfeed5678);
      command(2, words(33, input.byteLength, 0x8, 0, 0, 0));
      const inputWrite = new Uint8Array(12 + input.byteLength);
      inputWrite.set(words(33, 0, input.byteLength)); inputWrite.set(new Uint8Array(input.buffer), 12); command(3, inputWrite);
      command(12, words(34, 2, 33, 42, 1, 1, 3, 0, 0)); // R32_UINT SRV, elements [1, 4)
      command(2, words(35, initial.byteLength, 0x80, 0, 0, 0));
      const outputWrite = new Uint8Array(12 + initial.byteLength);
      outputWrite.set(words(35, 0, initial.byteLength)); outputWrite.set(new Uint8Array(initial.buffer), 12); command(3, outputWrite);
      command(12, words(36, 3, 35, 42, 1, 2, 3, 0, 0)); // R32_UINT UAV, elements [2, 5)
      shader(37, compute);
      command(19, words(2, 0, 1, 34));
      command(17, words(2, 37));
      command(43, words(0, 1, 36, 0xffffffff));
      command(42, words(3, 1, 1));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);

      const resource = gpu.res.get(35);
      const readback = gpu.device.createBuffer({ size: initial.byteLength, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
      const encoder = gpu.device.createCommandEncoder();
      encoder.copyBufferToBuffer(resource.gpu, 0, readback, 0, initial.byteLength);
      gpu.device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);
      const values = Array.from(new Uint32Array(readback.getMappedRange().slice(0)));
      readback.unmap(); readback.destroy(); gpu.device.destroy();
      return { values, expected: [input[1] + 1, input[2] + 1, input[3] + 1], dispatches: gpu.stats.dispatches, errors: gpu.stats.errors, logs };
    } catch (error) {
      return { error: String(error), logs };
    }
  });
  const typedValuesMatchViews = typedBufferResult.values?.every((value, i) =>
    (i < 2 || i >= 5) ? value === 0xfeed5678 : value === typedBufferResult.expected?.[i - 2]);
  check('D3D11 typed-buffer SRV/UAV dispatch honors formats and element ranges',
    typedBufferResult.dispatches === 1 && typedBufferResult.errors.length === 0 && typedValuesMatchViews,
    JSON.stringify(typedBufferResult));

  const drawParamsResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const messages = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (m) => messages.push(m), { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID, uint instanceId : SV_InstanceID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x + float(instanceId) * 4.0, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const secondPixel = tool.compileHlsl('float4 main() : SV_Target { return float4(0.0, 1.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(2, words(11, 6, 1, 0, 0, 0));
      command(3, new Uint8Array([...words(11, 0, 6), 0, 0, 1, 0, 2, 0]));
      shader(7, 0, vertex); shader(8, 1, pixel); shader(9, 1, secondPixel);
      command(14, words(11, 57, 0));
      command(17, words(0, 7)); command(17, words(1, 8));
      command(16, words(4)); command(21, words(1, 2, 0));
      command(32, words(3, 1, 0, 1, 2)); // baseVertex=1, firstInstance=2
      command(17, words(1, 9));
      command(32, words(3, 1, 0, 3, 4)); // distinct per-draw baseVertex and firstInstance
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), stats: gpu.stats, errors: messages };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message, errors: messages };
    }
  });
  check('draw-parameter render pipeline validates and draws with nonzero indexed offsets',
    drawParamsResult.stats?.draws === 2 && drawParamsResult.stats.errors.length === 0,
    JSON.stringify(drawParamsResult));
  check('SV_VertexID and SV_InstanceID use distinct base offsets for each draw',
    drawParamsResult.pixel?.every((value, i) => Math.abs(value - [0, 255, 0, 255][i]) <= 1),
    JSON.stringify(drawParamsResult));

  const instanceStepResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(float2 position : POSITION, float2 offset : TEXCOORD0) : SV_Position {
          return float4(position + offset, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        float4 main(float4 position : SV_Position) : SV_Target {
          return position.x < 32.0 ? float4(1.0, 0.0, 0.0, 1.0) : float4(0.0, 1.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      const bufferWrite = (id, bytes) => {
        const payload = new Uint8Array(12 + bytes.length);
        payload.set(words(id, 0, bytes.length)); payload.set(bytes, 12); command(3, payload);
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(2, words(11, 24, 1, 0, 0, 8));
      command(2, words(12, 16, 1, 0, 0, 8));
      const vertices = new Uint8Array(24), vertexData = new DataView(vertices.buffer);
      [[-0.22, -0.4], [-0.22, 0.4], [0.22, 0]].forEach(([x, y], i) => {
        vertexData.setFloat32(i * 8, x, true); vertexData.setFloat32(i * 8 + 4, y, true);
      });
      bufferWrite(11, vertices);
      const instances = new Uint8Array(16), instanceData = new DataView(instances.buffer);
      instanceData.setFloat32(0, -0.48, true); instanceData.setFloat32(4, 0, true);
      instanceData.setFloat32(8, 0.48, true); instanceData.setFloat32(12, 0, true);
      bufferWrite(12, instances);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(7, words(10, 2,
        0, 16, 0, 0, 0, 0,
        1, 16, 1, 0, 1, 2)); // step-rate 2 repeats each instance record twice
      command(13, words(0, 2, 11, 8, 0, 12, 8, 0));
      command(15, words(10)); command(16, words(4));
      command(17, words(0, 7)); command(17, words(1, 8));
      command(21, words(1, 2, 0));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(31, words(3, 2, 0, 0));
      command(31, words(3, 2, 0, 2));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { left: at(16, 32), right: at(48, 32), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 instance data step rate repeats records for the requested instance count',
    instanceStepResult.draws === 2 && instanceStepResult.errors.length === 0
      && near(instanceStepResult.left, [255, 0, 0, 255], 1)
      && near(instanceStepResult.right, [0, 255, 0, 255], 1),
    JSON.stringify(instanceStepResult));

  const scissorResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 10, true); // id
      rasterView.setUint32(4, 3, true); // D3D11_FILL_SOLID
      rasterView.setUint32(8, 1, true); // D3D11_CULL_NONE
      rasterView.setUint32(28, 1, true); // DepthClipEnable
      rasterView.setUint32(32, 1, true); // ScissorEnable
      command(10, raster);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(16, words(4)); command(21, words(1, 2, 0)); command(25, words(10));
      command(23, words(1, 16, 16, 48, 48));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { inside: at(32, 32), outside: at(8, 32), errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 rasterizer scissor state limits fragments to the requested rectangle',
    scissorResult.errors.length === 0 && near(scissorResult.inside, [255, 0, 0, 255], 1)
      && near(scissorResult.outside, [0, 0, 0, 255], 1),
    JSON.stringify(scissorResult));

  const cullResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const red = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const green = tool.compileHlsl('float4 main() : SV_Target { return float4(0.0, 1.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      const raster = (id, cullMode) => {
        const payload = new Uint8Array(44), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setUint32(4, 3, true); // solid fill
        view.setUint32(8, cullMode, true); view.setUint32(12, 0, true); // clockwise front faces
        view.setUint32(28, 1, true); command(10, payload);
      };
      raster(13, 3); // cull back faces; the fullscreen triangle is clockwise
      raster(14, 2); // cull front faces
      shader(7, 0, vertex); shader(8, 1, red); shader(9, 1, green);
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(25, words(13));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(17, words(0, 7)); command(17, words(1, 8)); command(29, words(3, 0));
      command(25, words(14)); command(17, words(1, 9)); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 front-face winding and cull mode discard front faces while keeping back-cull output',
    cullResult.draws === 2 && cullResult.errors.length === 0
      && near(cullResult.pixel, [255, 0, 0, 255], 1),
    JSON.stringify(cullResult));

  const wireframeResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          uint local = vertexId % 3;
          float center = vertexId < 3 ? -0.5 : 0.5;
          float x = local == 0 ? center - 0.4 : local == 1 ? center + 0.4 : center;
          float y = local == 2 ? 0.65 : -0.75;
          float2 trianglePosition = float2(x, y);
          float2 stripPosition = vertexId == 6 ? float2(-0.9, 0.75)
            : vertexId == 7 ? float2(-0.1, 0.75)
            : vertexId == 8 ? float2(-0.9, -0.75) : float2(-0.1, -0.75);
          float2 p = vertexId >= 6 ? stripPosition : trianglePosition;
          return float4(p, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(2, words(11, 12, 0, 0x40, 0, 0, 0));
      command(2, words(12, 12, 0, 0x40, 0, 0, 0));
      const indices = new Uint8Array(12), indexData = new DataView(indices.buffer);
      [0, 1, 2].forEach((value, i) => indexData.setUint32(i * 4, value, true));
      const indexWrite = new Uint8Array(12 + indices.length);
      indexWrite.set(words(11, 0, indices.length)); indexWrite.set(indices, 12); command(3, indexWrite);
      command(33, words(12, 11));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 6, true); rasterView.setUint32(4, 2, true); rasterView.setUint32(8, 1, true);
      command(10, raster); command(25, words(6)); // wireframe, no culling
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(21, words(1, 2, 0)); command(16, words(4));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(29, words(3, 0));
      command(14, words(12, 42, 0)); command(30, words(3, 0, 3));
      command(16, words(5)); command(29, words(4, 6));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const samplePixel = (x, y) => Array.from(pixels.data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
      const isRed = (color) => color[0] > 250 && color[1] < 2 && color[2] < 2 && color[3] > 250;
      let leftEdgeRed = 0, rightEdgeRed = 0, stripEdgeRed = 0;
      for (let y = 54; y <= 58; y++) {
        for (let x = 14; x <= 18; x++) if (isRed(samplePixel(x, y))) leftEdgeRed++;
        for (let x = 46; x <= 50; x++) if (isRed(samplePixel(x, y))) rightEdgeRed++;
      }
      for (let y = 6; y <= 10; y++) for (let x = 14; x <= 18; x++) if (isRed(samplePixel(x, y))) stripEdgeRed++;
      const result = {
        leftEdgeRed, rightEdgeRed, stripEdgeRed,
        leftInterior: samplePixel(16, 34), rightInterior: samplePixel(48, 34), stripInterior: samplePixel(8, 32),
        stripDiagonal: samplePixel(15, 32), draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 wireframe fill draws triangle edges and leaves the interior clear',
    wireframeResult.leftEdgeRed >= 3 && wireframeResult.rightEdgeRed >= 3 && wireframeResult.stripEdgeRed >= 3
      && near(wireframeResult.leftInterior, [0, 0, 0, 255], 1) && near(wireframeResult.rightInterior, [0, 0, 0, 255], 1)
      && near(wireframeResult.stripInterior, [0, 0, 0, 255], 1) && near(wireframeResult.stripDiagonal, [255, 0, 0, 255], 1)
      && wireframeResult.draws === 3 && wireframeResult.errors.length === 0,
    JSON.stringify(wireframeResult));

  const depthBiasResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.5, 1.0);
        }
      `, 'main', 'vs_5_0');
      const red = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const green = tool.compileHlsl('float4 main() : SV_Target { return float4(0.0, 1.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const blue = tool.compileHlsl('float4 main() : SV_Target { return float4(0.0, 0.0, 1.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 64, 64, 1, 1, 39, 0x40, 0, 1, 0)); // depth32float texture
      command(12, words(4, 1, 3, 39, 3, 0, 1, 0, 1)); // DSV
      const raster = (id, bias) => {
        const payload = new Uint8Array(44), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setUint32(4, 3, true); view.setUint32(8, 1, true);
        view.setInt32(16, bias, true); view.setUint32(28, 1, true); view.setUint32(32, 1, true);
        command(10, payload);
      };
      raster(13, 0); raster(14, 100000);
      const depth = new Uint8Array(56), depthView = new DataView(depth.buffer);
      depthView.setUint32(0, 15, true); depthView.setUint32(4, 1, true);
      depthView.setUint32(8, 1, true); depthView.setUint32(12, 4, true); // depth enabled, write all, LESS_EQUAL
      command(11, depth);
      shader(7, 0, vertex); shader(8, 1, red); shader(9, 1, green); shader(10, 1, blue);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(11, 0)); command(15, words(11)); command(16, words(4));
      command(21, words(1, 2, 4)); command(25, words(13)); command(26, words(15, 0));
      const clearColor = new Uint8Array(20), colorView = new DataView(clearColor.buffer);
      colorView.setUint32(0, 2, true); colorView.setFloat32(16, 1, true); command(27, clearColor);
      const clearDepth = new Uint8Array(16), depthClearView = new DataView(clearDepth.buffer);
      depthClearView.setUint32(0, 4, true); depthClearView.setUint32(4, 1, true);
      depthClearView.setFloat32(8, 1, true); command(28, clearDepth);
      command(23, words(1, 0, 0, 64, 64)); command(29, words(3, 0));
      command(17, words(1, 9)); command(23, words(1, 0, 0, 32, 64)); command(29, words(3, 0));
      command(17, words(1, 10)); command(25, words(14)); command(23, words(1, 32, 0, 64, 64)); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x) => {
        const i = (32 * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { unbias: at(16), biased: at(48), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 constant depth bias moves an equal-depth draw behind the stored surface',
    depthBiasResult.draws === 3 && depthBiasResult.errors.length === 0
      && near(depthBiasResult.unbias, [0, 255, 0, 255], 1)
      && near(depthBiasResult.biased, [255, 0, 0, 255], 1),
    JSON.stringify(depthBiasResult));

  const stencilResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const red = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const green = tool.compileHlsl('float4 main() : SV_Target { return float4(0.0, 1.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3;
        const data = new Uint8Array(8 + padded), view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 64, 64, 1, 1, 45, 0x40, 0, 1, 0)); // D24S8 depth-stencil texture
      command(12, words(4, 1, 3, 45, 3, 0, 1, 0, 1)); // DSV
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 12, true);
      rasterView.setUint32(4, 3, true); // solid fill
      rasterView.setUint32(8, 1, true); // no culling
      rasterView.setUint32(28, 1, true); // depth clip enabled
      rasterView.setUint32(32, 1, true); // scissor enabled
      command(10, raster);
      const stencilState = (id, writeMask, compare, passOp) => {
        const payload = new Uint8Array(56), view = new DataView(payload.buffer);
        view.setUint32(0, id, true);
        view.setUint32(4, 0, true); // depth disabled
        view.setUint32(8, 0, true);
        view.setUint32(12, 8, true); // depth compare: always
        view.setUint32(16, 1, true); // stencil enabled
        view.setUint8(20, 0xff); view.setUint8(21, writeMask);
        for (const face of [24, 40]) {
          view.setUint32(face, 1, true); // fail: keep
          view.setUint32(face + 4, 1, true); // depth fail: keep
          view.setUint32(face + 8, passOp, true);
          view.setUint32(face + 12, compare, true);
        }
        command(11, payload);
      };
      stencilState(13, 0xff, 8, 3); // always pass and write reference value
      stencilState(14, 0, 3, 1); // draw only where stored stencil equals reference
      shader(7, 0, vertex); shader(8, 1, red); shader(9, 1, green);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 4)); command(25, words(12));
      const clearColor = new Uint8Array(20), clearColorView = new DataView(clearColor.buffer);
      clearColorView.setUint32(0, 2, true); clearColorView.setFloat32(16, 1, true); command(27, clearColor);
      const clearDepth = new Uint8Array(16), clearDepthView = new DataView(clearDepth.buffer);
      clearDepthView.setUint32(0, 4, true); clearDepthView.setUint32(4, 3, true);
      clearDepthView.setFloat32(8, 1, true); command(28, clearDepth);
      command(26, words(13, 1)); command(23, words(1, 0, 0, 32, 64)); command(29, words(3, 0));
      command(17, words(1, 9)); command(26, words(14, 1));
      command(23, words(1, 0, 0, 64, 64)); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { written: at(16, 32), rejected: at(48, 32), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 stencil replace writes the reference value and equal comparison masks the next draw',
    stencilResult.draws === 2 && stencilResult.errors.length === 0
      && near(stencilResult.written, [0, 255, 0, 255], 1)
      && near(stencilResult.rejected, [0, 0, 0, 255], 1),
    JSON.stringify(stencilResult));

  const reverseBlendResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl('float4 main() : SV_Target { return float4(0.25, 0.5, 0.0, 1.0); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      const blend = new Uint8Array(268), blendView = new DataView(blend.buffer);
      blendView.setUint32(0, 11, true); // state id
      blendView.setUint32(12, 1, true); // RenderTarget[0].BlendEnable
      blendView.setUint32(16, 2, true); // SrcBlend = ONE
      blendView.setUint32(20, 2, true); // DestBlend = ONE
      blendView.setUint32(24, 3, true); // BlendOp = REV_SUBTRACT
      blendView.setUint32(28, 2, true); // SrcBlendAlpha = ONE
      blendView.setUint32(32, 2, true); // DestBlendAlpha = ONE
      blendView.setUint32(36, 3, true); // BlendOpAlpha = REV_SUBTRACT
      blendView.setUint32(40, 0xf, true); // RenderTarget[0].WriteMask
      command(9, blend);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8)); command(16, words(4));
      command(21, words(1, 2, 0)); command(24, words(11, 0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000, 0xffffffff));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(12, 1, true); clearView.setFloat32(16, 1, true);
      command(27, clear); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 reverse-subtract blending computes destination minus source',
    near(reverseBlendResult.pixel, [0, 0, 255, 0], 1) && reverseBlendResult.errors.length === 0,
    JSON.stringify(reverseBlendResult));

  const blendModesResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl('float4 main() : SV_Target { return float4(0.25, 0.5, 0.1, 0.8); }', 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 12, true); rasterView.setUint32(4, 3, true);
      rasterView.setUint32(8, 1, true); rasterView.setUint32(28, 1, true); rasterView.setUint32(32, 1, true);
      command(10, raster);
      const blend = (id, op) => {
        const payload = new Uint8Array(268), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setUint32(12, 1, true);
        view.setUint32(16, 2, true); view.setUint32(20, 2, true); view.setUint32(24, op, true);
        view.setUint32(28, 2, true); view.setUint32(32, 2, true); view.setUint32(36, op, true);
        view.setUint32(40, 0xf, true); command(9, payload);
      };
      blend(13, 1); blend(14, 2); blend(15, 4); blend(16, 5);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(25, words(12));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); [0.5, 0.1, 0.4, 0.3].forEach((v, i) => clearView.setFloat32(4 + i * 4, v, true)); command(27, clear);
      const rects = [[0, 0, 32, 32], [32, 0, 64, 32], [0, 32, 32, 64], [32, 32, 64, 64]];
      for (let i = 0; i < rects.length; i++) {
        const id = 13 + i, [left, top, right, bottom] = rects[i];
        command(24, words(id, 0, 0, 0, 0, 0xf));
        command(23, words(1, left, top, right, bottom));
        command(29, words(3, 0));
      }
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = {
        add: at(16, 16), subtract: at(48, 16), min: at(16, 48), max: at(48, 48),
        draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 ADD, SUBTRACT, MIN, and MAX blend operations match channel-wise pixels',
    blendModesResult.draws === 4 && blendModesResult.errors.length === 0
      && near(blendModesResult.add, [191, 153, 128, 255], 1)
      && near(blendModesResult.subtract, [0, 102, 0, 128], 1)
      && near(blendModesResult.min, [64, 26, 26, 77], 1)
      && near(blendModesResult.max, [128, 128, 102, 204], 1),
    JSON.stringify(blendModesResult));

  const mrtResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        struct Output { float4 first : SV_Target0; float4 second : SV_Target1; };
        Output main() {
          Output output;
          output.first = float4(1.0, 0.0, 0.0, 1.0);
          output.second = float4(0.0, 0.0, 1.0, 1.0);
          return output;
        }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(4, words(3, 64, 64, 1, 1, 28, 0x20, 0, 1, 0));
      command(4, words(4, 64, 64, 1, 1, 87, 0x20, 0, 1, 0));
      command(12, words(5, 0, 3, 28, 4, 0, 1, 0, 1));
      command(12, words(6, 0, 4, 87, 4, 0, 1, 0, 1));
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(16, words(4)); command(21, words(2, 5, 6, 0));
      command(29, words(3, 0));
      gpu.execute(append());
      const [first, second] = await Promise.all([gpu.readBackTexture(3), gpu.readBackTexture(4)]);
      const pixelAt = (image) => {
        const i = (32 * image.width + 32) * 4;
        return Array.from(image.data.slice(i, i + 4));
      };
      const result = { first: pixelAt(first), second: pixelAt(second), errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 multiple render targets write independent pixel outputs',
    !mrtResult.error && mrtResult.errors?.length === 0
      && near(mrtResult.first || [], [255, 0, 0, 255], 1)
      && near(mrtResult.second || [], [0, 0, 255, 255], 1),
    JSON.stringify(mrtResult));

  const renderTextureResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const red = tool.compileHlsl('float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 1.0); }', 'main', 'ps_5_0');
      const sample = tool.compileHlsl(`
        Texture2D<float4> source : register(t0);
        SamplerState sourceSampler : register(s0);
        float4 main() : SV_Target { return source.SampleLevel(sourceSampler, float2(0.5, 0.5), 0.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 64, 64, 1, 1, 28, 0x28, 0, 1, 0)); // texture is both RTV and SRV
      command(12, words(4, 0, 3, 28, 4, 0, 1, 0, 1)); // texture RTV
      command(12, words(5, 2, 3, 28, 4, 0, 1, 0, 1)); // texture SRV
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, red); shader(9, 1, sample);
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      const clear = (id) => {
        const payload = new Uint8Array(20), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setFloat32(16, 1, true); command(27, payload);
      };
      command(21, words(1, 2, 0)); clear(2);
      command(21, words(1, 4, 0)); clear(4);
      command(17, words(0, 7)); command(17, words(1, 8)); command(29, words(3, 0));
      command(21, words(1, 2, 0));
      command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(17, words(1, 9)); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 render target texture can be sampled by a later draw',
    renderTextureResult.draws === 2 && renderTextureResult.errors.length === 0
      && near(renderTextureResult.pixel, [255, 0, 0, 255], 1),
    JSON.stringify(renderTextureResult));

  const textureArrayResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture2DArray<float4> source : register(t0);
        SamplerState sourceSampler : register(s0);
        float4 main() : SV_Target { return source.SampleLevel(sourceSampler, float3(0.5, 0.5, 1.0), 0.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 4, 4, 1, 2, 28, 0x8, 0, 1, 0)); // two-layer RGBA8 SRV texture
      command(12, words(5, 2, 3, 28, 5, 0, 1, 0, 2)); // view both array layers
      const uploadSlice = (slice, rgba) => {
        const texels = new Uint8Array(4 * 4 * 4);
        for (let i = 0; i < 16; i++) texels.set(rgba, i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, 0, slice, 0, 0, 4, 4, 16, texels.length));
        payload.set(texels, 36); command(5, payload);
      };
      uploadSlice(0, [255, 0, 0, 255]); uploadSlice(1, [0, 255, 0, 255]);
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 Texture2DArray SRV samples the selected second layer',
    textureArrayResult.draws === 1 && textureArrayResult.errors.length === 0
      && near(textureArrayResult.pixel, [0, 255, 0, 255], 1),
    JSON.stringify(textureArrayResult));

  const cubeTextureResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        TextureCube<float4> source : register(t0);
        SamplerState sourceSampler : register(s0);
        float4 main() : SV_Target { return source.SampleLevel(sourceSampler, float3(1.0, 0.0, 0.0), 0.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 4, 4, 1, 6, 28, 0x8, 0, 1, 0x4)); // six faces, cube misc flag
      command(12, words(5, 2, 3, 28, 9, 0, 1, 0, 6)); // TextureCube SRV
      const faces = [
        [255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255],
        [255, 255, 0, 255], [255, 0, 255, 255], [0, 255, 255, 255],
      ];
      for (let slice = 0; slice < faces.length; slice++) {
        const texels = new Uint8Array(64);
        for (let i = 0; i < 16; i++) texels.set(faces[slice], i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, 0, slice, 0, 0, 4, 4, 16, texels.length));
        payload.set(texels, 36); command(5, payload);
      }
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 TextureCube SRV selects the positive-X face',
    cubeTextureResult.draws === 1 && cubeTextureResult.errors.length === 0
      && near(cubeTextureResult.pixel, [255, 0, 0, 255], 1),
    JSON.stringify(cubeTextureResult));

  const constantRangeResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        cbuffer Params : register(b0) { float4 tint; };
        float4 main() : SV_Target { return tint; }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      const buffer = (id, colorOffset, color) => {
        command(2, words(id, 512, 4, 0, 0, 16));
        const bytes = new Uint8Array(512), view = new DataView(bytes.buffer);
        color.forEach((value, i) => view.setFloat32(colorOffset + i * 4, value, true));
        const payload = new Uint8Array(12 + bytes.length);
        payload.set(words(id, 0, bytes.length)); payload.set(bytes, 12); command(3, payload);
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      buffer(11, 16, [0.25, 0.5, 0.75, 1]);
      buffer(12, 256, [1, 1, 0, 1]);
      buffer(14, 0, [0, 0, 0, 1]);
      buffer(15, 0, [0.25, 0.75, 0.5, 1]);
      command(33, words(14, 15)); // constant-buffer CopyResource destination <- source
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 13, true); rasterView.setUint32(4, 3, true);
      rasterView.setUint32(8, 1, true); rasterView.setUint32(28, 1, true); rasterView.setUint32(32, 1, true);
      command(10, raster);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(25, words(13));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); clearView.setFloat32(16, 1, true); command(27, clear);
      command(44, words(1, 0, 1, 11, 1, 4)); // firstConstant=1, copied from misaligned offset 16
      command(23, words(1, 0, 0, 32, 64)); command(29, words(3, 0));
      command(44, words(1, 0, 1, 12, 16, 4)); // firstConstant=16, directly bound at offset 256
      command(23, words(1, 32, 0, 64, 64)); command(29, words(3, 0));
      command(44, words(1, 0, 1, 14, 0, 4));
      command(23, words(1, 0, 32, 32, 64)); command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y = 32) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const result = { packed: at(16, 16), aligned: at(48), copied: at(16, 48), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 constant-buffer CopyResource and D3D11.1 ranges preserve bound pixel data',
    constantRangeResult.draws === 3 && constantRangeResult.errors.length === 0
      && near(constantRangeResult.packed, [64, 128, 191, 255], 1)
      && near(constantRangeResult.aligned, [255, 255, 0, 255], 1)
      && near(constantRangeResult.copied, [64, 191, 128, 255], 1),
    JSON.stringify(constantRangeResult));

  const texture3dResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture3D<float4> volume : register(t0);
        SamplerState volumeSampler : register(s0);
        float4 main() : SV_Target { return volume.SampleLevel(volumeSampler, float3(0.5, 0.5, 0.99), 0.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(46, words(3, 4, 4, 2, 1, 28, 0x8, 0, 0)); // 4x4x2 volume texture
      command(46, words(13, 4, 4, 2, 1, 28, 0x8, 0, 0)); // Copy destination
      command(12, words(5, 2, 13, 28, 8, 0, 1, 0, 1)); // Texture3D SRV on the copy
      const uploadSlice = (z, rgba) => {
        const texels = new Uint8Array(64);
        for (let i = 0; i < 16; i++) texels.set(rgba, i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, 0, z, 0, 0, 4, 4, 16, texels.length));
        payload.set(texels, 36); command(5, payload);
      };
      uploadSlice(0, [255, 0, 0, 255]); uploadSlice(1, [0, 255, 0, 255]);
      command(33, words(13, 3)); // CopyResource preserves the volume slices
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 Texture3D CopyResource preserves slices for SRV sampling',
    texture3dResult.draws === 1 && texture3dResult.errors.length === 0
      && near(texture3dResult.pixel, [0, 255, 0, 255], 1),
    JSON.stringify(texture3dResult));

  const texture3dMipsResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture3D<float4> volume : register(t0);
        SamplerState volumeSampler : register(s0);
        float4 main() : SV_Target { return volume.SampleLevel(volumeSampler, float3(0.5, 0.5, 0.5), 2.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(46, words(3, 4, 4, 4, 3, 28, 0x8, 0, 0)); // 4x4x4 volume with full mip chain
      command(12, words(5, 2, 3, 28, 8, 0, 3, 0, 1)); // Texture3D SRV covering all mips
      const uploadSlice = (z, rgba) => {
        const texels = new Uint8Array(64);
        for (let i = 0; i < 16; i++) texels.set(rgba, i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, 0, z, 0, 0, 4, 4, 16, texels.length));
        payload.set(texels, 36); command(5, payload);
      };
      uploadSlice(0, [255, 0, 0, 255]); uploadSlice(1, [255, 0, 0, 255]);
      uploadSlice(2, [0, 255, 0, 255]); uploadSlice(3, [0, 255, 0, 255]);
      command(39, words(5)); // GenerateMips
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, dispatches: gpu.stats.dispatches, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 GenerateMips box-filters a 3D volume through its final mip',
    texture3dMipsResult.draws === 1 && texture3dMipsResult.dispatches === 2 && texture3dMipsResult.errors.length === 0
      && near(texture3dMipsResult.pixel, [128, 128, 0, 255], 2),
    JSON.stringify(texture3dMipsResult));

  const texture3dUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    const gpuLogs = [];
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, (message) => gpuLogs.push(message), { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        RWTexture3D<float> volume : register(u0);
        [numthreads(4, 4, 2)]
        void main(uint3 id : SV_DispatchThreadID) { volume[id] = 0.25; }
      `, 'main', 'cs_5_0');
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture3D<float> volume : register(t0);
        SamplerState volumeSampler : register(s0);
        float4 main() : SV_Target { return float4(volume.SampleLevel(volumeSampler, float3(0.5, 0.5, 0.5), 0.0), 0.0, 0.0, 1.0); }
      `, 'main', 'ps_5_0');
      const stream = [];
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(4 * i, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const padded = (payload.length + 3) & ~3, data = new Uint8Array(8 + padded);
        const view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, padded, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1)); // back-buffer RTV
      command(46, words(3, 4, 4, 2, 1, 41, 0x88, 0, 0)); // 4x4x2 R32_FLOAT SRV + UAV volume
      command(12, words(4, 3, 3, 41, 8, 0, 1, 0, 2)); // 3D UAV covering both W slices
      command(12, words(5, 2, 3, 41, 8, 0, 1, 0, 1)); // 3D SRV
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 10, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      command(2, words(11, 24, 1, 0, 0, 8));
      const vertices = new Uint8Array(24), vertexData = new DataView(vertices.buffer);
      [-1, -1, -1, 3, 3, -1].forEach((value, i) => vertexData.setFloat32(i * 4, value, true));
      const write = new Uint8Array(12 + vertices.length);
      write.set(words(11, 0, vertices.length)); write.set(vertices, 12); command(3, write);
      shader(6, 2, compute); shader(7, 0, vertex); shader(8, 1, pixel);
      command(7, words(12, 1, 0, 16, 0, 0, 0, 0));
      command(17, words(2, 6)); command(17, words(0, 7)); command(17, words(1, 8));
      command(43, words(0, 1, 4, 0xffffffff));
      command(42, words(1, 1, 1));
      command(21, words(1, 2, 0)); command(15, words(12)); command(16, words(4));
      command(13, words(0, 1, 11, 8, 0));
      command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 10));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(i, i + 4)), dispatches: gpu.stats.dispatches, draws: gpu.stats.draws, errors: gpu.stats.errors, logs: gpuLogs };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message, logs: gpuLogs };
    }
  });
  check('D3D11 3D UAV compute writes are visible through a Texture3D SRV',
    texture3dUavResult.dispatches === 1 && texture3dUavResult.draws === 1 && texture3dUavResult.errors.length === 0
      && near(texture3dUavResult.pixel, [64, 0, 0, 255], 1),
    JSON.stringify(texture3dUavResult));

  const caps = await page.textContent('#caps');
  console.log('capabilities:', caps.replace(/\s+/g, ' ').trim());
  const gpu = await page.evaluate(async () => {
    if (!('gpu' in navigator)) return 'navigator.gpu missing';
    try { const a = await navigator.gpu.requestAdapter(); return a ? 'adapter: ' + (a.info?.description || a.info?.vendor || 'ok') : 'no adapter'; } catch (e) { return 'error: ' + e.message; }
  });
  console.log('WebGPU in this headless Chromium:', gpu);
  const real = logs.filter((l) => l.startsWith('PAGEERROR') && !/Instance dropped/.test(l));
  check('no page errors', real.length === 0, real.join('\n'));
  if (logs.some((l) => /\[gpu\]/.test(l))) console.log(logs.filter((l) => /\[gpu\]/.test(l)).join('\n'));
} finally {
  await browser.close();
  server.kill();
  rmSync(tempDir, { recursive: true, force: true });
}
console.log(failures ? `\n${failures} FAILED` : '\nbrowser tests passed');
process.exit(failures ? 1 : 0);
