import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AssetRangeCache, ASSET_CHUNK_SIZE } from '../web/assets.js';
import { createDirectoryAssetIndex, createRemoteDirectoryFiles, DirectoryAssetSource } from '../web/direct-assets.js';

function folderFile(path, content, { shortRead = false, lastModified = 1_700_000_000_000 } = {}) {
  const bytes = Buffer.from(content);
  const slices = [];
  return {
    name: path.split('/').at(-1),
    size: bytes.length,
    lastModified,
    webkitRelativePath: `Install/${path}`,
    slices,
    arrayBuffer: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    slice(start, end) {
      slices.push([start, end]);
      const selected = bytes.subarray(start, shortRead ? Math.max(start, end - 1) : end);
      return new Blob([selected]);
    },
  };
}

function readIndexRecords(index) {
  const view = new DataView(index.bytes.buffer, index.bytes.byteOffset, index.bytes.byteLength);
  const records = [];
  let cursor = 24;
  for (let i = 0; i < view.getUint32(4, true); i++) {
    const pathLength = view.getUint16(cursor, true);
    cursor += 2;
    const path = new TextDecoder().decode(index.bytes.subarray(cursor, cursor + pathLength));
    cursor += pathLength;
    records.push({
      path,
      start: view.getBigUint64(cursor, true),
      length: view.getBigUint64(cursor + 8, true),
      lastAccessTime: view.getBigInt64(cursor + 16, true),
      lastWriteTime: view.getBigInt64(cursor + 24, true),
      creationTime: view.getBigInt64(cursor + 32, true),
      mode: view.getUint32(cursor + 40, true),
      nlink: view.getUint16(cursor + 44, true),
      isDirectory: view.getUint8(cursor + 46) === 1,
    });
    cursor += 47;
  }
  return records;
}

test('directory index normalizes the selected root and preserves metadata for boot images', () => {
  const exe = folderFile('Game.exe', 'MZ');
  const dll = folderFile('x.dll', 'DLL');
  const a = folderFile('data/a.bin', [1, 2, 3]);
  const b = folderFile('data/b.bin', [4, 5]);
  const index = createDirectoryAssetIndex([exe, dll, a, b], ['game.exe', 'x.dll']);

  assert.equal(new TextDecoder().decode(index.bytes.subarray(0, 4)), 'AGP2');
  assert.equal(index.executables.join(','), 'game.exe');
  assert.equal(index.packageLength, BigInt(index.bytes.byteLength + 10));
  assert.deepEqual(readIndexRecords(index), [
    { path: 'data\\a.bin', start: BigInt(index.bytes.byteLength), length: 3n, lastAccessTime: -(1n << 63n), lastWriteTime: 1_700_000_000n, creationTime: -(1n << 63n), mode: 0xffff_ffff, nlink: 0xffff, isDirectory: false },
    { path: 'data\\b.bin', start: BigInt(index.bytes.byteLength + 3), length: 2n, lastAccessTime: -(1n << 63n), lastWriteTime: 1_700_000_000n, creationTime: -(1n << 63n), mode: 0xffff_ffff, nlink: 0xffff, isDirectory: false },
    { path: 'game.exe', start: BigInt(index.bytes.byteLength + 5), length: 2n, lastAccessTime: -(1n << 63n), lastWriteTime: 1_700_000_000n, creationTime: -(1n << 63n), mode: 0xffff_ffff, nlink: 0xffff, isDirectory: false },
    { path: 'x.dll', start: BigInt(index.bytes.byteLength + 7), length: 3n, lastAccessTime: -(1n << 63n), lastWriteTime: 1_700_000_000n, creationTime: -(1n << 63n), mode: 0xffff_ffff, nlink: 0xffff, isDirectory: false },
  ]);
  assert.equal(index.filesByPath.get('data\\a.bin'), a);
});

test('directory index records real metadata for explicit directories without adding payload bytes', () => {
  const dir = {
    name: 'assets', size: 0, relativePath: 'game/assets', webkitRelativePath: 'Install/game/assets', isDirectory: true,
    lastAccessTime: 1_600_000_000, lastWriteTime: 1_700_000_000, creationTime: 1_500_000_000,
    mode: 0o040755, nlink: 1, arrayBuffer: async () => new ArrayBuffer(0),
  };
  const file = folderFile('game/assets/data.bin', [1, 2, 3]);
  const index = createDirectoryAssetIndex([dir, file]);
  const records = readIndexRecords(index);

  assert.equal(records[0].path, 'game\\assets');
  assert.equal(records[0].isDirectory, true);
  assert.equal(records[0].lastAccessTime, 1_600_000_000n);
  assert.equal(records[0].mode, 0o040755);
  assert.equal(records[1].path, 'game\\assets\\data.bin');
  assert.equal(records[1].isDirectory, false);
  assert.equal(index.packageLength, BigInt(index.bytes.byteLength + 3));
});

test('directory index rejects duplicate case-folded paths and traversal paths', () => {
  assert.throws(() => createDirectoryAssetIndex([
    folderFile('data/same.bin', [1]),
    folderFile('DATA/SAME.BIN', [2]),
  ]), /duplicate.*data\\same\.bin/i);
  assert.throws(() => createDirectoryAssetIndex([
    folderFile('../outside.exe', 'MZ'),
  ]), /unsafe.*path/i);
});

test('remote directory files preserve host metadata and stream exact byte ranges', async () => {
  const requests = [];
  const [file] = createRemoteDirectoryFiles([{
    path: 'Game/Bin/data.bin', size: 4, lastAccessTime: 1_600_000_000,
    lastWriteTime: 1_700_000_000, creationTime: 1_500_000_000, mode: 0o100644, nlink: 2,
  }], 'http://127.0.0.1/__local/sims3/file', async (url, options) => {
    const range = options.headers?.Range;
    requests.push({ path: new URL(url).searchParams.get('path'), range });
    if (range) return new Response(new Uint8Array([20, 30]), {
      status: 206,
      headers: { 'Content-Range': 'bytes 1-2/4' },
    });
    return new Response(new Uint8Array([10, 20, 30, 40]), { status: 200 });
  });

  assert.deepEqual([...new Uint8Array(await file.slice(1, 3).arrayBuffer())], [20, 30]);
  assert.deepEqual([...new Uint8Array(await file.arrayBuffer())], [10, 20, 30, 40]);
  assert.deepEqual({
    lastAccessTime: file.lastAccessTime,
    lastWriteTime: file.lastWriteTime,
    creationTime: file.creationTime,
    mode: file.mode,
    nlink: file.nlink,
  }, {
    lastAccessTime: 1_600_000_000,
    lastWriteTime: 1_700_000_000,
    creationTime: 1_500_000_000,
    mode: 0o100644,
    nlink: 2,
  });
  assert.deepEqual(requests, [
    { path: 'game/bin/data.bin', range: 'bytes=1-2' },
    { path: 'game/bin/data.bin', range: undefined },
  ]);
});

test('direct file ranges read only the requested spans across adjacent files', async () => {
  const a = folderFile('data/a.bin', [1, 2, 3]);
  const b = folderFile('data/b.bin', [4, 5]);
  const index = createDirectoryAssetIndex([a, b]);
  const source = new DirectoryAssetSource(index);
  const start = index.fileRanges[0].start + 2n;

  const result = await source.readRange(start, 2);

  assert.deepEqual([...result], [3, 4]);
  assert.deepEqual(a.slices, [[2, 3]]);
  assert.deepEqual(b.slices, [[0, 1]]);
});

test('direct range cache reuses a small file across different guest subranges', async () => {
  const file = folderFile('data/small.bin', [10, 20, 30, 40]);
  const index = createDirectoryAssetIndex([file]);
  const source = new DirectoryAssetSource(index);
  const cache = new AssetRangeCache(null, index.packageLength, {
    maxBytes: ASSET_CHUNK_SIZE,
    fileRanges: index.fileRanges,
    readAheadChunks: 0,
    readRange: (start, length) => source.readRange(start, length),
  });
  const offset = index.fileRanges[0].start + 1n;

  await cache.ensureRange(offset, 1);
  const output = new Uint8Array(1);
  assert.equal(cache.copyRange(offset, 1, output), true);
  assert.deepEqual([...output], [20]);
  await cache.ensureRange(offset + 2n, 1);
  assert.equal(cache.copyRange(offset + 2n, 1, output), true);
  assert.deepEqual([...output], [40]);
  assert.deepEqual(file.slices, [[0, 4]]);
});

test('direct range cache does not expand reads for files above the small-file limit', async () => {
  const file = folderFile('data/large.bin', Buffer.alloc(16 * 1024 + 1, 7));
  const index = createDirectoryAssetIndex([file]);
  const source = new DirectoryAssetSource(index);
  const cache = new AssetRangeCache(null, index.packageLength, {
    maxBytes: ASSET_CHUNK_SIZE,
    fileRanges: index.fileRanges,
    readAheadChunks: 0,
    readRange: (start, length) => source.readRange(start, length),
  });
  const offset = index.fileRanges[0].start + 5n;

  await cache.ensureRange(offset, 1);

  const output = new Uint8Array(1);
  assert.equal(cache.copyRange(offset, 1, output), true);
  assert.deepEqual([...output], [7]);
  assert.deepEqual(file.slices, [[5, 6]]);
});

test('direct source rejects short reads with the selected path', async () => {
  const file = folderFile('data/short.bin', [7, 8, 9], { shortRead: true });
  const index = createDirectoryAssetIndex([file]);
  const source = new DirectoryAssetSource(index);

  await assert.rejects(
    source.readRange(index.fileRanges[0].start, 3),
    /short read.*data\\short\.bin/i,
  );
});
