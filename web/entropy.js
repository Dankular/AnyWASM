const CRYPTO_CHUNK_BYTES = 65_536;

export function fillCryptoRandom(memory, pointer, length, provider) {
  const start = pointer >>> 0;
  const size = length >>> 0;
  if (size === 0) return 1;

  try {
    const cryptoProvider = provider === undefined ? globalThis.crypto : provider;
    if (typeof cryptoProvider?.getRandomValues !== 'function' || !memory?.buffer) return 0;

    const buffer = memory.buffer;
    if (start > buffer.byteLength || size > buffer.byteLength - start) return 0;
    for (let offset = 0; offset < size; offset += CRYPTO_CHUNK_BYTES) {
      const count = Math.min(CRYPTO_CHUNK_BYTES, size - offset);
      cryptoProvider.getRandomValues(new Uint8Array(buffer, start + offset, count));
    }
    return 1;
  } catch {
    return 0;
  }
}
