import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFileSync } from 'node:child_process';
import { readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { AnyWasm } from '../web/anywasm.js';

const repo = fileURLToPath(new URL('../', import.meta.url));
const runtimeBytes = readFileSync(join(repo, 'target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm'));
const cli = join(repo, `target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`);
const tempBundle = join(tmpdir(), `anywasm-asset-test-${process.pid}.awb`);
execFileSync(cli, ['pack', join(repo, 'fixtures/hello.exe'), '-o', tempBundle], { cwd: repo, stdio: 'ignore' });

function makePackage(path, payload) {
  const name = Buffer.from(path, 'utf8');
  const indexLength = 2 + name.length + 16;
  const dataOffset = 24 + indexLength;
  const packageLength = dataOffset + payload.length;
  const bytes = Buffer.alloc(packageLength);
  bytes.write('AGP1', 0, 'ascii');
  bytes.writeUInt32LE(1, 4);
  bytes.writeBigUInt64LE(BigInt(indexLength), 8);
  bytes.writeBigUInt64LE(BigInt(packageLength), 16);
  bytes.writeUInt16LE(name.length, 24);
  name.copy(bytes, 26);
  bytes.writeBigUInt64LE(BigInt(dataOffset), 26 + name.length);
  bytes.writeBigUInt64LE(BigInt(payload.length), 34 + name.length);
  payload.copy(bytes, dataOffset);
  return { bytes, dataOffset, indexEnd: dataOffset };
}

const payload = Buffer.from('only-the-requested-game-bytes');
const pack = makePackage('data\\stream.bin', payload);
const ranges = [];
const server = createServer((req, res) => {
  ranges.push(req.headers.range);
  const match = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? '');
  if (!match) { res.writeHead(416).end(); return; }
  const start = Number(match[1]);
  const end = Number(match[2]);
  const data = pack.bytes.subarray(start, end + 1);
  res.writeHead(206, {
    'Content-Length': data.length,
    'Content-Range': `bytes ${start}-${end}/${pack.bytes.length}`,
  });
  res.end(data);
});

try {
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = `http://127.0.0.1:${server.address().port}/game.agp`;
  const wasm = await AnyWasm.create(runtimeBytes);
  wasm.load(readFileSync(tempBundle));
  await wasm.attachAssets(url);
  assert.equal(typeof wasm.x.aw_test_asset_read, 'function', 'WASM test API must expose remote VFS reads');

  const name = new TextEncoder().encode('data/stream.bin');
  const namePtr = wasm.x.aw_alloc(name.length) >>> 0;
  const outputPtr = wasm.x.aw_alloc(payload.length) >>> 0;
  try {
    new Uint8Array(wasm.x.memory.buffer, namePtr, name.length).set(name);
    let result = wasm.x.aw_test_asset_read(namePtr, name.length, 0n, payload.length, outputPtr);
    let state = wasm.x.asyncify_get_state();
    assert.equal(state, 1, 'first cache miss must unwind the guest stack');
    wasm.x.asyncify_stop_unwind();
    await wasm.waitForHost();
    wasm.x.asyncify_start_rewind(wasm.asyncifyData);
    result = wasm.x.aw_test_asset_read(namePtr, name.length, 0n, payload.length, outputPtr);
    state = wasm.x.asyncify_get_state();
    assert.equal(state, 0, 'rewind must finish after cached bytes are copied');
    assert.equal(result, 0);
    assert.deepEqual(Buffer.from(new Uint8Array(wasm.x.memory.buffer, outputPtr, payload.length)), payload);

    const requestsAfterMiss = ranges.length;
    result = wasm.x.aw_test_asset_read(namePtr, name.length, 0n, payload.length, outputPtr);
    assert.equal(result, 0);
    assert.equal(ranges.length, requestsAfterMiss, 'cache hit must not fetch another range');
    assert.deepEqual(ranges.slice(0, 2), ['bytes=0-23', `bytes=24-${pack.indexEnd - 1}`]);
    assert.equal(ranges.length, 3, 'exactly one payload chunk should be fetched');
  } finally {
    wasm.x.aw_free(namePtr, name.length);
    wasm.x.aw_free(outputPtr, payload.length);
  }
  console.log('WASM asset streaming smoke passed: cache miss unwound, fetched one range, and resumed with exact bytes.');
} finally {
  await new Promise((resolve) => server.close(resolve));
  rmSync(tempBundle, { force: true });
}
