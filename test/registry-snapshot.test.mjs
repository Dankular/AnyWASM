import assert from 'node:assert/strict';
import test from 'node:test';
import { encodeHostRegistrySnapshot, selectSharedHostRegistrySnapshot } from '../web/registry-snapshot.js';

function reader(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 0;
  return {
    text() {
      const length = view.getUint32(offset, true); offset += 4;
      const value = new TextDecoder().decode(bytes.subarray(offset, offset + length)); offset += length;
      return value;
    },
    u32() { const value = view.getUint32(offset, true); offset += 4; return value; },
    u64() { const value = view.getBigUint64(offset, true); offset += 8; return value; },
    data(length) { const value = bytes.slice(offset, offset + length); offset += length; return value; },
    get offset() { return offset; },
  };
}

test('encodes native Sims registry metadata and raw value bytes without narrowing FILETIME', () => {
  const valueData = Uint8Array.from([0x44, 0, 0x3a, 0, 0, 0]);
  const snapshot = {
    available: true,
    keys: [{
      path: 'hkey_local_machine\\software\\sims(steam)',
      className: 'Sims',
      securityDescriptorSize: 240,
      lastWriteFiletime: '134366741272083884',
      subkeys: ['The Sims 3'],
      values: [{ name: 'install dir', type: 1, data: btoa(String.fromCharCode(...valueData)) }],
    }],
  };
  const bytes = encodeHostRegistrySnapshot(snapshot);
  assert.deepEqual([...bytes.subarray(0, 16)], [0x41, 0x57, 0x52, 0x45, 0x47, 0x32, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
  const r = reader(bytes.subarray(16));
  assert.equal(r.text(), snapshot.keys[0].path);
  assert.equal(r.text(), 'Sims');
  assert.equal(r.u32(), 240);
  assert.equal(r.u64(), 134366741272083884n);
  assert.equal(r.u32(), 1);
  assert.equal(r.text(), 'The Sims 3');
  assert.equal(r.u32(), 1);
  assert.equal(r.text(), 'install dir');
  assert.equal(r.u32(), 1);
  const dataLength = r.u32();
  assert.equal(dataLength, valueData.length);
  assert.deepEqual(r.data(dataLength), valueData);
  assert.equal(r.offset + 16, bytes.length);
});

test('encodes only the explicitly confirmed 32-bit-view missing registry paths', () => {
  const snapshot = {
    available: true,
    keys: [{
      path: 'hkey_local_machine\\software\\sims(steam)',
      className: '',
      securityDescriptorSize: 0,
      lastWriteFiletime: '0',
      subkeys: [],
      values: [],
    }],
    missingKeys32: [
      'hkey_local_machine\\software\\apple computer, inc.\\quicktime',
      'hkey_current_user\\software\\apple computer, inc.\\quicktime',
    ],
  };
  const bytes = encodeHostRegistrySnapshot(snapshot);
  const r = reader(bytes.subarray(8));
  assert.equal(r.u32(), 1); // present key count
  assert.equal(r.u32(), 2); // exact confirmed missing keys in 32-bit view
  assert.equal(r.text(), snapshot.missingKeys32[0]);
  assert.equal(r.text(), snapshot.missingKeys32[1]);
  assert.equal(r.text(), snapshot.keys[0].path);
});

test('selects only the shared Shell Folders key and exact QuickTime tree for non-direct launches', () => {
  const shellFolders = 'hkey_current_user\\software\\microsoft\\windows\\currentversion\\explorer\\shell folders';
  const quickTime = 'hkey_current_user\\software\\apple computer, inc.\\quicktime';
  const snapshot = selectSharedHostRegistrySnapshot({
    available: true,
    installPath: 'D:\\The Sims 3',
    keys: [
      { path: 'hkey_local_machine\\software\\sims(steam)' },
      { path: shellFolders },
      { path: quickTime },
      { path: `${quickTime}\\preferences` },
      { path: 'hkey_current_user\\software\\apple computer, inc.\\other' },
    ],
    missingKeys32: [
      'hkey_local_machine\\software\\apple computer, inc.\\quicktime',
      quickTime,
      'hkey_local_machine\\software\\unrelated',
    ],
  });
  assert.equal(snapshot.available, true);
  assert.deepEqual(snapshot.keys.map(({ path }) => path), [shellFolders, quickTime, `${quickTime}\\preferences`]);
  assert.deepEqual(snapshot.missingKeys32, [
    'hkey_local_machine\\software\\apple computer, inc.\\quicktime',
    quickTime,
  ]);
});

test('rejects missing snapshots and malformed raw data instead of encoding empty success', () => {
  assert.throws(() => encodeHostRegistrySnapshot({ available: false, reason: 'host key missing' }), /unavailable/);
  assert.throws(() => encodeHostRegistrySnapshot({
    available: true,
    keys: [{ path: 'key', className: '', securityDescriptorSize: 0, lastWriteFiletime: '0', subkeys: [], values: [{ name: 'x', type: 1, data: '***' }] }],
  }), /base64/);
  assert.throws(() => encodeHostRegistrySnapshot({
    available: true,
    keys: [{ path: 'key', className: '', securityDescriptorSize: 0, lastWriteFiletime: '0', subkeys: [], values: [] }],
    missingKeys32: 'hkey_local_machine\\software\\example',
  }), /32-bit missing-key list/);
});
