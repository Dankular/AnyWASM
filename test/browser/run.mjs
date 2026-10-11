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


import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';

const server = spawn('node', ['web/serve.mjs', '8099'], { cwd: new URL('../../', import.meta.url).pathname, stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 800));
let failures = 0;
const check = (n, ok, d = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${n}${ok ? '' : ' :: ' + d}`); if (!ok) failures++; };

const dir = '/opt/pw-browsers';
const versioned = readdirSync(dir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const exe = process.env.CHROMIUM || (versioned ? `${dir}/${versioned}/chrome-linux/chrome` : undefined);
const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'] });
try {
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (m) => logs.push(m.text()));
  page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));

  await page.goto('http://localhost:8099/index.html?run=hello');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const text = await page.textContent('#term');
  const stat = await page.textContent('#stat');
  check('hello.exe output shown in the page', text.includes('Hello from a Windows PE running inside AnyWASM!'), text);
  check('status shows clean exit', /exited \(exit code 0\)/.test(stat), stat);

  await page.goto('http://localhost:8099/index.html?run=crt_test');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 30000 });
  const t2 = await page.textContent('#term');
  check('crt_test.exe full output in the page', t2.includes('sscanf=3 x+y=46 word=hello') && t2.includes('atexit handler ran') && t2.includes('arg2=second arg'), t2);
  check('crt_test.exe stderr shown', t2.includes('to stderr'), t2);
  check('crt_test.exe exit code 7', (await page.getAttribute('#stat', 'data-exit')) === '7');

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
  // The page runs in test mode (`&test=1`): the executor does not touch the canvas texture, because in
  // this headless Chromium build getCurrentTexture() breaks later mapAsync calls and screenshots do not
  // capture WebGPU canvases at all (both reproduced in isolation). The back buffer is read directly.
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

  // ── Direct3D 11 textured quads + depth buffer + indexed draws ──
  await page.goto('http://localhost:8099/index.html?run=d3d11_textured&test=1');
  await page.waitForFunction(() => document.getElementById('stat').dataset.status !== undefined, null, { timeout: 60000 });
  const texText = await page.textContent('#term');
  check('textured program ran to completion', texText.includes('frames=3') && /exited \(exit code 0\)/.test(await page.textContent('#stat')), texText);
  const g2 = await page.evaluate(() => window.__gpuStats);
  check('textured: 9 indexed draws, no validation errors', g2 && g2.draws >= 9 && g2.errors.length === 0, JSON.stringify(g2));
  await page.evaluate(() => { window.__pixels = null; window.__worker.postMessage({ type: 'capture' }); });
  await page.waitForFunction(() => window.__pixels, null, { timeout: 20000 });
  const p2 = await page.evaluate(() => { const p = window.__pixels; return { w: p.width, h: p.height, data: Array.from(p.data) }; });
  const at2 = (x, y) => { const i = (y * p2.w + x) * 4; return p2.data.slice(i, i + 4); };
  const RED = [255, 0, 0, 255], GREEN = [0, 255, 0, 255], BLUE = [0, 0, 255, 255], WHITE = [255, 255, 255, 255], BG = [0, 0, 51, 255];
  // Quad A: x 64..384, y 72..264, red.  Quad B: x 256..576, y 72..264, green, behind A.
  check('near quad A only region is red', near(at2(100, 150), RED, 1), JSON.stringify(at2(100, 150)));
  check('far quad B only region is green', near(at2(500, 150), GREEN, 1), JSON.stringify(at2(500, 150)));
  check('overlap shows the NEAR quad (depth test; B drawn after A)', near(at2(320, 150), RED, 1), JSON.stringify(at2(320, 150)));
  check('overlap edge: last column of A is red, first column right of A is green', near(at2(382, 150), RED, 1) && near(at2(387, 150), GREEN, 1), JSON.stringify([at2(382, 150), at2(387, 150)]));
  // Quad C: x 128..512, y 312..456, whole 2x2 texture with point sampling: red green / blue white.
  check('quad C top-left texel red', near(at2(200, 340), RED, 1), JSON.stringify(at2(200, 340)));
  check('quad C top-right texel green', near(at2(440, 340), GREEN, 1), JSON.stringify(at2(440, 340)));
  check('quad C bottom-left texel blue', near(at2(200, 430), BLUE, 1), JSON.stringify(at2(200, 430)));
  check('quad C bottom-right texel white', near(at2(440, 430), WHITE, 1), JSON.stringify(at2(440, 430)));
  check('background untouched', near(at2(10, 10), BG, 1) && near(at2(320, 290), BG, 1), JSON.stringify([at2(10, 10), at2(320, 290)]));
  check('point sampling gives hard texel edge (no blend at the seam)', [at2(319, 340), at2(320, 340)].every((c) => near(c, RED, 1) || near(c, GREEN, 1)), JSON.stringify([at2(319, 340), at2(320, 340)]));

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
}
console.log(failures ? `\n${failures} FAILED` : '\nbrowser tests passed');
process.exit(failures ? 1 : 0);
