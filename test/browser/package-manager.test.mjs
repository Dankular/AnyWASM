import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { readFile, access, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const state = { catalog: null, manifests: new Map(), bundles: new Map(), bundleRequests: [] };
let server;
let browser;
let origin;

async function responseFor(pathname) {
  if (pathname === '/') return { contentType: 'text/html', body: '<!doctype html><title>Package tests</title>' };
  if (pathname === '/fixture/catalog.json' || pathname === '/web/catalog.json') {
    return { contentType: 'application/json', body: JSON.stringify(state.catalog) };
  }
  if (state.manifests.has(pathname)) {
    return { contentType: 'application/json', body: JSON.stringify(state.manifests.get(pathname)) };
  }
  if (state.bundles.has(pathname)) {
    state.bundleRequests.push(pathname);
    const bundle = state.bundles.get(pathname);
    return {
      status: bundle.status,
      contentType: 'application/octet-stream',
      body: bundle.bytes,
      contentLength: bundle.contentLength,
      interrupted: bundle.interrupted,
    };
  }
  if (pathname.startsWith('/web/')) {
    const relative = pathname.slice('/web/'.length);
    if (relative.includes('..')) return { status: 400, contentType: 'text/plain', body: 'invalid path' };
    try {
      const body = await readFile(join(repoRoot, 'web', relative));
      const contentType = relative.endsWith('.js') ? 'text/javascript'
        : relative.endsWith('.html') ? 'text/html'
          : relative.endsWith('.json') ? 'application/json' : 'application/octet-stream';
      return { contentType, body };
    } catch {
      return { status: 404, contentType: 'text/plain', body: 'not found' };
    }
  }
  return { status: 404, contentType: 'text/plain', body: 'not found' };
}

function packageFixture(id, { version = '1.0.0', dependencies = [], bytes = Buffer.from(id) } = {}) {
  const slug = `${id}-${version}`;
  const manifestPath = `/fixture/manifests/${slug}.json`;
  const bundlePath = `/fixture/bundles/${slug}.awb`;
  return {
    entry: { id, version, name: id, manifest: manifestPath },
    manifestPath,
    manifest: {
      schemaVersion: 1,
      id,
      version,
      name: id,
      entry: 'app.exe',
      dependencies,
      bundle: {
        url: `../bundles/${slug}.awb`,
        size: bytes.byteLength,
        sha256: createHash('sha256').update(bytes).digest('hex'),
      },
    },
    bundlePath,
    bundle: { status: 200, bytes },
  };
}

function configurePackages(fixtures) {
  state.manifests = new Map(fixtures.map((fixture) => [fixture.manifestPath, fixture.manifest]));
  state.bundles = new Map(fixtures.map((fixture) => [fixture.bundlePath, fixture.bundle]));
  state.bundleRequests = [];
  state.catalog = { schemaVersion: 1, name: 'Fixture Catalog', packages: fixtures.map((fixture) => fixture.entry) };
}

async function openManager(t) {
  const context = await browser.newContext();
  t.after(() => context.close());
  const page = await context.newPage();
  await page.goto(origin);
  await page.evaluate(async (catalogUrl) => {
    const { PackageManager } = await import('/web/package-manager.js');
    window.testPackageManager = new PackageManager(catalogUrl);
    await window.testPackageManager.refreshCatalogs();
  }, `${origin}/fixture/catalog.json`);
  return page;
}

async function openApp(t) {
  const context = await browser.newContext();
  t.after(() => context.close());
  const page = await context.newPage();
  await page.goto(`${origin}/web/index.html`);
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('available'));
  return page;
}

before(async () => {
  server = createServer(async (request, response) => {
    const result = await responseFor(new URL(request.url, 'http://localhost').pathname);
    if (result.interrupted) {
      response.writeHead(result.status || 200, {
        'content-type': result.contentType,
        'content-length': String(result.contentLength),
        connection: 'close',
      });
      response.socket.end(result.body);
      return;
    }
    response.writeHead(result.status || 200, { 'content-type': result.contentType });
    response.end(result.body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;

  let executablePath = process.env.CHROMIUM;
  if (!executablePath && process.platform === 'win32') {
    executablePath = 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe';
  } else if (!executablePath && process.platform !== 'win32') {
    const browserDir = '/opt/pw-browsers';
    await access(browserDir);
    const versioned = (await readdir(browserDir)).filter((name) => /^chromium-\d+$/.test(name)).sort().pop();
    executablePath = versioned ? join(browserDir, versioned, 'chrome-linux', 'chrome') : undefined;
  }
  browser = await chromium.launch({ ...(executablePath ? { executablePath } : {}), headless: true, args: ['--no-sandbox'] });
});

after(async () => {
  await browser?.close();
  await new Promise((resolve, reject) => server?.close((error) => error ? reject(error) : resolve()));
});

test('installs the exact 128-package dependency limit', async (t) => {
  const fixtures = Array.from({ length: 128 }, (_, index) => {
    const id = `pkg-${String(index).padStart(3, '0')}`;
    const dependencies = index === 127 ? [] : [{ id: `pkg-${String(index + 1).padStart(3, '0')}`, version: '1.0.0' }];
    return packageFixture(id, { dependencies });
  });
  configurePackages(fixtures);
  const page = await openManager(t);
  const installed = await page.evaluate(() => window.testPackageManager.install('pkg-000', '1.0.0'));
  assert.equal(installed.length, 128);
  assert.equal((await page.evaluate(() => window.testPackageManager.listInstalled())).length, 128);
});

test('rejects a dependency graph above the 128-package limit before downloading bundles', async (t) => {
  const fixtures = Array.from({ length: 129 }, (_, index) => {
    const id = `pkg-${String(index).padStart(3, '0')}`;
    const dependencies = index === 128 ? [] : [{ id: `pkg-${String(index + 1).padStart(3, '0')}`, version: '1.0.0' }];
    return packageFixture(id, { dependencies });
  });
  configurePackages(fixtures);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.install('pkg-000', '1.0.0')), /dependency graph exceeds 128 packages/);
  assert.equal(state.bundleRequests.length, 0);
  assert.equal((await page.evaluate(() => window.testPackageManager.listInstalled())).length, 0);
});

test('rejects malformed manifest identity before downloading its bundle', async (t) => {
  const fixture = packageFixture('invalid-app');
  fixture.manifest.id = 'different-app';
  configurePackages([fixture]);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.install('invalid-app', '1.0.0')), /manifest identity does not match/);
  assert.equal(state.bundleRequests.length, 0);
  assert.equal((await page.evaluate(() => window.testPackageManager.listInstalled())).length, 0);
});

test('rejects a missing pinned dependency before downloading artifacts', async (t) => {
  configurePackages([packageFixture('root-app', { dependencies: [{ id: 'missing-lib', version: '1.0.0' }] })]);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.install('root-app', '1.0.0')), /missing dependency missing-lib@1.0.0/);
  assert.equal(state.bundleRequests.length, 0);
});

test('rejects a dependency cycle before downloading artifacts', async (t) => {
  configurePackages([
    packageFixture('cycle-a', { dependencies: [{ id: 'cycle-b', version: '1.0.0' }] }),
    packageFixture('cycle-b', { dependencies: [{ id: 'cycle-a', version: '1.0.0' }] }),
  ]);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.install('cycle-a', '1.0.0')), /dependency cycle includes cycle-a@1.0.0/);
  assert.equal(state.bundleRequests.length, 0);
});

test('rejects conflicting exact versions selected through dependencies before downloads', async (t) => {
  configurePackages([
    packageFixture('root-app', { dependencies: [
      { id: 'left-lib', version: '1.0.0' },
      { id: 'right-lib', version: '1.0.0' },
    ] }),
    packageFixture('left-lib', { dependencies: [{ id: 'shared-lib', version: '1.0.0' }] }),
    packageFixture('right-lib', { dependencies: [{ id: 'shared-lib', version: '2.0.0' }] }),
    packageFixture('shared-lib', { version: '1.0.0' }),
    packageFixture('shared-lib', { version: '2.0.0' }),
  ]);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.install('root-app', '1.0.0')), /dependency conflict for shared-lib/);
  assert.equal(state.bundleRequests.length, 0);
});

test('keeps a previously installed version when replacement download is interrupted', async (t) => {
  const fixture = packageFixture('stable-app', { bytes: Buffer.from('previous valid bundle') });
  configurePackages([fixture]);
  const page = await openManager(t);
  await page.evaluate(() => window.testPackageManager.install('stable-app', '1.0.0'));
  fixture.manifest.bundle = {
    url: '../bundles/stable-app-1.0.0.awb',
    size: Buffer.byteLength('replacement bundle'),
    sha256: createHash('sha256').update('replacement bundle').digest('hex'),
  };
  state.bundles.set(fixture.bundlePath, {
    status: 200,
    bytes: Buffer.from('short'),
    contentLength: Buffer.byteLength('replacement bundle'),
    interrupted: true,
  });
  await assert.rejects(
    page.evaluate(() => window.testPackageManager.install('stable-app', '1.0.0')),
    /Failed to fetch|ERR_CONTENT_LENGTH|network/i,
  );
  const savedBytes = await page.evaluate(async () => {
    const record = await window.testPackageManager.getInstalled('stable-app', '1.0.0');
    return Array.from(new Uint8Array(record.bundle));
  });
  assert.deepEqual(Buffer.from(savedBytes), Buffer.from('previous valid bundle'));
});

test('shows storage quota failures without reporting a package as installed', async (t) => {
  configurePackages([packageFixture('quota-app')]);
  const page = await openApp(t);
  await page.evaluate(() => {
    const originalPut = IDBObjectStore.prototype.put;
    IDBObjectStore.prototype.put = function (...args) {
      if (this.name === 'installs') throw new DOMException('simulated storage quota', 'QuotaExceededError');
      return originalPut.apply(this, args);
    };
  });
  await page.locator('.package-card').filter({ hasText: 'quota-app' }).getByRole('button', { name: 'Install' }).click();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('Install failed: simulated storage quota'));
  assert.equal(await page.locator('.package-card').filter({ hasText: 'quota-app' }).getByRole('button', { name: 'Install' }).count(), 1);
  assert.equal(await page.locator('.package-card').filter({ hasText: 'quota-app' }).getByRole('button', { name: 'Launch' }).count(), 0);
  await page.reload();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('0 installed'));
  assert.equal(await page.locator('.package-card').filter({ hasText: 'quota-app' }).getByRole('button', { name: 'Install' }).count(), 1);
});

test('persists installs across reload and blocks removing a required dependency', async (t) => {
  configurePackages([
    packageFixture('base-lib'),
    packageFixture('test-app', { dependencies: [{ id: 'base-lib', version: '1.0.0' }] }),
  ]);
  const page = await openApp(t);
  const appCard = page.locator('.package-card').filter({ hasText: 'test-app' });
  await appCard.getByRole('button', { name: 'Install' }).click();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('2 installed'));
  await page.reload();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('2 installed'));

  const baseCard = page.locator('.package-card').filter({ hasText: 'base-lib' });
  assert.equal(await appCard.getByRole('button', { name: 'Launch' }).count(), 1);
  await baseCard.getByRole('button', { name: 'Remove' }).click();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('is required by test-app@1.0.0'));
  assert.equal(await baseCard.getByRole('button', { name: 'Remove' }).isEnabled(), true);

  await page.locator('.package-card').filter({ hasText: 'test-app' }).getByRole('button', { name: 'Remove' }).click();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('1 installed'));
  await page.locator('.package-card').filter({ hasText: 'base-lib' }).getByRole('button', { name: 'Remove' }).click();
  await page.waitForFunction(() => document.querySelector('#package-status')?.textContent.includes('0 installed'));
  assert.equal(await page.locator('.package-card').filter({ hasText: 'base-lib' }).getByRole('button', { name: 'Install' }).count(), 1);
});

test('rejects a script URL before storing an added catalog', async (t) => {
  configurePackages([packageFixture('safe-app')]);
  const page = await openManager(t);
  await assert.rejects(page.evaluate(() => window.testPackageManager.addCatalog('javascript:alert(1)')), /must use HTTP or HTTPS/);
  const catalogs = await page.evaluate(() => window.testPackageManager.listCatalogs());
  assert.equal(catalogs.length, 1);
});
