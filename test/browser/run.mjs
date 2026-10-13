// Drives the real web page in headless Chromium and checks the program output on screen.
import { chromium } from 'playwright-core';
import { decodePng } from './png.mjs';
import { waitForPresentInterval } from '../../web/frame-pacing.js';
import { GpuExecutor } from '../../web/gpu.js';

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
const texturedBundle = join(tempDir, 'd3d11_textured.awb');
const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'unimpl.exe'), '-o', unimplBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'compute.exe'), '-o', computeBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'jit_state.exe'), '-o', jitStateBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'system_random_test.exe'), '-o', systemRandomBundle], { cwd: repoRoot, stdio: 'ignore' });
execFileSync(cliPath, ['pack', join(repoRoot, 'fixtures', 'd3d11_textured.exe'), '-o', texturedBundle], { cwd: repoRoot, stdio: 'ignore' });

const server = spawn(process.execPath, ['web/serve.mjs', '8099'], { cwd: repoRoot, stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 800));
let failures = 0;
const check = (n, ok, d = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${n}${ok ? '' : ' :: ' + d}`); if (!ok) failures++; };

const pacedFrames = [];
let unsynchronizedYields = 0;
const intervalZeroWaited = await waitForPresentInterval(0, async () => pacedFrames.push('frame'), async () => { unsynchronizedYields++; });
check('DXGI present interval zero yields without waiting for vblank', intervalZeroWaited === 0 && unsynchronizedYields === 1 && pacedFrames.length === 0,
  JSON.stringify({ intervalZeroWaited, unsynchronizedYields, pacedFrames: [...pacedFrames] }));
const intervalThreeWaited = await waitForPresentInterval(3, async () => pacedFrames.push('frame'), async () => { unsynchronizedYields++; });
check('DXGI present interval three waits for three vblanks', intervalThreeWaited === 3 && pacedFrames.length === 3 && unsynchronizedYields === 1,
  JSON.stringify({ intervalThreeWaited, unsynchronizedYields, pacedFrames }));

const stateProbe = new GpuExecutor({}, null);
const emptyPayload = new DataView(new ArrayBuffer(0));
const emptyBytes = new Uint8Array(0);
stateProbe.st.layout = 17;
stateProbe.st.cb[0][2] = 31;
stateProbe.st.d3d9RenderStates.set(4, 9);
stateProbe.dispatch(51, emptyPayload, emptyBytes); // PUSH_STATE
stateProbe.dispatch(49, emptyPayload, emptyBytes); // CLEAR_STATE
stateProbe.dispatch(52, emptyPayload, emptyBytes); // POP_STATE
check('GPU command-list state restore preserves nested pipeline bindings',
  stateProbe.st.layout === 17 && stateProbe.st.cb[0][2] === 31 && stateProbe.st.d3d9RenderStates.get(4) === 9,
  JSON.stringify({ layout: stateProbe.st.layout, constantBuffer: stateProbe.st.cb[0][2], renderState: stateProbe.st.d3d9RenderStates.get(4) }));

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

  const frameJitWorker = await page.evaluate(async (bundle) => {
    const wasm = await (await fetch('/anywasm_wasm.wasm')).arrayBuffer();
    const guest = new Uint8Array(bundle);
    const canvas = document.createElement('canvas');
    canvas.width = 640; canvas.height = 480;
    const offscreen = canvas.transferControlToOffscreen();
    const worker = new Worker('/worker.js', { type: 'module' });
    try {
      return await new Promise((resolve, reject) => {
        let gpu = null;
        const framePacing = [];
        const messages = [];
        let loadProgressCount = 0;
        const timeout = setTimeout(() => resolve({ timeout: true, gpu, loadProgressCount, messages }), 60_000);
        worker.onerror = (event) => { clearTimeout(timeout); reject(new Error(event.message)); };
        worker.onmessage = ({ data }) => {
          if (data.type === 'load-progress') loadProgressCount++;
          messages.push(data.type === 'out'
            ? { type: data.type, stream: data.stream, text: data.text }
            : data.type === 'load-progress'
              ? { type: data.type, message: data.message, trace: data.trace }
              : { type: data.type });
          if (messages.length > 12) messages.shift();
          if (data.type === 'gpu') gpu = data;
          if (data.type === 'frame-pacing') framePacing.push(data);
          if (data.type === 'status') {
            worker.postMessage({ type: 'capture' });
            worker.__frameStatus = data;
          } else if (data.type === 'pixels' && worker.__frameStatus) {
            clearTimeout(timeout);
            const sample = (x, y) => Array.from(data.data.slice((y * data.width + x) * 4, (y * data.width + x + 1) * 4));
            resolve({
              ...worker.__frameStatus,
              gpu,
              framePacing,
              pixels: { width: data.width, height: data.height, clear: sample(10, 10), blended: sample(100, 150), texture: sample(200, 340) },
            });
          }
        };
        worker.postMessage({ type: 'start', wasm, bundle: guest, canvas: offscreen, testMode: true, jitMinPrefix: 1 },
          [wasm, guest.buffer, offscreen]);
      });
    } finally {
      worker.terminate();
    }
  }, Array.from(readFileSync(texturedBundle)));
  const framePixelNear = (actual, expected, tolerance = 1) =>
    actual?.length === expected.length && actual.every((value, index) => Math.abs(value - expected[index]) <= tolerance);
  check('DXGI frame waits advance the virtual clock by elapsed time in the JIT worker',
    frameJitWorker.status === jitBrowser.exited && frameJitWorker.exitCode === 0 && frameJitWorker.frames === 3
      && frameJitWorker.jitStats?.executions > 0 && frameJitWorker.gpu?.draws === 18
      && frameJitWorker.gpu?.presents === 3 && frameJitWorker.gpu?.errors?.length === 0
      && frameJitWorker.framePacing?.length === 3
      && frameJitWorker.framePacing.every(({ syncInterval, vblanksWaited, elapsed100ns, virtualTicksAdded }) =>
        syncInterval === 1 && vblanksWaited === 1 && elapsed100ns > 0 && virtualTicksAdded === elapsed100ns * 10)
      && frameJitWorker.pixels?.width === 640 && frameJitWorker.pixels?.height === 480
      && framePixelNear(frameJitWorker.pixels.clear, [0, 0, 51, 255])
      && framePixelNear(frameJitWorker.pixels.blended, [128, 0, 26, 255])
      && framePixelNear(frameJitWorker.pixels.texture, [255, 0, 0, 255]),
    JSON.stringify(frameJitWorker));

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
      const rect = canvas.getBoundingClientRect();
      canvas.style.transform = `translate(${Math.round(rect.left) - rect.left}px, ${Math.round(rect.top) - rect.top}px)`;
    });
    await page.locator('#screen').scrollIntoViewIfNeeded();
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
      return {
        x: rect.left + canvas.clientLeft + 520 * canvas.clientWidth / 1024,
        y: rect.top + canvas.clientTop + 404 * canvas.clientHeight / 768,
      };
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
  const stagedPixels = triText.match(/staging readback corner=(\d+),(\d+),(\d+),(\d+) center=(\d+),(\d+),(\d+),(\d+)/);
  check('D3D11 staging Map(READ) returns rendered backbuffer pixels',
    !!stagedPixels && stagedPixels.slice(1, 5).join(',') === '0,0,51,255'
      && stagedPixels.slice(5, 8).every((v) => Number(v) >= 70 && Number(v) <= 100)
      && Number(stagedPixels[8]) === 255,
    stagedPixels?.[0] || triText);
  check('D3D11 staging buffer Map(READ) returns the copied constant-buffer values',
    triText.includes('staging buffer readback=1,1,1,1'), triText);
  check('D3D11 staging R32_FLOAT Map(READ) preserves two rows of single-precision values',
    triText.includes('staging R32_FLOAT readback=0.25,0.50,0.75,1.00 row_pitch=8'), triText);
  check('D3D11 staging R32_UINT Map(READ) preserves unsigned words in two rows',
    triText.includes('staging R32_UINT readback=12345678,ffffffff,00000000,80000000 row_pitch=8'), triText);
  check('D3D11 staging R32_SINT Map(READ) preserves signed bit patterns in two rows',
    triText.includes('staging R32_SINT readback=80000000,ffffffff,00000000,7fffffff row_pitch=8'), triText);
  check('D3D11 staging R16_UNORM Map(READ) preserves normalized words',
    triText.includes('staging R16_UNORM readback=0000,4000,8000,ffff row_pitch=4'), triText);
  check('D3D11 staging R16_SNORM Map(READ) preserves signed normalized words',
    triText.includes('staging R16_SNORM readback=8000,c000,0000,7fff row_pitch=4'), triText);
  check('D3D11 staging R16_UINT Map(READ) preserves unsigned words',
    triText.includes('staging R16_UINT readback=0000,0001,7fff,ffff row_pitch=4'), triText);
  check('D3D11 staging R16_SINT Map(READ) preserves signed words',
    triText.includes('staging R16_SINT readback=8000,ffff,0000,7fff row_pitch=4'), triText);
  check('D3D11 staging R8_SNORM Map(READ) preserves signed normalized bytes',
    triText.includes('staging R8_SNORM readback=80,c0,00,7f row_pitch=2'), triText);
  check('D3D11 staging R8_UINT Map(READ) preserves unsigned bytes',
    triText.includes('staging R8_UINT readback=00,01,7f,ff row_pitch=2'), triText);
  check('D3D11 staging R8_SINT Map(READ) preserves signed bytes',
    triText.includes('staging R8_SINT readback=80,ff,00,7f row_pitch=2'), triText);
  check('D3D11 staging A8_UNORM Map(READ) preserves alpha bytes',
    triText.includes('staging A8_UNORM readback=11,7f,80,ff row_pitch=2'), triText);
  check('D3D11 staging R32G32_FLOAT Map(READ) preserves two-channel rows and row pitch',
    triText.includes('staging R32G32_FLOAT readback=0.125,-0.250,0.500,1.250,-0.500,2.000,-1.000,0.750 row_pitch=16'), triText);
  check('D3D11 staging R32G32_UINT Map(READ) preserves packed channel words',
    triText.includes('staging R32G32_UINT readback=12345678,9abcdef0,01020304,ffffffff,00000001,80000000,deadbeef,55aa33cc row_pitch=16'), triText);
  check('D3D11 staging R32G32_SINT Map(READ) preserves signed channel bit patterns',
    triText.includes('staging R32G32_SINT readback=80000000,ffffffff,00000000,00000001,7fffffff,87654321,fffffffe,0000002a row_pitch=16'), triText);
  check('D3D11 staging R32G32B32A32_FLOAT Map(READ) preserves four-channel rows',
    triText.includes('staging R32G32B32A32_FLOAT readback=0.125,-0.250,0.500,1.250,-0.500,2.000,-1.000,0.750,3.500,-4.250,5.125,-6.500,7.750,8.500,-9.250,10.000 row_pitch=32'), triText);
  check('D3D11 staging R32G32B32A32_UINT Map(READ) preserves wide packed channel words',
    triText.includes('staging R32G32B32A32_UINT readback=12345678,9abcdef0,01020304,ffffffff,00000001,80000000,deadbeef,55aa33cc,00000000,ffffffff,13579bdf,2468ace0,abcdef01,10203040,55667788,99aabbcc row_pitch=32'), triText);
  check('D3D11 staging R32G32B32A32_SINT Map(READ) preserves wide signed bit patterns',
    triText.includes('staging R32G32B32A32_SINT readback=80000000,ffffffff,00000000,00000001,7fffffff,87654321,fffffffe,0000002a,ffffffff,80000001,fffffffd,00000003,12345678,edcba987,40000000,c0000000 row_pitch=32'), triText);
  check('D3D11 staging RG16_FLOAT Map(READ) preserves two-channel rows',
    triText.includes('staging RG16_FLOAT readback=3400,b400,3800,3c00,b800,4000,bc00,3a00 row_pitch=8'), triText);
  check('D3D11 staging RG16_UNORM Map(READ) preserves normalized words',
    triText.includes('staging RG16_UNORM readback=0000,7fff,8000,ffff,1234,abcd,5555,aaaa row_pitch=8'), triText);
  check('D3D11 staging RG16_SNORM Map(READ) preserves signed normalized words',
    triText.includes('staging RG16_SNORM readback=8000,c000,0000,7fff,1234,edcb,4000,c000 row_pitch=8'), triText);
  check('D3D11 staging RG16_UINT Map(READ) preserves unsigned words',
    triText.includes('staging RG16_UINT readback=0000,ffff,1234,abcd,8000,7fff,5555,aaaa row_pitch=8'), triText);
  check('D3D11 staging RG16_SINT Map(READ) preserves signed words',
    triText.includes('staging RG16_SINT readback=8000,ffff,0000,7fff,edcb,1234,c000,4000 row_pitch=8'), triText);
  check('D3D11 staging RG8_SNORM Map(READ) preserves signed normalized channels',
    triText.includes('staging RG8_SNORM readback=80,c0,00,7f,12,ed,40,c0 row_pitch=4'), triText);
  check('D3D11 staging RG8_UINT Map(READ) preserves unsigned channels',
    triText.includes('staging RG8_UINT readback=00,ff,12,ab,80,7f,55,aa row_pitch=4'), triText);
  check('D3D11 staging RG8_SINT Map(READ) preserves signed channels',
    triText.includes('staging RG8_SINT readback=80,ff,00,7f,ed,12,c0,40 row_pitch=4'), triText);
  check('D3D11 staging RGBA16_FLOAT Map(READ) preserves packed half-float values',
    triText.includes('staging RGBA16_FLOAT readback=3400,3800,3c00,4000 row_pitch=16'), triText);
  check('D3D11 staging RGBA16_UNORM Map(READ) preserves normalized channel words',
    triText.includes('staging RGBA16_UNORM readback=0000,1111,2222,3333,4444,5555,6666,7777,8888,9999,aaaa,bbbb,cccc,dddd,eeee,ffff row_pitch=16'), triText);
  check('D3D11 staging RGBA16_SNORM Map(READ) preserves signed normalized channel words',
    triText.includes('staging RGBA16_SNORM readback=8000,c000,0000,7fff,1234,edcb,4000,c000,ffff,8001,fffd,0003,4000,c000,2000,e000 row_pitch=16'), triText);
  check('D3D11 staging RGBA16_UINT Map(READ) preserves unsigned channel words',
    triText.includes('staging RGBA16_UINT readback=0000,ffff,1234,abcd,8000,7fff,5555,aaaa,0001,fffe,1357,2468,dead,beef,0102,0304 row_pitch=16'), triText);
  check('D3D11 staging RGBA16_SINT Map(READ) preserves signed channel words',
    triText.includes('staging RGBA16_SINT readback=8000,ffff,0000,7fff,edcb,1234,c000,4000,ffff,8001,fffd,0003,4000,c000,2000,e000 row_pitch=16'), triText);
  check('D3D11 staging RGBA8_UNORM Map(READ) preserves normalized channel bytes',
    triText.includes('staging RGBA8_UNORM readback=00,11,22,33,44,55,66,77,88,99,aa,bb,cc,dd,ee,ff row_pitch=8'), triText);
  check('D3D11 staging RGBA8_SNORM Map(READ) preserves signed normalized channel bytes',
    triText.includes('staging RGBA8_SNORM readback=80,c0,00,7f,12,ed,40,c0,ff,81,fd,03,40,c0,20,e0 row_pitch=8'), triText);
  check('D3D11 staging RGBA8_UINT Map(READ) preserves unsigned channel bytes',
    triText.includes('staging RGBA8_UINT readback=00,ff,12,ab,80,7f,55,aa,01,fe,13,24,de,ef,02,04 row_pitch=8'), triText);
  check('D3D11 staging RGBA8_SINT Map(READ) preserves signed channel bytes',
    triText.includes('staging RGBA8_SINT readback=80,ff,00,7f,ed,12,c0,40,ff,81,fd,03,40,c0,20,e0 row_pitch=8'), triText);
  check('D3D11 staging BGRA8_UNORM Map(READ) preserves raw channel bytes',
    triText.includes('staging BGRA8_UNORM readback=10,20,30,40,50,60,70,80,90,100,110,120,130,140,150,160 row_pitch=8'), triText);
  check('D3D11 staging R16_FLOAT Map(READ) preserves packed half-float rows',
    triText.includes('staging R16_FLOAT readback=3400,3800,3c00,4000,4200,4400 row_pitch=6'), triText);
  check('D3D11 staging R10G10B10A2_UNORM Map(READ) preserves packed words',
    triText.includes('staging R10G10B10A2_UNORM readback=12345678,9abcdef0,55aa33cc,ffffffff row_pitch=8'), triText);
  check('D3D11 staging R10G10B10A2_UINT Map(READ) preserves packed integer words',
    triText.includes('staging R10G10B10A2_UINT readback=12345678,9abcdef0,55aa33cc,ffffffff row_pitch=8'), triText);
  check('D3D11 staging R11G11B10_FLOAT Map(READ) preserves packed float words',
    triText.includes('staging R11G11B10_FLOAT readback=3c003c00,7bf0fe00,00000000,7f7fffff row_pitch=8'), triText);
  check('D3D11 staging R8G8_UNORM Map(READ) preserves two-channel row packing',
    triText.includes('staging R8G8_UNORM readback=10,20,30,40,50,60,70,80,90,100,110,120 row_pitch=6'), triText);
  check('D3D11 staging R8_UNORM Map(READ) repacks odd-width rows tightly',
    triText.includes('staging R8_UNORM subresource0=11,12,13,21,22,23 row_pitch=3'), triText);
  check('D3D11 staging R8_UNORM Map(READ) selects the requested mip and array slice',
    triText.includes('staging R8_UNORM subresource3=61 row_pitch=1'), triText);
  check('D3D11 staging D32_FLOAT Map(READ) copies depth-only texels',
    triText.includes('staging D32_FLOAT readback=0.375,0.375,0.375,0.375 row_pitch=8'), triText);
  check('D3D11 staging D16_UNORM Map(READ) preserves normalized depth texels',
    triText.includes('staging D16_UNORM readback=4000,4000,4000,4000 row_pitch=4'), triText);
  check('D3D11 staging RGBA8_SRGB Map(READ) preserves encoded colour bytes',
    triText.includes('staging RGBA8_SRGB readback=10,20,30,40,50,60,70,80,90,100,110,120,130,140,150,160 row_pitch=8'), triText);
  check('D3D11 staging BGRA8_SRGB Map(READ) preserves encoded colour bytes',
    triText.includes('staging BGRA8_SRGB readback=10,20,30,40,50,60,70,80,90,100,110,120,130,140,150,160 row_pitch=8'), triText);
  check('D3D11 staging Texture3D Map(READ) preserves row and depth pitches',
    triText.includes('staging Texture3D R32_FLOAT readback=0.125,0.250,0.375,0.500,0.625,0.750,0.875,1.000 row_pitch=8 depth_pitch=16'), triText);
  const depthStencilReadbackResult = await page.evaluate(async () => {
    const { GpuExecutor } = await import('/gpu.js');
    const gpu = new GpuExecutor(document.createElement('canvas'), null, () => {}, { noPresent: true });
    try {
      await gpu.init();
      if (!gpu.features.has('depth32float-stencil8')) {
        gpu.device.destroy();
        return { skipped: 'adapter does not support depth32float-stencil8' };
      }
      gpu.createTexture(1, 2, 2, 1, 1, 20, 0x40, 0, 0); // D32_FLOAT_S8X24_UINT depth-stencil texture
      const pass = gpu.encoder().beginRenderPass({ colorAttachments: [], depthStencilAttachment: {
        view: gpu.res.get(1).gpu.createView(), depthLoadOp: 'clear', depthStoreOp: 'store', depthClearValue: 0.375,
        stencilLoadOp: 'clear', stencilStoreOp: 'store', stencilClearValue: 0x5a,
      } });
      pass.end();
      const bytes = Array.from(await gpu.readBackResource(1));
      const errors = gpu.stats.errors;
      gpu.device.destroy();
      return { bytes, errors };
    } catch (error) {
      gpu.device?.destroy();
      return { error: String(error) };
    }
  });
  const depthStencilPixel = [0x00, 0x00, 0xc0, 0x3e, 0x5a, 0x00, 0x00, 0x00];
  const expectedDepthStencilBytes = [...depthStencilPixel, ...depthStencilPixel, ...depthStencilPixel, ...depthStencilPixel];
  check('D3D11 D32_FLOAT_S8X24 readback interleaves raw depth, stencil, and padding bytes',
    Boolean(depthStencilReadbackResult.skipped)
      || (!depthStencilReadbackResult.error && depthStencilReadbackResult.errors.length === 0
        && depthStencilReadbackResult.bytes.length === expectedDepthStencilBytes.length
        && depthStencilReadbackResult.bytes.every((value, i) => value === expectedDepthStencilBytes[i])),
    JSON.stringify(depthStencilReadbackResult));
  const depth24ReadbackResult = await page.evaluate(async () => {
    const { GpuExecutor } = await import('/gpu.js');
    const gpu = new GpuExecutor(document.createElement('canvas'), null, () => {}, { noPresent: true });
    try {
      await gpu.init();
      gpu.createTexture(1, 2, 2, 1, 1, 45, 0x40, 0, 0); // D24_UNORM_S8_UINT
      const pass = gpu.encoder().beginRenderPass({ colorAttachments: [], depthStencilAttachment: {
        view: gpu.res.get(1).gpu.createView(), depthLoadOp: 'clear', depthStoreOp: 'store', depthClearValue: 0.375,
        stencilLoadOp: 'clear', stencilStoreOp: 'store', stencilClearValue: 0x5a,
      } });
      pass.end();
      const bytes = Array.from(await gpu.readBackResource(1));
      const errors = gpu.stats.errors;
      gpu.device.destroy();
      return { bytes, errors };
    } catch (error) {
      gpu.device?.destroy();
      return { error: String(error) };
    }
  });
  const depth24Pixel = [0x00, 0x00, 0x60, 0x5a];
  const expectedDepth24Bytes = [...depth24Pixel, ...depth24Pixel, ...depth24Pixel, ...depth24Pixel];
  check('D3D11 D24_UNORM_S8 readback packs depth and stencil into four-byte texels',
    !depth24ReadbackResult.error && depth24ReadbackResult.errors?.length === 0
      && depth24ReadbackResult.bytes?.length === expectedDepth24Bytes.length
      && depth24ReadbackResult.bytes.every((value, i) => value === expectedDepth24Bytes[i]),
    JSON.stringify(depth24ReadbackResult));
  const packed16ReadbackResult = await page.evaluate(async () => {
    const { GpuExecutor } = await import('/gpu.js');
    const gpu = new GpuExecutor(document.createElement('canvas'), null, () => {}, { noPresent: true });
    try {
      await gpu.init();
      const formats = [
        { name: 'B5G6R5_UNORM', dxgi: 85, values: [0x001f, 0x07e0, 0xf800, 0xffff, 0x0000, 0x8410],
          pixels: [0, 0, 1, 1, 0, 1, 0, 1, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 1, 16 / 31, 32 / 63, 16 / 31, 1] },
        { name: 'B5G5R5A1_UNORM', dxgi: 86, values: [0x001f, 0x03e0, 0x7c00, 0x8000, 0xffff, 0x4210],
          pixels: [0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 16 / 31, 16 / 31, 16 / 31, 0] },
        { name: 'B4G4R4A4_UNORM', dxgi: 115, values: [0xf00f, 0x0f00, 0x00f0, 0x000f, 0xffff, 0x8421],
          pixels: [0, 0, 1, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 1, 1, 1, 4 / 15, 2 / 15, 1 / 15, 8 / 15] },
      ];
      const readbacks = [];
      for (const [index, format] of formats.entries()) {
        const rowPitch = 10, source = new Uint8Array(rowPitch * 2).fill(0xee), view = new DataView(source.buffer);
        format.values.forEach((value, pixel) => view.setUint16(Math.floor(pixel / 3) * rowPitch + pixel % 3 * 2, value, true));
        const id = index + 1;
        gpu.createTexture(id, 3, 2, 1, 1, format.dxgi, 0, 0, 0);
        gpu.textureWrite(id, 0, 0, 0, 0, 3, 2, rowPitch, source.length, source);
        const pixels = await gpu.readBackTexture(id);
        readbacks.push({ name: format.name, expected: Array.from(source).filter((_, byte) => byte % rowPitch < 6),
          expectedPixels: format.pixels, backingFormat: gpu.res.get(id).format, pixels: Array.from(pixels.data),
          bytes: Array.from(await gpu.readBackResource(id)) });
      }
      const errors = gpu.stats.errors;
      gpu.device.destroy();
      return { readbacks, errors };
    } catch (error) {
      gpu.device?.destroy();
      return { error: String(error) };
    }
  });
  check('D3D11 legacy packed 16-bit staging textures preserve odd-width padded rows',
    !packed16ReadbackResult.error && packed16ReadbackResult.errors?.length === 0
      && packed16ReadbackResult.readbacks?.length === 3
      && packed16ReadbackResult.readbacks.every(({ expected, expectedPixels, backingFormat, pixels, bytes }) =>
        backingFormat === 'rgba16float' && bytes.length === expected.length
        && bytes.every((value, i) => value === expected[i])
        && pixels.length === expectedPixels.length
        && pixels.every((value, i) => Math.abs(value - expectedPixels[i]) <= 0.001)),
    JSON.stringify(packed16ReadbackResult));
  const bcReadbackResult = await page.evaluate(async () => {
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 8; canvas.height = 8;
    const gpu = new GpuExecutor(canvas, null, () => {}, { noPresent: true });
    try {
      await gpu.init();
      if (!gpu.features.has('texture-compression-bc')) {
        gpu.device.destroy();
        return { skipped: 'adapter does not support BC textures' };
      }
      const formats = [
        { name: 'BC1_TYPELESS', dxgi: 70, blockBytes: 8 },
        { name: 'BC1_UNORM', dxgi: 71, blockBytes: 8 },
        { name: 'BC1_SRGB', dxgi: 72, blockBytes: 8 },
        { name: 'BC2_TYPELESS', dxgi: 73, blockBytes: 16 },
        { name: 'BC2_UNORM', dxgi: 74, blockBytes: 16 },
        { name: 'BC2_SRGB', dxgi: 75, blockBytes: 16 },
        { name: 'BC3_TYPELESS', dxgi: 76, blockBytes: 16 },
        { name: 'BC3_UNORM', dxgi: 77, blockBytes: 16 },
        { name: 'BC3_SRGB', dxgi: 78, blockBytes: 16 },
        { name: 'BC4_TYPELESS', dxgi: 79, blockBytes: 8 },
        { name: 'BC4_UNORM', dxgi: 80, blockBytes: 8 },
        { name: 'BC4_SNORM', dxgi: 81, blockBytes: 8 },
        { name: 'BC5_TYPELESS', dxgi: 82, blockBytes: 16 },
        { name: 'BC5_UNORM', dxgi: 83, blockBytes: 16 },
        { name: 'BC5_SNORM', dxgi: 84, blockBytes: 16 },
        { name: 'BC6H_TYPELESS', dxgi: 94, blockBytes: 16 },
        { name: 'BC6H_UF16', dxgi: 95, blockBytes: 16 },
        { name: 'BC6H_SF16', dxgi: 96, blockBytes: 16 },
        { name: 'BC7_TYPELESS', dxgi: 97, blockBytes: 16 },
        { name: 'BC7_UNORM', dxgi: 98, blockBytes: 16 },
        { name: 'BC7_SRGB', dxgi: 99, blockBytes: 16 },
      ];
      const readbacks = [];
      for (const [index, format] of formats.entries()) {
        const rowBytes = 2 * format.blockBytes, rowPitch = rowBytes + 4;
        const source = new Uint8Array(rowPitch * 2).fill(0xee);
        for (let i = 0; i < rowBytes; i++) {
          source[i] = i + 1;
          source[rowPitch + i] = 0x80 + i;
        }
        const id = 7 + index;
        gpu.createTexture(id, 8, 8, 1, 1, format.dxgi, 0, 0, 0);
        gpu.textureWrite(id, 0, 0, 0, 0, 8, 8, rowPitch, source.length, source);
        readbacks.push({ name: format.name, blockBytes: format.blockBytes, bytes: Array.from(await gpu.readBackResource(id)) });
      }
      gpu.device.destroy();
      return { readbacks };
    } catch (error) {
      gpu.device?.destroy();
      return { error: String(error) };
    }
  });
  check('D3D11 all BC DXGI format variants preserve compressed staging block rows',
    Boolean(bcReadbackResult.skipped)
      || (!bcReadbackResult.error && bcReadbackResult.readbacks?.length === 21
        && bcReadbackResult.readbacks.every((item) => {
          const rowBytes = 2 * item.blockBytes;
          const expected = Array.from({ length: rowBytes }, (_, i) => i + 1)
            .concat(Array.from({ length: rowBytes }, (_, i) => 0x80 + i));
          return item.bytes.length === expected.length && item.bytes.every((value, i) => value === expected[i]);
        })),
    JSON.stringify(bcReadbackResult));
  const typelessReadbackResult = await page.evaluate(async () => {
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    const gpu = new GpuExecutor(canvas, null, () => {}, { noPresent: true });
    try {
      await gpu.init();
      const formats = [
        { name: 'R32G32B32A32_TYPELESS', dxgi: 1, bytesPerPixel: 16 },
        { name: 'R16G16B16A16_TYPELESS', dxgi: 9, bytesPerPixel: 8 },
        { name: 'R32G32_TYPELESS', dxgi: 15, bytesPerPixel: 8 },
        { name: 'R10G10B10A2_TYPELESS', dxgi: 23, bytesPerPixel: 4 },
        { name: 'R8G8B8A8_TYPELESS', dxgi: 27, bytesPerPixel: 4 },
        { name: 'R16G16_TYPELESS', dxgi: 33, bytesPerPixel: 4 },
        { name: 'R32_TYPELESS', dxgi: 39, bytesPerPixel: 4 },
        { name: 'R8G8_TYPELESS', dxgi: 48, bytesPerPixel: 2 },
        { name: 'R16_TYPELESS', dxgi: 53, bytesPerPixel: 2 },
        { name: 'R8_TYPELESS', dxgi: 60, bytesPerPixel: 1 },
        { name: 'B8G8R8X8_UNORM', dxgi: 88, bytesPerPixel: 4 },
        { name: 'B8G8R8A8_TYPELESS', dxgi: 90, bytesPerPixel: 4 },
        { name: 'B8G8R8X8_TYPELESS', dxgi: 92, bytesPerPixel: 4 },
        { name: 'B8G8R8X8_UNORM_SRGB', dxgi: 93, bytesPerPixel: 4 },
      ];
      const readbacks = [];
      for (const [index, format] of formats.entries()) {
        const rowBytes = 3 * format.bytesPerPixel, rowPitch = rowBytes + 4;
        const source = new Uint8Array(rowPitch * 2).fill(0xee), expected = [];
        for (let row = 0; row < 2; row++) {
          for (let byte = 0; byte < rowBytes; byte++) {
            const value = (format.dxgi * 3 + row * 67 + byte * 11) & 0xff;
            source[row * rowPitch + byte] = value;
            expected.push(value);
          }
        }
        const id = 100 + index;
        gpu.createTexture(id, 3, 2, 1, 1, format.dxgi, 0, 0, 0);
        gpu.textureWrite(id, 0, 0, 0, 0, 3, 2, rowPitch, source.length, source);
        readbacks.push({ name: format.name, expected, bytes: Array.from(await gpu.readBackResource(id)) });
      }
      const arrayId = 200;
      gpu.createTexture(arrayId, 3, 2, 2, 2, 61, 0, 0, 0);
      const subresources = [];
      for (let subresource = 0; subresource < 4; subresource++) {
        const mip = subresource % 2, slice = Math.floor(subresource / 2);
        const width = Math.max(1, 3 >> mip), height = Math.max(1, 2 >> mip);
        const rowPitch = width + 2, source = new Uint8Array(rowPitch * height).fill(0xee), expected = [];
        for (let row = 0; row < height; row++) for (let x = 0; x < width; x++) {
          const value = 10 + subresource * 40 + row * 10 + x;
          source[row * rowPitch + x] = value;
          expected.push(value);
        }
        gpu.textureWrite(arrayId, mip, slice, 0, 0, width, height, rowPitch, source.length, source);
        subresources.push({ subresource, expected, bytes: Array.from(await gpu.readBackResource(arrayId, subresource)) });
      }
      gpu.device.destroy();
      return { readbacks, subresources };
    } catch (error) {
      gpu.device?.destroy();
      return { error: String(error) };
    }
  });
  check('D3D11 typeless and BGRX staging formats preserve packed rows and discard row padding',
    !typelessReadbackResult.error && typelessReadbackResult.readbacks?.length === 14
      && typelessReadbackResult.readbacks.every(({ expected, bytes }) => bytes.length === expected.length && bytes.every((value, i) => value === expected[i])),
    JSON.stringify(typelessReadbackResult));
  check('D3D11 staging readback selects all four mip/array subresources with tight rows',
    typelessReadbackResult.subresources?.length === 4
      && typelessReadbackResult.subresources.every(({ expected, bytes }) => bytes.length === expected.length && bytes.every((value, i) => value === expected[i])),
    JSON.stringify(typelessReadbackResult.subresources));
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
    const style = getComputedStyle(canvas);
    return {
      aspectWidth: width, aspectHeight: height,
      borderWidth: parseFloat(style.borderLeftWidth) + parseFloat(style.borderRightWidth),
      borderHeight: parseFloat(style.borderTopWidth) + parseFloat(style.borderBottomWidth),
    };
  });
  const canvasPixels = decodePng(await page.locator('#screen').screenshot());
  const atCanvas = (x, y) => { const i = (y * canvasPixels.w + x) * 4; return Array.from(canvasPixels.data.slice(i, i + 4)); };
  const canvasContentAspect = (width, height) =>
    (width - canvasLayout.borderWidth) / (height - canvasLayout.borderHeight);
  check('canvas screenshot preserves the guest window aspect ratio',
    canvasPixels.w === 640 && Math.abs(canvasContentAspect(canvasPixels.w, canvasPixels.h) - canvasLayout.aspectWidth / canvasLayout.aspectHeight) < 0.01,
    `${canvasPixels.w}x${canvasPixels.h} at ${canvasLayout.aspectWidth}/${canvasLayout.aspectHeight}`);
  check('canvas blit preserves the clear colour', near(atCanvas(10, 10), [0, 0, 51, 255], 2), JSON.stringify(atCanvas(10, 10)));
  const centroidY = Math.round(1 + 288.5 / 480 * (canvasPixels.h - 2));
  check('canvas blit presents the interpolated triangle', near(atCanvas(320, centroidY), [85, 85, 85, 255], 6), JSON.stringify(atCanvas(320, centroidY)));
  await page.locator('#screen').evaluate((canvas) => { canvas.style.width = '320px'; });
  const scaledCanvasPixels = decodePng(await page.locator('#screen').screenshot());
  const atScaledCanvas = (x, y) => { const i = (y * scaledCanvasPixels.w + x) * 4; return Array.from(scaledCanvasPixels.data.slice(i, i + 4)); };
  check('scaled canvas screenshot preserves the guest window aspect ratio',
    scaledCanvasPixels.w === 320 && Math.abs(canvasContentAspect(scaledCanvasPixels.w, scaledCanvasPixels.h) - canvasLayout.aspectWidth / canvasLayout.aspectHeight) < 0.01,
    `${scaledCanvasPixels.w}x${scaledCanvasPixels.h}`);
  const scaledCentroidY = Math.round(1 + 288.5 / 480 * (scaledCanvasPixels.h - 2));
  check('scaled canvas blit preserves the interpolated triangle', near(atScaledCanvas(160, scaledCentroidY), [85, 85, 85, 255], 6), JSON.stringify(atScaledCanvas(160, scaledCentroidY)));

  const hidpiPage = await browser.newPage({ viewport: { width: 1280, height: 1000 }, deviceScaleFactor: 2 });
  const hidpiErrors = [];
  hidpiPage.on('pageerror', (error) => hidpiErrors.push(error.message));
  await hidpiPage.goto('http://localhost:8099/index.html?run=d3d11_triangle');
  await hidpiPage.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const hidpiStats = await hidpiPage.evaluate(() => ({
    dpr: devicePixelRatio,
    canvas: { width: document.getElementById('screen').width, height: document.getElementById('screen').height },
    rect: document.getElementById('screen').getBoundingClientRect().toJSON(),
    gpu: window.__gpuStats,
  }));
  const hidpiPixels = decodePng(await hidpiPage.locator('#screen').screenshot());
  const hidpiAt = (x, y) => Array.from(hidpiPixels.data.slice((y * hidpiPixels.w + x) * 4, (y * hidpiPixels.w + x) * 4 + 4));
  const hidpiScaleX = hidpiPixels.w / hidpiStats.rect.width, hidpiScaleY = hidpiPixels.h / hidpiStats.rect.height;
  const hidpiClear = hidpiAt(Math.round(10 * hidpiScaleX), Math.round(10 * hidpiScaleY));
  const hidpiCentroid = hidpiAt(Math.round(1 + (hidpiPixels.w - 2) / 2), Math.round(1 + 288.5 / 480 * (hidpiPixels.h - 2)));
  check('DPR2 worker canvas retains the 640x480 guest backing store and scales to physical pixels',
    hidpiStats.dpr === 2 && hidpiStats.canvas.width === 640 && hidpiStats.canvas.height === 480
      && hidpiPixels.w === 1280 && Math.abs(hidpiPixels.h - hidpiStats.rect.height * 2) <= 2
      && hidpiStats.gpu?.presents === 3 && hidpiStats.gpu.errors.length === 0 && hidpiErrors.length === 0,
    JSON.stringify({ dpr: hidpiStats.dpr, canvas: hidpiStats.canvas, rect: hidpiStats.rect,
      screenshot: [hidpiPixels.w, hidpiPixels.h], presents: hidpiStats.gpu?.presents, errors: hidpiStats.gpu?.errors, pageErrors: hidpiErrors }));
  check('DPR2 visible canvas preserves the clear and interpolated triangle pixels',
    near(hidpiClear, [0, 0, 51, 255], 2) && near(hidpiCentroid, [85, 85, 85, 255], 6),
    JSON.stringify({ clear: hidpiClear, centroid: hidpiCentroid }));
  await hidpiPage.close();

  const resizeProbe = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.id = 'swapchain-resize-probe';
    canvas.style.cssText = 'display:block;width:32px;height:24px;max-width:none;aspect-ratio:auto;border:0;border-radius:0;margin:0';
    document.body.append(canvas);
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {});
      await gpu.init();
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const data = new Uint8Array(8 + payload.length), view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, payload.length, true); data.set(payload, 8);
        return data;
      };
      const colorPayload = (viewId, color) => {
        const payload = new Uint8Array(20), view = new DataView(payload.buffer);
        view.setUint32(0, viewId, true);
        color.forEach((value, i) => view.setFloat32(4 + 4 * i, value, true));
        return payload;
      };
      const execute = (parts) => {
        const size = parts.reduce((n, part) => n + part.length, 0), stream = new Uint8Array(size);
        let offset = 0;
        for (const part of parts) { stream.set(part, offset); offset += part.length; }
        gpu.execute(stream);
      };
      execute([
        command(38, words(1, 0, 9, 32, 24, 28)),
        command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1)),
        command(27, colorPayload(2, [0.2, 0.4, 0.8, 1])),
        command(35, words(1, 9, 0)),
      ]);
      window.__swapchainResizeProbe = { canvas, gpu, command, words, colorPayload, execute };
      const rect = canvas.getBoundingClientRect();
      return { width: canvas.width, height: canvas.height, cssWidth: rect.width, cssHeight: rect.height,
        presents: gpu.stats.presents, errors: gpu.stats.errors };
    } catch (error) {
      return { error: error.message };
    }
  });
  check('swapchain initially presents a 32x24 visible canvas',
    resizeProbe.width === 32 && resizeProbe.height === 24 && resizeProbe.cssWidth === 32 && resizeProbe.cssHeight === 24
      && resizeProbe.presents === 1 && resizeProbe.errors?.length === 0,
    JSON.stringify(resizeProbe));
  const resizeCanvasFirst = decodePng(await page.locator('#swapchain-resize-probe').screenshot());
  const resizeCanvasFirstAt = (x, y) => Array.from(resizeCanvasFirst.data.slice((y * resizeCanvasFirst.w + x) * 4, (y * resizeCanvasFirst.w + x) * 4 + 4));
  check('32x24 swapchain blit presents the expected clear pixels',
    near(resizeCanvasFirstAt(1, 1), [51, 102, 204, 255], 1)
      && near(resizeCanvasFirstAt(resizeCanvasFirst.w - 2, resizeCanvasFirst.h - 2), [51, 102, 204, 255], 1),
    JSON.stringify({ size: [resizeCanvasFirst.w, resizeCanvasFirst.h], pixels: [resizeCanvasFirstAt(1, 1), resizeCanvasFirstAt(resizeCanvasFirst.w - 2, resizeCanvasFirst.h - 2)] }));
  const resizeProbeAfter = await page.evaluate(() => {
    const probe = window.__swapchainResizeProbe;
    const { gpu, canvas, command, words, colorPayload, execute } = probe;
    canvas.style.width = '96px'; canvas.style.height = '64px';
    execute([
      command(41, words(1, 9, 96, 64, 28)),
      command(27, colorPayload(2, [0, 1, 0, 1])),
      command(35, words(1, 9, 0)),
    ]);
    return { width: canvas.width, height: canvas.height, cssWidth: canvas.getBoundingClientRect().width,
      cssHeight: canvas.getBoundingClientRect().height, presents: gpu.stats.presents, errors: gpu.stats.errors };
  });
  check('ResizeBuffers updates the canvas backing store and presents again at 96x64',
    resizeProbeAfter.width === 96 && resizeProbeAfter.height === 64 && resizeProbeAfter.cssWidth === 96
      && resizeProbeAfter.cssHeight === 64 && resizeProbeAfter.presents === 2 && resizeProbeAfter.errors?.length === 0,
    JSON.stringify(resizeProbeAfter));
  const resizeCanvasSecond = decodePng(await page.locator('#swapchain-resize-probe').screenshot());
  const resizeCanvasSecondAt = (x, y) => Array.from(resizeCanvasSecond.data.slice((y * resizeCanvasSecond.w + x) * 4, (y * resizeCanvasSecond.w + x) * 4 + 4));
  check('96x64 swapchain blit replaces the old clear pixels across the resized canvas',
    near(resizeCanvasSecondAt(1, 1), [0, 255, 0, 255], 1)
      && near(resizeCanvasSecondAt(resizeCanvasSecond.w - 2, resizeCanvasSecond.h - 2), [0, 255, 0, 255], 1),
    JSON.stringify({ size: [resizeCanvasSecond.w, resizeCanvasSecond.h], pixels: [resizeCanvasSecondAt(1, 1), resizeCanvasSecondAt(resizeCanvasSecond.w - 2, resizeCanvasSecond.h - 2)] }));
  await page.evaluate(() => { window.__swapchainResizeProbe?.gpu.device.destroy(); document.getElementById('swapchain-resize-probe')?.remove(); });

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
      const coveragePixelShader = tool.compileHlsl(`
        struct PsOut { float4 color : SV_Target; uint coverage : SV_Coverage; };
        PsOut main() {
          PsOut output;
          output.color = float4(1.0, 0.0, 0.0, 1.0);
          output.coverage = 5;
          return output;
        }
      `, 'main', 'ps_5_0');
      const alphaCoveragePixelShader = tool.compileHlsl(
        'float4 main() : SV_Target { return float4(1.0, 0.0, 0.0, 0.5); }', 'main', 'ps_5_0');
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
      command(4, words(20, 8, 8, 1, 1, 28, 0x20, 0, 4, 0)); // 4x RGBA8 coverage-mask target
      command(12, words(21, 0, 20, 28, 4, 0, 1, 0, 1));
      command(27, words(21, 0, 0, 0, 0x3f800000)); // clear to black
      shader(12, 1, coveragePixelShader);
      command(17, words(1, 12));
      command(21, words(1, 21, 0));
      command(29, words(3, 0));
      command(4, words(22, 8, 8, 1, 1, 28, 0x20, 0, 1, 0)); // single-sample coverage resolve target
      command(47, words(22, 0, 20, 0, 28)); // ResolveSubresource(dst, 0, src, 0, RGBA8_UNORM)
      command(4, words(30, 8, 8, 1, 1, 29, 0x20, 0, 4, 0)); // 4x RGBA8 sRGB render target
      command(12, words(31, 0, 30, 29, 4, 0, 1, 0, 1));
      command(27, words(31, 0, 0, 0, 0x3f800000));
      shader(11, 1, pixelShader);
      command(17, words(1, 11));
      command(21, words(1, 31, 0));
      command(29, words(3, 0));
      command(4, words(32, 8, 8, 1, 1, 29, 0x20, 0, 1, 0)); // single-sample RGBA8 sRGB resolve target
      command(47, words(32, 0, 30, 0, 29)); // ResolveSubresource(dst, 0, src, 0, RGBA8_UNORM_SRGB)
      command(4, words(33, 8, 8, 1, 1, 91, 0x20, 0, 4, 0)); // 4x BGRA8 sRGB render target
      command(12, words(34, 0, 33, 91, 4, 0, 1, 0, 1));
      command(27, words(34, 0, 0, 0, 0x3f800000));
      command(21, words(1, 34, 0));
      command(29, words(3, 0));
      command(4, words(35, 8, 8, 1, 1, 91, 0x20, 0, 1, 0)); // single-sample BGRA8 sRGB resolve target
      command(47, words(35, 0, 33, 0, 91)); // ResolveSubresource(dst, 0, src, 0, BGRA8_UNORM_SRGB)
      command(4, words(40, 8, 8, 1, 1, 24, 0x20, 0, 4, 0)); // 4x R10G10B10A2 render target
      command(12, words(41, 0, 40, 24, 4, 0, 1, 0, 1));
      command(21, words(1, 41, 0));
      command(29, words(3, 0));
      command(4, words(42, 8, 8, 1, 1, 24, 0x20, 0, 1, 0)); // single-sample R10G10B10A2 resolve target
      command(47, words(42, 0, 40, 0, 24)); // ResolveSubresource(dst, 0, src, 0, R10G10B10A2_UNORM)
      command(4, words(43, 8, 8, 1, 1, 26, 0x20, 0, 4, 0)); // 4x R11G11B10_FLOAT render target
      command(12, words(44, 0, 43, 26, 4, 0, 1, 0, 1));
      command(21, words(1, 44, 0));
      command(29, words(3, 0));
      command(4, words(45, 8, 8, 1, 1, 26, 0x20, 0, 1, 0)); // single-sample R11G11B10 resolve target
      command(47, words(45, 0, 43, 0, 26)); // ResolveSubresource(dst, 0, src, 0, R11G11B10_FLOAT)
      command(4, words(23, 8, 8, 1, 1, 28, 0x20, 0, 4, 0)); // 4x RGBA8 alpha-to-coverage target
      command(12, words(24, 0, 23, 28, 4, 0, 1, 0, 1));
      command(27, words(24, 0, 0, 0x3f800000, 0x3f800000)); // clear to blue
      command(4, words(25, 8, 8, 1, 1, 28, 0x20, 0, 1, 0)); // single-sample resolve target
      const alphaCoverageBlend = new Uint8Array(268), alphaCoverageBlendView = new DataView(alphaCoverageBlend.buffer);
      alphaCoverageBlendView.setUint32(0, 13, true); // state id
      alphaCoverageBlendView.setUint32(4, 1, true); // AlphaToCoverageEnable
      alphaCoverageBlendView.setUint32(40, 0xf, true); // RenderTarget[0].RenderTargetWriteMask
      command(9, alphaCoverageBlend);
      shader(13, 1, alphaCoveragePixelShader);
      command(17, words(1, 13));
      command(21, words(1, 24, 0));
      command(22, words(1, 0, 0, 0x41000000, 0x41000000, 0, 0x3f800000)); // 8x8 viewport
      command(24, words(13, 0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000, 0xffffffff));
      command(29, words(3, 0));
      command(47, words(25, 0, 23, 0, 28)); // ResolveSubresource(dst, 0, src, 0, RGBA8_UNORM)
      command(4, words(26, 8, 8, 1, 1, 28, 0x20, 0, 4, 0)); // 4x RGBA8 sample-mask target
      command(12, words(27, 0, 26, 28, 4, 0, 1, 0, 1));
      command(27, words(27, 0, 0, 0x3f800000, 0x3f800000)); // clear to blue
      command(4, words(28, 8, 8, 1, 1, 28, 0x20, 0, 1, 0)); // single-sample resolve target
      command(21, words(1, 27, 0));
      command(24, words(0, 0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000, 0x5)); // enable samples 0 and 2
      command(29, words(3, 0));
      command(47, words(28, 0, 26, 0, 28)); // ResolveSubresource(dst, 0, src, 0, RGBA8_UNORM)
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackTexture(3);
      const bgraPixels = await gpu.readBackTexture(6);
      const float16Pixels = await gpu.readBackTexture(9);
      const coveragePixels = await gpu.readBackTexture(22);
      const rgbaSrgbPixels = await gpu.readBackTexture(32);
      const bgraSrgbPixels = await gpu.readBackTexture(35);
      const rgb10Pixels = await gpu.readBackResource(42);
      const rgb10View = new DataView(rgb10Pixels.buffer, rgb10Pixels.byteOffset, rgb10Pixels.byteLength);
      const rgb10Word = rgb10View.getUint32((4 * 8 + 4) * 4, true);
      const rgb10Pixel = [rgb10Word & 0x3ff, (rgb10Word >>> 10) & 0x3ff, (rgb10Word >>> 20) & 0x3ff, rgb10Word >>> 30]
        .map((value, channel) => value / (channel === 3 ? 3 : 1023));
      const r11Pixels = await gpu.readBackResource(45);
      const r11Word = new DataView(r11Pixels.buffer, r11Pixels.byteOffset, r11Pixels.byteLength).getUint32((4 * 8 + 4) * 4, true);
      const decodeUnsignedFloat = (bits, mantissaBits) => {
        const exponent = (bits >>> mantissaBits) & 0x1f, mantissa = bits & ((1 << mantissaBits) - 1);
        if (exponent === 0) return mantissa * 2 ** (-14 - mantissaBits);
        if (exponent === 0x1f) return mantissa === 0 ? Infinity : NaN;
        return (1 + mantissa / 2 ** mantissaBits) * 2 ** (exponent - 15);
      };
      const r11Pixel = [decodeUnsignedFloat(r11Word & 0x7ff, 6), decodeUnsignedFloat((r11Word >>> 11) & 0x7ff, 6), decodeUnsignedFloat(r11Word >>> 22, 5)];
      const alphaCoveragePixels = await gpu.readBackTexture(25);
      const alphaCoverageIndex = (4 * alphaCoveragePixels.width + 4) * 4;
      const sampleMaskPixels = await gpu.readBackTexture(28);
      const sampleMaskIndex = (4 * sampleMaskPixels.width + 4) * 4;
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
        coveragePixelsMatch: coveragePixels.data.every((value, i) => Math.abs(value - [128, 0, 0, 255][i % 4]) <= 1),
        alphaCoverageSamples: gpu.res.get(23)?.samples,
        alphaCoveragePixel: Array.from(alphaCoveragePixels.data.slice(alphaCoverageIndex, alphaCoverageIndex + 4)),
        sampleMaskSamples: gpu.res.get(26)?.samples,
        sampleMaskPixel: Array.from(sampleMaskPixels.data.slice(sampleMaskIndex, sampleMaskIndex + 4)),
        rgbaSrgbSamples: gpu.res.get(30)?.samples,
        rgbaSrgbFormat: rgbaSrgbPixels.format,
        rgbaSrgbPixelsMatch: rgbaSrgbPixels.data.every((value, i) => Math.abs(value - [137, 188, 225, 255][i % 4]) <= 1),
        bgraSrgbSamples: gpu.res.get(33)?.samples,
        bgraSrgbFormat: bgraSrgbPixels.format,
        bgraSrgbPixelsMatch: bgraSrgbPixels.data.every((value, i) => Math.abs(value - [137, 188, 225, 255][i % 4]) <= 1),
        rgb10Format: gpu.res.get(40)?.format,
        rgb10Samples: gpu.res.get(40)?.samples,
        rgb10ResolveFormat: gpu.res.get(42)?.format,
        rgb10Pixel,
        r11Format: gpu.res.get(43)?.format,
        r11Samples: gpu.res.get(43)?.samples,
        r11ResolveFormat: gpu.res.get(45)?.format,
        r11Pixel,
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
  check('4x D3D11 RGBA8/BGRA8/RGBA16F/sRGB/R10G10B10A2_UNORM/R11G11B10_FLOAT resolve pixels',
    msaaResolveResult.samples === 4 && msaaResolveResult.width === 8 && msaaResolveResult.height === 8
      && msaaResolveResult.format === 'rgba8unorm' && msaaResolveResult.allPixelsMatch
      && msaaResolveResult.bgraSamples === 4 && msaaResolveResult.bgraFormat === 'bgra8unorm'
      && msaaResolveResult.bgraPixelsMatch && msaaResolveResult.float16Samples === 4
      && msaaResolveResult.float16Format === 'rgba16float' && msaaResolveResult.float16PixelsMatch
      && msaaResolveResult.coveragePixelsMatch && msaaResolveResult.rgbaSrgbSamples === 4
      && msaaResolveResult.rgbaSrgbFormat === 'rgba8unorm-srgb' && msaaResolveResult.rgbaSrgbPixelsMatch
      && msaaResolveResult.bgraSrgbSamples === 4 && msaaResolveResult.bgraSrgbFormat === 'bgra8unorm-srgb'
      && msaaResolveResult.bgraSrgbPixelsMatch && msaaResolveResult.rgb10Format === 'rgb10a2unorm'
      && msaaResolveResult.rgb10Samples === 4 && msaaResolveResult.rgb10ResolveFormat === 'rgb10a2unorm'
      && msaaResolveResult.rgb10Pixel?.every((value, i) => Math.abs(value - [0.25, 0.5, 0.75, 1][i]) <= 0.0015)
      && msaaResolveResult.r11Format === 'rg11b10ufloat' && msaaResolveResult.r11Samples === 4
      && msaaResolveResult.r11ResolveFormat === 'rg11b10ufloat'
      && msaaResolveResult.r11Pixel?.every((value, i) => Math.abs(value - [0.25, 0.5, 0.75][i]) <= 0.001)
      && msaaResolveResult.draws === 10 && msaaResolveResult.errors?.length === 0,
    JSON.stringify(msaaResolveResult));
  check('D3D11 OMSetBlendState sample mask restricts 4x MSAA coverage',
    msaaResolveResult.sampleMaskSamples === 4
      && near(msaaResolveResult.sampleMaskPixel || [], [128, 0, 128, 191], 1)
      && msaaResolveResult.errors?.length === 0,
    JSON.stringify(msaaResolveResult));
  check('D3D11 alpha-to-coverage resolves a partial sample mask over preserved clear pixels',
    msaaResolveResult.alphaCoverageSamples === 4
      && msaaResolveResult.alphaCoveragePixel?.[0] > 0 && msaaResolveResult.alphaCoveragePixel[0] < 255
      && msaaResolveResult.alphaCoveragePixel?.[2] > 0 && msaaResolveResult.alphaCoveragePixel[2] < 255
      && msaaResolveResult.errors?.length === 0,
    JSON.stringify(msaaResolveResult));
  check('Diligent DrawMap vertex and pixel shaders validate as a WebGPU render pipeline',
    msaaResolveResult.drawMapPipelineValidated, msaaResolveResult.drawMapPipelineError || JSON.stringify(msaaResolveResult));
  const drawMapClearLuminance = msaaResolveResult.drawMapClearPixel?.slice(0, 3).reduce((sum, channel) => sum + channel, 0) ?? 0;
  const drawMapWallLuminance = msaaResolveResult.drawMapWallPixel?.slice(0, 3).reduce((sum, channel) => sum + channel, 0) ?? 0;
  check('Diligent DrawMap ray marching darkens its center for a zero-distance wall sample',
    msaaResolveResult.drawMapClearPixel?.[3] === 255 && msaaResolveResult.drawMapWallPixel?.[3] === 255
      && drawMapClearLuminance > drawMapWallLuminance,
    msaaResolveResult.drawMapRenderError || JSON.stringify({ clearSdf: msaaResolveResult.drawMapClearPixel, wallSdf: msaaResolveResult.drawMapWallPixel }));

  const multisampledDepthResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 16; canvas.height = 16;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertex = (depth) => tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, ${depth}, 1.0);
        }
      `, 'main', 'vs_5_0');
      const nearVertex = vertex(0.25), farVertex = vertex(0.75);
      const greenPixel = tool.compileHlsl('float4 main() : SV_Target { return float4(0, 1, 0, 1); }', 'main', 'ps_5_0');
      const redPixel = tool.compileHlsl('float4 main() : SV_Target { return float4(1, 0, 0, 1); }', 'main', 'ps_5_0');
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
      command(4, words(3, 16, 16, 1, 1, 28, 0x20, 0, 4, 0)); // 4x RGBA8 render target
      command(12, words(4, 0, 3, 28, 4, 0, 1, 0, 1));
      command(4, words(5, 16, 16, 1, 1, 44, 0x40, 0, 4, 0)); // 4x typeless D24S8 depth resource
      command(12, words(6, 1, 5, 45, 3, 0, 1, 0, 1)); // D24S8 DSV
      command(4, words(7, 16, 16, 1, 1, 28, 0x20, 0, 1, 0)); // single-sample resolve target
      const depthState = new Uint8Array(56), depthView = new DataView(depthState.buffer);
      depthView.setUint32(0, 8, true); depthView.setUint32(4, 1, true);
      depthView.setUint32(8, 1, true); depthView.setUint32(12, 2, true); // depth enabled, write ALL, LESS
      command(11, depthState);
      shader(9, 0, nearVertex); shader(10, 0, farVertex);
      shader(11, 1, greenPixel); shader(12, 1, redPixel);
      command(21, words(1, 4, 6));
      command(26, words(8, 0));
      command(27, words(4, 0, 0, 0, 0x3f800000));
      command(28, words(6, 3, 0x3f800000, 0)); // clear depth and stencil
      command(17, words(0, 9)); command(17, words(1, 11)); command(29, words(3, 0)); // near green
      command(17, words(0, 10)); command(17, words(1, 12)); command(29, words(3, 0)); // far red must fail
      command(47, words(7, 0, 3, 0, 28));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackTexture(7);
      const center = Array.from(pixels.data.slice((8 * pixels.width + 8) * 4, (8 * pixels.width + 8) * 4 + 4));
      const result = { center, draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 4x D24S8 depth testing keeps the nearer draw through color resolve',
    multisampledDepthResult.draws === 2 && multisampledDepthResult.errors?.length === 0
      && near(multisampledDepthResult.center || [], [0, 255, 0, 255], 1),
    JSON.stringify(multisampledDepthResult));

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

      stream.length = 0;
      const source3d = 40, destination3d = 41;
      gpu.createTexture(source3d, 4, 4, 1, 4, 28, 0, 0, 0, '3d');
      gpu.createTexture(destination3d, 4, 4, 1, 4, 28, 0, 0, 0, '3d');
      for (let z = 0; z < 4; z++) {
        const sourceSlice = new Uint8Array(4 * 4 * 4), destinationSlice = new Uint8Array(4 * 4 * 4);
        for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) {
          const offset = (y * 4 + x) * 4;
          sourceSlice.set([x + 4 * y + 16 * z, 0, 0, 255], offset);
          destinationSlice.set([9, 8, 7, 255], offset);
        }
        gpu.textureWrite(source3d, 0, z, 0, 0, 4, 4, 16, sourceSlice.length, sourceSlice);
        gpu.textureWrite(destination3d, 0, z, 0, 0, 4, 4, 16, destinationSlice.length, destinationSlice);
      }
      command(40, words(destination3d, 0, 0, 1, 1, source3d, 0, 1, 1, 1, 1, 3, 3, 3));
      gpu.execute(append());
      const copiedVolume = await gpu.readBackResource(destination3d, 0);
      const atVolume = (x, y, z) => Array.from(copiedVolume.slice(((z * 4 + y) * 4 + x) * 4, ((z * 4 + y) * 4 + x) * 4 + 4));
      result.copy3d = {
        copied: [atVolume(0, 1, 1), atVolume(1, 1, 1), atVolume(0, 2, 2), atVolume(1, 2, 2)],
        outside: [atVolume(2, 1, 1), atVolume(0, 0, 1), atVolume(0, 1, 0)],
      };

      stream.length = 0;
      const sourceBuffer = 50, destinationBuffer = 51;
      const sourceBytes = Uint8Array.from({ length: 32 }, (_, i) => i + 1);
      const destinationBytes = new Uint8Array(32).fill(0xee);
      command(2, words(sourceBuffer, sourceBytes.length, 0, 0, 0, 0));
      command(2, words(destinationBuffer, destinationBytes.length, 0, 0, 0, 0));
      const writeBuffer = (id, data) => {
        const payload = new Uint8Array(12 + data.length);
        payload.set(words(id, 0, data.length)); payload.set(data, 12); command(3, payload);
      };
      writeBuffer(sourceBuffer, sourceBytes); writeBuffer(destinationBuffer, destinationBytes);
      command(40, words(destinationBuffer, 0, 5, 0, 0, sourceBuffer, 0, 1, 7, 0, 0, 18, 1, 1));
      gpu.execute(append());
      result.copyBuffer = Array.from(await gpu.readBackResource(destinationBuffer));

      stream.length = 0;
      const overlappingBuffer = 52;
      const overlappingBytes = Uint8Array.from({ length: 32 }, (_, i) => i);
      command(2, words(overlappingBuffer, overlappingBytes.length, 0, 0, 0, 0));
      writeBuffer(overlappingBuffer, overlappingBytes);
      command(40, words(overlappingBuffer, 0, 8, 0, 0, overlappingBuffer, 0, 1, 4, 0, 0, 16, 1, 1));
      gpu.execute(append());
      result.copyBufferOverlap = Array.from(await gpu.readBackResource(overlappingBuffer));
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
  check('D3D11 CopySubresourceRegion copies a 3D box across depth and preserves untouched voxels',
    copyRegionResult.copy3d?.copied?.map((pixel) => pixel[0]).join(',') === '21,22,41,42'
      && copyRegionResult.copy3d?.outside?.every((pixel) => pixel[0] === 9 && pixel[1] === 8 && pixel[2] === 7 && pixel[3] === 255),
    JSON.stringify(copyRegionResult.copy3d));
  const expectedBufferRegion = Array(32).fill(0xee);
  expectedBufferRegion.splice(5, 11, ...Array.from({ length: 11 }, (_, i) => i + 8));
  check('D3D11 CopySubresourceRegion copies unaligned buffer byte ranges and preserves both edges',
    copyRegionResult.copyBuffer?.every((byte, i) => byte === expectedBufferRegion[i]) && copyRegionResult.errors.length === 0,
    JSON.stringify(copyRegionResult.copyBuffer));
  const expectedBufferOverlap = Array.from({ length: 32 }, (_, i) => i >= 8 && i < 20 ? i - 4 : i);
  check('D3D11.1 overlapping buffer CopySubresourceRegion uses the original source bytes',
    copyRegionResult.copyBufferOverlap?.every((byte, i) => byte === expectedBufferOverlap[i]) && copyRegionResult.errors.length === 0,
    JSON.stringify(copyRegionResult.copyBufferOverlap));
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
        SamplerState mirrorSampler : register(s2);
        SamplerState mirrorOnceSampler : register(s3);
        SamplerState borderSampler : register(s4);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 borderStraddle = image.Sample(borderSampler, float2(0.9, 0.5));
          return position.x < 10.0
            ? image.SampleLevel(wrapSampler, float2(1.25, 0.5), 0.0)
            : position.x < 20.0
              ? image.SampleLevel(mirrorSampler, float2(1.75, 0.5), 0.0)
              : position.x < 30.0
                ? image.SampleLevel(clampSampler, float2(1.25, 0.5), 0.0)
                : position.x < 40.0
                  ? image.SampleLevel(mirrorOnceSampler, float2(2.75, 0.5), 0.0)
                  : position.x < 50.0
                    ? image.SampleLevel(mirrorOnceSampler, float2(-0.75, 0.5), 0.0)
                    : position.x < 58.0
                      ? image.SampleLevel(borderSampler, float2(1.25, 0.5), 0.0)
                      : borderStraddle;
        }
      `, 'main', 'ps_5_0');
      const linearPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState linearSampler : register(s0);
        float4 main() : SV_Target { return image.SampleLevel(linearSampler, float2(0.5, 0.5), 0.0); }
      `, 'main', 'ps_5_0');
      const borderGradientPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main() : SV_Target {
          return image.SampleGrad(borderSampler, float2(0.9, 0.5), float2(0.0, 0.0), float2(0.0, 0.0));
        }
      `, 'main', 'ps_5_0');
      const borderBiasPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main() : SV_Target { return image.SampleBias(borderSampler, float2(0.9, 0.5), 0.0); }
      `, 'main', 'ps_5_0');
      const anisotropicBorderPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState anisotropicBorderSampler : register(s5);
        float4 main() : SV_Target {
          return image.SampleLevel(anisotropicBorderSampler, float2(2.0, 0.5), 0.0);
        }
      `, 'main', 'ps_5_0');
      const anisotropicBorderEdgePixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState anisotropicBorderSampler : register(s5);
        float4 main() : SV_Target {
          return image.SampleGrad(anisotropicBorderSampler, float2(0.95, 0.5), float2(0.2, 0.0), float2(0.0, 0.001));
        }
      `, 'main', 'ps_5_0');
      const anisotropicBorderMethodsPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState anisotropicBorderSampler : register(s5);
        float4 main(float4 position : SV_Position) : SV_Target {
          float localX = position.x < 32.0 ? position.x : position.x - 36.0;
          float2 coordinate = float2(0.95 + (localX - 4.5) * 0.2, 0.5);
          return position.x < 32.0
            ? image.Sample(anisotropicBorderSampler, coordinate)
            : image.SampleBias(anisotropicBorderSampler, coordinate, 0.0);
        }
      `, 'main', 'ps_5_0');
      const a8BorderPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main() : SV_Target {
          return image.SampleLevel(borderSampler, float2(2.0, 0.5), 0.0);
        }
      `, 'main', 'ps_5_0');
      const r8BorderPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 edge = image.SampleLevel(borderSampler, float2(0.0, 0.5), 0.0);
          float4 outside = image.SampleLevel(borderSampler, float2(2.0, 0.5), 0.0);
          return position.x < 32.0 ? edge : outside;
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
      const sampler = (id, addressU, filter = 0, borderColor = [0, 0, 0, 0], maxAnisotropy = 1) => {
        const data = new Uint8Array(56), view = new DataView(data.buffer);
        view.setUint32(0, id, true); view.setUint32(4, filter, true);
        view.setUint32(8, addressU, true); view.setUint32(12, 3, true); view.setUint32(16, 3, true);
        view.setUint32(24, maxAnisotropy, true); view.setUint32(28, 1, true);
        borderColor.forEach((value, index) => view.setFloat32(32 + index * 4, value, true));
        view.setFloat32(48, 0, true); view.setFloat32(52, 0, true);
        return data;
      };
      command(8, sampler(15, 1)); // wrap U
      command(8, sampler(16, 3)); // clamp U
      command(8, sampler(17, 3, 0x15)); // linear min/mag/mip, clamp U
      command(8, sampler(18, 2)); // mirror U
      command(8, sampler(19, 5, 0x15)); // mirror once U with linear filtering
      command(8, sampler(20, 4, 0x15, [0, 0, 1, 1])); // linear blue border U
      shader(7, 0, vertex); shader(8, 1, pixel); shader(9, 1, linearPixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(21, words(1, 2, 0)); command(16, words(4));
      command(19, words(1, 0, 1, 14)); command(20, words(1, 0, 5, 15, 16, 18, 19, 20));
      command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      shader(13, 1, anisotropicBorderPixel);
      command(8, sampler(26, 4, 0x55, [0.25, 0.5, 0.75, 1], 8)); // 8x anisotropic filter with BORDER U
      command(17, words(1, 13)); command(20, words(1, 5, 1, 26)); command(29, words(3, 0));
      const anisotropicSize = stream.reduce((total, part) => total + part.length, 0), anisotropicCommands = new Uint8Array(anisotropicSize);
      let anisotropicOffset = 0; for (const part of stream) { anisotropicCommands.set(part, anisotropicOffset); anisotropicOffset += part.length; }
      gpu.execute(anisotropicCommands);
      const anisotropicPixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      shader(41, 1, anisotropicBorderEdgePixel);
      command(8, sampler(27, 4, 0x55, [0.25, 0.5, 0.75, 1], 8)); // 8x anisotropic filter with BORDER U
      command(17, words(1, 41)); command(20, words(1, 5, 1, 27)); command(29, words(3, 0));
      const anisotropicEdgeSize = stream.reduce((total, part) => total + part.length, 0), anisotropicEdgeCommands = new Uint8Array(anisotropicEdgeSize);
      let anisotropicEdgeOffset = 0; for (const part of stream) { anisotropicEdgeCommands.set(part, anisotropicEdgeOffset); anisotropicEdgeOffset += part.length; }
      gpu.execute(anisotropicEdgeCommands);
      const anisotropicEdgePixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      shader(42, 1, anisotropicBorderMethodsPixel);
      command(8, sampler(28, 4, 0x55, [0.25, 0.5, 0.75, 1], 8)); // 8x anisotropic filter with BORDER U
      command(17, words(1, 42)); command(20, words(1, 5, 1, 28)); command(29, words(3, 0));
      const anisotropicMethodsSize = stream.reduce((total, part) => total + part.length, 0), anisotropicMethodsCommands = new Uint8Array(anisotropicMethodsSize);
      let anisotropicMethodsOffset = 0; for (const part of stream) { anisotropicMethodsCommands.set(part, anisotropicMethodsOffset); anisotropicMethodsOffset += part.length; }
      gpu.execute(anisotropicMethodsCommands);
      const anisotropicMethodsPixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      command(17, words(1, 9));
      command(20, words(1, 0, 1, 17));
      command(29, words(3, 0));
      const secondSize = stream.reduce((total, part) => total + part.length, 0), secondCommands = new Uint8Array(secondSize);
      let secondOffset = 0; for (const part of stream) { secondCommands.set(part, secondOffset); secondOffset += part.length; }
      gpu.execute(secondCommands);
      const linearPixels = await gpu.readBackbuffer(1);
      const at = (data, x) => Array.from(data.data.slice((32 * data.width + x) * 4, (32 * data.width + x) * 4 + 4));
      stream.length = 0;
      shader(10, 1, borderGradientPixel);
      command(17, words(1, 10)); command(20, words(1, 0, 1, 20)); command(29, words(3, 0));
      const gradientSize = stream.reduce((total, part) => total + part.length, 0), gradientCommands = new Uint8Array(gradientSize);
      let gradientOffset = 0; for (const part of stream) { gradientCommands.set(part, gradientOffset); gradientOffset += part.length; }
      gpu.execute(gradientCommands);
      const gradientPixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      shader(11, 1, borderBiasPixel);
      command(17, words(1, 11)); command(29, words(3, 0));
      const biasSize = stream.reduce((total, part) => total + part.length, 0), biasCommands = new Uint8Array(biasSize);
      let biasOffset = 0; for (const part of stream) { biasCommands.set(part, biasOffset); biasOffset += part.length; }
      gpu.execute(biasCommands);
      const biasPixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      command(4, words(21, 1, 1, 1, 1, 61, 8, 0, 1, 0)); // single-channel R8_UNORM texture
      const r8Upload = new Uint8Array(40);
      r8Upload.set(words(21, 0, 0, 0, 0, 1, 1, 4, 4));
      r8Upload[36] = 128;
      command(5, r8Upload);
      command(12, words(22, 2, 21, 61, 4, 0, 1, 0, 1));
      command(8, sampler(21, 4, 0x15, [1, 0.25, 0.5, 0.25])); // linear border with deliberately non-default missing channels
      shader(12, 1, r8BorderPixel);
      command(17, words(1, 12)); command(19, words(1, 0, 1, 22)); command(20, words(1, 0, 1, 21));
      command(29, words(3, 0));
      const r8Size = stream.reduce((total, part) => total + part.length, 0), r8Commands = new Uint8Array(r8Size);
      let r8Offset = 0; for (const part of stream) { r8Commands.set(part, r8Offset); r8Offset += part.length; }
      gpu.execute(r8Commands);
      const r8Pixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      command(4, words(23, 1, 1, 1, 1, 49, 8, 0, 1, 0)); // two-channel RG8_UNORM texture
      const rg8Upload = new Uint8Array(40);
      rg8Upload.set(words(23, 0, 0, 0, 0, 1, 1, 4, 4));
      rg8Upload[36] = 128; rg8Upload[37] = 64;
      command(5, rg8Upload);
      command(12, words(24, 2, 23, 49, 4, 0, 1, 0, 1));
      command(8, sampler(22, 4, 0x15, [1, 0.25, 0.5, 0.25]));
      command(19, words(1, 0, 1, 24)); command(20, words(1, 0, 1, 22));
      command(29, words(3, 0));
      const rg8Size = stream.reduce((total, part) => total + part.length, 0), rg8Commands = new Uint8Array(rg8Size);
      let rg8Offset = 0; for (const part of stream) { rg8Commands.set(part, rg8Offset); rg8Offset += part.length; }
      gpu.execute(rg8Commands);
      const rg8Pixels = await gpu.readBackbuffer(1);

      stream.length = 0;
      command(4, words(25, 1, 1, 1, 1, 85, 8, 0, 1, 0)); // RGB B5G6R5_UNORM texture
      const r565Upload = new Uint8Array(40);
      r565Upload.set(words(25, 0, 0, 0, 0, 1, 1, 4, 4));
      r565Upload[37] = 0xf8; // opaque red (R=31, G=0, B=0)
      command(5, r565Upload);
      command(12, words(26, 2, 25, 85, 4, 0, 1, 0, 1));
      command(8, sampler(23, 4, 0x15, [1, 0.25, 0.5, 0.25]));
      command(19, words(1, 0, 1, 26)); command(20, words(1, 0, 1, 23));
      command(29, words(3, 0));
      const r565Size = stream.reduce((total, part) => total + part.length, 0), r565Commands = new Uint8Array(r565Size);
      let r565Offset = 0; for (const part of stream) { r565Commands.set(part, r565Offset); r565Offset += part.length; }
      gpu.execute(r565Commands);
      const r565Pixels = await gpu.readBackbuffer(1);
      const bcSkipped = !gpu.features.has('texture-compression-bc');
      let bcPixels = null;
      let bc2Pixels = null;
      let bc3Pixels = null;
      let bc4Pixels = null;
      let bc5Pixels = null;
      if (!bcSkipped) {
        stream.length = 0;
        command(4, words(27, 4, 4, 1, 1, 71, 8, 0, 1, 0)); // opaque red BC1 texture
        const bcUpload = new Uint8Array(44);
        bcUpload.set(words(27, 0, 0, 0, 0, 4, 4, 8, 8));
        bcUpload[37] = 0xf8; // BC1 endpoint 0 is red; all indices select endpoint 0
        command(5, bcUpload);
        command(12, words(28, 2, 27, 71, 4, 0, 1, 0, 1));
        command(8, sampler(24, 4, 0x15, [1, 0.25, 0.5, 0.25]));
        command(19, words(1, 0, 1, 28)); command(20, words(1, 0, 1, 24));
        command(29, words(3, 0));
        const bcSize = stream.reduce((total, part) => total + part.length, 0), bcCommands = new Uint8Array(bcSize);
        let bcOffset = 0; for (const part of stream) { bcCommands.set(part, bcOffset); bcOffset += part.length; }
        gpu.execute(bcCommands);
        bcPixels = await gpu.readBackbuffer(1);

        stream.length = 0;
        command(4, words(29, 4, 4, 1, 1, 80, 8, 0, 1, 0)); // zero-valued BC4_UNORM texture
        const bc4Upload = new Uint8Array(44);
        bc4Upload.set(words(29, 0, 0, 0, 0, 4, 4, 8, 8));
        command(5, bc4Upload);
        command(12, words(30, 2, 29, 80, 4, 0, 1, 0, 1));
        command(8, sampler(25, 4, 0x15, [1, 0.25, 0.5, 0.25]));
        command(19, words(1, 0, 1, 30)); command(20, words(1, 0, 1, 25));
        command(29, words(3, 0));
        const bc4Size = stream.reduce((total, part) => total + part.length, 0), bc4Commands = new Uint8Array(bc4Size);
        let bc4Offset = 0; for (const part of stream) { bc4Commands.set(part, bc4Offset); bc4Offset += part.length; }
        gpu.execute(bc4Commands);
        bc4Pixels = await gpu.readBackbuffer(1);

        stream.length = 0;
        shader(41, 1, r8BorderPixel);
        command(4, words(33, 4, 4, 1, 1, 74, 16, 0, 1, 0)); // BC2_UNORM with half alpha
        const bc2Upload = new Uint8Array(52);
        bc2Upload.set(words(33, 0, 0, 0, 0, 4, 4, 16, 16));
        bc2Upload.fill(0x88, 36, 44); // explicit alpha 8/15 in every texel
        bc2Upload[45] = 0xf8; // BC1 color endpoint 0 is red; all indices select endpoint 0
        command(5, bc2Upload);
        command(12, words(34, 2, 33, 74, 4, 0, 1, 0, 1));
        command(8, sampler(26, 4, 0x15, [1, 0.25, 0.5, 0.25]));
        command(19, words(1, 0, 1, 34)); command(20, words(1, 0, 1, 26));
        command(29, words(3, 0));
        const bc2Size = stream.reduce((total, part) => total + part.length, 0), bc2Commands = new Uint8Array(bc2Size);
        let bc2Offset = 0; for (const part of stream) { bc2Commands.set(part, bc2Offset); bc2Offset += part.length; }
        gpu.execute(bc2Commands);
        bc2Pixels = await gpu.readBackbuffer(1);

        stream.length = 0;
        command(4, words(35, 4, 4, 1, 1, 77, 16, 0, 1, 0)); // BC3_UNORM with interpolated alpha
        const bc3Upload = new Uint8Array(52);
        bc3Upload.set(words(35, 0, 0, 0, 0, 4, 4, 16, 16));
        bc3Upload[36] = 255; bc3Upload[37] = 0; // alpha endpoints for an interpolated alpha palette
        bc3Upload.set([0x92, 0x24, 0x49, 0x92, 0x24, 0x49], 38); // every texel selects alpha palette index 2
        bc3Upload[45] = 0xf8; // BC1 color endpoint 0 is red; all indices select endpoint 0
        command(5, bc3Upload);
        command(12, words(36, 2, 35, 77, 4, 0, 1, 0, 1));
        command(8, sampler(27, 4, 0x15, [1, 0.25, 0.5, 0.25]));
        command(19, words(1, 0, 1, 36)); command(20, words(1, 0, 1, 27));
        command(29, words(3, 0));
        const bc3Size = stream.reduce((total, part) => total + part.length, 0), bc3Commands = new Uint8Array(bc3Size);
        let bc3Offset = 0; for (const part of stream) { bc3Commands.set(part, bc3Offset); bc3Offset += part.length; }
        gpu.execute(bc3Commands);
        bc3Pixels = await gpu.readBackbuffer(1);

        stream.length = 0;
        command(4, words(37, 4, 4, 1, 1, 83, 16, 0, 1, 0)); // BC5_UNORM with two constant channels
        const bc5Upload = new Uint8Array(52);
        bc5Upload.set(words(37, 0, 0, 0, 0, 4, 4, 16, 16));
        bc5Upload[36] = 128; bc5Upload[37] = 128; // red block
        bc5Upload[44] = 128; bc5Upload[45] = 128; // green block
        command(5, bc5Upload);
        command(12, words(38, 2, 37, 83, 4, 0, 1, 0, 1));
        command(8, sampler(28, 4, 0x15, [1, 0.25, 0.5, 0.25]));
        command(19, words(1, 0, 1, 38)); command(20, words(1, 0, 1, 28));
        command(29, words(3, 0));
        const bc5Size = stream.reduce((total, part) => total + part.length, 0), bc5Commands = new Uint8Array(bc5Size);
        let bc5Offset = 0; for (const part of stream) { bc5Commands.set(part, bc5Offset); bc5Offset += part.length; }
        gpu.execute(bc5Commands);
        bc5Pixels = await gpu.readBackbuffer(1);
      }

      stream.length = 0;
      command(4, words(31, 1, 1, 1, 1, 65, 8, 0, 1, 0)); // A8_UNORM texture
      const a8Upload = new Uint8Array(40);
      a8Upload.set(words(31, 0, 0, 0, 0, 1, 1, 4, 4));
      a8Upload[36] = 128;
      command(5, a8Upload);
      command(12, words(32, 2, 31, 65, 4, 0, 1, 0, 1));
      command(8, sampler(27, 4, 0x15, [1, 0.25, 0.5, 0.25]));
      shader(40, 1, a8BorderPixel);
      command(17, words(1, 40)); command(19, words(1, 0, 1, 32)); command(20, words(1, 0, 1, 27)); command(29, words(3, 0));
      const a8Size = stream.reduce((total, part) => total + part.length, 0), a8Commands = new Uint8Array(a8Size);
      let a8Offset = 0; for (const part of stream) { a8Commands.set(part, a8Offset); a8Offset += part.length; }
      gpu.execute(a8Commands);
      const a8Pixels = await gpu.readBackbuffer(1);
      const result = {
        wrap: at(pixels, 5), mirror: at(pixels, 15), clamp: at(pixels, 25), mirrorOncePositive: at(pixels, 35),
        mirrorOnceNegative: at(pixels, 45), borderOut: at(pixels, 53), borderStraddle: at(pixels, 61),
        anisotropicBorderOutside: at(anisotropicPixels, 32),
        anisotropicBorderEdge: at(anisotropicEdgePixels, 32),
        anisotropicBorderSample: at(anisotropicMethodsPixels, 4),
        anisotropicBorderSampleBias: at(anisotropicMethodsPixels, 40),
        a8BorderOutside: at(a8Pixels, 32),
        linear: at(linearPixels, 32), borderGrad: at(gradientPixels, 32), borderBias: at(biasPixels, 32),
        r8BorderEdge: at(r8Pixels, 16), r8BorderOutside: at(r8Pixels, 48),
        rg8BorderEdge: at(rg8Pixels, 16), rg8BorderOutside: at(rg8Pixels, 48),
        rgbBorderEdge: at(r565Pixels, 16), rgbBorderOutside: at(r565Pixels, 48),
        bcSkipped, bcBorderEdge: bcPixels && at(bcPixels, 16), bcBorderOutside: bcPixels && at(bcPixels, 48),
        bc2BorderEdge: bc2Pixels && at(bc2Pixels, 16), bc2BorderOutside: bc2Pixels && at(bc2Pixels, 48),
        bc3BorderEdge: bc3Pixels && at(bc3Pixels, 16), bc3BorderOutside: bc3Pixels && at(bc3Pixels, 48),
        bc4BorderEdge: bc4Pixels && at(bc4Pixels, 16), bc4BorderOutside: bc4Pixels && at(bc4Pixels, 48),
        bc5BorderEdge: bc5Pixels && at(bc5Pixels, 16), bc5BorderOutside: bc5Pixels && at(bc5Pixels, 48),
        draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 sampler modes, anisotropic BORDER, and R8/RG8/RGB/A8 border defaults match pixels',
    samplerAddressResult.draws === (samplerAddressResult.bcSkipped ? 11 : 16) && samplerAddressResult.errors.length === 0
      && near(samplerAddressResult.wrap, [255, 0, 0, 255], 1)
      && near(samplerAddressResult.mirror, [255, 0, 0, 255], 1)
      && near(samplerAddressResult.clamp, [0, 255, 0, 255], 1)
      && near(samplerAddressResult.mirrorOncePositive || [], [0, 255, 0, 255], 1)
      && near(samplerAddressResult.mirrorOnceNegative || [], [0, 255, 0, 255], 1)
      && near(samplerAddressResult.borderOut || [], [0, 0, 255, 255], 1)
      && near(samplerAddressResult.borderStraddle || [], [0, 179, 77, 255], 2)
      && near(samplerAddressResult.anisotropicBorderOutside || [], [64, 128, 191, 255], 1)
      && near(samplerAddressResult.a8BorderOutside || [], [0, 0, 0, 64], 1)
      && near(samplerAddressResult.borderGrad || [], [0, 179, 77, 255], 2)
      && near(samplerAddressResult.borderBias || [], [0, 179, 77, 255], 2)
      && near(samplerAddressResult.r8BorderEdge || [], [192, 0, 0, 255], 2)
      && near(samplerAddressResult.r8BorderOutside || [], [255, 0, 0, 255], 1)
      && near(samplerAddressResult.rg8BorderEdge || [], [192, 64, 0, 255], 2)
      && near(samplerAddressResult.rg8BorderOutside || [], [255, 64, 0, 255], 1)
      && near(samplerAddressResult.rgbBorderEdge || [], [255, 32, 64, 255], 2)
      && near(samplerAddressResult.rgbBorderOutside || [], [255, 64, 128, 255], 1)
      && (samplerAddressResult.bcSkipped || near(samplerAddressResult.bcBorderEdge || [], [255, 32, 64, 159], 2)
        && near(samplerAddressResult.bcBorderOutside || [], [255, 64, 128, 64], 1))
      && near(samplerAddressResult.linear, [128, 128, 0, 255], 1),
    JSON.stringify(samplerAddressResult));
  check('D3D11 anisotropic BORDER includes edge-crossing SampleGrad taps',
    near(samplerAddressResult.anisotropicBorderEdge || [], [33, 190, 98, 255], 3),
    JSON.stringify(samplerAddressResult.anisotropicBorderEdge));
  check('D3D11 anisotropic BORDER filters implicit Sample and SampleBias edge-crossing taps',
    near(samplerAddressResult.anisotropicBorderSample || [], [33, 190, 98, 255], 3)
      && near(samplerAddressResult.anisotropicBorderSampleBias || [], [33, 190, 98, 255], 3),
    JSON.stringify({ sample: samplerAddressResult.anisotropicBorderSample, sampleBias: samplerAddressResult.anisotropicBorderSampleBias }));
  check(`D3D11 BC1 BORDER ${samplerAddressResult.bcSkipped ? 'adapter capability check (compressed sampling skipped)' : 'edge and outside pixels'}`,
    samplerAddressResult.bcSkipped || near(samplerAddressResult.bcBorderEdge || [], [255, 32, 64, 159], 2)
      && near(samplerAddressResult.bcBorderOutside || [], [255, 64, 128, 64], 1),
    JSON.stringify(samplerAddressResult));
  check(`D3D11 BC4 BORDER ${samplerAddressResult.bcSkipped ? 'adapter capability check (compressed sampling skipped)' : 'single-channel defaults'}`,
    samplerAddressResult.bcSkipped || near(samplerAddressResult.bc4BorderEdge || [], [128, 0, 0, 255], 2)
      && near(samplerAddressResult.bc4BorderOutside || [], [255, 0, 0, 255], 1),
    JSON.stringify(samplerAddressResult));
  check(`D3D11 BC2_UNORM BORDER ${samplerAddressResult.bcSkipped ? 'adapter capability check (compressed sampling skipped)' : 'explicit alpha pixels'}`,
    samplerAddressResult.bcSkipped || near(samplerAddressResult.bc2BorderEdge || [], [255, 32, 64, 100], 2)
      && near(samplerAddressResult.bc2BorderOutside || [], [255, 64, 128, 64], 1),
    JSON.stringify(samplerAddressResult));
  check(`D3D11 BC3_UNORM BORDER ${samplerAddressResult.bcSkipped ? 'adapter capability check (compressed sampling skipped)' : 'block alpha pixels'}`,
    samplerAddressResult.bcSkipped || near(samplerAddressResult.bc3BorderEdge || [], [255, 32, 64, 141], 2)
      && near(samplerAddressResult.bc3BorderOutside || [], [255, 64, 128, 64], 1),
    JSON.stringify(samplerAddressResult));
  check(`D3D11 BC5_UNORM BORDER ${samplerAddressResult.bcSkipped ? 'adapter capability check (compressed sampling skipped)' : 'two-channel defaults'}`,
    samplerAddressResult.bcSkipped || near(samplerAddressResult.bc5BorderEdge || [], [192, 96, 0, 255], 2)
      && near(samplerAddressResult.bc5BorderOutside || [], [255, 64, 0, 255], 1),
    JSON.stringify(samplerAddressResult));

  const gatherBorderResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const logs = [];
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
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
        SamplerState borderSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 edge = image.GatherRed(borderSampler, float2(0.1, 0.5));
          float4 offset = image.GatherRed(borderSampler, float2(0.1, 0.5), int2(1, 0));
          float4 outside = image.GatherRed(borderSampler, float2(2.0, 0.5));
          return position.x < 21.0 ? edge : position.x < 43.0 ? offset : outside;
        }
      `, 'main', 'ps_5_0');
      const comparisonPixel = tool.compileHlsl(`
        Texture2D<float> depthImage : register(t0);
        SamplerComparisonState borderSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 edge = depthImage.GatherCmp(borderSampler, float2(0.1, 0.5), 0.6);
          float4 outside = depthImage.GatherCmp(borderSampler, float2(2.0, 0.5), 0.6);
          return position.x < 32.0 ? edge : outside;
        }
      `, 'main', 'ps_5_0');
      const arrayPixel = tool.compileHlsl(`
        Texture2DArray<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 edge = image.GatherRed(borderSampler, float3(0.1, 0.5, 1.0));
          float4 outside = image.GatherRed(borderSampler, float3(2.0, 0.5, 1.0));
          return position.x < 32.0 ? edge : outside;
        }
      `, 'main', 'ps_5_0');
      const alphaGatherPixel = tool.compileHlsl(`
        Texture2D<float4> image : register(t0);
        SamplerState borderSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float4 edge = image.GatherAlpha(borderSampler, float2(0.1, 0.5));
          float4 outside = image.GatherAlpha(borderSampler, float2(2.0, 0.5));
          return position.x < 32.0 ? edge : outside;
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
      command(4, words(10, 2, 2, 1, 1, 28, 8, 0, 1, 0));
      const upload = new Uint8Array(52);
      upload.set(words(10, 0, 0, 0, 0, 2, 2, 8, 16));
      upload.set([64, 0, 0, 255, 128, 0, 0, 255, 192, 0, 0, 255, 255, 0, 0, 255], 36);
      command(5, upload);
      command(12, words(11, 2, 10, 28, 4, 0, 1, 0, 1));
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 12, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 4, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setUint32(24, 1, true); samplerView.setUint32(28, 1, true);
      samplerView.setFloat32(32, 0.125, true); samplerView.setFloat32(36, 0.25, true);
      samplerView.setFloat32(40, 0.5, true); samplerView.setFloat32(44, 0.75, true);
      command(8, sampler);
      shader(20, 0, vertex); shader(21, 1, pixel);
      command(17, words(0, 20)); command(17, words(1, 21));
      command(21, words(1, 2, 0)); command(16, words(4));
      command(19, words(1, 0, 1, 11)); command(20, words(1, 0, 1, 12));
      command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const at = (x) => Array.from(pixels.data.slice((32 * pixels.width + x) * 4, (32 * pixels.width + x) * 4 + 4));
      const edge = at(16), offsetPixels = at(32), outside = at(53);
      stream.length = 0;
      command(4, words(13, 2, 2, 1, 1, 40, 0x48, 0, 1, 0)); // D32_FLOAT depth + SRV
      command(12, words(14, 1, 13, 40, 3, 0, 1, 0, 1)); // depth-stencil view
      command(12, words(15, 2, 13, 40, 4, 0, 1, 0, 1)); // shader-resource view
      command(28, words(14, 1, 0x3f000000, 0)); // depth 0.5
      const comparisonSampler = new Uint8Array(56), comparisonView = new DataView(comparisonSampler.buffer);
      comparisonView.setUint32(0, 32, true); comparisonView.setUint32(4, 0x80, true);
      comparisonView.setUint32(8, 4, true); comparisonView.setUint32(12, 3, true); comparisonView.setUint32(16, 3, true);
      comparisonView.setUint32(24, 1, true); comparisonView.setUint32(28, 2, true); // comparison LESS
      comparisonView.setFloat32(32, 0.9, true);
      command(8, comparisonSampler);
      shader(22, 1, comparisonPixel);
      command(17, words(1, 22)); command(19, words(1, 0, 1, 15)); command(20, words(1, 0, 1, 32));
      command(29, words(3, 0));
      const comparisonSize = stream.reduce((total, part) => total + part.length, 0), comparisonCommands = new Uint8Array(comparisonSize);
      let comparisonOffset = 0; for (const part of stream) { comparisonCommands.set(part, comparisonOffset); comparisonOffset += part.length; }
      gpu.execute(comparisonCommands);
      const comparisonPixels = await gpu.readBackbuffer(1);
      const compareAt = (x) => Array.from(comparisonPixels.data.slice((32 * comparisonPixels.width + x) * 4, (32 * comparisonPixels.width + x) * 4 + 4));
      const comparisonEdge = compareAt(16), comparisonOutside = compareAt(48);
      stream.length = 0;
      command(4, words(16, 2, 2, 1, 2, 28, 8, 0, 1, 0)); // two-slice RGBA8 array texture
      command(12, words(17, 2, 16, 28, 5, 0, 1, 0, 2)); // both array slices
      const uploadSlice = (slice, texels) => {
        const payload = new Uint8Array(52);
        payload.set(words(16, 0, slice, 0, 0, 2, 2, 8, 16));
        payload.set(texels, 36);
        command(5, payload);
      };
      uploadSlice(0, [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
      uploadSlice(1, [64, 0, 0, 255, 128, 0, 0, 255, 192, 0, 0, 255, 255, 0, 0, 255]);
      shader(23, 1, arrayPixel);
      command(17, words(1, 23)); command(19, words(1, 0, 1, 17)); command(20, words(1, 0, 1, 12));
      command(29, words(3, 0));
      const arraySize = stream.reduce((total, part) => total + part.length, 0), arrayCommands = new Uint8Array(arraySize);
      let arrayOffset = 0; for (const part of stream) { arrayCommands.set(part, arrayOffset); arrayOffset += part.length; }
      gpu.execute(arrayCommands);
      const arrayPixels = await gpu.readBackbuffer(1);
      const arrayAt = (x) => Array.from(arrayPixels.data.slice((32 * arrayPixels.width + x) * 4, (32 * arrayPixels.width + x) * 4 + 4));
      stream.length = 0;
      command(4, words(24, 2, 2, 1, 1, 61, 8, 0, 1, 0)); // R8_UNORM texture
      const r8Upload = new Uint8Array(40);
      r8Upload.set(words(24, 0, 0, 0, 0, 2, 2, 2, 4));
      r8Upload.set([32, 64, 96, 128], 36);
      command(5, r8Upload);
      command(12, words(25, 2, 24, 61, 4, 0, 1, 0, 1));
      shader(26, 1, alphaGatherPixel);
      command(17, words(1, 26)); command(19, words(1, 0, 1, 25)); command(20, words(1, 0, 1, 12));
      command(29, words(3, 0));
      const alphaSize = stream.reduce((total, part) => total + part.length, 0), alphaCommands = new Uint8Array(alphaSize);
      let alphaOffset = 0; for (const part of stream) { alphaCommands.set(part, alphaOffset); alphaOffset += part.length; }
      gpu.execute(alphaCommands);
      const alphaPixels = await gpu.readBackbuffer(1);
      const alphaAt = (x) => Array.from(alphaPixels.data.slice((32 * alphaPixels.width + x) * 4, (32 * alphaPixels.width + x) * 4 + 4));
      const result = {
        edge, offsetPixels, outside, comparisonEdge, comparisonOutside, arrayEdge: arrayAt(16), arrayOutside: arrayAt(48),
        r8AlphaEdge: alphaAt(16), r8AlphaOutside: alphaAt(48),
        draws: gpu.stats.draws, errors: gpu.stats.errors, logs,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
      check('D3D11 Gather applies BORDER per tap at edge/outside footprints and honors explicit offsets',
    gatherBorderResult.draws === 4 && gatherBorderResult.errors.length === 0
      && near([...gatherBorderResult.edge].sort((a, b) => a - b), [32, 32, 64, 191], 1)
      && near([...gatherBorderResult.offsetPixels].sort((a, b) => a - b), [64, 128, 192, 255], 1)
      && near(gatherBorderResult.outside || [], [32, 32, 32, 32], 1)
      && near([...gatherBorderResult.comparisonEdge].sort((a, b) => a - b), [0, 0, 255, 255], 1)
      && near(gatherBorderResult.comparisonOutside || [], [255, 255, 255, 255], 1)
      && near([...gatherBorderResult.arrayEdge].sort((a, b) => a - b), [32, 32, 64, 191], 1)
      && near(gatherBorderResult.arrayOutside || [], [32, 32, 32, 32], 1)
      && near(gatherBorderResult.r8AlphaEdge || [], [255, 255, 255, 255], 1)
      && near(gatherBorderResult.r8AlphaOutside || [], [255, 255, 255, 255], 1),
    JSON.stringify(gatherBorderResult));

  const comparisonSamplerResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 64; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const logs = [];
      const gpu = new GpuExecutor(canvas, tool, (message) => logs.push(message), { noPresent: true });
      await gpu.init();
      const vertex = tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        Texture2D<float> depthImage : register(t0);
        SamplerComparisonState neverSampler : register(s0);
        SamplerComparisonState lessSampler : register(s1);
        SamplerComparisonState equalSampler : register(s2);
        SamplerComparisonState lessEqualSampler : register(s3);
        SamplerComparisonState greaterSampler : register(s4);
        SamplerComparisonState notEqualSampler : register(s5);
        SamplerComparisonState greaterEqualSampler : register(s6);
        SamplerComparisonState alwaysSampler : register(s7);
        SamplerComparisonState neverLinearSampler : register(s8);
        SamplerComparisonState lessLinearSampler : register(s9);
        SamplerComparisonState equalLinearSampler : register(s10);
        SamplerComparisonState lessEqualLinearSampler : register(s11);
        SamplerComparisonState greaterLinearSampler : register(s12);
        SamplerComparisonState notEqualLinearSampler : register(s13);
        SamplerComparisonState greaterEqualLinearSampler : register(s14);
        SamplerComparisonState alwaysLinearSampler : register(s15);
        float4 main(float4 position : SV_Position) : SV_Target {
          float reference = position.y < 16.0 ? 0.4 : 0.6;
          float2 coordinate = position.y < 16.0 ? float2(0.5, 0.5) : float2(2.0, 0.5);
          float levelZeroResult;
          if (position.x < 8.0) levelZeroResult = depthImage.SampleCmpLevelZero(neverSampler, coordinate, reference);
          else if (position.x < 16.0) levelZeroResult = depthImage.SampleCmpLevelZero(lessSampler, coordinate, reference);
          else if (position.x < 24.0) levelZeroResult = depthImage.SampleCmpLevelZero(equalSampler, coordinate, reference);
          else if (position.x < 32.0) levelZeroResult = depthImage.SampleCmpLevelZero(lessEqualSampler, coordinate, reference);
          else if (position.x < 40.0) levelZeroResult = depthImage.SampleCmpLevelZero(greaterSampler, coordinate, reference);
          else if (position.x < 48.0) levelZeroResult = depthImage.SampleCmpLevelZero(notEqualSampler, coordinate, reference);
          else if (position.x < 56.0) levelZeroResult = depthImage.SampleCmpLevelZero(greaterEqualSampler, coordinate, reference);
          else levelZeroResult = depthImage.SampleCmpLevelZero(alwaysSampler, coordinate, reference);

          float2 edgeCoordinate = float2(0.9, 0.5);
          float2 outsideCoordinate = float2(2.0, 0.5);
          float neverEdge = depthImage.SampleCmp(neverLinearSampler, edgeCoordinate, 0.6);
          float lessEdge = depthImage.SampleCmp(lessLinearSampler, edgeCoordinate, 0.6);
          float equalEdge = depthImage.SampleCmp(equalLinearSampler, edgeCoordinate, 0.6);
          float lessEqualEdge = depthImage.SampleCmp(lessEqualLinearSampler, edgeCoordinate, 0.6);
          float greaterEdge = depthImage.SampleCmp(greaterLinearSampler, edgeCoordinate, 0.6);
          float notEqualEdge = depthImage.SampleCmp(notEqualLinearSampler, edgeCoordinate, 0.6);
          float greaterEqualEdge = depthImage.SampleCmp(greaterEqualLinearSampler, edgeCoordinate, 0.6);
          float alwaysEdge = depthImage.SampleCmp(alwaysLinearSampler, edgeCoordinate, 0.6);
          float neverOutside = depthImage.SampleCmp(neverLinearSampler, outsideCoordinate, 0.6);
          float lessOutside = depthImage.SampleCmp(lessLinearSampler, outsideCoordinate, 0.6);
          float equalOutside = depthImage.SampleCmp(equalLinearSampler, outsideCoordinate, 0.6);
          float lessEqualOutside = depthImage.SampleCmp(lessEqualLinearSampler, outsideCoordinate, 0.6);
          float greaterOutside = depthImage.SampleCmp(greaterLinearSampler, outsideCoordinate, 0.6);
          float notEqualOutside = depthImage.SampleCmp(notEqualLinearSampler, outsideCoordinate, 0.6);
          float greaterEqualOutside = depthImage.SampleCmp(greaterEqualLinearSampler, outsideCoordinate, 0.6);
          float alwaysOutside = depthImage.SampleCmp(alwaysLinearSampler, outsideCoordinate, 0.6);

          float sampleCmpEdge;
          float sampleCmpOutside;
          if (position.x < 8.0) { sampleCmpEdge = neverEdge; sampleCmpOutside = neverOutside; }
          else if (position.x < 16.0) { sampleCmpEdge = lessEdge; sampleCmpOutside = lessOutside; }
          else if (position.x < 24.0) { sampleCmpEdge = equalEdge; sampleCmpOutside = equalOutside; }
          else if (position.x < 32.0) { sampleCmpEdge = lessEqualEdge; sampleCmpOutside = lessEqualOutside; }
          else if (position.x < 40.0) { sampleCmpEdge = greaterEdge; sampleCmpOutside = greaterOutside; }
          else if (position.x < 48.0) { sampleCmpEdge = notEqualEdge; sampleCmpOutside = notEqualOutside; }
          else if (position.x < 56.0) { sampleCmpEdge = greaterEqualEdge; sampleCmpOutside = greaterEqualOutside; }
          else { sampleCmpEdge = alwaysEdge; sampleCmpOutside = alwaysOutside; }

          float result = position.y < 32.0 ? levelZeroResult : position.y < 48.0 ? sampleCmpEdge : sampleCmpOutside;
          return float4(result, 0.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');
      const anisotropicComparisonPixel = tool.compileHlsl(`
        Texture2D<float> depthImage : register(t0);
        SamplerComparisonState anisotropicLessSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float2 coordinate = float2(0.95 + (position.x - 4.5) * 0.2, 0.5);
          float result = depthImage.SampleCmp(anisotropicLessSampler, coordinate, 0.4);
          return float4(result, 0.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');
      const anisotropicComparisonLevelZeroPixel = tool.compileHlsl(`
        Texture2D<float> depthImage : register(t0);
        SamplerComparisonState anisotropicLessSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float2 coordinate = float2(0.95 + (position.x - 4.5) * 0.2, 0.5);
          float result = depthImage.SampleCmpLevelZero(anisotropicLessSampler, coordinate, 0.4);
          return float4(result, 0.0, 0.0, 1.0);
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
      command(4, words(20, 1, 1, 1, 1, 40, 0x48, 0, 1, 0)); // D32_FLOAT depth + SRV
      command(12, words(21, 1, 20, 40, 3, 0, 1, 0, 1)); // depth-stencil view
      command(12, words(22, 2, 20, 40, 4, 0, 1, 0, 1)); // shader-resource view
      command(28, words(21, 1, 0x3f000000, 0)); // clear stored depth to 0.5
      const samplerIds = [];
      const borderValues = [0.25, 0.75, 0.6, 0.75, 0.75, 0.6, 0.75, 0.25];
      for (let register = 0; register < 16; register++) {
        const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
        const comparison = register % 8 + 1;
        const samplerId = 30 + register;
        samplerIds.push(samplerId);
        samplerView.setUint32(0, samplerId, true);
        samplerView.setUint32(4, register < 8 ? 0x80 : 0x95, true); // point level-zero samplers, linear implicit-LOD samplers
        samplerView.setUint32(8, 4, true);
        samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
        samplerView.setUint32(24, 1, true); samplerView.setUint32(28, comparison, true);
        samplerView.setFloat32(32, borderValues[comparison - 1], true);
        samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 0, true);
        command(8, sampler);
      }
      shader(24, 0, vertex); shader(25, 1, pixel);
      command(17, words(0, 24)); command(17, words(1, 25));
      command(21, words(1, 2, 0)); command(16, words(4));
      command(19, words(1, 0, 1, 22)); command(20, words(1, 0, samplerIds.length, ...samplerIds));
      command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const at = (data, x, y) => Array.from(data.data.slice((y * data.width + x) * 4, (y * data.width + x) * 4 + 4));
      const readComparisonRow = (y) => Array.from({ length: 8 }, (_, i) => at(pixels, 4 + i * 8, y)[0]);
      stream.length = 0;
      const anisotropicSampler = new Uint8Array(56), anisotropicSamplerView = new DataView(anisotropicSampler.buffer);
      anisotropicSamplerView.setUint32(0, 46, true);
      anisotropicSamplerView.setUint32(4, 0xd5, true); // 8x anisotropic comparison, linear filters
      anisotropicSamplerView.setUint32(8, 4, true); anisotropicSamplerView.setUint32(12, 3, true); anisotropicSamplerView.setUint32(16, 3, true);
      anisotropicSamplerView.setUint32(24, 8, true); anisotropicSamplerView.setUint32(28, 2, true); // max anisotropy 8, LESS comparison
      anisotropicSamplerView.setFloat32(32, 0.25, true); // BORDER depth
      command(8, anisotropicSampler);
      shader(26, 1, anisotropicComparisonPixel);
      command(17, words(1, 26)); command(20, words(1, 0, 1, 46)); command(29, words(3, 0));
      const anisotropicSize = stream.reduce((total, part) => total + part.length, 0), anisotropicCommands = new Uint8Array(anisotropicSize);
      let anisotropicOffset = 0; for (const part of stream) { anisotropicCommands.set(part, anisotropicOffset); anisotropicOffset += part.length; }
      gpu.execute(anisotropicCommands);
      const anisotropicPixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      shader(27, 1, anisotropicComparisonLevelZeroPixel);
      command(17, words(1, 27)); command(29, words(3, 0));
      const anisotropicLevelZeroSize = stream.reduce((total, part) => total + part.length, 0), anisotropicLevelZeroCommands = new Uint8Array(anisotropicLevelZeroSize);
      let anisotropicLevelZeroOffset = 0; for (const part of stream) { anisotropicLevelZeroCommands.set(part, anisotropicLevelZeroOffset); anisotropicLevelZeroOffset += part.length; }
      gpu.execute(anisotropicLevelZeroCommands);
      const anisotropicLevelZeroPixels = await gpu.readBackbuffer(1);
      const result = {
        levelZeroInside: readComparisonRow(8), levelZeroOutside: readComparisonRow(24),
        sampleCmpEdge: readComparisonRow(40), sampleCmpOutside: readComparisonRow(56),
        anisotropicSampleCmp: at(anisotropicPixels, 4, 32),
        anisotropicSampleCmpLevelZero: at(anisotropicLevelZeroPixels, 4, 32),
        draws: gpu.stats.draws, errors: gpu.stats.errors, logs,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 comparison samplers apply all eight compare functions to point and linear BORDER taps',
    comparisonSamplerResult.draws === 3 && comparisonSamplerResult.errors.length === 0
      && comparisonSamplerResult.levelZeroInside?.every((v, i) => v === [0, 255, 0, 255, 0, 255, 0, 255][i])
      && comparisonSamplerResult.levelZeroOutside?.every((v, i) => v === [0, 255, 255, 255, 0, 0, 0, 255][i])
      && comparisonSamplerResult.sampleCmpEdge?.every((v, i) => v === [0, 102, 102, 102, 153, 153, 153, 255][i])
      && comparisonSamplerResult.sampleCmpOutside?.every((v, i) => v === [0, 255, 255, 255, 0, 0, 0, 255][i])
      && near(comparisonSamplerResult.anisotropicSampleCmp || [], [110, 0, 0, 255], 3)
      && near(comparisonSamplerResult.anisotropicSampleCmpLevelZero || [], [110, 0, 0, 255], 3),
    JSON.stringify(comparisonSamplerResult));

  const depthCompareResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 192; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const vertices = [0.25, 0.5, 0.75].map((depth) => tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, ${depth.toFixed(2)}, 1.0);
        }
      `, 'main', 'vs_5_0'));
      const pixel = tool.compileHlsl('float4 main() : SV_Target { return 1.0; }', 'main', 'ps_5_0');
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
      command(38, words(1, 0, 9, 192, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(20, 192, 64, 1, 1, 40, 0x48, 0, 1, 0));
      command(12, words(21, 1, 20, 40, 3, 0, 1, 0, 1));
      command(21, words(1, 2, 21));
      command(27, words(2, 0, 0, 0, 0x3f800000));
      command(28, words(21, 1, 0x3f000000, 0));
      shader(7, 0, vertices[0]); shader(8, 1, pixel); shader(9, 0, vertices[1]); shader(10, 0, vertices[2]);
      command(17, words(0, 7)); command(17, words(1, 8)); command(16, words(4));
      for (let compare = 1; compare <= 8; compare++) {
        const state = new Uint8Array(56), view = new DataView(state.buffer);
        view.setUint32(0, 30 + compare, true);
        view.setUint32(4, 1, true); // depth enabled
        view.setUint32(8, 0, true); // preserve the cleared reference depth
        view.setUint32(12, compare, true); // D3D11_COMPARISON_FUNC
        command(11, state);
      }
      for (let compare = 1; compare <= 8; compare++) {
        command(26, words(30 + compare, 0));
        for (let value = 0; value < 3; value++) {
          const viewport = new Uint8Array(28), view = new DataView(viewport.buffer);
          view.setUint32(0, 1, true);
          view.setFloat32(4, (compare - 1) * 24 + value * 8, true);
          view.setFloat32(8, 0, true);
          view.setFloat32(12, 8, true);
          view.setFloat32(16, 64, true);
          view.setFloat32(20, 0, true);
          view.setFloat32(24, 1, true);
          command(22, viewport);
          command(17, words(0, [7, 9, 10][value]));
          command(29, words(3, 0));
        }
      }
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const bands = Array.from({ length: 8 }, (_, compare) => Array.from({ length: 3 }, (_, value) =>
        pixels.data[(32 * pixels.width + compare * 24 + value * 8 + 4) * 4]));
      const result = { bands, draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  const depthComparePixels = [
    [0, 0, 0], [255, 0, 0], [0, 255, 0], [255, 255, 0],
    [0, 0, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
  ];
  check('D3D11 depth state implements all eight compare functions below, at, and above equal depth',
    depthCompareResult.draws === 24 && depthCompareResult.errors.length === 0
      && depthCompareResult.bands?.every((row, compare) => row.length === 3
        && row.every((pixel, value) => pixel === depthComparePixels[compare][value])),
    JSON.stringify(depthCompareResult));

  const depthWriteMaskResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 96; canvas.height = 64;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const makeVertex = (z) => tool.compileHlsl(`
        float4 main(uint vertexId : SV_VertexID) : SV_Position {
          float x = vertexId == 2 ? 3.0 : -1.0;
          float y = vertexId == 1 ? 3.0 : -1.0;
          return float4(x, y, ${z}, 1.0);
        }
      `, 'main', 'vs_5_0');
      const redVertex = makeVertex('0.25'), blueVertex = makeVertex('0.75');
      const redPixel = tool.compileHlsl('float4 main() : SV_Target { return float4(1, 0, 0, 1); }', 'main', 'ps_5_0');
      const bluePixel = tool.compileHlsl('float4 main() : SV_Target { return float4(0, 0, 1, 1); }', 'main', 'ps_5_0');
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
      const floatBits = (value) => {
        const data = new DataView(new ArrayBuffer(4)); data.setFloat32(0, value, true); return data.getUint32(0, true);
      };
      command(38, words(1, 0, 9, 96, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 96, 64, 1, 1, 40, 0x40, 0, 1, 0)); // D32_FLOAT depth
      command(12, words(4, 1, 3, 40, 3, 0, 1, 0, 1)); // DSV
      command(21, words(1, 2, 4));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 12, true); rasterView.setUint32(4, 3, true); rasterView.setUint32(8, 1, true);
      rasterView.setUint32(28, 1, true); // depth clip enabled
      rasterView.setUint32(32, 1, true); // scissor enabled
      command(10, raster);
      command(25, words(12));
      const depthState = (id, enabled, writeMask) => {
        const payload = new Uint8Array(56), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setUint32(4, Number(enabled), true);
        view.setUint32(8, writeMask, true); view.setUint32(12, 2, true); // LESS
        command(11, payload);
      };
      depthState(10, false, 0); // depth disabled control
      depthState(11, true, 0); // DepthWriteMask = ZERO
      depthState(12, true, 1); // DepthWriteMask = ALL
      shader(7, 0, redVertex); shader(8, 0, blueVertex);
      shader(9, 1, redPixel); shader(10, 1, bluePixel);
      command(16, words(4));
      command(27, words(2, 0, 0, 0, 0x3f800000));
      command(28, words(4, 1, 0x3f800000, 0)); // clear depth to 1.0
      command(22, words(1, 0, 0, 0x42c00000, 0x42800000, 0, 0x3f800000)); // 96x64 viewport
      const draw = (depthId, left, right) => {
        command(26, words(depthId, 0));
        command(23, words(1, left, 0, right, 64));
        command(17, words(0, 7)); command(17, words(1, 9)); command(29, words(3, 0)); // nearer red
        command(17, words(0, 8)); command(17, words(1, 10)); command(29, words(3, 0)); // farther blue
      };
      command(26, words(10, 0));
      command(23, words(1, 0, 0, 32, 64));
      command(17, words(0, 8)); command(17, words(1, 10)); command(29, words(3, 0)); // blue-only pipeline control
      draw(11, 32, 64);
      draw(12, 64, 96);
      const size = stream.reduce((n, part) => n + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => Array.from(pixels.data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
      const result = {
        depthDisabled: at(16, 32), disabledWrite: at(48, 32), enabledWrite: at(80, 32), draws: gpu.stats.draws,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 depth write mask controls whether a nearer fragment occludes later draws',
    depthWriteMaskResult.draws === 5 && depthWriteMaskResult.errors.length === 0
      && near(depthWriteMaskResult.depthDisabled || [], [0, 0, 255, 255], 1)
      && near(depthWriteMaskResult.disabledWrite || [], [0, 0, 255, 255], 1)
      && near(depthWriteMaskResult.enabledWrite || [], [255, 0, 0, 255], 1),
    JSON.stringify(depthWriteMaskResult));

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
      texel.set([255, 0, 0, 128, 0, 255, 0, 128, 0, 0, 255, 128, 255, 255, 255, 128], 36);
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
        view.setUint32(offset + 16, 0x40ff0000, true); // quarter-alpha red D3DCOLOR
        view.setFloat32(offset + 20, u, true); view.setFloat32(offset + 24, v, true);
      });
      const upload = new Uint8Array(12 + vertices.length);
      upload.set(words(11, 0, vertices.length)); upload.set(vertices, 12); command(3, upload);
      command(13, words(0, 1, 11, 28, 0));
      command(16, words(4)); command(29, words(3, 0));
      gpu.execute(append());
      const selectedPixels = await gpu.readBackbuffer(1);
      const selectedAt = (20 * selectedPixels.width + 32) * 4;

      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0, true); clearView.setFloat32(8, 0, true);
      clearView.setFloat32(12, 0, true); clearView.setFloat32(16, 1, true);
      const renderOperation = async (colorOp, setup = null, alphaOp = 2, textureFactor = 0xffffffff, colorArg0 = 1, alphaArg0 = 1) => {
        stream.length = 0;
        setup?.();
        command(27, clear);
        command(45, words(0x144, 64, 64, colorOp, 2, 1, alphaOp, 2, 1, textureFactor, colorArg0, alphaArg0));
        command(16, words(4)); command(29, words(3, 0));
        gpu.execute(append());
        const pixels = await gpu.readBackbuffer(1);
        const at = (20 * pixels.width + 32) * 4;
        return Array.from(pixels.data.slice(at, at + 4));
      };
      const modulatedPixel = await renderOperation(4);
      const selectedArg2Pixel = await renderOperation(3);
      const modulated2xPixel = await renderOperation(5);
      const addedPixel = await renderOperation(7);
      const subtractedPixel = await renderOperation(10);
      const addSmoothPixel = await renderOperation(11);
      const blendedDiffuseAlphaPixel = await renderOperation(12);
      const blendedTextureAlphaPixel = await renderOperation(13);
      const blendedTextureAlphaPmPixel = await renderOperation(15);
      const blendedCurrentAlphaPixel = await renderOperation(16);
      const blendFactorAlphaPixel = await renderOperation(14, () => command(48, words(60, 0x40ffffff)), 2, 0x40ffffff);
      const blendFactorAlphaHighPixel = await renderOperation(14, () => command(48, words(60, 0xc0ffffff)), 2, 0xc0ffffff);
      const signedVertices = vertices.slice(), signedView = new DataView(signedVertices.buffer);
      for (let i = 0; i < 3; i++) signedView.setUint32(i * 28 + 16, 0x33333333, true);
      const signedUpload = new Uint8Array(12 + signedVertices.length);
      signedUpload.set(words(11, 0, signedVertices.length)); signedUpload.set(signedVertices, 12);
      const signedTextureUpload = new Uint8Array(36 + 16);
      signedTextureUpload.set(words(13, 0, 0, 0, 0, 2, 2, 8, 16));
      signedTextureUpload.set([115, 115, 115, 128, 115, 115, 115, 128, 115, 115, 115, 128, 115, 115, 115, 128], 36);
      const setupSignedArgs = () => { command(3, signedUpload); command(5, signedTextureUpload); };
      const addSignedPixel = await renderOperation(8, setupSignedArgs);
      const addSigned2xPixel = await renderOperation(9, setupSignedArgs);
      const signedAlphaTextureUpload = new Uint8Array(36 + 16);
      signedAlphaTextureUpload.set(words(13, 0, 0, 0, 0, 2, 2, 8, 16));
      signedAlphaTextureUpload.set([115, 115, 115, 192, 115, 115, 115, 192, 115, 115, 115, 192, 115, 115, 115, 192], 36);
      const setupSignedAlphaArgs = () => command(5, signedAlphaTextureUpload);
      const alphaBlendDiffusePixel = await renderOperation(2, setupSignedAlphaArgs, 12);
      const alphaBlendTexturePixel = await renderOperation(2, setupSignedAlphaArgs, 13);
      const setupAlphaBlendFactor = () => {
        setupSignedAlphaArgs();
        command(48, words(60, 0x40ffffff));
      };
      const alphaBlendFactorPixel = await renderOperation(2, setupAlphaBlendFactor, 14, 0x40ffffff);
      const alphaBlendTexturePmPixel = await renderOperation(2, setupSignedAlphaArgs, 15);
      const alphaBlendCurrentPixel = await renderOperation(2, setupSignedAlphaArgs, 16);
      const alphaModulated2xPixel = await renderOperation(2, setupSignedAlphaArgs, 5);
      const alphaModulated4xPixel = await renderOperation(2, setupSignedAlphaArgs, 6);
      const alphaAddedPixel = await renderOperation(2, setupSignedAlphaArgs, 7);
      const alphaAddSignedPixel = await renderOperation(2, setupSignedAlphaArgs, 8);
      const alphaAddSigned2xPixel = await renderOperation(2, setupSignedAlphaArgs, 9);
      const alphaSubtractedPixel = await renderOperation(2, setupSignedAlphaArgs, 10);
      const alphaAddSmoothPixel = await renderOperation(2, setupSignedAlphaArgs, 11);
      const modulateAlphaAddColorPixel = await renderOperation(18);
      const modulateColorAddAlphaPixel = await renderOperation(19);
      const modulateInvAlphaAddColorPixel = await renderOperation(20);
      const modulateInvColorAddAlphaPixel = await renderOperation(21);
      const dotProduct3Pixel = await renderOperation(24, setupSignedArgs);
      const multiplyAddPixel = await renderOperation(25, setupSignedArgs, 2, 0xffffffff, 2);
      const lerpPixel = await renderOperation(26, setupSignedArgs, 2, 0xffffffff, 1);
      const alphaMultiplyAddPixel = await renderOperation(2, setupSignedAlphaArgs, 25, 0xffffffff, 1, 2);
      const alphaLerpPixel = await renderOperation(2, setupSignedAlphaArgs, 26, 0xffffffff, 1, 2);
      const mirrorOnceSampler = sampler.slice();
      new DataView(mirrorOnceSampler.buffer).setUint32(8, 5, true);
      const mirrorOnceVertices = vertices.slice(), mirrorOnceView = new DataView(mirrorOnceVertices.buffer);
      for (let i = 0; i < 3; i++) mirrorOnceView.setFloat32(i * 28 + 20, -0.75, true);
      const mirrorOnceUpload = new Uint8Array(12 + mirrorOnceVertices.length);
      mirrorOnceUpload.set(words(11, 0, mirrorOnceVertices.length)); mirrorOnceUpload.set(mirrorOnceVertices, 12);
      const mirrorOncePixel = await renderOperation(2, () => {
        command(5, texel);
        command(8, mirrorOnceSampler);
        command(3, mirrorOnceUpload);
      });
      const result = {
        selectedPixel: Array.from(selectedPixels.data.slice(selectedAt, selectedAt + 4)),
        modulatedPixel,
        selectedArg2Pixel,
        modulated2xPixel,
        addedPixel,
        subtractedPixel,
        addSmoothPixel,
        addSignedPixel,
        addSigned2xPixel,
        alphaBlendDiffusePixel,
        alphaBlendTexturePixel,
        alphaBlendFactorPixel,
        alphaBlendTexturePmPixel,
        alphaBlendCurrentPixel,
        alphaModulated2xPixel,
        alphaModulated4xPixel,
        alphaAddedPixel,
        alphaAddSignedPixel,
        alphaAddSigned2xPixel,
        alphaSubtractedPixel,
        alphaAddSmoothPixel,
        modulateAlphaAddColorPixel,
        modulateColorAddAlphaPixel,
        modulateInvAlphaAddColorPixel,
        modulateInvColorAddAlphaPixel,
        dotProduct3Pixel,
        multiplyAddPixel,
        lerpPixel,
        alphaMultiplyAddPixel,
        alphaLerpPixel,
        mirrorOncePixel,
        blendedDiffuseAlphaPixel,
        blendedTextureAlphaPixel,
        blendedTextureAlphaPmPixel,
        blendedCurrentAlphaPixel,
        blendFactorAlphaPixel,
        blendFactorAlphaHighPixel,
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
    d3d9TextureResult.draws === 37 && d3d9TextureResult.errors.length === 0
      && near(d3d9TextureResult.selectedPixel, [127, 127, 127, 128], 2),
    JSON.stringify(d3d9TextureResult));
  check('D3D9 fixed-function MIRROR_ONCE mirrors negative UVs before linear filtering',
    near(d3d9TextureResult.mirrorOncePixel, [127, 255, 127, 128], 2), JSON.stringify(d3d9TextureResult.mirrorOncePixel));
  check('D3D9 stage-0 MODULATE multiplies texture RGB by diffuse vertex color',
    near(d3d9TextureResult.modulatedPixel, [127, 0, 0, 128], 2), JSON.stringify(d3d9TextureResult));
  check('D3D9 stage-0 SELECTARG2, MODULATE2X, ADD, and SUBTRACT match pixel equations',
    d3d9TextureResult.draws === 37 && near(d3d9TextureResult.selectedArg2Pixel, [255, 0, 0, 128], 2)
      && near(d3d9TextureResult.modulated2xPixel, [255, 0, 0, 128], 2)
      && near(d3d9TextureResult.addedPixel, [255, 127, 127, 128], 2)
      && near(d3d9TextureResult.subtractedPixel, [0, 127, 127, 128], 2),
    JSON.stringify(d3d9TextureResult));
  check('D3D9 stage-0 alpha BLENDDIFFUSEALPHA uses interpolated diffuse alpha',
    near(d3d9TextureResult.alphaBlendDiffusePixel, [115, 115, 115, 79], 2), JSON.stringify(d3d9TextureResult.alphaBlendDiffusePixel));
  check('D3D9 stage-0 alpha BLENDTEXTUREALPHA uses sampled texture alpha',
    near(d3d9TextureResult.alphaBlendTexturePixel, [115, 115, 115, 157], 2), JSON.stringify(d3d9TextureResult.alphaBlendTexturePixel));
  check('D3D9 stage-0 alpha BLENDFACTORALPHA uses the texture-factor alpha byte',
    near(d3d9TextureResult.alphaBlendFactorPixel, [115, 115, 115, 86], 2), JSON.stringify(d3d9TextureResult.alphaBlendFactorPixel));
  check('D3D9 stage-0 alpha BLENDTEXTUREALPHAPM handles premultiplied alpha',
    near(d3d9TextureResult.alphaBlendTexturePmPixel, [115, 115, 115, 205], 2), JSON.stringify(d3d9TextureResult.alphaBlendTexturePmPixel));
  check('D3D9 stage-0 alpha BLENDCURRENTALPHA uses current alpha',
    near(d3d9TextureResult.alphaBlendCurrentPixel, [115, 115, 115, 79], 2), JSON.stringify(d3d9TextureResult.alphaBlendCurrentPixel));
  check('D3D9 stage-0 BLENDTEXTUREALPHA mixes texture colour and diffuse colour',
    near(d3d9TextureResult.blendedTextureAlphaPixel, [191, 64, 64, 128], 2), JSON.stringify(d3d9TextureResult.blendedTextureAlphaPixel));
  check('D3D9 stage-0 BLENDDIFFUSEALPHA uses interpolated diffuse alpha',
    near(d3d9TextureResult.blendedDiffuseAlphaPixel, [223, 32, 32, 128], 2), JSON.stringify(d3d9TextureResult.blendedDiffuseAlphaPixel));
  check('D3D9 stage-0 BLENDCURRENTALPHA uses current-stage diffuse alpha',
    near(d3d9TextureResult.blendedCurrentAlphaPixel, [223, 32, 32, 128], 2), JSON.stringify(d3d9TextureResult.blendedCurrentAlphaPixel));
  check('D3D9 stage-0 BLENDTEXTUREALPHAPM handles premultiplied texture alpha',
    near(d3d9TextureResult.blendedTextureAlphaPmPixel, [255, 127, 127, 128], 2), JSON.stringify(d3d9TextureResult.blendedTextureAlphaPmPixel));
  check('D3D9 stage-0 ADDSMOOTH adds both arguments and subtracts their product',
    near(d3d9TextureResult.addSmoothPixel, [255, 127, 127, 128], 2), JSON.stringify(d3d9TextureResult.addSmoothPixel));
  check('D3D9 stage-0 ADDSIGNED applies the half-range bias',
    near(d3d9TextureResult.addSignedPixel, [39, 39, 39, 128], 2), JSON.stringify(d3d9TextureResult.addSignedPixel));
  check('D3D9 stage-0 ADDSIGNED2X doubles the biased sum',
    near(d3d9TextureResult.addSigned2xPixel, [77, 77, 77, 128], 2), JSON.stringify(d3d9TextureResult.addSigned2xPixel));
  check('D3D9 stage-0 alpha MODULATE2X doubles the product',
    near(d3d9TextureResult.alphaModulated2xPixel, [115, 115, 115, 77], 2), JSON.stringify(d3d9TextureResult.alphaModulated2xPixel));
  check('D3D9 stage-0 alpha MODULATE4X quadruples the product and clamps',
    near(d3d9TextureResult.alphaModulated4xPixel, [115, 115, 115, 153], 2), JSON.stringify(d3d9TextureResult.alphaModulated4xPixel));
  check('D3D9 stage-0 alpha ADD sums both arguments',
    near(d3d9TextureResult.alphaAddedPixel, [115, 115, 115, 243], 2), JSON.stringify(d3d9TextureResult.alphaAddedPixel));
  check('D3D9 stage-0 alpha ADDSIGNED applies the half-range bias',
    near(d3d9TextureResult.alphaAddSignedPixel, [115, 115, 115, 116], 2), JSON.stringify(d3d9TextureResult.alphaAddSignedPixel));
  check('D3D9 stage-0 alpha ADDSIGNED2X doubles the biased sum',
    near(d3d9TextureResult.alphaAddSigned2xPixel, [115, 115, 115, 231], 2), JSON.stringify(d3d9TextureResult.alphaAddSigned2xPixel));
  check('D3D9 stage-0 alpha SUBTRACT subtracts the second argument',
    near(d3d9TextureResult.alphaSubtractedPixel, [115, 115, 115, 141], 2), JSON.stringify(d3d9TextureResult.alphaSubtractedPixel));
  check('D3D9 stage-0 alpha ADDSMOOTH subtracts the product from the sum',
    near(d3d9TextureResult.alphaAddSmoothPixel, [115, 115, 115, 205], 2), JSON.stringify(d3d9TextureResult.alphaAddSmoothPixel));
  check('D3D9 stage-0 BLENDFACTORALPHA uses the texture-factor alpha byte',
    near(d3d9TextureResult.blendFactorAlphaPixel, [223, 32, 32, 128], 2), JSON.stringify(d3d9TextureResult.blendFactorAlphaPixel));
  check('D3D9 stage-0 BLENDFACTORALPHA changes when the render-state factor changes',
    near(d3d9TextureResult.blendFactorAlphaHighPixel, [159, 96, 96, 128], 2), JSON.stringify(d3d9TextureResult.blendFactorAlphaHighPixel));
  check('D3D9 stage-0 MODULATEALPHA_ADDCOLOR uses arg1 alpha times arg2 color',
    near(d3d9TextureResult.modulateAlphaAddColorPixel, [153, 153, 153, 192], 2), JSON.stringify(d3d9TextureResult.modulateAlphaAddColorPixel));
  check('D3D9 stage-0 MODULATECOLOR_ADDALPHA uses arg1 alpha as the addend',
    near(d3d9TextureResult.modulateColorAddAlphaPixel, [215, 215, 215, 192], 2), JSON.stringify(d3d9TextureResult.modulateColorAddAlphaPixel));
  check('D3D9 stage-0 MODULATEINVALPHA_ADDCOLOR uses inverse arg1 alpha',
    near(d3d9TextureResult.modulateInvAlphaAddColorPixel, [128, 128, 128, 192], 2), JSON.stringify(d3d9TextureResult.modulateInvAlphaAddColorPixel));
  check('D3D9 stage-0 MODULATEINVCOLOR_ADDALPHA uses inverse arg1 color',
    near(d3d9TextureResult.modulateInvColorAddAlphaPixel, [220, 220, 220, 192], 2), JSON.stringify(d3d9TextureResult.modulateInvColorAddAlphaPixel));
  check('D3D9 stage-0 DOTPRODUCT3 computes and clamps the signed RGB dot product',
    near(d3d9TextureResult.dotProduct3Pixel, [45, 45, 45, 128], 2), JSON.stringify(d3d9TextureResult.dotProduct3Pixel));
  check('D3D9 stage-0 MULTIPLYADD uses COLORARG0 plus the color product',
    near(d3d9TextureResult.multiplyAddPixel, [138, 138, 138, 128], 2), JSON.stringify(d3d9TextureResult.multiplyAddPixel));
  check('D3D9 stage-0 LERP interpolates color arguments with COLORARG0',
    near(d3d9TextureResult.lerpPixel, [64, 64, 64, 128], 2), JSON.stringify(d3d9TextureResult.lerpPixel));
  check('D3D9 stage-0 alpha MULTIPLYADD uses ALPHAARG0 plus the alpha product',
    near(d3d9TextureResult.alphaMultiplyAddPixel, [115, 115, 115, 230], 2), JSON.stringify(d3d9TextureResult.alphaMultiplyAddPixel));
  check('D3D9 stage-0 alpha LERP interpolates alpha arguments with ALPHAARG0',
    near(d3d9TextureResult.alphaLerpPixel, [115, 115, 115, 157], 2), JSON.stringify(d3d9TextureResult.alphaLerpPixel));

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
      command(29, words(3, 0)); command(35, words(1, 9, 3));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const observedPixel = Array.from(pixels.data.slice(i, i + 4));
      const readback = await gpu.readBackTexture(4); // read the texture through its D3D11 UAV view
      const uavValues = Array.from(readback.data);
      const uavCenter = uavValues[2 * readback.width + 2];
      const result = {
        pixel: observedPixel,
        presentSyncInterval: gpu.lastPresentSyncInterval,
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
  check('GPU executor preserves the DXGI present sync interval', computeResult.presentSyncInterval === 3,
    JSON.stringify(computeResult.presentSyncInterval));
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
      const rgba32UintCompute = tool.compileHlsl(`
        RWTexture2D<uint4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = uint4(0x12345678, 0xabcdef01, id.x, id.y); }
      `, 'main', 'cs_5_0');
      const rgba32SintCompute = tool.compileHlsl(`
        RWTexture2D<int4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = int4(-12345678, 12345678, int(id.x) - 8, int(id.y) * 3); }
      `, 'main', 'cs_5_0');
      const rg32FloatCompute = tool.compileHlsl(`
        RWTexture2D<float2> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = float2(float(id.x) + 0.5, float(id.y) + 0.25); }
      `, 'main', 'cs_5_0');
      const rg32UintCompute = tool.compileHlsl(`
        RWTexture2D<uint2> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = uint2(0x12345678, id.x + id.y * 4); }
      `, 'main', 'cs_5_0');
      const rg32SintCompute = tool.compileHlsl(`
        RWTexture2D<int2> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = int2(-12345678, int(id.x) - int(id.y) * 4); }
      `, 'main', 'cs_5_0');
      const rgba16UintCompute = tool.compileHlsl(`
        RWTexture2D<uint4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = uint4(0x1234, 0xabcd, 0x8000, 0xffff); }
      `, 'main', 'cs_5_0');
      const rgba16SintCompute = tool.compileHlsl(`
        RWTexture2D<int4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = int4(-32768, -1, 0, 32767); }
      `, 'main', 'cs_5_0');
      const rgba8SnormCompute = tool.compileHlsl(`
        RWTexture2D<float4> dst : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = float4(-64.0 / 127.0, -32.0 / 127.0, 32.0 / 127.0, 64.0 / 127.0); }
      `, 'main', 'cs_5_0');
      const groupSystemValuesCompute = tool.compileHlsl(`
        RWTexture2D<uint4> dst : register(u0);
        [numthreads(2, 2, 1)]
        void main(uint3 dispatchId : SV_DispatchThreadID, uint3 groupId : SV_GroupID,
                  uint3 groupThreadId : SV_GroupThreadID, uint groupIndex : SV_GroupIndex) {
          dst[dispatchId.xy] = uint4(groupId.x, groupId.y, groupThreadId.x + groupThreadId.y * 2, groupIndex);
        }
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
      command(4, words(73, 4, 4, 1, 1, 3, 0x88, 0, 1, 0)); // RGBA32_UINT SRV + UAV texture
      command(12, words(74, 3, 73, 3, 4, 0, 1, 0, 1)); // RGBA32_UINT UAV
      shader(13, rgba32UintCompute);
      command(17, words(2, 13));
      command(43, words(0, 1, 74, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(83, 4, 4, 1, 1, 4, 0x88, 0, 1, 0)); // RGBA32_SINT SRV + UAV texture
      command(12, words(84, 3, 83, 4, 4, 0, 1, 0, 1)); // RGBA32_SINT UAV
      shader(14, rgba32SintCompute);
      command(17, words(2, 14));
      command(43, words(0, 1, 84, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(93, 4, 4, 1, 1, 16, 0x88, 0, 1, 0)); // RG32_FLOAT SRV + UAV texture
      command(12, words(94, 3, 93, 16, 4, 0, 1, 0, 1)); // RG32_FLOAT UAV
      shader(15, rg32FloatCompute);
      command(17, words(2, 15));
      command(43, words(0, 1, 94, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(103, 4, 4, 1, 1, 17, 0x88, 0, 1, 0)); // RG32_UINT SRV + UAV texture
      command(12, words(104, 3, 103, 17, 4, 0, 1, 0, 1)); // RG32_UINT UAV
      shader(16, rg32UintCompute);
      command(17, words(2, 16));
      command(43, words(0, 1, 104, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(113, 4, 4, 1, 1, 18, 0x88, 0, 1, 0)); // RG32_SINT SRV + UAV texture
      command(12, words(114, 3, 113, 18, 4, 0, 1, 0, 1)); // RG32_SINT UAV
      shader(17, rg32SintCompute);
      command(17, words(2, 17));
      command(43, words(0, 1, 114, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(123, 4, 4, 1, 1, 12, 0x88, 0, 1, 0)); // RGBA16_UINT SRV + UAV texture
      command(12, words(124, 3, 123, 12, 4, 0, 1, 0, 1)); // RGBA16_UINT UAV
      shader(18, rgba16UintCompute);
      command(17, words(2, 18));
      command(43, words(0, 1, 124, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(133, 4, 4, 1, 1, 14, 0x88, 0, 1, 0)); // RGBA16_SINT SRV + UAV texture
      command(12, words(134, 3, 133, 14, 4, 0, 1, 0, 1)); // RGBA16_SINT UAV
      shader(19, rgba16SintCompute);
      command(17, words(2, 19));
      command(43, words(0, 1, 134, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(143, 4, 4, 1, 1, 31, 0x88, 0, 1, 0)); // RGBA8_SNORM SRV + UAV texture
      command(12, words(144, 3, 143, 31, 4, 0, 1, 0, 1)); // RGBA8_SNORM UAV
      shader(20, rgba8SnormCompute);
      command(17, words(2, 20));
      command(43, words(0, 1, 144, 0xffffffff));
      command(42, words(4, 4, 1));
      command(4, words(153, 4, 4, 1, 1, 3, 0x88, 0, 1, 0)); // RGBA32_UINT system-value texture
      command(12, words(154, 3, 153, 3, 4, 0, 1, 0, 1)); // RGBA32_UINT UAV
      shader(21, groupSystemValuesCompute);
      command(17, words(2, 21));
      command(43, words(0, 1, 154, 0xffffffff));
      command(42, words(2, 2, 1)); // 2x2 workgroups of 2x2 threads
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
      const rgba32UintReadback = await gpu.readBackTexture(74);
      const rgba32SintReadback = await gpu.readBackTexture(84);
      const rg32FloatReadback = await gpu.readBackTexture(94);
      const rg32UintReadback = await gpu.readBackTexture(104);
      const rg32SintReadback = await gpu.readBackTexture(114);
      const rgba16UintReadback = await gpu.readBackTexture(124);
      const rgba16SintReadback = await gpu.readBackTexture(134);
      const rgba8SnormReadback = await gpu.readBackTexture(144);
      const groupSystemValuesReadback = await gpu.readBackTexture(154);
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
        rgba32UintFormat: rgba32UintReadback.format,
        rgba32UintValues: Array.from(rgba32UintReadback.data),
        rgba32SintFormat: rgba32SintReadback.format,
        rgba32SintValues: Array.from(rgba32SintReadback.data),
        rg32FloatFormat: rg32FloatReadback.format,
        rg32FloatValues: Array.from(rg32FloatReadback.data),
        rg32UintFormat: rg32UintReadback.format,
        rg32UintValues: Array.from(rg32UintReadback.data),
        rg32SintFormat: rg32SintReadback.format,
        rg32SintValues: Array.from(rg32SintReadback.data),
        rgba16UintFormat: rgba16UintReadback.format,
        rgba16UintValues: Array.from(rgba16UintReadback.data),
        rgba16SintFormat: rgba16SintReadback.format,
        rgba16SintValues: Array.from(rgba16SintReadback.data),
        rgba8SnormFormat: rgba8SnormReadback.format,
        rgba8SnormValues: Array.from(rgba8SnormReadback.data),
        groupSystemValuesFormat: groupSystemValuesReadback.format,
        groupSystemValues: Array.from(groupSystemValuesReadback.data),
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
      && typedTextureUavResult.dispatches === 16 && typedTextureUavResult.errors.length === 0
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
      && typedTextureUavResult.rgbaSintValues.every((value, i) => value === [-1, -2, 3, 4][i % 4])
      && typedTextureUavResult.rgba32UintFormat === 'rgba32uint'
      && typedTextureUavResult.rgba32UintValues.length === 64
      && typedTextureUavResult.rgba32UintValues.every((value, i) => value === [0x12345678, 0xabcdef01, (i >> 2) % 4, Math.floor(i / 16)][i % 4])
      && typedTextureUavResult.rgba32SintFormat === 'rgba32sint'
      && typedTextureUavResult.rgba32SintValues.length === 64
      && typedTextureUavResult.rgba32SintValues.every((value, i) => value === [-12345678, 12345678, (i >> 2) % 4 - 8, Math.floor(i / 16) * 3][i % 4])
      && typedTextureUavResult.rg32FloatFormat === 'rg32float'
      && typedTextureUavResult.rg32FloatValues.length === 32
      && typedTextureUavResult.rg32FloatValues.every((value, i) => value === ((i % 2) === 0 ? ((i >> 1) % 4) + 0.5 : Math.floor((i >> 1) / 4) + 0.25))
      && typedTextureUavResult.rg32UintFormat === 'rg32uint'
      && typedTextureUavResult.rg32UintValues.length === 32
      && typedTextureUavResult.rg32UintValues.every((value, i) => value === ((i % 2) === 0 ? 0x12345678 : (i >> 1)))
      && typedTextureUavResult.rg32SintFormat === 'rg32sint'
      && typedTextureUavResult.rg32SintValues.length === 32
      && typedTextureUavResult.rg32SintValues.every((value, i) => value === ((i % 2) === 0 ? -12345678 : ((i >> 1) % 4) - Math.floor((i >> 1) / 4) * 4))
      && typedTextureUavResult.rgba16UintFormat === 'rgba16uint'
      && typedTextureUavResult.rgba16UintValues.length === 64
      && typedTextureUavResult.rgba16UintValues.every((value, i) => value === [0x1234, 0xabcd, 0x8000, 0xffff][i % 4])
      && typedTextureUavResult.rgba16SintFormat === 'rgba16sint'
      && typedTextureUavResult.rgba16SintValues.length === 64
      && typedTextureUavResult.rgba16SintValues.every((value, i) => value === [-32768, -1, 0, 32767][i % 4])
      && typedTextureUavResult.rgba8SnormFormat === 'rgba8snorm'
      && typedTextureUavResult.rgba8SnormValues.length === 64
      && typedTextureUavResult.rgba8SnormValues.every((value, i) => value === [-64, -32, 32, 64][i % 4]),
    JSON.stringify(typedTextureUavResult));
  check('SV_GroupID, SV_GroupThreadID, and SV_GroupIndex identify each compute lane',
    typedTextureUavResult.groupSystemValuesFormat === 'rgba32uint'
      && typedTextureUavResult.groupSystemValues.length === 64
      && typedTextureUavResult.groupSystemValues.every((value, i) => {
        const pixel = i >> 2, x = pixel % 4, y = Math.floor(pixel / 4);
        return value === [x >> 1, y >> 1, (x % 2) + (y % 2) * 2, (x % 2) + (y % 2) * 2][i % 4];
      }),
    JSON.stringify({ format: typedTextureUavResult.groupSystemValuesFormat, values: typedTextureUavResult.groupSystemValues, errors: typedTextureUavResult.errors }));

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
      const copiedInitial = new Uint32Array(16).fill(0xdeadbeef);
      command(2, words(26, copiedInitial.byteLength, 0x80, 0, 0x20, 0)); // raw CopyResource destination
      const copiedWrite = new Uint8Array(12 + copiedInitial.byteLength);
      copiedWrite.set(words(26, 0, copiedInitial.byteLength)); copiedWrite.set(new Uint8Array(copiedInitial.buffer), 12); command(3, copiedWrite);
      command(12, words(24, 3, 23, 39, 1, 2, 3, 1, 0)); // R32_TYPELESS raw view, elements [2, 5)
      shader(25, compute);
      command(17, words(2, 25));
      command(43, words(0, 1, 24, 0xffffffff));
      command(42, words(3, 1, 1));
      command(33, words(26, 23)); // copy the GPU-written raw buffer
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);

      const resource = gpu.res.get(26);
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
  check('D3D11 raw-buffer CopyResource preserves UAV writes and FirstElement/NumElements',
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
      const copied = new Uint32Array(16).fill(0xdeadbeef);
      command(2, words(38, copied.byteLength, 0x8, 0, 0, 0)); // typed CopyResource destination
      const copiedWrite = new Uint8Array(12 + copied.byteLength);
      copiedWrite.set(words(38, 0, copied.byteLength)); copiedWrite.set(new Uint8Array(copied.buffer), 12); command(3, copiedWrite);
      command(33, words(38, 33)); // typed-buffer CopyResource destination <- source
      command(12, words(34, 2, 38, 42, 1, 1, 3, 0, 0)); // copied R32_UINT SRV, elements [1, 4)
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
  check('D3D11 typed-buffer CopyResource feeds an SRV/UAV dispatch with element ranges',
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

  const clearStateResult = await page.evaluate(async () => {
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
        const data = new Uint8Array(8 + payload.length), view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, payload.length, true); data.set(payload, 8);
        stream.push(data);
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      shader(3, 0, vertex); shader(4, 1, pixel);
      command(17, words(0, 3)); command(17, words(1, 4));
      command(16, words(4)); command(21, words(1, 2, 0));
      command(29, words(3, 0));
      command(49, words()); // D3D11 ClearState
      command(21, words(1, 2, 0)); // bind a render target after ClearState
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true);
      clearView.setFloat32(4, 0.0, true); clearView.setFloat32(8, 0.0, true);
      clearView.setFloat32(12, 1.0, true); clearView.setFloat32(16, 1.0, true);
      command(27, clear);
      command(29, words(3, 0)); // cleared shaders leave the blue clear visible
      const bytes = stream.reduce((size, part) => size + part.length, 0);
      const commands = new Uint8Array(bytes);
      let offset = 0;
      for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const index = (32 * pixels.width + 32) * 4;
      const result = { pixel: Array.from(pixels.data.slice(index, index + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors, logs };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message, logs };
    }
  });
  check('D3D11 ClearState resets shaders before the next draw',
    clearStateResult.draws === 1 && clearStateResult.errors.length === 0
      && near(clearStateResult.pixel, [0, 0, 255, 255], 1),
    JSON.stringify(clearStateResult));

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

  const frontFacingResult = await page.evaluate(async () => {
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
          uint triangleId = vertexId / 3;
          uint local = vertexId % 3;
          if (triangleId != 0) local = local == 1 ? 2 : local == 2 ? 1 : local;
          float center = triangleId == 0 ? -0.5 : 0.5;
          float x = local == 0 ? center - 0.4 : local == 1 ? center + 0.4 : center;
          float y = local == 2 ? 0.65 : -0.75;
          return float4(x, y, 0.0, 1.0);
        }
      `, 'main', 'vs_5_0');
      const pixel = tool.compileHlsl(`
        float4 main(bool frontFace : SV_IsFrontFace) : SV_Target {
          return frontFace ? float4(0.0, 1.0, 0.0, 1.0) : float4(1.0, 0.0, 0.0, 1.0);
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
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 13, true); rasterView.setUint32(4, 3, true); rasterView.setUint32(8, 1, true);
      rasterView.setUint32(28, 1, true);
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(10, raster); command(25, words(13));
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(16, words(4)); command(21, words(1, 2, 0)); command(29, words(6, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const atPixel = (x, y) => Array.from(pixels.data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
      const result = { backFace: atPixel(16, 32), frontFace: atPixel(48, 32), errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('SV_IsFrontFace distinguishes opposite triangle windings',
    !frontFacingResult.error && frontFacingResult.errors?.length === 0
      && near(frontFacingResult.backFace, [255, 0, 0, 255], 1)
      && near(frontFacingResult.frontFace, [0, 255, 0, 255], 1),
    JSON.stringify(frontFacingResult));

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
      command(2, words(11, 12, 0x2, 0, 0, 0)); // D3D11_BIND_INDEX_BUFFER source
      command(2, words(12, 12, 0x2, 0, 0, 0)); // D3D11_BIND_INDEX_BUFFER destination
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

  const svDepthResult = await page.evaluate(async () => {
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
      const pixel = (color, depth) => tool.compileHlsl(`
        struct PsOut { float4 color : SV_Target; float depth : SV_Depth; };
        PsOut main() {
          PsOut output;
          output.color = ${color};
          output.depth = ${depth};
          return output;
        }
      `, 'main', 'ps_5_0');
      const green = pixel('float4(0.0, 1.0, 0.0, 1.0)', '0.75');
      const red = pixel('float4(1.0, 0.0, 0.0, 1.0)', '0.25');
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
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 64, 64, 1, 1, 39, 0x40, 0, 1, 0));
      command(12, words(4, 1, 3, 39, 3, 0, 1, 0, 1));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 13, true); rasterView.setUint32(4, 3, true); rasterView.setUint32(8, 1, true);
      rasterView.setUint32(28, 1, true);
      command(10, raster);
      const depthState = new Uint8Array(56), depthView = new DataView(depthState.buffer);
      depthView.setUint32(0, 15, true); depthView.setUint32(4, 1, true);
      depthView.setUint32(8, 1, true); depthView.setUint32(12, 2, true);
      command(11, depthState);
      shader(7, 0, vertex); shader(8, 1, green); shader(9, 1, red);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(16, words(4)); command(21, words(1, 2, 4));
      command(25, words(13)); command(26, words(15, 0));
      const clearColor = new Uint8Array(20), colorView = new DataView(clearColor.buffer);
      colorView.setUint32(0, 2, true); colorView.setFloat32(16, 1, true); command(27, clearColor);
      const clearDepth = new Uint8Array(16), depthClearView = new DataView(clearDepth.buffer);
      depthClearView.setUint32(0, 4, true); depthClearView.setUint32(4, 1, true);
      depthClearView.setFloat32(8, 1, true); command(28, clearDepth);
      command(23, words(1, 0, 0, 64, 64)); command(29, words(3, 0));
      command(17, words(1, 9)); command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let at = 0; for (const part of stream) { commands.set(part, at); at += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const i = (32 * pixels.width + 32) * 4;
      const result = { center: Array.from(pixels.data.slice(i, i + 4)), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('SV_Depth output controls later depth testing',
    !svDepthResult.error && svDepthResult.draws === 2 && svDepthResult.errors.length === 0
      && near(svDepthResult.center, [255, 0, 0, 255], 1),
    JSON.stringify(svDepthResult));

  const stencilResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 72; canvas.height = 64;
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
      command(38, words(1, 0, 9, 72, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(4, words(3, 72, 64, 1, 1, 45, 0x40, 0, 1, 0)); // D24S8 depth-stencil texture
      command(12, words(4, 1, 3, 45, 3, 0, 1, 0, 1)); // DSV
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 12, true);
      rasterView.setUint32(4, 3, true); // solid fill
      rasterView.setUint32(8, 1, true); // no culling
      rasterView.setUint32(28, 1, true); // depth clip enabled
      rasterView.setUint32(32, 1, true); // scissor enabled
      command(10, raster);
      const stencilState = (id, writeMask, compare, passOp, readMask = 0xff, options = {}) => {
        const payload = new Uint8Array(56), view = new DataView(payload.buffer);
        view.setUint32(0, id, true);
        view.setUint32(4, options.depthEnable ?? 0, true);
        view.setUint32(8, options.depthWrite ?? 0, true);
        view.setUint32(12, options.depthCompare ?? 8, true);
        view.setUint32(16, 1, true); // stencil enabled
        view.setUint8(20, readMask); view.setUint8(21, writeMask);
        for (const face of [24, 40]) {
          view.setUint32(face, options.failOp ?? 1, true);
          view.setUint32(face + 4, options.depthFailOp ?? 1, true);
          view.setUint32(face + 8, passOp, true);
          view.setUint32(face + 12, compare, true);
        }
        command(11, payload);
      };
      const operations = [
        { name: 'keep', op: 1, initial: 5, reference: 9, expected: 5 },
        { name: 'zero', op: 2, initial: 5, reference: 9, expected: 0 },
        { name: 'replace', op: 3, initial: 5, reference: 9, expected: 9 },
        { name: 'increment-clamp', op: 4, initial: 5, reference: 9, expected: 6 },
        { name: 'decrement-clamp', op: 5, initial: 5, reference: 9, expected: 4 },
        { name: 'invert', op: 6, initial: 5, reference: 9, expected: 250 },
        { name: 'increment-wrap', op: 7, initial: 255, reference: 9, expected: 0 },
        { name: 'decrement-wrap', op: 8, initial: 0, reference: 9, expected: 255 },
        { name: 'masked-replace', op: 3, initial: 0xa5, reference: 0x3c, expected: 0xac, writeMask: 0x0f },
      ];
      stencilState(13, 0xff, 8, 3); // seed each region with the reference value
      for (let i = 0; i < operations.length; i++) stencilState(20 + i, operations[i].writeMask ?? 0xff, 8, operations[i].op);
      stencilState(40, 0, 3, 1); // draw only where stored stencil equals reference
      stencilState(60, 0, 3, 1, 0x0f); // compare only the low stencil nibble
      shader(7, 0, vertex); shader(8, 1, red); shader(9, 1, green);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 4)); command(25, words(12));
      const clearColor = new Uint8Array(20), clearColorView = new DataView(clearColor.buffer);
      clearColorView.setUint32(0, 2, true); clearColorView.setFloat32(16, 1, true); command(27, clearColor);
      const clearDepth = new Uint8Array(16), clearDepthView = new DataView(clearDepth.buffer);
      clearDepthView.setUint32(0, 4, true); clearDepthView.setUint32(4, 3, true);
      clearDepthView.setFloat32(8, 1, true); command(28, clearDepth);
      for (let i = 0; i < operations.length; i++) {
        const operation = operations[i], left = i * 8;
        const scissor = () => command(23, words(1, left, 0, left + 8, 64));
        command(26, words(13, operation.initial)); scissor(); command(29, words(3, 0));
        command(26, words(20 + i, operation.reference)); scissor(); command(29, words(3, 0));
        command(17, words(1, 9)); command(26, words(40, operation.expected));
        scissor(); command(29, words(3, 0)); command(17, words(1, 8));
      }
      gpu.execute(append());
      const operationFrame = await gpu.readBackbuffer(1);
      const at = (frame, x, y) => {
        const i = (y * frame.width + x) * 4;
        return Array.from(frame.data.slice(i, i + 4));
      };
      const operationPixels = operations.map((_, i) => at(operationFrame, i * 8 + 4, 32));

      const drawFailureOperations = (mode) => {
        stream.length = 0;
        const depthClear = new Uint8Array(16), depthClearView = new DataView(depthClear.buffer);
        depthClearView.setUint32(0, 4, true); depthClearView.setUint32(4, 3, true);
        depthClearView.setFloat32(8, mode === 'depth-fail' ? 0.25 : 1, true);
        command(27, clearColor); command(28, depthClear);
        for (let i = 0; i < operations.length; i++) {
          const operation = operations[i], left = i * 8;
          const scissor = () => command(23, words(1, left, 0, left + 8, 64));
          command(26, words(13, operation.initial)); scissor(); command(29, words(3, 0));
          const stateId = (mode === 'fail' ? 70 : 90) + i;
          if (mode === 'fail') {
            stencilState(stateId, operation.writeMask ?? 0xff, 1, 1, 0xff, { failOp: operation.op });
          } else {
            stencilState(stateId, operation.writeMask ?? 0xff, 8, 1, 0xff, {
              depthEnable: 1, depthCompare: 2, depthFailOp: operation.op,
            });
          }
          command(17, words(1, 9)); command(26, words(stateId, operation.reference)); scissor(); command(29, words(3, 0));
          command(17, words(1, 9)); command(26, words(40, operation.expected)); scissor(); command(29, words(3, 0));
          command(17, words(1, 8));
        }
        gpu.execute(append());
        return gpu.readBackbuffer(1);
      };
      const failFrame = await drawFailureOperations('fail');
      const failPixels = operations.map((_, i) => at(failFrame, i * 8 + 4, 32));
      const depthFailFrame = await drawFailureOperations('depth-fail');
      const depthFailPixels = operations.map((_, i) => at(depthFailFrame, i * 8 + 4, 32));

      const comparisonValues = [4, 5, 6], comparisonNames = ['never', 'less', 'equal', 'less-equal', 'greater', 'not-equal', 'greater-equal', 'always'];
      stream.length = 0;
      command(27, clearColor); command(28, clearDepth);
      for (let i = 0; i < comparisonNames.length; i++) stencilState(50 + i, 0, i + 1, 1);
      for (let func = 0; func < comparisonNames.length; func++) {
        for (let valueIndex = 0; valueIndex < comparisonValues.length; valueIndex++) {
          const stored = comparisonValues[valueIndex];
          const left = func * 8, top = Math.floor(valueIndex * 64 / 3), bottom = Math.floor((valueIndex + 1) * 64 / 3);
          const scissor = () => command(23, words(1, left, top, left + 8, bottom));
          command(17, words(1, 8)); command(26, words(13, stored)); scissor(); command(29, words(3, 0));
          command(17, words(1, 9)); command(26, words(50 + func, 5)); scissor(); command(29, words(3, 0));
        }
      }
      const maskScissor = () => command(23, words(1, 64, 0, 72, 64));
      command(17, words(1, 8)); command(26, words(13, 0xa5)); maskScissor(); command(29, words(3, 0));
      command(17, words(1, 9)); command(26, words(60, 0x05)); maskScissor(); command(29, words(3, 0));
      gpu.execute(append());
      const comparisonFrame = await gpu.readBackbuffer(1);
      const result = {
        operationNames: operations.map(({ name }) => name),
        operationPixels,
        failPixels,
        depthFailPixels,
        comparisonNames,
        comparisonPixels: comparisonNames.map((_, func) => comparisonValues.map((__, value) => {
          const top = Math.floor(value * 64 / 3), bottom = Math.floor((value + 1) * 64 / 3);
          return at(comparisonFrame, func * 8 + 4, Math.floor((top + bottom) / 2));
        })),
        readMaskPixel: at(comparisonFrame, 68, 32),
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  const stencilComparisonPasses = [
    [false, false, false], [false, false, true], [false, true, false], [false, true, true],
    [true, false, false], [true, false, true], [true, true, false], [true, true, true],
  ];
  check('D3D11 stencil operations and all comparison functions match boundary values',
    stencilResult.draws === 131 && stencilResult.errors.length === 0
      && stencilResult.operationPixels?.length === 9
      && stencilResult.operationPixels.every((pixel) => near(pixel, [0, 255, 0, 255], 1))
      && stencilResult.failPixels?.length === 9
      && stencilResult.failPixels.every((pixel) => near(pixel, [0, 255, 0, 255], 1))
      && stencilResult.depthFailPixels?.length === 9
      && stencilResult.depthFailPixels.every((pixel) => near(pixel, [0, 255, 0, 255], 1))
      && stencilResult.comparisonPixels?.length === 8
      && stencilResult.comparisonPixels.every((row, func) => row.length === 3
        && row.every((pixel, value) => near(pixel, stencilComparisonPasses[func][value] ? [0, 255, 0, 255] : [255, 0, 0, 255], 1)))
      && near(stencilResult.readMaskPixel || [], [0, 255, 0, 255], 1),
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
    canvas.width = 80; canvas.height = 64;
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
      command(38, words(1, 0, 9, 80, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      const raster = new Uint8Array(44), rasterView = new DataView(raster.buffer);
      rasterView.setUint32(0, 12, true); rasterView.setUint32(4, 3, true);
      rasterView.setUint32(8, 1, true); rasterView.setUint32(28, 1, true); rasterView.setUint32(32, 1, true);
      command(10, raster);
      const blend = (id, src, dst, op, writeMask = 0xf) => {
        const payload = new Uint8Array(268), view = new DataView(payload.buffer);
        view.setUint32(0, id, true); view.setUint32(12, 1, true);
        view.setUint32(16, src, true); view.setUint32(20, dst, true); view.setUint32(24, op, true);
        view.setUint32(28, src, true); view.setUint32(32, dst, true); view.setUint32(36, op, true);
        view.setUint32(40, writeMask, true); command(9, payload);
      };
      blend(13, 2, 2, 1); blend(14, 2, 2, 2); blend(15, 2, 2, 4); blend(16, 2, 2, 5);
      blend(17, 5, 6, 1, 0x7); // src-alpha / inverse-src-alpha
      blend(18, 7, 8, 1, 0x7); // destination-alpha / inverse-destination-alpha
      blend(19, 11, 2, 1, 0x7); // saturate / one
      blend(20, 3, 4, 1, 0x7); // source-color / inverse-source-color
      blend(21, 9, 10, 1, 0x7); // destination-color / inverse-destination-color
      blend(22, 14, 15, 1, 0x7); // blend-constant / inverse-blend-constant
      const maskedBlend = new Uint8Array(268), maskedBlendView = new DataView(maskedBlend.buffer);
      maskedBlendView.setUint32(0, 23, true); // state id
      maskedBlendView.setUint32(40, 0x5, true); // write red and blue only
      command(9, maskedBlend);
      const separateAlphaBlend = new Uint8Array(268), separateAlphaBlendView = new DataView(separateAlphaBlend.buffer);
      separateAlphaBlendView.setUint32(0, 24, true); // state id
      separateAlphaBlendView.setUint32(12, 1, true); // RenderTarget[0].BlendEnable
      separateAlphaBlendView.setUint32(16, 2, true); // color SrcBlend = ONE
      separateAlphaBlendView.setUint32(20, 1, true); // color DestBlend = ZERO
      separateAlphaBlendView.setUint32(24, 1, true); // color BlendOp = ADD
      separateAlphaBlendView.setUint32(28, 2, true); // alpha SrcBlend = ONE
      separateAlphaBlendView.setUint32(32, 2, true); // alpha DestBlend = ONE
      separateAlphaBlendView.setUint32(36, 2, true); // alpha BlendOp = SUBTRACT
      separateAlphaBlendView.setUint32(40, 0xf, true);
      command(9, separateAlphaBlend);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(25, words(12));
      const clear = new Uint8Array(20), clearView = new DataView(clear.buffer);
      clearView.setUint32(0, 2, true); [0.5, 0.1, 0.4, 0.3].forEach((v, i) => clearView.setFloat32(4 + i * 4, v, true)); command(27, clear);
      const factorRects = Array.from({ length: 6 }, (_, i) => [Math.floor(i * 64 / 6), 32, Math.floor((i + 1) * 64 / 6), 64]);
      const tests = [
        ...[13, 14, 15, 16].map((id, i) => ({ id, rect: [i * 16, 0, (i + 1) * 16, 32], factor: [0, 0, 0, 0] })),
        ...[17, 18, 19, 20, 21, 22].map((id, i) => ({
          id,
          rect: factorRects[i],
          factor: i === 5 ? [0.25, 0.5, 0.75, 0.5] : [0, 0, 0, 0],
        })),
      ];
      const floatBits = (value) => {
        const data = new DataView(new ArrayBuffer(4)); data.setFloat32(0, value, true); return data.getUint32(0, true);
      };
      for (const { id, rect: [left, top, right, bottom], factor } of tests) {
        command(24, words(id, ...factor.map(floatBits), 0xf));
        command(23, words(1, left, top, right, bottom));
        command(29, words(3, 0));
      }
      command(24, words(23, 0, 0, 0, 0, 0xffffffff));
      command(22, words(1, 0x42800000, 0, 0x41800000, 0x42000000, 0, 0x3f800000)); // viewport x=64, width=16
      command(23, words(1, 64, 0, 80, 32));
      command(29, words(3, 0));
      command(24, words(24, 0, 0, 0, 0, 0xffffffff));
      command(22, words(1, 0x42800000, 0x42000000, 0x41800000, 0x42000000, 0, 0x3f800000)); // viewport x=64, y=32
      command(23, words(1, 64, 32, 80, 64));
      command(29, words(3, 0));
      gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y) => {
        const i = (y * pixels.width + x) * 4;
        return Array.from(pixels.data.slice(i, i + 4));
      };
      const factorAt = (i) => {
        const left = Math.floor(i * 64 / 6), right = Math.floor((i + 1) * 64 / 6);
        return at(Math.floor((left + right) / 2), 48);
      };
      const result = {
        add: at(8, 16), subtract: at(24, 16), min: at(40, 16), max: at(56, 16),
        srcAlpha: factorAt(0), dstAlpha: factorAt(1), srcAlphaSaturate: factorAt(2),
        srcColor: factorAt(3), dstColor: factorAt(4), blendConstant: factorAt(5),
        channelMask: at(72, 16),
        separateAlpha: at(72, 48),
        draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 ADD, SUBTRACT, MIN, and MAX blend operations match channel-wise pixels',
    blendModesResult.draws === 12 && blendModesResult.errors.length === 0
      && near(blendModesResult.add, [191, 153, 128, 255], 1)
      && near(blendModesResult.subtract, [0, 102, 0, 128], 1)
      && near(blendModesResult.min, [64, 26, 26, 77], 1)
      && near(blendModesResult.max, [128, 128, 102, 204], 1),
    JSON.stringify(blendModesResult));
  check('D3D11 blend factors cover alpha, color, saturate, and blend-constant paths',
    blendModesResult.draws === 12 && blendModesResult.errors.length === 0
      && ['srcAlpha', 'dstAlpha', 'srcAlphaSaturate', 'srcColor', 'dstColor', 'blendConstant']
        .every((name) => Array.isArray(blendModesResult[name]))
      && near(blendModesResult.srcAlpha, [77, 107, 41, 77], 1)
      && near(blendModesResult.dstAlpha, [108, 56, 79, 77], 1)
      && near(blendModesResult.srcAlphaSaturate, [172, 115, 120, 77], 1)
      && near(blendModesResult.srcColor, [112, 77, 94, 77], 1)
      && near(blendModesResult.dstColor, [96, 36, 71, 77], 1)
      && near(blendModesResult.blendConstant, [112, 77, 45, 77], 1),
    JSON.stringify(blendModesResult));
  check('D3D11 render target write mask preserves disabled color and alpha channels',
    blendModesResult.draws === 12 && blendModesResult.errors.length === 0
      && near(blendModesResult.channelMask || [], [64, 26, 26, 77], 1),
    JSON.stringify(blendModesResult));
  check('D3D11 color and alpha blend factors and operations apply independently',
    blendModesResult.draws === 12 && blendModesResult.errors.length === 0
      && near(blendModesResult.separateAlpha || [], [64, 128, 26, 128], 1),
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
          output.second = float4(0.0, 0.0, 1.0, 0.25);
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
      const blend = new Uint8Array(268), blendView = new DataView(blend.buffer);
      blendView.setUint32(0, 11, true); // state id
      blendView.setUint32(8, 1, true); // IndependentBlendEnable
      blendView.setUint32(12, 1, true); // RenderTarget[0].BlendEnable
      blendView.setUint32(16, 2, true); // RenderTarget[0].SrcBlend = ONE
      blendView.setUint32(20, 2, true); // RenderTarget[0].DestBlend = ONE
      blendView.setUint32(24, 1, true); // RenderTarget[0].BlendOp = ADD
      blendView.setUint32(28, 2, true); // RenderTarget[0].SrcBlendAlpha = ONE
      blendView.setUint32(32, 2, true); // RenderTarget[0].DestBlendAlpha = ONE
      blendView.setUint32(36, 1, true); // RenderTarget[0].BlendOpAlpha = ADD
      blendView.setUint32(40, 0xf, true); // RenderTarget[0].WriteMask
      blendView.setUint32(44, 0, true); // RenderTarget[1].BlendEnable
      blendView.setUint32(72, 0xf, true); // RenderTarget[1].WriteMask
      command(9, blend);
      command(24, words(11, 0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000, 0xffffffff));
      command(27, words(5, 0, 0, 0x3f800000, 0x3f800000)); // clear target 0 to blue
      command(27, words(6, 0, 0x3f800000, 0, 0x3f800000)); // clear target 1 to green
      command(29, words(3, 0));
      command(4, words(9, 64, 64, 1, 1, 28, 0x20, 0, 1, 0));
      command(12, words(10, 0, 9, 28, 4, 0, 1, 0, 1));
      for (const [index, factor] of [16, 17, 18, 19].entries()) {
        const payload = new Uint8Array(268), view = new DataView(payload.buffer);
        view.setUint32(0, 12 + index, true); // state id
        view.setUint32(12, 1, true); // RenderTarget[0].BlendEnable
        view.setUint32(16, factor, true); // SrcBlend = SRC1_COLOR / inverse / alpha / inverse-alpha
        view.setUint32(20, 2, true); // DestBlend = ONE
        view.setUint32(24, 1, true); // BlendOp = ADD
        view.setUint32(28, 2, true); // SrcBlendAlpha = ONE
        view.setUint32(32, 1, true); // DestBlendAlpha = ZERO
        view.setUint32(36, 1, true); // BlendOpAlpha = ADD
        view.setUint32(40, 0xf, true); // RenderTarget[0].WriteMask
        command(9, payload);
      }
      command(21, words(1, 10, 0));
      command(27, words(10, 0, 0x3f800000, 0, 0x3f800000)); // clear to green
      const viewportXs = [0, 0x41800000, 0x42000000, 0x42400000];
      for (let index = 0; index < viewportXs.length; index++) {
        command(24, words(12 + index, 0x3f800000, 0x3f800000, 0x3f800000, 0x3f800000, 0xffffffff));
        command(22, words(1, viewportXs[index], 0, 0x41800000, 0x42800000, 0, 0x3f800000));
        command(29, words(3, 0));
      }
      gpu.execute(append());
      const [first, second, dualSource] = await Promise.all([gpu.readBackTexture(3), gpu.readBackTexture(4), gpu.readBackTexture(9)]);
      const pixelAt = (image) => {
        const i = (32 * image.width + 32) * 4;
        return Array.from(image.data.slice(i, i + 4));
      };
      const pixelAtXY = (image, x, y) => {
        const i = (y * image.width + x) * 4;
        return Array.from(image.data.slice(i, i + 4));
      };
      const result = {
        first: pixelAt(first), second: pixelAt(second),
        dualSourcePixels: [8, 24, 40, 56].map((x) => pixelAtXY(dualSource, x, 32)),
        dualSourceFeature: gpu.features.has('dual-source-blending'), errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 independent blend descriptors apply per render target',
    !mrtResult.error && mrtResult.errors?.length === 0
      && near(mrtResult.first || [], [255, 0, 255, 255], 1)
      && near(mrtResult.second || [], [0, 0, 255, 64], 1),
    JSON.stringify(mrtResult));
  check('D3D11 SRC1 color and alpha factors read the second pixel shader output',
    !mrtResult.error && mrtResult.dualSourceFeature
      && near(mrtResult.dualSourcePixels?.[0] || [], [0, 255, 0, 255], 1)
      && near(mrtResult.dualSourcePixels?.[1] || [], [255, 255, 0, 255], 1)
      && near(mrtResult.dualSourcePixels?.[2] || [], [64, 255, 0, 255], 1)
      && near(mrtResult.dualSourcePixels?.[3] || [], [191, 255, 0, 255], 1)
      && mrtResult.errors?.length === 0,
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

  const texture1dArrayResult = await page.evaluate(async () => {
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
        Texture1DArray<float4> source : register(t0);
        SamplerState sourceSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          if (position.x < 20.0) {
            return source.SampleLevel(sourceSampler, float2(0.5, 0.0), 1.0);
          }
          if (position.x >= 44.0) {
            return source.SampleLevel(sourceSampler, float2(0.5, 1.0), 2.0);
          }
          uint width, elements, levels;
          if (position.y < 32.0) {
            source.GetDimensions(0, width, elements, levels);
          } else {
            source.GetDimensions(1, width, elements, levels);
          }
          return float4(float(width) / 16.0, float(elements) / 4.0, float(levels) / 8.0, 1.0);
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
      const append = () => {
        const size = stream.reduce((n, part) => n + part.length, 0), data = new Uint8Array(size);
        let at = 0; for (const part of stream) { data.set(part, at); at += part.length; }
        return data;
      };
      const shader = (id, stage, dxbc) => {
        const head = words(id, stage, dxbc.length), payload = new Uint8Array(head.length + dxbc.length);
        payload.set(head); payload.set(dxbc, head.length); command(6, payload);
      };
      command(38, words(1, 0, 9, 64, 64, 28));
      command(12, words(2, 0, 9, 28, 4, 0, 1, 0, 1));
      command(53, words(3, 4, 3, 2, 28, 0x8, 0, 0));
      command(12, words(5, 2, 3, 28, 3, 0, 3, 0, 2));
      const upload = (mip, slice, width, color) => {
        const texels = new Uint8Array(width * 4);
        for (let i = 0; i < width; i++) texels.set(color, i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, mip, slice, 0, 0, width, 1, width * 4, texels.length));
        payload.set(texels, 36); command(5, payload);
      };
      upload(1, 0, 2, [255, 0, 0, 255]);
      upload(2, 1, 1, [0, 255, 0, 255]);
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 0, true);
      samplerView.setUint32(4, 3, true); samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackTexture(9);
      const at = (x, y = 32) => Array.from(pixels.data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
      const result = {
        mip1Layer0: at(16),
        mip2Layer1: at(48),
        dimensionsMip0: at(32, 16),
        dimensionsMip1: at(32, 48),
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 Texture1DArray samples nonzero mips and array layers through its 2D backing texture',
    !texture1dArrayResult.error && texture1dArrayResult.errors?.length === 0
      && near(texture1dArrayResult.mip1Layer0 || [], [255, 0, 0, 255], 1)
      && near(texture1dArrayResult.mip2Layer1 || [], [0, 255, 0, 255], 1)
      && near(texture1dArrayResult.dimensionsMip0 || [], [64, 128, 96, 255], 1)
      && near(texture1dArrayResult.dimensionsMip1 || [], [32, 128, 96, 255], 1),
    JSON.stringify(texture1dArrayResult));

  const texture1dArrayUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 8; canvas.height = 8;
    try {
      const tool = new ShaderTool(new URL('/shaders/', location.href));
      await tool.init();
      const gpu = new GpuExecutor(canvas, tool, () => {}, { noPresent: true });
      await gpu.init();
      const compute = tool.compileHlsl(`
        RWTexture1DArray<float4> outputImage : register(u0);
        [numthreads(1, 1, 1)]
        void main(uint3 id : SV_DispatchThreadID) {
          outputImage[id.xy] = float4((float(id.x) + 1.0) / 5.0, float(id.y), 0.4, 1.0);
        }
      `, 'main', 'cs_5_0');
      const words = (...values) => {
        const data = new Uint8Array(values.length * 4), view = new DataView(data.buffer);
        values.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true));
        return data;
      };
      const command = (op, payload) => {
        const data = new Uint8Array(8 + payload.length), view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, payload.length, true); data.set(payload, 8);
        return data;
      };
      const stream = [];
      const emit = (op, payload) => { stream.push(command(op, payload)); };
      emit(53, words(3, 4, 1, 2, 28, 0x80, 0, 0)); // four-wide, two-layer RGBA8_UNORM Texture1D UAV
      emit(12, words(4, 3, 3, 28, 3, 0, 1, 0, 2)); // Texture1DArray UAV
      const shaderHead = words(6, 2, compute.length), shaderPayload = new Uint8Array(shaderHead.length + compute.length);
      shaderPayload.set(shaderHead); shaderPayload.set(compute, shaderHead.length); emit(6, shaderPayload);
      emit(17, words(2, 6));
      emit(43, words(0, 1, 4, 0xffffffff));
      emit(42, words(4, 2, 1));
      const commands = new Uint8Array(stream.reduce((size, part) => size + part.length, 0));
      let offset = 0;
      for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const layer0 = await gpu.readBackResource(3, 0);
      const layer1 = await gpu.readBackResource(3, 1);
      const result = {
        layer0: Array.from(layer0),
        layer1: Array.from(layer1),
        dispatches: gpu.stats.dispatches,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 Texture1DArray UAV dispatch writes every element in both layers',
    !texture1dArrayUavResult.error && texture1dArrayUavResult.dispatches === 1
      && texture1dArrayUavResult.errors?.length === 0
      && texture1dArrayUavResult.layer0?.length === 16
      && texture1dArrayUavResult.layer1?.length === 16
      && texture1dArrayUavResult.layer0.every((value, i) => value === (i % 4 === 0 ? [51, 102, 153, 204][i >> 2]
        : i % 4 === 1 ? 0 : i % 4 === 2 ? 102 : 255))
      && texture1dArrayUavResult.layer1.every((value, i) => value === (i % 4 === 0 ? [51, 102, 153, 204][i >> 2]
        : i % 4 === 1 ? 255 : i % 4 === 2 ? 102 : 255)),
    JSON.stringify(texture1dArrayUavResult));

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
        float4 main(float4 position : SV_Position) : SV_Target {
          return position.x < 20.0
            ? source.SampleLevel(sourceSampler, float3(1.25, 0.5, 1.0), 0.0)
            : source.SampleLevel(sourceSampler, float3(0.9, 0.5, 1.0), 0.0);
        }
      `, 'main', 'ps_5_0');
      const anisotropicPixel = tool.compileHlsl(`
        Texture2DArray<float4> source : register(t0);
        SamplerState anisotropicSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float2 coordinate = float2(0.95 + (position.x - 32.5) * 0.2, 0.5);
          return source.Sample(anisotropicSampler, float3(coordinate, 1.0));
        }
      `, 'main', 'ps_5_0');
      const anisotropicGradPixel = tool.compileHlsl(`
        Texture2DArray<float4> source : register(t0);
        SamplerState anisotropicSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float2 coordinate = float2(0.95 + (position.x - 32.5) * 0.2, 0.5);
          // vkd3d rejects the documented float2 gradients here, but lowers float3 gradients to vec2 xy.
          return source.SampleGrad(anisotropicSampler, float3(coordinate, 1.0), float3(0.2, 0.0, 0.0), float3(0.0, 0.0, 0.0));
        }
      `, 'main', 'ps_5_0');
      const anisotropicBiasPixel = tool.compileHlsl(`
        Texture2DArray<float4> source : register(t0);
        SamplerState anisotropicSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float2 coordinate = float2(0.95 + (position.x - 32.5) * 0.2, 0.5);
          return source.SampleBias(anisotropicSampler, float3(coordinate, 1.0), 0.0);
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
      samplerView.setUint32(8, 4, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      [0, 0, 1, 1].forEach((value, index) => samplerView.setFloat32(32 + index * 4, value, true));
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      const anisotropicSamplerData = new Uint8Array(56), anisotropicSamplerView = new DataView(anisotropicSamplerData.buffer);
      anisotropicSamplerView.setUint32(0, 9, true); anisotropicSamplerView.setUint32(4, 0x55, true);
      anisotropicSamplerView.setUint32(8, 4, true); anisotropicSamplerView.setUint32(12, 3, true); anisotropicSamplerView.setUint32(16, 3, true);
      anisotropicSamplerView.setUint32(24, 8, true);
      [0, 0, 1, 1].forEach((value, index) => anisotropicSamplerView.setFloat32(32 + index * 4, value, true));
      anisotropicSamplerView.setFloat32(48, 0, true); anisotropicSamplerView.setFloat32(52, 2, true);
      command(8, anisotropicSamplerData); shader(10, 1, anisotropicPixel);
      command(17, words(1, 10)); command(20, words(1, 0, 1, 9)); command(29, words(3, 0));
      gpu.execute(append());
      const anisotropicPixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      const anisotropicGradSamplerData = anisotropicSamplerData.slice();
      new DataView(anisotropicGradSamplerData.buffer).setUint32(0, 10, true);
      command(8, anisotropicGradSamplerData); shader(11, 1, anisotropicGradPixel);
      command(17, words(1, 11)); command(20, words(1, 0, 1, 10)); command(29, words(3, 0));
      gpu.execute(append());
      const anisotropicGradPixels = await gpu.readBackbuffer(1);
      stream.length = 0;
      shader(12, 1, anisotropicBiasPixel);
      command(17, words(1, 12)); command(29, words(3, 0));
      gpu.execute(append());
      const anisotropicBiasPixels = await gpu.readBackbuffer(1);
      const at = (data, x) => Array.from(data.data.slice((32 * data.width + x) * 4, (32 * data.width + x) * 4 + 4));
      const result = {
        outside: at(pixels, 10), straddle: at(pixels, 32), anisotropicLayerEdge: at(anisotropicPixels, 32),
        anisotropicLayerGradEdge: at(anisotropicGradPixels, 32),
        anisotropicLayerBiasEdge: at(anisotropicBiasPixels, 32),
        draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 BORDER sampling returns its color for Texture2DArray taps and blends edge footprints',
    textureArrayResult.draws === 4 && textureArrayResult.errors.length === 0
      && near(textureArrayResult.outside || [], [0, 0, 255, 255], 1)
      && near(textureArrayResult.straddle || [], [0, 230, 26, 255], 2),
    JSON.stringify(textureArrayResult));
  check('D3D11 anisotropic BORDER Sample filters the selected Texture2DArray layer across the edge',
    near(textureArrayResult.anisotropicLayerEdge || [], [0, 151, 104, 255], 3),
    JSON.stringify(textureArrayResult.anisotropicLayerEdge));
  check('D3D11 anisotropic BORDER SampleGrad filters the selected Texture2DArray layer across the edge',
    near(textureArrayResult.anisotropicLayerGradEdge || [], [0, 151, 104, 255], 3),
    JSON.stringify(textureArrayResult.anisotropicLayerGradEdge));
  check('D3D11 anisotropic BORDER SampleBias filters the selected Texture2DArray layer across the edge',
    near(textureArrayResult.anisotropicLayerBiasEdge || [], [0, 151, 104, 255], 3),
    JSON.stringify(textureArrayResult.anisotropicLayerBiasEdge));

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
        float4 main(float4 position : SV_Position) : SV_Target {
          float3 direction = position.x < 10.0 ? float3(1.0, 0.0, 0.0)
            : position.x < 20.0 ? float3(-1.0, 0.0, 0.0)
            : position.x < 30.0 ? float3(0.0, 1.0, 0.0)
            : position.x < 40.0 ? float3(0.0, -1.0, 0.0)
            : position.x < 50.0 ? float3(0.0, 0.0, 1.0)
            : position.x < 52.0 ? float3(0.0, 0.0, -1.0)
            : position.x < 56.0 ? float3(1.0, 1.0, 0.0)
            : position.x < 60.0 ? float3(1.0, 0.0, 1.0)
            : float3(1.0, -1.0, 0.0);
          return source.SampleLevel(sourceSampler, direction, 0.0);
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
      const at = (x) => Array.from(pixels.data.slice((32 * pixels.width + x) * 4, (32 * pixels.width + x) * 4 + 4));
      const result = {
        faces: [4, 14, 24, 34, 44, 50].map(at),
        edges: [54, 58, 62].map(at),
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 TextureCube SRV selects all six signed-axis faces',
    cubeTextureResult.draws === 1 && cubeTextureResult.errors.length === 0
      && cubeTextureResult.faces?.length === 6
      && cubeTextureResult.faces.every((pixel, index) => near(pixel, [
        [255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255],
        [255, 255, 0, 255], [255, 0, 255, 255], [0, 255, 255, 255],
      ][index], 1)),
    JSON.stringify(cubeTextureResult));
  check('D3D11 TextureCube filtering blends taps across adjacent signed-axis faces',
    cubeTextureResult.edges?.length === 3
      && cubeTextureResult.edges.every((pixel, index) => near(pixel, [
        [128, 0, 128, 255], [255, 0, 128, 255], [255, 128, 0, 255],
      ][index], 2)),
    JSON.stringify(cubeTextureResult));

  const cubeArrayResult = await page.evaluate(async () => {
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
        TextureCubeArray<float4> source : register(t0);
        TextureCubeArray<float4> halfSource : register(t1);
        SamplerState sourceSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          if (position.y >= 32.0) {
            uint width, height, elements, levels;
            if (position.x < 32.0) source.GetDimensions(0, width, height, elements, levels);
            else source.GetDimensions(1, width, height, elements, levels);
            return float4(float(width) / 4.0, float(height) / 4.0, float(elements) / 16.0, float(levels) / 2.0);
          }
          if (position.y < 16.0) {
            float cubeIndex = position.x < 32.0 ? 0.0 : 1.0;
            return halfSource.SampleLevel(sourceSampler, float4(1.0, 0.0, 0.0, cubeIndex), 0.0);
          }
          float4 coordinate = position.x < 20.0 ? float4(1.0, 0.0, 0.0, 0.0)
            : position.x < 40.0 ? float4(1.0, 0.0, 0.0, 1.0)
            : float4(1.0, 1.0, 0.0, 0.0);
          return source.SampleLevel(sourceSampler, coordinate, 0.0);
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
      command(4, words(3, 4, 4, 2, 18, 28, 0x8, 0, 1, 0x4));
      command(12, words(5, 2, 3, 28, 10, 0, 2, 6, 2)); // two mips, first face 6, two cubes
      command(4, words(11, 4, 4, 1, 18, 10, 0x8, 0, 1, 0x4)); // RGBA16_FLOAT cube array
      command(12, words(12, 2, 11, 10, 10, 0, 1, 6, 2)); // two cubes starting at face 6
      const cubeFaces = [
        [[255, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255]],
        [[0, 255, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255], [255, 0, 255, 255], [0, 255, 255, 255]],
        [[0, 0, 255, 255], [0, 0, 255, 255], [0, 0, 255, 255], [0, 0, 255, 255], [0, 0, 255, 255], [0, 0, 255, 255]],
      ];
      for (let slice = 0; slice < 18; slice++) {
        const color = cubeFaces[Math.floor(slice / 6)][slice % 6];
        const texels = new Uint8Array(64);
        for (let i = 0; i < 16; i++) texels.set(color, i * 4);
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(3, 0, slice, 0, 0, 4, 4, 16, texels.length));
        payload.set(texels, 36); command(5, payload);
        const mip1Texels = new Uint8Array(16);
        for (let i = 0; i < 4; i++) mip1Texels.set(color, i * 4);
        const mip1Payload = new Uint8Array(36 + mip1Texels.length);
        mip1Payload.set(words(3, 1, slice, 0, 0, 2, 2, 8, mip1Texels.length));
        mip1Payload.set(mip1Texels, 36); command(5, mip1Payload);
      }
      for (let slice = 0; slice < 18; slice++) {
        const values = slice < 12 ? [0x3400, 0x3800, 0x3c00, 0x3c00] : [0x3c00, 0x3400, 0x3800, 0x3c00];
        const texels = new Uint8Array(128), texelView = new DataView(texels.buffer);
        for (let i = 0; i < 16; i++) values.forEach((value, channel) => texelView.setUint16(i * 8 + channel * 2, value, true));
        const payload = new Uint8Array(36 + texels.length);
        payload.set(words(11, 0, slice, 0, 0, 4, 4, 32, texels.length));
        payload.set(texels, 36); command(5, payload);
      }
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 6, true); samplerView.setUint32(4, 0x15, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 2, 5, 12)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x, y = 16) => Array.from(pixels.data.slice((y * pixels.width + x) * 4, (y * pixels.width + x) * 4 + 4));
      const result = {
        selectedCube0: at(10),
        selectedCube1: at(30),
        selectedCube0Edge: at(54),
        selectedHalfCube0: at(10, 8),
        selectedHalfCube1: at(50, 8),
        dimensions: at(10, 48),
        mip1Dimensions: at(42, 48),
        viewDimension: gpu.views.get(5)?.cache?.dim,
        halfViewDimension: gpu.views.get(12)?.cache?.dim,
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 TextureCubeArray SRVs select cube ranges from RGBA8_UNORM and RGBA16_FLOAT',
    cubeArrayResult.draws === 1 && cubeArrayResult.errors?.length === 0
      && cubeArrayResult.viewDimension === 'cube-array'
      && cubeArrayResult.halfViewDimension === 'cube-array'
      && near(cubeArrayResult.selectedCube0 || [], [0, 255, 0, 255], 1)
      && near(cubeArrayResult.selectedCube1 || [], [0, 0, 255, 255], 1)
      && near(cubeArrayResult.selectedHalfCube0 || [], [64, 128, 255, 255], 1)
      && near(cubeArrayResult.selectedHalfCube1 || [], [255, 64, 128, 255], 1)
      && near(cubeArrayResult.selectedCube0Edge || [], [0, 128, 128, 255], 2)
      && near(cubeArrayResult.dimensions || [], [255, 255, 32, 255], 1)
      && near(cubeArrayResult.mip1Dimensions || [], [128, 128, 32, 255], 1),
    JSON.stringify(cubeArrayResult));

  const cubeArrayCompareResult = await page.evaluate(async () => {
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
        TextureCubeArray<float> depthImage32 : register(t0);
        TextureCubeArray<float> depthImage16 : register(t1);
        TextureCubeArray<float> depthImage24 : register(t2);
        TextureCubeArray<float> depthImage32S8 : register(t3);
        SamplerComparisonState lessSampler : register(s0);
        float4 main(float4 position : SV_Position) : SV_Target {
          float cubeIndex = position.x < 32.0 ? 0.0 : 1.0;
          float4 coordinate = float4(1.0, 0.0, 0.0, cubeIndex);
          float levelZero32 = depthImage32.SampleCmpLevelZero(lessSampler, coordinate, 0.5);
          float implicitLevel32 = depthImage32.SampleCmp(lessSampler, coordinate, 0.5);
          float levelZero16 = depthImage16.SampleCmpLevelZero(lessSampler, coordinate, 0.5);
          float implicitLevel16 = depthImage16.SampleCmp(lessSampler, coordinate, 0.5);
          float levelZero24 = depthImage24.SampleCmpLevelZero(lessSampler, coordinate, 0.5);
          float implicitLevel24 = depthImage24.SampleCmp(lessSampler, coordinate, 0.5);
          float levelZero32S8 = depthImage32S8.SampleCmpLevelZero(lessSampler, coordinate, 0.5);
          float implicitLevel32S8 = depthImage32S8.SampleCmp(lessSampler, coordinate, 0.5);
          bool implicitMatches = implicitLevel32 == levelZero32
            && implicitLevel16 == levelZero16 && implicitLevel24 == levelZero24
            && implicitLevel32S8 == levelZero32S8 && levelZero32S8 == levelZero32;
          return float4(levelZero32, levelZero16, levelZero24, implicitMatches ? 1.0 : 0.0);
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
      command(4, words(3, 4, 4, 1, 18, 40, 0x48, 0, 1, 0x4)); // D32_FLOAT cube array
      command(4, words(4, 4, 4, 1, 18, 55, 0x48, 0, 1, 0x4)); // D16_UNORM cube array
      command(4, words(11, 4, 4, 1, 18, 44, 0x48, 0, 1, 0x4)); // R24G8_TYPELESS cube array
      command(4, words(13, 4, 4, 1, 18, 19, 0x48, 0, 1, 0x4)); // R32G8X24_TYPELESS cube array
      for (let face = 0; face < 18; face++) {
        const dsv = 20 + face;
        const dsv16 = 50 + face;
        const dsv24 = 70 + face;
        const dsv32s8 = 90 + face;
        const depth = face < 6 ? 0x3f000000 : face < 12 ? 0x3e800000 : 0x3f400000;
        command(12, words(dsv, 1, 3, 40, 4, 0, 1, face, 1));
        command(28, words(dsv, 1, depth, 0));
        command(12, words(dsv16, 1, 4, 55, 4, 0, 1, face, 1));
        command(28, words(dsv16, 1, depth, 0));
        command(12, words(dsv24, 1, 11, 45, 4, 0, 1, face, 1));
        command(28, words(dsv24, 1, depth, 0));
        command(12, words(dsv32s8, 1, 13, 20, 4, 0, 1, face, 1));
        command(28, words(dsv32s8, 1, depth, 0));
      }
      command(12, words(5, 2, 3, 40, 10, 0, 1, 6, 2)); // two cubes starting at face 6
      command(12, words(8, 2, 4, 55, 10, 0, 1, 6, 2)); // D16 view of the same two cubes
      command(12, words(12, 2, 11, 46, 10, 0, 1, 6, 2)); // depth-only D24 view of the same two cubes
      command(12, words(14, 2, 13, 21, 10, 0, 1, 6, 2)); // depth-only D32S8 view of the same two cubes
      const sampler = new Uint8Array(56), samplerView = new DataView(sampler.buffer);
      samplerView.setUint32(0, 30, true); samplerView.setUint32(4, 0x80, true);
      samplerView.setUint32(8, 3, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      samplerView.setUint32(24, 1, true); samplerView.setUint32(28, 2, true); // point filter, LESS
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 0, true); command(8, sampler);
      shader(6, 0, vertex); shader(7, 1, pixel);
      command(17, words(0, 6)); command(17, words(1, 7));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 4, 5, 8, 12, 14)); command(20, words(1, 0, 1, 30));
      command(29, words(3, 0));
      const size = stream.reduce((total, part) => total + part.length, 0), commands = new Uint8Array(size);
      let offset = 0; for (const part of stream) { commands.set(part, offset); offset += part.length; }
      gpu.execute(commands);
      const pixels = await gpu.readBackbuffer(1);
      const at = (x) => Array.from(pixels.data.slice((32 * pixels.width + x) * 4, (32 * pixels.width + x) * 4 + 4));
      const result = {
        cube0: at(16), cube1: at(48),
        viewDimension: gpu.views.get(5)?.cache?.dim,
        view16Dimension: gpu.views.get(8)?.cache?.dim,
        view24Dimension: gpu.views.get(12)?.cache?.dim,
        view32S8Dimension: gpu.views.get(14)?.cache?.dim,
        draws: gpu.stats.draws, errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 D32/D16/D24/D32S8 TextureCubeArray comparisons cover depth-only SRV views',
    cubeArrayCompareResult.draws === 1 && cubeArrayCompareResult.errors?.length === 0
      && cubeArrayCompareResult.viewDimension === 'cube-array'
      && cubeArrayCompareResult.view16Dimension === 'cube-array'
      && cubeArrayCompareResult.view24Dimension === 'cube-array'
      && cubeArrayCompareResult.view32S8Dimension === 'cube-array'
      && near(cubeArrayCompareResult.cube0 || [], [0, 0, 0, 255], 1)
      && near(cubeArrayCompareResult.cube1 || [], [255, 255, 255, 255], 1),
    JSON.stringify(cubeArrayCompareResult));

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
        float4 main(float4 position : SV_Position) : SV_Target {
          return position.x < 20.0
            ? volume.SampleLevel(volumeSampler, float3(1.25, 0.5, 0.99), 0.0)
            : volume.SampleLevel(volumeSampler, float3(0.9, 0.5, 0.99), 0.0);
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
      samplerView.setUint32(8, 4, true); samplerView.setUint32(12, 3, true); samplerView.setUint32(16, 3, true);
      [0, 0, 1, 1].forEach((value, index) => samplerView.setFloat32(32 + index * 4, value, true));
      samplerView.setFloat32(48, 0, true); samplerView.setFloat32(52, 2, true); command(8, sampler);
      shader(7, 0, vertex); shader(8, 1, pixel);
      command(17, words(0, 7)); command(17, words(1, 8));
      command(7, words(10, 0)); command(15, words(10)); command(16, words(4));
      command(21, words(1, 2, 0)); command(19, words(1, 0, 1, 5)); command(20, words(1, 0, 1, 6));
      command(29, words(3, 0)); gpu.execute(append());
      const pixels = await gpu.readBackbuffer(1);
      const at = (x) => Array.from(pixels.data.slice((32 * pixels.width + x) * 4, (32 * pixels.width + x) * 4 + 4));
      const result = { outside: at(10), straddle: at(32), draws: gpu.stats.draws, errors: gpu.stats.errors };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 Texture3D CopyResource and BORDER sampling preserve slices and edge footprints',
    texture3dResult.draws === 1 && texture3dResult.errors.length === 0
      && near(texture3dResult.outside || [], [0, 0, 255, 255], 1)
      && near(texture3dResult.straddle || [], [0, 230, 26, 255], 2),
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
      const uintCompute = tool.compileHlsl(`
        RWTexture3D<uint> volume : register(u0);
        [numthreads(2, 2, 2)]
        void main(uint3 id : SV_DispatchThreadID) { volume[id] = id.x + id.y * 4 + id.z * 16; }
      `, 'main', 'cs_5_0');
      const uintPixel = tool.compileHlsl(`
        Texture3D<uint> volume : register(t0);
        float4 main() : SV_Target {
          uint value = volume.Load(int4(1, 1, 1, 0));
          return float4(float(value) / 31.0, 0.0, 0.0, 1.0);
        }
      `, 'main', 'ps_5_0');
      const rgba3dCompute = tool.compileHlsl(`
        RWTexture3D<float4> volume : register(u0);
        [numthreads(2, 2, 2)]
        void main(uint3 id : SV_DispatchThreadID) { volume[id] = float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'cs_5_0');
      const rgba3dPixel = tool.compileHlsl(`
        Texture3D<float4> volume : register(t0);
        float4 main() : SV_Target { return volume.Load(int4(1, 1, 1, 0)); }
      `, 'main', 'ps_5_0');
      const rgba32_3dCompute = tool.compileHlsl(`
        RWTexture3D<float4> volume : register(u0);
        [numthreads(2, 2, 2)]
        void main(uint3 id : SV_DispatchThreadID) { volume[id] = float4(0.25, 0.5, 0.75, 1.0); }
      `, 'main', 'cs_5_0');
      const rgba32_3dPixel = tool.compileHlsl(`
        Texture3D<float4> volume : register(t0);
        float4 main() : SV_Target { return volume.Load(int4(1, 1, 1, 0)); }
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
      command(29, words(3, 0));
      gpu.execute(append());
      const floatPixels = await gpu.readBackbuffer(1);
      const center = (32 * floatPixels.width + 32) * 4;
      const floatPixel = Array.from(floatPixels.data.slice(center, center + 4));
      stream.length = 0;
      command(46, words(13, 4, 4, 2, 1, 42, 0x88, 0, 0)); // 4x4x2 R32_UINT SRV + UAV volume
      command(12, words(14, 3, 13, 42, 8, 0, 1, 0, 2)); // 3D UAV covering both W slices
      command(12, words(15, 2, 13, 42, 8, 0, 1, 0, 1)); // 3D SRV
      shader(9, 2, uintCompute); shader(10, 1, uintPixel);
      command(17, words(2, 9)); command(43, words(0, 1, 14, 0xffffffff));
      command(42, words(2, 2, 1)); // 2x2x1 workgroups of 2x2x2 threads
      command(17, words(1, 10)); command(19, words(1, 0, 1, 15));
      command(21, words(1, 2, 0)); command(29, words(3, 0));
      gpu.execute(append());
      const uintBackbuffer = await gpu.readBackbuffer(1);
      const uintObservedPixel = Array.from(uintBackbuffer.data.slice(center, center + 4));
      const uintPixels = await gpu.readBackResource(13);
      const uintValues = Array.from(new Uint32Array(uintPixels.buffer, uintPixels.byteOffset, uintPixels.byteLength / 4));
      stream.length = 0;
      command(46, words(17, 4, 4, 2, 1, 28, 0x88, 0, 0)); // 4x4x2 RGBA8_UNORM SRV + UAV volume
      command(12, words(18, 3, 17, 28, 8, 0, 1, 0, 2)); // 3D UAV covering both W slices
      command(12, words(19, 2, 17, 28, 8, 0, 1, 0, 1)); // 3D SRV
      shader(11, 2, rgba3dCompute); shader(12, 1, rgba3dPixel);
      command(17, words(2, 11)); command(43, words(0, 1, 18, 0xffffffff));
      command(42, words(2, 2, 1));
      command(17, words(1, 12)); command(19, words(1, 0, 1, 19));
      command(21, words(1, 2, 0)); command(29, words(3, 0));
      gpu.execute(append());
      const rgbaBackbuffer = await gpu.readBackbuffer(1);
      const rgbaPixel = Array.from(rgbaBackbuffer.data.slice(center, center + 4));
      const rgbaPixels = await gpu.readBackResource(17);
      const rgbaValues = Array.from(rgbaPixels);
      stream.length = 0;
      command(46, words(21, 4, 4, 2, 1, 2, 0x88, 0, 0)); // 4x4x2 RGBA32_FLOAT SRV + UAV volume
      command(12, words(22, 3, 21, 2, 8, 0, 1, 0, 2)); // 3D UAV covering both W slices
      command(12, words(23, 2, 21, 2, 8, 0, 1, 0, 1)); // 3D SRV
      shader(13, 2, rgba32_3dCompute); shader(14, 1, rgba32_3dPixel);
      command(17, words(2, 13)); command(43, words(0, 1, 22, 0xffffffff));
      command(42, words(2, 2, 1));
      command(17, words(1, 14)); command(19, words(1, 0, 1, 23));
      command(21, words(1, 2, 0)); command(29, words(3, 0));
      gpu.execute(append());
      const rgba32Backbuffer = await gpu.readBackbuffer(1);
      const rgba32Pixel = Array.from(rgba32Backbuffer.data.slice(center, center + 4));
      const rgba32Pixels = await gpu.readBackResource(21);
      const rgba32Values = Array.from(new Float32Array(rgba32Pixels.buffer, rgba32Pixels.byteOffset, rgba32Pixels.byteLength / 4));
      const result = {
        pixel: floatPixel,
        uintPixel: uintObservedPixel,
        uintValues,
        rgbaPixel,
        rgbaValues,
        rgba32Pixel,
        rgba32Values,
        dispatches: gpu.stats.dispatches,
        draws: gpu.stats.draws,
        errors: gpu.stats.errors,
        logs: gpuLogs,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message, logs: gpuLogs };
    }
  });
  check('D3D11 R32_FLOAT 3D UAV writes remain visible through a Texture3D SRV',
    texture3dUavResult.dispatches === 4 && texture3dUavResult.draws === 4 && texture3dUavResult.errors.length === 0
      && near(texture3dUavResult.pixel, [64, 0, 0, 255], 1),
    JSON.stringify(texture3dUavResult));
  check('D3D11 R32_UINT Texture3D UAV writes reach all voxels and the following draw',
    texture3dUavResult.dispatches === 4 && texture3dUavResult.draws === 4 && texture3dUavResult.errors.length === 0
      && near(texture3dUavResult.uintPixel, [173, 0, 0, 255], 1)
      && texture3dUavResult.uintValues?.length === 32
      && texture3dUavResult.uintValues.every((value, i) => value === i),
    JSON.stringify(texture3dUavResult));
  check('D3D11 RGBA8_UNORM Texture3D UAV writes normalized voxels and samples them in the next draw',
    texture3dUavResult.dispatches === 4 && texture3dUavResult.draws === 4 && texture3dUavResult.errors.length === 0
      && near(texture3dUavResult.rgbaPixel, [64, 128, 191, 255], 1)
      && texture3dUavResult.rgbaValues?.length === 128
      && texture3dUavResult.rgbaValues.every((value, i) => Math.abs(value - [64, 128, 191, 255][i % 4]) <= 1),
    JSON.stringify(texture3dUavResult));
  check('D3D11 RGBA32_FLOAT Texture3D UAV writes all channels and samples them in the next draw',
    texture3dUavResult.dispatches === 4 && texture3dUavResult.draws === 4 && texture3dUavResult.errors.length === 0
      && near(texture3dUavResult.rgba32Pixel, [64, 128, 191, 255], 1)
      && texture3dUavResult.rgba32Values?.length === 128
      && texture3dUavResult.rgba32Values.every((value, i) => value === [0.25, 0.5, 0.75, 1.0][i % 4]),
    JSON.stringify(texture3dUavResult));

  const clearUavResult = await page.evaluate(async () => {
    const { ShaderTool } = await import('/shaders.js');
    const { GpuExecutor } = await import('/gpu.js');
    const canvas = document.createElement('canvas');
    canvas.width = 8; canvas.height = 8;
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
        const data = new Uint8Array(8 + payload.length), view = new DataView(data.buffer);
        view.setUint32(0, op, true); view.setUint32(4, payload.length, true); data.set(payload, 8); stream.push(data);
      };
      const append = () => {
        const data = new Uint8Array(stream.reduce((size, part) => size + part.length, 0));
        let offset = 0; for (const part of stream) { data.set(part, offset); offset += part.length; }
        stream.length = 0;
        return data;
      };
      const floatBits = (value) => { const bytes = new ArrayBuffer(4); new DataView(bytes).setFloat32(0, value, true); return new DataView(bytes).getUint32(0, true); };
      command(4, words(3, 2, 2, 1, 1, 28, 0x80, 0, 0, 1));
      command(12, words(4, 3, 3, 28, 4, 0, 1, 0, 0));
      const initial = new Uint8Array(16).fill(9), upload = new Uint8Array(36 + initial.length);
      upload.set(words(3, 0, 0, 0, 0, 2, 2, 8, initial.length)); upload.set(initial, 36); command(5, upload);
      command(50, words(4, 1, floatBits(0.25), floatBits(0.5), floatBits(0.75), floatBits(1)));
      gpu.execute(append());
      const floatClear = await gpu.readBackTexture(4);
      command(50, words(4, 0, 0xdead_beef, 0x102, 0x203, 0x304));
      gpu.execute(append());
      const uintClear = await gpu.readBackTexture(4);
      command(2, words(6, 16, 0x80, 0, 0, 0));
      const typedInitial = new Uint32Array([10, 11, 12, 13]);
      const typedUpload = new Uint8Array(12 + typedInitial.byteLength);
      typedUpload.set(words(6, 0, typedInitial.byteLength)); typedUpload.set(new Uint8Array(typedInitial.buffer), 12); command(3, typedUpload);
      command(12, words(5, 3, 6, 42, 1, 1, 2, 0, 0)); // R32_UINT view over elements 1 and 2
      command(50, words(5, 0, 0xdead_beef, 2, 3, 4));
      gpu.execute(append());
      const typedBuffer = await gpu.readBackResource(6);
      command(2, words(7, 16, 0x80, 0, 0x20, 0)); // raw UAV buffer
      const rawInitial = new Uint32Array([20, 21, 22, 23]);
      const rawUpload = new Uint8Array(12 + rawInitial.byteLength);
      rawUpload.set(words(7, 0, rawInitial.byteLength)); rawUpload.set(new Uint8Array(rawInitial.buffer), 12); command(3, rawUpload);
      command(12, words(8, 3, 7, 0, 1, 1, 2, 1, 0)); // raw view over dwords 1 and 2
      command(50, words(8, 0, 0x12345678, 0xaabbccdd, 0xeeeeeeee, 0xffffffff));
      gpu.execute(append());
      const rawBuffer = await gpu.readBackResource(7);
      command(2, words(21, 24, 0x80, 0, 0x40, 8)); // structured buffer with 8-byte elements
      const structuredInitial = new Uint32Array([30, 31, 32, 33, 34, 35]);
      const structuredUpload = new Uint8Array(12 + structuredInitial.byteLength);
      structuredUpload.set(words(21, 0, structuredInitial.byteLength)); structuredUpload.set(new Uint8Array(structuredInitial.buffer), 12); command(3, structuredUpload);
      command(12, words(22, 3, 21, 0, 1, 1, 1, 0, 0)); // one structured element at FirstElement=1
      command(50, words(22, 0, 0x55667788, 0xaabbccdd, 0xeeeeeeee, 0xffffffff));
      gpu.execute(append());
      const structuredBuffer = await gpu.readBackResource(21);
      command(46, words(9, 2, 2, 3, 1, 2, 0x80, 0, 0)); // 2x2x3 RGBA32_FLOAT texture
      command(12, words(10, 3, 9, 2, 8, 0, 1, 1, 1)); // only W slice 1
      command(50, words(10, 1, floatBits(0.5), floatBits(0.25), floatBits(0.75), floatBits(1)));
      gpu.execute(append());
      const volume = await gpu.readBackResource(9);
      command(4, words(11, 2, 2, 1, 3, 28, 0x80, 0, 0, 1)); // three-layer RGBA8 array
      command(12, words(12, 3, 11, 28, 5, 0, 1, 1, 1)); // only array layer 1
      command(50, words(12, 0, 0x11223344, 0x55667788, 0x99aabbcc, 0xddeeff00));
      gpu.execute(append());
      const arrayLayers = await Promise.all([0, 1, 2].map((slice) => gpu.readBackResource(11, slice)));
      command(2, words(13, 24, 0x80, 0, 0, 0));
      const rgInitial = new Float32Array([1, 2, 3, 4, 5, 6]);
      const rgUpload = new Uint8Array(12 + rgInitial.byteLength);
      rgUpload.set(words(13, 0, rgInitial.byteLength)); rgUpload.set(new Uint8Array(rgInitial.buffer), 12); command(3, rgUpload);
      command(12, words(14, 3, 13, 16, 1, 1, 1, 0, 0)); // one R32G32_FLOAT element at FirstElement=1
      command(50, words(14, 1, floatBits(0.5), floatBits(1), floatBits(3), floatBits(4)));
      gpu.execute(append());
      const rgBuffer = await gpu.readBackResource(13);
      command(4, words(15, 1, 1, 1, 1, 10, 0x80, 0, 0, 1)); // RGBA16_FLOAT
      command(12, words(16, 3, 15, 10, 4, 0, 1, 0, 0));
      command(50, words(16, 1, floatBits(0.5), floatBits(-1), floatBits(1.5), floatBits(1)));
      gpu.execute(append());
      const halfFloat = await gpu.readBackResource(15);
      command(4, words(17, 1, 1, 1, 1, 31, 0x80, 0, 0, 1)); // RGBA8_SNORM
      command(12, words(18, 3, 17, 31, 4, 0, 1, 0, 0));
      command(50, words(18, 1, floatBits(0.25), floatBits(-0.25), floatBits(0.5), floatBits(0.75)));
      gpu.execute(append());
      const snorm = await gpu.readBackResource(17);
      command(4, words(19, 1, 1, 1, 1, 12, 0x80, 0, 0, 1)); // RGBA16_UINT
      command(12, words(20, 3, 19, 12, 4, 0, 1, 0, 0));
      command(50, words(20, 0, 0x12345678, 0xabcdef01, 0x33335555, 0x9999aaaa));
      gpu.execute(append());
      const uint16 = await gpu.readBackResource(19);
      command(4, words(23, 1, 1, 1, 1, 41, 0x80, 0, 0, 1)); // R32_FLOAT
      command(12, words(24, 3, 23, 41, 4, 0, 1, 0, 0));
      command(50, words(24, 1, 0x7fc00000, floatBits(1), floatBits(2), floatBits(3)));
      gpu.execute(append());
      const nanFloat = await gpu.readBackResource(23);
      const result = {
        float: Array.from(floatClear.data), uint: Array.from(uintClear.data),
        typed: Array.from(new Uint32Array(typedBuffer.buffer, typedBuffer.byteOffset, typedBuffer.byteLength / 4)),
        raw: Array.from(new Uint32Array(rawBuffer.buffer, rawBuffer.byteOffset, rawBuffer.byteLength / 4)),
        structured: Array.from(new Uint32Array(structuredBuffer.buffer, structuredBuffer.byteOffset, structuredBuffer.byteLength / 4)),
        volume: Array.from(new Float32Array(volume.buffer, volume.byteOffset, volume.byteLength / 4)),
        array: arrayLayers.map((layer) => Array.from(layer)),
        rg: Array.from(new Float32Array(rgBuffer.buffer, rgBuffer.byteOffset, rgBuffer.byteLength / 4)),
        half: Array.from(new Uint16Array(halfFloat.buffer, halfFloat.byteOffset, halfFloat.byteLength / 2)),
        snorm: Array.from(new Int8Array(snorm.buffer, snorm.byteOffset, snorm.byteLength)),
        uint16: Array.from(new Uint16Array(uint16.buffer, uint16.byteOffset, uint16.byteLength / 2)),
        nanFloat: new Float32Array(nanFloat.buffer, nanFloat.byteOffset, nanFloat.byteLength / 4)[0],
        errors: gpu.stats.errors,
      };
      gpu.device.destroy();
      return result;
    } catch (error) {
      return { error: error.message };
    }
  });
  check('D3D11 ClearUnorderedAccessViewFloat converts values into the normalized view and orders after writes',
    clearUavResult.float?.length === 16
      && clearUavResult.float.every((value, i) => Math.abs(value - [64, 128, 191, 255][i % 4]) <= 1)
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint writes lower channel bits without format conversion',
    clearUavResult.uint?.length === 16
      && clearUavResult.uint.every((value, i) => value === [0xef, 0x02, 0x03, 0x04][i % 4])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint respects a typed buffer element range',
    JSON.stringify(clearUavResult.typed) === JSON.stringify([10, 0xdead_beef, 0xdead_beef, 13])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint repeats Values[0] over the selected raw buffer range',
    JSON.stringify(clearUavResult.raw) === JSON.stringify([20, 0x12345678, 0x12345678, 23])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint repeats Values[0] across the selected structured buffer element',
    JSON.stringify(clearUavResult.structured) === JSON.stringify([30, 31, 0x55667788, 0x55667788, 34, 35])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewFloat limits a Texture3D clear to FirstWSlice and WSize',
    clearUavResult.volume?.length === 48
      && clearUavResult.volume.slice(0, 16).every((value) => value === 0)
      && clearUavResult.volume.slice(16, 32).every((value, i) => value === [0.5, 0.25, 0.75, 1][i % 4])
      && clearUavResult.volume.slice(32).every((value) => value === 0)
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint limits a Texture2DArray clear to the selected layer',
    clearUavResult.array?.length === 3
      && clearUavResult.array[0].every((value) => value === 0)
      && clearUavResult.array[1].every((value, i) => value === [0x44, 0x88, 0xcc, 0x00][i % 4])
      && clearUavResult.array[2].every((value) => value === 0)
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewFloat respects typed RG32 buffer element size and range',
    JSON.stringify(clearUavResult.rg) === JSON.stringify([1, 2, 0.5, 1, 5, 6])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewFloat packs values into RGBA16_FLOAT channels',
    JSON.stringify(clearUavResult.half) === JSON.stringify([0x3800, 0xbc00, 0x3e00, 0x3c00])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewFloat converts signed normalized channels',
    JSON.stringify(clearUavResult.snorm) === JSON.stringify([32, -32, 64, 95])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewUint truncates each RGBA16_UINT channel to 16 bits',
    JSON.stringify(clearUavResult.uint16) === JSON.stringify([0x5678, 0xef01, 0x5555, 0xaaaa])
      && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));
  check('D3D11 ClearUnorderedAccessViewFloat preserves NaN in a float UAV',
    Number.isNaN(clearUavResult.nanFloat) && clearUavResult.errors?.length === 0,
    JSON.stringify(clearUavResult));

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
