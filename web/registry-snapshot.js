const encoder = new TextEncoder();
const signature = new Uint8Array([0x41, 0x57, 0x52, 0x45, 0x47, 0x31, 0, 0]); // AWREG1\0\0
const maxRegistryBytes = 16 * 1024 * 1024;

function requireU32(value, label) {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new Error(`registry snapshot ${label} is outside the DWORD range`);
  }
  return value;
}

function encodeText(value, label) {
  if (typeof value !== 'string' || value.includes('\0')) {
    throw new Error(`registry snapshot ${label} must be a NUL-free string`);
  }
  const bytes = encoder.encode(value);
  if (bytes.length > 1024 * 1024) throw new Error(`registry snapshot ${label} exceeds the text limit`);
  return bytes;
}

function decodeBase64(value, label) {
  if (typeof value !== 'string' || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    throw new Error(`registry snapshot ${label} is not valid base64`);
  }
  const decoded = atob(value);
  const bytes = Uint8Array.from(decoded, (character) => character.charCodeAt(0));
  if (bytes.length > maxRegistryBytes) throw new Error(`registry snapshot ${label} exceeds the data limit`);
  return bytes;
}

export function encodeHostRegistrySnapshot(snapshot) {
  if (snapshot?.available !== true || !Array.isArray(snapshot.keys) || snapshot.keys.length === 0 || snapshot.keys.length > 4096) {
    throw new Error('host Sims registry snapshot is unavailable or has an invalid key list');
  }
  const chunks = [signature];
  const header = new Uint8Array(4);
  new DataView(header.buffer).setUint32(0, snapshot.keys.length, true);
  chunks.push(header);
  let totalBytes = 12;
  const push = (bytes) => {
    totalBytes += bytes.length;
    if (totalBytes > 32 * 1024 * 1024) throw new Error('host Sims registry snapshot exceeds 32 MiB');
    chunks.push(bytes);
  };
  const pushU32 = (value, label) => {
    const bytes = new Uint8Array(4);
    new DataView(bytes.buffer).setUint32(0, requireU32(value, label), true);
    push(bytes);
  };
  const pushU64 = (value, label) => {
    let number;
    try { number = BigInt(value); } catch { throw new Error(`registry snapshot ${label} is not a 64-bit integer`); }
    if (number < 0n || number > 0xffff_ffff_ffff_ffffn) throw new Error(`registry snapshot ${label} is outside the ULONGLONG range`);
    const bytes = new Uint8Array(8);
    new DataView(bytes.buffer).setBigUint64(0, number, true);
    push(bytes);
  };
  const pushText = (value, label) => {
    const bytes = encodeText(value, label);
    pushU32(bytes.length, `${label} length`);
    push(bytes);
  };

  for (const [index, key] of snapshot.keys.entries()) {
    if (!Array.isArray(key.subkeys) || !Array.isArray(key.values) || key.subkeys.length > 65_536 || key.values.length > 65_536) {
      throw new Error(`registry snapshot key ${index} has invalid child arrays`);
    }
    pushText(key.path, `key ${index} path`);
    pushText(key.className, `key ${index} class`);
    pushU32(key.securityDescriptorSize, `key ${index} security descriptor size`);
    pushU64(key.lastWriteFiletime, `key ${index} last-write time`);
    pushU32(key.subkeys.length, `key ${index} subkey count`);
    for (const [childIndex, child] of key.subkeys.entries()) pushText(child, `key ${index} subkey ${childIndex}`);
    pushU32(key.values.length, `key ${index} value count`);
    for (const [valueIndex, value] of key.values.entries()) {
      pushText(value.name, `key ${index} value ${valueIndex} name`);
      pushU32(value.type, `key ${index} value ${valueIndex} type`);
      const data = decodeBase64(value.data, `key ${index} value ${valueIndex} data`);
      pushU32(data.length, `key ${index} value ${valueIndex} data length`);
      push(data);
    }
  }

  const bytes = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}
