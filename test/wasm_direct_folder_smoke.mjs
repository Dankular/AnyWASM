// Exercises staged folder loading and an Asyncify-backed read from a selected local file.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { AnyWasm, Status } from '../web/anywasm.js';
import { createDirectoryAssetIndex } from '../web/direct-assets.js';

const runtimeBytes = readFileSync(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm', import.meta.url));
const exeBytes = readFileSync(new URL('../fixtures/hello.exe', import.meta.url));
// Keep this above the small-file cache threshold so the smoke checks an exact range read.
const streamedBytes = new Uint8Array(32 * 1024);
streamedBytes.set(new TextEncoder().encode('the'), 5);

function selectedFile(path, bytes) {
  const data = bytes.slice();
  const slices = [];
  return {
    name: path.split('/').at(-1),
    size: data.byteLength,
    webkitRelativePath: `Selected/${path}`,
    slices,
    arrayBuffer: async () => data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength),
    slice(start, end) {
      slices.push([start, end]);
      return new Blob([data.subarray(start, end)]);
    },
  };
}

function allocate(wasm, bytes) {
  const ptr = wasm.x.aw_alloc(bytes.byteLength) >>> 0;
  new Uint8Array(wasm.x.memory.buffer, ptr, bytes.byteLength).set(bytes);
  return ptr;
}

const exe = selectedFile('hello.exe', exeBytes);
const dataFile = selectedFile('data/stream.bin', streamedBytes);
const files = [exe, dataFile];
const index = createDirectoryAssetIndex(files, ['hello.exe']);
const entryBytes = new TextEncoder().encode('hello.exe');

// Verify the C ABI staging sequence and that aw_run cannot run an unfinished process.
const staged = await AnyWasm.create(runtimeBytes);
assert.equal(typeof staged.x.aw_load_begin, 'function', 'runtime must export the staged loader');
assert.equal(typeof staged.x.aw_load_add_file, 'function', 'runtime must accept individual boot files');
assert.equal(typeof staged.x.aw_load_finish, 'function', 'runtime must finish staged loads');
const entryPtr = allocate(staged, entryBytes);
const indexPtr = allocate(staged, index.bytes);
const exePtr = allocate(staged, exeBytes);
try {
  assert.equal(staged.x.aw_load_begin(entryPtr, entryBytes.length, indexPtr, index.bytes.length), 0, staged.message());
  assert.equal(staged.x.aw_run(1n), 3, 'unfinished staged process must not execute');
  assert.match(staged.message(), /not been finished/i);

  assert.equal(staged.x.aw_load_add_file(entryPtr, entryBytes.length, exePtr, exeBytes.length), 0, staged.message());
  assert.equal(staged.x.aw_load_add_file(entryPtr, entryBytes.length, exePtr, exeBytes.length), 1, 'duplicate boot paths must be rejected');
  assert.match(staged.message(), /duplicate.*hello\.exe/i);
  assert.equal(staged.x.aw_load_finish(), 0, staged.message());
  let stdout = '';
  const run = await staged.runToEnd({ onOutput: (stream, text) => { if (stream === 'stdout') stdout += text; } });
  assert.equal(run.status, Status.EXITED);
  assert.equal(run.exitCode, 0);
  assert.match(stdout, /Hello from a Windows PE running inside AnyWASM!/);
} finally {
  staged.x.aw_free(entryPtr, entryBytes.length);
  staged.x.aw_free(indexPtr, index.bytes.length);
  staged.x.aw_free(exePtr, exeBytes.length);
}

const invalid = await AnyWasm.create(runtimeBytes);
const badIndex = index.bytes.slice();
badIndex[0] = 0;
const badEntryPtr = allocate(invalid, entryBytes);
const badIndexPtr = allocate(invalid, badIndex);
try {
  assert.equal(invalid.x.aw_load_begin(badEntryPtr, entryBytes.length, badIndexPtr, badIndex.length), 1);
  assert.match(invalid.message(), /corrupt asset package index: unsupported magic/i);
} finally {
  invalid.x.aw_free(badEntryPtr, entryBytes.length);
  invalid.x.aw_free(badIndexPtr, badIndex.length);
}
const missingEntryBytes = new TextEncoder().encode('missing.exe');
const missingEntryPtr = allocate(invalid, missingEntryBytes);
const validIndexPtr = allocate(invalid, index.bytes);
try {
  assert.equal(invalid.x.aw_load_begin(missingEntryPtr, missingEntryBytes.length, validIndexPtr, index.bytes.length), 0);
  assert.equal(invalid.x.aw_load_finish(), 1, 'finish must reject an entry point that was not staged');
  assert.match(invalid.message(), /missing\.exe: not in VFS/i);
} finally {
  invalid.x.aw_free(missingEntryPtr, missingEntryBytes.length);
  invalid.x.aw_free(validIndexPtr, index.bytes.length);
}

// Verify the high-level browser path streams an exact partial File.slice through Asyncify.
const wasm = await AnyWasm.create(runtimeBytes);
let afterLoadBeginCalled = false;
await wasm.loadDirectory('hello.exe', files, undefined, {
  afterLoadBegin: () => {
    afterLoadBeginCalled = true;
    wasm.setHostChangeNotificationsEnabled(true);
  },
});
assert.equal(afterLoadBeginCalled, true, 'loadDirectory must expose the initialized process before staging boot files');
assert.equal(typeof wasm.x.aw_test_asset_read, 'function', 'runtime must include the streaming smoke-test ABI');
const assetPath = new TextEncoder().encode('data/stream.bin');
const assetPathPtr = allocate(wasm, assetPath);
const outputPtr = wasm.x.aw_alloc(3) >>> 0;
try {
  let result = wasm.x.aw_test_asset_read(assetPathPtr, assetPath.length, 5n, 3, outputPtr);
  assert.equal(wasm.x.asyncify_get_state(), 1, 'first direct range read must suspend on a cache miss');
  wasm.x.asyncify_stop_unwind();
  await wasm.waitForHost();
  wasm.x.asyncify_start_rewind(wasm.asyncifyData);
  result = wasm.x.aw_test_asset_read(assetPathPtr, assetPath.length, 5n, 3, outputPtr);
  assert.equal(wasm.x.asyncify_get_state(), 0, 'cached direct range read must finish after rewind');
  assert.equal(result, 0);
  assert.deepEqual([...new Uint8Array(wasm.x.memory.buffer, outputPtr, 3)], [116, 104, 101]);
  assert.deepEqual(dataFile.slices, [[5, 8]], 'only the guest-requested bytes should be read from the selected file');

  result = wasm.x.aw_test_asset_read(assetPathPtr, assetPath.length, 5n, 3, outputPtr);
  assert.equal(result, 0);
  assert.deepEqual(dataFile.slices, [[5, 8]], 'a cache hit must not read the File again');
} finally {
  wasm.x.aw_free(assetPathPtr, assetPath.length);
  wasm.x.aw_free(outputPtr, 3);
}

const guestRun = await wasm.runToEnd();
assert.equal(guestRun.status, Status.EXITED);
assert.equal(guestRun.exitCode, 0);

// The direct launcher defaults the working directory to the selected executable's parent.
const cwdExeBytes = readFileSync(new URL('../fixtures/cwd_bundle_test.exe', import.meta.url));
const cwdExe = selectedFile('game/assets/cwd_bundle_test.exe', cwdExeBytes);
const cwdConfig = selectedFile('game/assets/config.txt', new TextEncoder().encode('working directory fixture'));
const cwdWasm = await AnyWasm.create(runtimeBytes);
await cwdWasm.loadDirectory('game/assets/cwd_bundle_test.exe', [cwdExe, cwdConfig]);
const cwdRun = await cwdWasm.runToEnd();
assert.equal(cwdRun.status, Status.EXITED);
assert.equal(cwdRun.exitCode, 0, 'relative VFS access should use the selected executable directory');

console.log('Direct folder WASM smoke passed: staged launch, partial local range read, Asyncify resume, and cache hit.');
