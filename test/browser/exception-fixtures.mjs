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
const fixtureCases = [
  { name: 'eh3-dynamic', status: '1', exitCode: '0' },
  { name: 'eh4-dynamic', status: '1', exitCode: '0' },
  { name: 'eh3-x86-dynamic', status: '1', exitCode: '0' },
  { name: 'eh3-x86-direct', status: '1', exitCode: '0' },
  { name: 'seh-x86', status: '1', exitCode: '0' },
  { name: 'terminate-x86', status: '3', exitCode: '0', message: 'std::terminate was called' },
  { name: 'terminate-unhandled-x86', status: '3', exitCode: '0', message: 'std::terminate was called' },
];
const missing = fixtureCases
  .map(({ name }) => join(fixtureDirectory, `${name}.exe`))
  .filter((path) => !existsSync(path));
if (missing.length) {
  throw new Error(`build MSVC exception fixtures first: ${missing.join(', ')}`);
}
if (!process.env.CHROMIUM) throw new Error('set CHROMIUM to a local Chromium executable');

const cliPath = join(repoRoot, 'target', 'debug', `anywasm${process.platform === 'win32' ? '.exe' : ''}`);
const tempRoot = resolve(tmpdir());
const tempDirectory = mkdtempSync(join(tempRoot, 'anywasm-browser-exception-'));
assert.equal(dirname(resolve(tempDirectory)), tempRoot, 'temporary packages must stay in the temp directory');
const bundles = fixtureCases.map(({ name, ...expected }) => {
  const executable = join(fixtureDirectory, `${name}.exe`);
  const bundle = join(tempDirectory, `${name}.awb`);
  execFileSync(cliPath, ['pack', executable, '-o', bundle], { cwd: repoRoot, stdio: 'ignore' });
  return { name, bundle, expected };
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
  for (const { name, bundle, expected } of bundles) {
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
    assert.equal(result.status, expected.status, `${label} returned the wrong status: ${JSON.stringify({ result, terminalOutput })}`);
    assert.equal(result.exitCode, expected.exitCode, `${label} returned the wrong exit code: ${JSON.stringify({ result, terminalOutput })}`);
    if (expected.message) assert.ok(result.text.includes(expected.message), `${label} missed its diagnostic: ${result.text}`);
    results.push({ fixture: name, ...result });
  }

  assert.deepEqual(pageErrors, [], 'browser exception fixture runs should not raise page errors');
  console.log(JSON.stringify({ mode: 'HLE-only browser packages', results, pageErrors }, null, 2));
} finally {
  await browser?.close();
  server.kill();
  rmSync(tempDirectory, { recursive: true, force: true });
}
