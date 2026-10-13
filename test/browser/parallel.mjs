// Runs the two-thread prime-counting fixture in headless Chromium, once with the cooperative
// scheduler and once with `?sharedCpuWorkers`, and reports which mode actually ran, the result and
// the wall time. Correctness (identical output and exit code) is asserted in both modes; the
// speedup is only reported, because it depends on the machine's core count.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { chromium } from 'playwright-core';

const repoRoot = new URL('../../', import.meta.url).pathname;
const server = spawn(process.execPath, ['web/serve.mjs', '8131'], { cwd: repoRoot, stdio: 'ignore' });
await new Promise((resolve) => setTimeout(resolve, 800));
const dir = '/opt/pw-browsers';
const versioned = readdirSync(dir).filter((d) => /^chromium-\d+$/.test(d)).sort().pop();
const executablePath = process.env.CHROMIUM || (versioned ? `${dir}/${versioned}/chrome-linux/chrome` : undefined);
const browser = await chromium.launch({ executablePath, args: ['--no-sandbox'] });
const results = {};
try {
  for (const [name, query] of [['cooperative', ''], ['shared', '&sharedCpuWorkers']]) {
    const page = await browser.newPage();
    const logs = [];
    page.on('console', (m) => logs.push(m.text()));
    page.on('pageerror', (e) => logs.push('PAGEERROR ' + e.message));
    const started = Date.now();
    await page.goto(`http://localhost:8131/index.html?run=parallel_test${query}`);
    await page.waitForFunction(() => document.getElementById('stat')?.dataset.status !== undefined, null, { timeout: 120000 });
    const elapsed = Date.now() - started;
    const output = await page.textContent('#term');
    const stat = await page.textContent('#stat');
    const mode = await page.evaluate(() => window.__executionMode);
    console.log(`${name}: ${elapsed} ms · mode=${mode?.mode} workers=${mode?.workerCount} reason=${mode?.reason || '-'}\n  stat: ${stat}`);
    results[name] = { elapsed, mode, output, stat, logs };
    assert.match(output, /t0=5133 t1=4459 total=9592/, `${name}: wrong thread results: ${output}`);
    assert.match(stat, /exit code 0/, `${name}: ${stat}`);
    await page.close();
  }
  if (results.shared.mode?.mode !== 'shared-cpu-workers') {
    console.log(`NOTE: shared CPU workers were not used (${results.shared.mode?.reason || 'no reason reported'}); the run fell back to cooperative execution.`);
    process.exitCode = process.env.REQUIRE_SHARED ? 1 : 0;
  } else {
    console.log(`shared/cooperative wall time ratio: ${(results.shared.elapsed / results.cooperative.elapsed).toFixed(2)}`);
  }
} finally {
  await browser.close();
  server.kill();
}
