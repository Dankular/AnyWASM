export const ASSET_CHUNK_SIZE = 256 * 1024;
export const ASSET_CACHE_LIMIT = 64 * 1024 * 1024;
const ASSET_HEADER_SIZE = 24;
const MAX_ASSET_INDEX_SIZE = 64 * 1024 * 1024;

async function fetchRange(url, start, end, expectedTotal, fetcher) {
  const response = await fetcher(url, { headers: { Range: `bytes=${start}-${end}` } });
  if (response.status !== 206) {
    throw new Error(`asset server ignored byte range ${start}-${end} (HTTP ${response.status}; expected 206)`);
  }
  const contentRange = response.headers.get('content-range');
  const parsed = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(contentRange || '');
  if (!parsed || BigInt(parsed[1]) !== start || BigInt(parsed[2]) !== end) {
    throw new Error(`asset server returned an invalid Content-Range for ${start}-${end}`);
  }
  const total = BigInt(parsed[3]);
  if (expectedTotal !== null && total !== expectedTotal) {
    throw new Error(`asset package length changed from ${expectedTotal} to ${total}`);
  }
  const expectedLength = Number(end - start + 1n);
  const contentLength = response.headers.get('content-length');
  if (contentLength !== null && Number(contentLength) !== expectedLength) {
    throw new Error(`asset server returned ${contentLength} bytes for a ${expectedLength}-byte range`);
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength !== expectedLength) {
    throw new Error(`asset server returned ${bytes.byteLength} bytes for a ${expectedLength}-byte range`);
  }
  return { bytes, total };
}

export async function fetchAssetIndex(url, fetcher = fetch) {
  const headerResult = await fetchRange(url, 0n, BigInt(ASSET_HEADER_SIZE - 1), null, fetcher);
  const header = headerResult.bytes;
  if (String.fromCharCode(...header.subarray(0, 4)) !== 'AGP1') {
    throw new Error('invalid AGP1 asset package header');
  }
  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  const indexLength = view.getBigUint64(8, true);
  const packageLength = view.getBigUint64(16, true);
  if (headerResult.total !== packageLength) {
    throw new Error(`asset package header length ${packageLength} disagrees with HTTP length ${headerResult.total}`);
  }
  if (indexLength > BigInt(MAX_ASSET_INDEX_SIZE)) {
    throw new Error(`asset index is too large (${indexLength} bytes)`);
  }
  const indexEnd = BigInt(ASSET_HEADER_SIZE) + indexLength;
  if (indexEnd > packageLength) throw new Error('asset index extends past the package length');
  const indexBytes = indexLength === 0n
    ? new Uint8Array(0)
    : (await fetchRange(url, BigInt(ASSET_HEADER_SIZE), indexEnd - 1n, packageLength, fetcher)).bytes;
  const bytes = new Uint8Array(Number(indexEnd));
  bytes.set(header, 0);
  bytes.set(indexBytes, ASSET_HEADER_SIZE);
  return { bytes, packageLength };
}

export class AssetRangeCache {
  constructor(url, packageLength, { maxBytes = ASSET_CACHE_LIMIT, fetcher = fetch, onProgress = () => {} } = {}) {
    if (!Number.isSafeInteger(maxBytes) || maxBytes < ASSET_CHUNK_SIZE) {
      throw new RangeError(`asset cache limit must be at least ${ASSET_CHUNK_SIZE} bytes`);
    }
    this.url = url;
    this.packageLength = BigInt(packageLength);
    this.maxBytes = maxBytes;
    this.fetcher = fetcher;
    this.onProgress = onProgress;
    this.cache = new Map();
    this.inflight = new Map();
    this.cacheBytes = 0;
    this.fetchedBytes = 0;
  }

  async ensureRange(offset, length) {
    const start = BigInt(offset);
    if (!Number.isSafeInteger(length) || length < 0) throw new RangeError('asset read length must be a nonnegative safe integer');
    const end = start + BigInt(length);
    if (start < 0n || end > this.packageLength) throw new RangeError('asset read is outside the package');
    if (length === 0) return;
    const first = start / BigInt(ASSET_CHUNK_SIZE);
    const last = (end - 1n) / BigInt(ASSET_CHUNK_SIZE);
    if (last - first + 1n > BigInt(Math.floor(this.maxBytes / ASSET_CHUNK_SIZE))) {
      throw new RangeError('guest asset read exceeds the cache capacity');
    }
    const waits = [];
    for (let chunk = first; chunk <= last; chunk++) {
      if (this.cache.has(chunk)) this.#touch(chunk);
      else waits.push(this.#loadChunk(chunk));
    }
    await Promise.all(waits);
  }

  copyRange(offset, length, destination) {
    const start = BigInt(offset);
    if (!(destination instanceof Uint8Array) || destination.byteLength < length) {
      throw new TypeError('asset destination must be a sufficiently large Uint8Array');
    }
    if (length === 0) return true;
    const end = start + BigInt(length);
    const first = start / BigInt(ASSET_CHUNK_SIZE);
    const last = (end - 1n) / BigInt(ASSET_CHUNK_SIZE);
    for (let chunk = first; chunk <= last; chunk++) {
      if (!this.cache.has(chunk)) return false;
    }
    let cursor = start;
    let written = 0;
    while (cursor < end) {
      const chunk = cursor / BigInt(ASSET_CHUNK_SIZE);
      const bytes = this.#touch(chunk);
      const within = Number(cursor % BigInt(ASSET_CHUNK_SIZE));
      const count = Math.min(bytes.length - within, Number(end - cursor));
      destination.set(bytes.subarray(within, within + count), written);
      cursor += BigInt(count);
      written += count;
    }
    return true;
  }

  #touch(chunk) {
    const bytes = this.cache.get(chunk);
    if (bytes) {
      this.cache.delete(chunk);
      this.cache.set(chunk, bytes);
    }
    return bytes;
  }

  #loadChunk(chunk) {
    if (this.inflight.has(chunk)) return this.inflight.get(chunk);
    const start = chunk * BigInt(ASSET_CHUNK_SIZE);
    const end = [start + BigInt(ASSET_CHUNK_SIZE), this.packageLength].reduce((a, b) => a < b ? a : b) - 1n;
    const promise = fetchRange(this.url, start, end, this.packageLength, this.fetcher)
      .then(({ bytes }) => {
        if (bytes.byteLength > this.maxBytes) throw new Error('asset cache limit is smaller than one fetched chunk');
        this.cache.set(chunk, bytes);
        this.cacheBytes += bytes.byteLength;
        this.fetchedBytes += bytes.byteLength;
        this.onProgress({ fetchedBytes: this.fetchedBytes, cacheBytes: this.cacheBytes, packageLength: this.packageLength });
        while (this.cacheBytes > this.maxBytes) {
          const oldest = this.cache.keys().next().value;
          const evicted = this.cache.get(oldest);
          this.cache.delete(oldest);
          this.cacheBytes -= evicted.byteLength;
        }
        return bytes;
      })
      .finally(() => this.inflight.delete(chunk));
    this.inflight.set(chunk, promise);
    return promise;
  }
}
