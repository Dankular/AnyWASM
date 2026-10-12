import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { createServer as createProbe } from 'node:net';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { AssetRangeCache, fetchAssetIndex } from '../web/assets.js';

const CHUNK = 256 * 1024;

function makePackage(payload) {
  const name = 'data/blob.bin';
  const indexLength = 2 + Buffer.byteLength(name) + 16;
  const dataOffset = 24 + indexLength;
  const packageLength = dataOffset + payload.byteLength;
  const bytes = Buffer.alloc(packageLength);
  bytes.write('AGP1', 0, 'ascii');
  bytes.writeUInt32LE(1, 4);
  bytes.writeBigUInt64LE(BigInt(indexLength), 8);
  bytes.writeBigUInt64LE(BigInt(packageLength), 16);
  bytes.writeUInt16LE(Buffer.byteLength(name), 24);
  bytes.write(name, 26, 'utf8');
  bytes.writeBigUInt64LE(BigInt(dataOffset), 26 + Buffer.byteLength(name));
  bytes.writeBigUInt64LE(BigInt(payload.byteLength), 34 + Buffer.byteLength(name));
  Buffer.from(payload).copy(bytes, dataOffset);
  return { bytes, dataOffset };
}

async function serve(t, bytes, { ignoreRange = false, badContentRange = false } = {}) {
  const ranges = [];
  const server = createServer((req, res) => {
    ranges.push(req.headers.range ?? null);
    if (ignoreRange) {
      res.writeHead(200, { 'Content-Length': bytes.length });
      res.end(bytes);
      return;
    }
    const match = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? '');
    if (!match) {
      res.writeHead(416, { 'Content-Range': `bytes */${bytes.length}` }).end();
      return;
    }
    const start = Number(match[1]);
    const end = Number(match[2]);
    const body = bytes.subarray(start, end + 1);
    const contentRange = badContentRange
      ? `bytes ${start + 1}-${end + 1}/${bytes.length}`
      : `bytes ${start}-${end}/${bytes.length}`;
    res.writeHead(206, { 'Content-Length': body.length, 'Content-Range': contentRange });
    res.end(body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  return { url: `http://127.0.0.1:${server.address().port}/game.agp`, ranges };
}

test('local static server serves exact byte ranges from disk', async (t) => {
  const webDir = fileURLToPath(new URL('../web/', import.meta.url));
  const demoDir = join(webDir, 'demo');
  const name = `range-${process.pid}-${Date.now()}.agp`;
  const file = join(demoDir, name);
  mkdirSync(demoDir, { recursive: true });
  const body = Buffer.from([0, 1, 2, 3, 4, 5, 6, 7]);
  writeFileSync(file, body);
  t.after(() => rmSync(file, { force: true }));

  const probe = createProbe();
  await new Promise((resolve) => probe.listen(0, '127.0.0.1', resolve));
  const port = probe.address().port;
  await new Promise((resolve) => probe.close(resolve));
  const child = spawn(process.execPath, [join(webDir, 'serve.mjs'), String(port)], { cwd: webDir, stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(async () => {
    child.kill();
    await Promise.race([once(child, 'exit'), new Promise((resolve) => setTimeout(resolve, 1000))]);
  });
  let serverOutput = '';
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`local server did not start: ${serverOutput}`)), 5000);
    child.stdout.on('data', (chunk) => {
      serverOutput += chunk;
      if (serverOutput.includes(`http://localhost:${port}`)) {
        clearTimeout(timeout);
        resolve();
      }
    });
    child.once('error', reject);
    child.once('exit', (code) => reject(new Error(`local server exited ${code}: ${serverOutput}`)));
  });

  const home = await fetch(`http://127.0.0.1:${port}/`);
  assert.equal(home.status, 200);
  assert.match(await home.text(), /AnyWASM/);

  const response = await fetch(`http://127.0.0.1:${port}/demo/${basename(file)}`, { headers: { Range: 'bytes=2-5' } });
  assert.equal(response.status, 206);
  assert.equal(response.headers.get('content-range'), 'bytes 2-5/8');
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), body.subarray(2, 6));
  const unsatisfied = await fetch(`http://127.0.0.1:${port}/demo/${basename(file)}`, { headers: { Range: 'bytes=20-25' } });
  assert.equal(unsatisfied.status, 416);
  assert.equal(unsatisfied.headers.get('content-range'), 'bytes */8');
});

test('asset index fetch and concurrent reads use exact range requests and share cached chunks', async (t) => {
  const payload = Uint8Array.from({ length: CHUNK + 17 }, (_, i) => (i * 13) & 255);
  const pack = makePackage(payload);
  const server = await serve(t, pack.bytes);

  const index = await fetchAssetIndex(server.url);
  assert.equal(index.packageLength, BigInt(pack.bytes.length));
  assert.deepEqual(Buffer.from(index.bytes), pack.bytes.subarray(0, 24 + Number(pack.bytes.readBigUInt64LE(8))));

  const cache = new AssetRangeCache(server.url, index.packageLength);
  const start = BigInt(CHUNK - 4);
  await Promise.all([cache.ensureRange(start, 12), cache.ensureRange(start, 12)]);
  const out = new Uint8Array(12);
  assert.equal(cache.copyRange(start, out.length, out), true);
  assert.deepEqual(Buffer.from(out), pack.bytes.subarray(CHUNK - 4, CHUNK + 8));
  assert.equal(server.ranges.filter((range) => range === `bytes=0-${CHUNK - 1}`).length, 1);
  assert.equal(server.ranges.filter((range) => range?.startsWith(`bytes=${CHUNK}-`)).length, 1);
});

test('asset cache evicts least-recently-used chunks within its byte budget', async (t) => {
  const payload = new Uint8Array(CHUNK * 2).map((_, i) => i & 255);
  const pack = makePackage(payload);
  const server = await serve(t, pack.bytes);
  const cache = new AssetRangeCache(server.url, BigInt(pack.bytes.length), { maxBytes: CHUNK });

  await cache.ensureRange(BigInt(pack.dataOffset), 1);
  await cache.ensureRange(BigInt(pack.dataOffset + CHUNK), 1);
  await cache.ensureRange(BigInt(pack.dataOffset), 1);

  const payloadRanges = server.ranges.filter((range) => range === `bytes=0-${CHUNK - 1}` || range === `bytes=${CHUNK}-${CHUNK * 2 - 1}`);
  assert.equal(payloadRanges.length, 3);
});

test('asset range fetch rejects ignored and mismatched Content-Range responses', async (t) => {
  const pack = makePackage(new Uint8Array(32));
  const ignored = await serve(t, pack.bytes, { ignoreRange: true });
  const cache = new AssetRangeCache(ignored.url, BigInt(pack.bytes.length));
  await assert.rejects(cache.ensureRange(BigInt(pack.dataOffset), 8), /206/);
  assert.deepEqual(ignored.ranges, [`bytes=0-${pack.bytes.length - 1}`]);

  const malformed = await serve(t, pack.bytes, { badContentRange: true });
  const badCache = new AssetRangeCache(malformed.url, BigInt(pack.bytes.length));
  await assert.rejects(badCache.ensureRange(BigInt(pack.dataOffset), 8), /Content-Range/);
});

test('asset cache rejects one guest read that cannot fit without self-eviction', async (t) => {
  const pack = makePackage(new Uint8Array(CHUNK * 3));
  const server = await serve(t, pack.bytes);
  const cache = new AssetRangeCache(server.url, BigInt(pack.bytes.length), { maxBytes: CHUNK });
  await assert.rejects(cache.ensureRange(1n, CHUNK + 1), /cache capacity/);
  assert.deepEqual(server.ranges, []);
});
