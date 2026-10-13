// Profiles the WebGPU executor (web/gpu.js) on the synthetic texture-heavy fixture. Reports the
// JavaScript time spent issuing each command stream opcode. GPU-side execution time is not
// visible from here, and the workload is a proxy for a loading phase, not a real game.
import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { chromium } from 'playwright-core';

const exe = process.argv[2] || 'd3d11_texload';
const repoRoot = new URL('../../', import.meta.url).pathname;
const server = spawn(process.execPath, ['web/serve.mjs', '8141'], { cwd: repoRoot, stdio: 'ignore' });
await new Promise((resolve) => setTimeout(resolve, 800));
const dir = '/opt/pw-browsers';
const versioned = readdirSync(dir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const executablePath = process.env.CHROMIUM || (versioned ? `${dir}/${versioned}/chrome-linux/chrome` : undefined);
const browser = await chromium.launch({ executablePath, args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'] });
try {
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (m) => logs.push(m.text()));
  page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));
  const started = Date.now();
  await page.goto(`http://localhost:8141/index.html?run=${exe}&profileGpu${process.env.TEST_MODE ? '&test=1' : ''}`);
  await page.waitForFunction(() => document.getElementById('stat')?.dataset.status !== undefined, null, { timeout: 180000 });
  const wall = Date.now() - started;
  await page.waitForTimeout(500);
  const stats = await page.evaluate(() => window.__gpuStats);
  const stat = await page.textContent('#stat');
  console.log(`${exe}: wall ${wall} ms\n${stat}`);
  if (!stats?.profile) {
    console.log('no GPU profile received', JSON.stringify(stats)?.slice(0, 300), logs.filter((l) => /gpu|error/i.test(l)).slice(0, 5));
  } else {
    const rows = Object.entries(stats.profile).sort((a, b) => b[1].ms - a[1].ms);
    const total = rows.reduce((sum, [, e]) => sum + e.ms, 0);
    console.log(`draws=${stats.draws} presents=${stats.presents} passes=${stats.passes} pipelines=${stats.pipelines} errors=${stats.errors.length}`);
    console.log(`JS time issuing GPU commands: ${total.toFixed(1)} ms\n`);
    console.log('opcode'.padEnd(20), 'count'.padStart(7), 'total ms'.padStart(10), 'share'.padStart(7), 'max ms'.padStart(9), 'MiB'.padStart(9));
    for (const [name, e] of rows.slice(0, 14)) {
      console.log(name.padEnd(20), String(e.count).padStart(7), e.ms.toFixed(1).padStart(10), `${(100 * e.ms / total).toFixed(1)}%`.padStart(7), e.maxMs.toFixed(1).padStart(9), (e.bytes / 1048576).toFixed(1).padStart(9));
    }
  }
} finally {
  await browser.close();
  server.kill();
}
