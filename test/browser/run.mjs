// Drives the real web page in headless Chromium and checks the program output on screen.
import { chromium } from 'playwright-core';
import { inflateSync } from 'node:zlib';

/** Minimal PNG decoder (8-bit RGB/RGBA, non-interlaced) - enough for Playwright screenshots. */
function decodePng(buf) {
  let p = 8, w = 0, h = 0, ct = 0; const idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p), type = buf.toString('ascii', p + 4, p + 8), data = buf.subarray(p + 8, p + 8 + len);
    if (type === 'IHDR') { w = data.readUInt32BE(0); h = data.readUInt32BE(4); ct = data[9]; if (data[8] !== 8 || data[12] !== 0) throw new Error('unsupported PNG'); }
    if (type === 'IDAT') idat.push(data);
    p += 12 + len;
  }
  const bpp = ct === 6 ? 4 : 3, stride = w * bpp, raw = inflateSync(Buffer.concat(idat)), out = Buffer.alloc(w * h * 4);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)], line = Buffer.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? line[x - bpp] : 0, b = prev[x], c = x >= bpp ? prev[x - bpp] : 0;
      let v = line[x];
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) { const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      line[x] = v & 255;
    }
    for (let x = 0; x < w; x++) { out[(y * w + x) * 4] = line[x * bpp]; out[(y * w + x) * 4 + 1] = line[x * bpp + 1]; out[(y * w + x) * 4 + 2] = line[x * bpp + 2]; out[(y * w + x) * 4 + 3] = bpp === 4 ? line[x * bpp + 3] : 255; }
    prev = line;
  }
  return { w, h, data: out };
}


import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-browser-'));
const unimplBundle = join(tempDir, 'unimpl.awb');
const computeBundle = join(tempDir, 'compute.awb');
const systemRandomBundle = join(tempDir, 'system_random.awb');
const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'unimpl.exe'), '-o', unimplBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'compute.exe'), '-o', computeBundle], { cwd: repoRoot, stdio: 'ignore' });
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
      jitBrowser.emitted.jitStats.fallbacks === 0 && jitBrowser.emitted.out.includes('primes<20000=2262') &&
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
      jitBrowser.afterGrowth.jitStats.fallbacks === 0 && jitBrowser.afterGrowth.out === jitBrowser.emitted.out &&
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
  check('Chrome worker executes opt-in emitted blocks without fallback',
    workerJit.status === jitBrowser.exited && workerJit.exitCode === 0 &&
      workerJit.jitStats?.executions > 0 && workerJit.jitStats?.registerRegisterHits > 0 &&
      workerJit.jitStats?.fallbacks === 0 && workerJit.out.includes('primes<20000=2262') &&
      workerJit.out.includes('fp=609') && workerJit.out.includes('gpa=ok'),
    JSON.stringify(workerJit));
  check('Chrome worker output and retired count match page-realm emitted execution',
    workerJit.out === jitBrowser.emitted.out && workerJit.err === jitBrowser.emitted.err &&
      workerJit.instructions === jitBrowser.emitted.instr,
    JSON.stringify({ worker: workerJit, page: jitBrowser.emitted }));

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
    // Input delivery is independent of WebGPU; expose the canvas in browsers without a GPU adapter.
    await page.locator('#screen').evaluate((canvas) => { canvas.hidden = false; });
    await page.locator('#screen').focus();
    await page.keyboard.press('a');
    await page.locator('#screen').click({ position: { x: 320, y: 240 } });
    await page.waitForFunction(() => {
      const term = document.querySelector('#term')?.textContent || '';
      return term.includes('guest input complete') || document.getElementById('stat').dataset.status !== undefined;
    }, null, { timeout: 10000 }).catch(() => {});
  }
  const inputText = await page.textContent('#term');
  const inputStat = await page.textContent('#stat');
  check('guest observed WM_KEYDOWN(A)', inputText.includes('guest saw keydown A'), inputText);
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
  const canvasPixels = decodePng(await page.locator('#screen').screenshot());
  const atCanvas = (x, y) => { const i = (y * canvasPixels.w + x) * 4; return Array.from(canvasPixels.data.slice(i, i + 4)); };
  check('canvas screenshot is 640x480', canvasPixels.w === 640 && canvasPixels.h === 480, `${canvasPixels.w}x${canvasPixels.h}`);
  check('canvas blit preserves the clear colour', near(atCanvas(10, 10), [0, 0, 51, 255], 2), JSON.stringify(atCanvas(10, 10)));
  check('canvas blit presents the interpolated triangle', near(atCanvas(320, 288), [85, 85, 85, 255], 6), JSON.stringify(atCanvas(320, 288)));

  // ── Direct3D 11 textured quads + depth buffer + indexed draws ──
  await page.goto('http://localhost:8099/index.html?run=d3d11_textured&test=1');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const texText = await page.textContent('#term');
  check('textured program ran to completion', texText.includes('frames=3') && /exited \(exit code 0\)/.test(await page.textContent('#stat')), texText);
  const g2 = await page.evaluate(() => window.__gpuStats);
  check('textured: 15 indexed draws, no validation errors', g2 && g2.draws === 15 && g2.errors.length === 0, JSON.stringify(g2));
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
  check('quad C bottom-right texel white', near(at2(440, 430), WHITE, 1), JSON.stringify(at2(440, 430)));
  check('background untouched', near(at2(10, 10), BG, 1) && near(at2(320, 290), BG, 1), JSON.stringify([at2(10, 10), at2(320, 290)]));
  check('point sampling gives hard texel edge (no blend at the seam)', [at2(319, 340), at2(320, 340)].every((c) => near(c, RED, 1) || near(c, GREEN, 1)), JSON.stringify([at2(319, 340), at2(320, 340)]));
  check('CopyResource exposes the source red texel in quad D', near(at2(32, 150), RED, 1), JSON.stringify(at2(32, 150)));
  check('buffer CopyResource exposes the red quad at its copied vertex-buffer position', near(at2(604, 150), RED, 1), JSON.stringify(at2(604, 150)));

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
      const output = gpu.res.get(3);
      const readback = gpu.device.createBuffer({ size: 256 * 4, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
      const encoder = gpu.device.createCommandEncoder();
      encoder.copyTextureToBuffer({ texture: output.gpu }, { buffer: readback, bytesPerRow: 256, rowsPerImage: 4 }, { width: 4, height: 4 });
      gpu.device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);
      const mapped = new DataView(readback.getMappedRange());
      const uavCenter = mapped.getFloat32(2 * 256 + 2 * 4, true);
      readback.unmap();
      const result = { pixel: observedPixel, uavCenter, stats: gpu.stats, errors: messages };
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
