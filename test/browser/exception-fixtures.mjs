// Runs compiler-generated MSVC exception fixtures through the shipped browser runtime.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const fixtureDirectory = join(repoRoot, 'target', 'msvc-cpp-eh');
const fixtureNames = [
  'eh3-dynamic',
  'eh4-dynamic',
  'eh3-x86-dynamic',
  'eh3-x86-direct',
  'seh-x86',
];
const missing = fixtureNames
  .map((name) => join(fixtureDirectory, `${name}.exe`))
  .filter((path) => !existsSync(path));
if (missing.length) {
  throw new Error(`build MSVC exception fixtures first: ${missing.join(', ')}`);
}
if (!process.env.CHROMIUM) throw new Error('set CHROMIUM to a local Chromium executable');

const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
const tempRoot = resolve(tmpdir());
const tempDirectory = mkdtempSync(join(tempRoot, 'anywasm-browser-exception-'));
assert.equal(dirname(resolve(tempDirectory)), tempRoot, 'temporary packages must stay in the temp directory');
const bundles = fixtureNames.map((name) => {
  const executable = join(fixtureDirectory, `${name}.exe`);
  const bundle = join(tempDirectory, `${name}.awb`);
  execFileSync(cliPath, ['pack', executable, '-o', bundle], { cwd: repoRoot, stdio: 'ignore' });
  return { name, bundle };
});

const server = spawn(process.execPath, ['web/serve.mjs', '8099'], { cwd: repoRoot, stdio: 'ignore' });
let browser;
try {
  await new Promise((resolve) => setTimeout(resolve, 800));
  browser = await chromium.launch({
    executablePath: process.env.CHROMIUM,
    args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan'],
  });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));
  await page.goto('http://localhost:8099/index.html?test=1');

  const results = [];
  for (const { name, bundle } of bundles) {
    const label = `${name}.awb`;
    await page.locator('#file').setInputFiles(bundle);
    await page.waitForFunction((expectedLabel) => {
      const status = document.querySelector('#stat');
      return status?.textContent?.startsWith(`${expectedLabel}:`) && status.dataset.status !== undefined;
    }, label, { timeout: 120_000 });
    const result = await page.locator('#stat').evaluate((status) => ({
      text: status.textContent,
      status: status.dataset.status,
      exitCode: status.dataset.exit,
    }));
    const terminalOutput = await page.locator('#term').textContent();
    assert.equal(result.status, '1', `${label} should exit cleanly: ${JSON.stringify({ result, terminalOutput })}`);
    assert.equal(result.exitCode, '0', `${label} returned a failing exit code: ${JSON.stringify({ result, terminalOutput })}`);
    results.push({ fixture: name, ...result });
  }

  assert.deepEqual(pageErrors, [], 'browser exception fixture runs should not raise page errors');
  console.log(JSON.stringify({ mode: 'HLE-only browser packages', results, pageErrors }, null, 2));
} finally {
  await browser?.close();
  server.kill();
  rmSync(tempDirectory, { recursive: true, force: true });
}
