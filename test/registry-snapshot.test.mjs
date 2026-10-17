import assert from 'node:assert/strict';
import test from 'node:test';
import { encodeHostRegistrySnapshot } from '../web/registry-snapshot.js';

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
  assert.deepEqual([...bytes.subarray(0, 12)], [0x41, 0x57, 0x52, 0x45, 0x47, 0x31, 0, 0, 1, 0, 0, 0]);
  const r = reader(bytes.subarray(12));
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
  assert.equal(r.offset + 12, bytes.length);
});

test('rejects missing snapshots and malformed raw data instead of encoding empty success', () => {
  assert.throws(() => encodeHostRegistrySnapshot({ available: false, reason: 'host key missing' }), /unavailable/);
  assert.throws(() => encodeHostRegistrySnapshot({
    available: true,
    keys: [{ path: 'key', className: '', securityDescriptorSize: 0, lastWriteFiletime: '0', subkeys: [], values: [{ name: 'x', type: 1, data: '***' }] }],
  }), /base64/);
});
