export const ASSET_CHUNK_SIZE = 256 * 1024;
export const ASSET_CACHE_LIMIT = 64 * 1024 * 1024;
const ASSET_HEADER_SIZE = 24;
const MAX_ASSET_INDEX_SIZE = 64 * 1024 * 1024;
const ASSET_READ_AHEAD_BYTES = ASSET_CHUNK_SIZE / 2;

export async function sha256Hex(bytes) {
  if (!globalThis.crypto?.subtle) throw new Error('Web Crypto SHA-256 is unavailable; use HTTPS or localhost');
  const input = bytes instanceof ArrayBuffer
    ? bytes
    : bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', input));
  return Array.from(digest, (value) => value.toString(16).padStart(2, '0')).join('');
}

function parseAssetFileRanges(bytes, packageLength) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entryCount = view.getUint32(4, true);
  const indexEnd = BigInt(bytes.byteLength);
  if (entryCount > Math.floor((bytes.byteLength - ASSET_HEADER_SIZE) / 18)) {
    throw new Error('asset index entry count cannot fit in the index');
  }

  const ranges = [];
  let cursor = ASSET_HEADER_SIZE;
  for (let i = 0; i < entryCount; i++) {
    const pathLength = view.getUint16(cursor, true);
    cursor += 2 + pathLength;
    if (cursor + 16 > bytes.byteLength) throw new Error('asset index record is truncated');
    const start = view.getBigUint64(cursor, true);
    const length = view.getBigUint64(cursor + 8, true);
    const end = start + length;
    if (start < indexEnd || end > packageLength) throw new Error('asset index range is outside the package');
    if (length > 0n) ranges.push({ start, end });
    cursor += 16;
  }
  if (cursor !== bytes.byteLength) throw new Error('asset index entry count does not match its length');
  ranges.sort((a, b) => a.start < b.start ? -1 : a.start > b.start ? 1 : 0);
  return ranges;
}

async function fetchRange(url, start, end, expectedTotal, fetcher) {
  let response;
  try {
    response = await fetcher(url, { headers: { Range: `bytes=${start}-${end}` } });
  } catch (error) {
    throw error;
  }
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
  return { bytes, packageLength, fileRanges: parseAssetFileRanges(bytes, packageLength) };
}

export class AssetRangeCache {
  constructor(url, packageLength, {
    maxBytes = ASSET_CACHE_LIMIT,
    fetcher = fetch,
    readRange = null,
    onProgress = () => {},
    fileRanges = [],
    maxConcurrentFetches = 4,
    readAheadChunks = 3,
  } = {}) {
    if (!Number.isSafeInteger(maxBytes) || maxBytes < ASSET_CHUNK_SIZE) {
      throw new RangeError(`asset cache limit must be at least ${ASSET_CHUNK_SIZE} bytes`);
    }
    this.url = url;
    this.packageLength = BigInt(packageLength);
    this.maxBytes = maxBytes;
    if (!Number.isSafeInteger(maxConcurrentFetches) || maxConcurrentFetches < 1 || maxConcurrentFetches > 32) {
      throw new RangeError('asset fetch concurrency must be between 1 and 32');
    }
    if (!Number.isSafeInteger(readAheadChunks) || readAheadChunks < 0 || readAheadChunks > 16) {
      throw new RangeError('asset read-ahead must be between 0 and 16 chunks');
    }
    this.fetcher = fetcher;
    if (readRange !== null && typeof readRange !== 'function') {
      throw new TypeError('asset range source must be a function');
    }
    this.readRange = readRange;
    this.onProgress = onProgress;
    this.fileRanges = fileRanges;
    this.cache = new Map();
    this.directCache = new Map();
    this.directCacheBytes = 0;
    this.directInflight = new Map();
    this.directQueue = [];
    this.activeDirectReads = 0;
    this.nextDirectOrder = 0;
    this.inflight = new Map();
    this.queue = [];
    this.nextQueueOrder = 0;
    this.activeFetches = 0;
    this.maxConcurrentFetches = maxConcurrentFetches;
    this.readAheadChunks = readAheadChunks;
    this.cacheBytes = 0;
    this.fetchedBytes = 0;
    this.lastRead = null;
  }

  async ensureRange(offset, length) {
    const start = BigInt(offset);
    if (!Number.isSafeInteger(length) || length < 0) throw new RangeError('asset read length must be a nonnegative safe integer');
    const end = start + BigInt(length);
    if (start < 0n || end > this.packageLength) throw new RangeError('asset read is outside the package');
    if (length === 0) return;
    if (this.readRange) {
      if (length > this.maxBytes) throw new RangeError('guest asset read exceeds the cache capacity');
      await this.#ensureDirectRange(start, length);
      this.lastRead = { end, fileRange: this.#findFileRange(start) };
      return;
    }
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

    const fileRange = this.#findFileRange(start);
    if (this.lastRead && this.lastRead.end === start &&
        this.lastRead.fileRange?.start === fileRange?.start &&
        this.lastRead.fileRange?.end === fileRange?.end) {
      this.#prefetchNext(start, end, fileRange);
    }
    this.lastRead = { end, fileRange };
  }

  copyRange(offset, length, destination) {
    const start = BigInt(offset);
    if (!(destination instanceof Uint8Array) || destination.byteLength < length) {
      throw new TypeError('asset destination must be a sufficiently large Uint8Array');
    }
    if (length === 0) return true;
    const end = start + BigInt(length);
    if (this.readRange) {
      const bytes = this.#touchDirectRange(start, length);
      if (!bytes) return false;
      destination.set(bytes);
      return true;
    }
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

  #touchDirectRange(start, length) {
    const key = `${start}:${length}`;
    const bytes = this.directCache.get(key);
    if (bytes) {
      this.directCache.delete(key);
      this.directCache.set(key, bytes);
    }
    return bytes;
  }

  #ensureDirectRange(start, length) {
    const key = `${start}:${length}`;
    const cached = this.#touchDirectRange(start, length);
    if (cached) return Promise.resolve(cached);
    const existing = this.directInflight.get(key);
    if (existing) return existing;

    let resolveRequest;
    let rejectRequest;
    const promise = new Promise((resolve, reject) => {
      resolveRequest = resolve;
      rejectRequest = reject;
    });
    this.directInflight.set(key, promise);
    this.directQueue.push({ key, start, length, order: this.nextDirectOrder++, resolve: resolveRequest, reject: rejectRequest });
    this.#pumpDirectQueue();
    return promise;
  }

  #pumpDirectQueue() {
    while (this.activeDirectReads < this.maxConcurrentFetches && this.directQueue.length) {
      const item = this.directQueue.shift();
      this.activeDirectReads++;
      Promise.resolve()
        .then(() => this.readRange(item.start, item.length))
        .then((bytes) => {
          if (!(bytes instanceof Uint8Array) || bytes.byteLength !== item.length) {
            const received = bytes?.byteLength ?? 'non-byte result';
            throw new Error(`asset source returned ${received} bytes for a ${item.length}-byte range`);
          }
          if (bytes.byteLength > this.maxBytes) throw new Error('asset cache limit is smaller than one direct range');
          if (!this.directCache.has(item.key)) {
            this.directCache.set(item.key, bytes);
            this.directCacheBytes += bytes.byteLength;
            this.fetchedBytes += bytes.byteLength;
            this.onProgress({ fetchedBytes: this.fetchedBytes, cacheBytes: this.directCacheBytes, packageLength: this.packageLength });
            while (this.directCacheBytes > this.maxBytes || this.directCache.size > 4096) {
              const oldest = this.directCache.keys().next().value;
              const evicted = this.directCache.get(oldest);
              this.directCache.delete(oldest);
              this.directCacheBytes -= evicted.byteLength;
            }
          }
          return this.directCache.get(item.key);
        })
        .then(item.resolve, item.reject)
        .finally(() => {
          this.directInflight.delete(item.key);
          this.activeDirectReads--;
          this.#pumpDirectQueue();
        })
        .catch(() => {});
    }
  }

  #findFileRange(offset) {
    let low = 0;
    let high = this.fileRanges.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (this.fileRanges[middle].start <= offset) low = middle + 1;
      else high = middle;
    }
    const range = this.fileRanges[low - 1];
    return range && offset < range.end ? range : null;
  }

  #prefetchNext(start, end, fileRange) {
    if (!fileRange || this.readAheadChunks === 0 || this.maxBytes < ASSET_CHUNK_SIZE * 2) return;
    const chunkSize = BigInt(ASSET_CHUNK_SIZE);
    const nextChunk = (end - 1n) / chunkSize + 1n;
    const nextStart = nextChunk * chunkSize;
    if (nextStart >= fileRange.end || nextStart - end > BigInt(ASSET_READ_AHEAD_BYTES)) return;
    const residentChunks = Math.floor(this.maxBytes / ASSET_CHUNK_SIZE);
    const first = start / chunkSize;
    const last = (end - 1n) / chunkSize;
    const inUseChunks = Number(last - first + 1n);
    const aheadLimit = Math.min(this.readAheadChunks, residentChunks - inUseChunks);
    if (aheadLimit <= 0) return;
    for (let ahead = 0; ahead < aheadLimit; ahead++) {
      const chunk = nextChunk + BigInt(ahead);
      if (chunk * chunkSize >= fileRange.end) break;
      void this.#loadChunk(chunk, 1).catch(() => {});
    }
  }

  #loadChunk(chunk, priority = 0) {
    if (this.cache.has(chunk)) return Promise.resolve(this.#touch(chunk));
    const existing = this.inflight.get(chunk);
    if (existing) {
      if (priority < existing.priority) {
        existing.priority = priority;
        this.#sortQueue();
        this.#pumpQueue();
      }
      return existing.promise;
    }
    const start = chunk * BigInt(ASSET_CHUNK_SIZE);
    const end = [start + BigInt(ASSET_CHUNK_SIZE), this.packageLength].reduce((a, b) => a < b ? a : b) - 1n;
    let resolveRequest;
    let rejectRequest;
    const promise = new Promise((resolve, reject) => {
      resolveRequest = resolve;
      rejectRequest = reject;
    });
    const item = { chunk, start, end, priority, order: this.nextQueueOrder++, resolve: resolveRequest, reject: rejectRequest };
    this.inflight.set(chunk, { promise, item, get priority() { return item.priority; }, set priority(value) { item.priority = value; } });
    this.queue.push(item);
    this.#sortQueue();
    this.#pumpQueue();
    return promise;
  }

  #sortQueue() {
    this.queue.sort((a, b) => a.priority - b.priority || a.order - b.order);
  }

  #pumpQueue() {
    this.#sortQueue();
    while (this.activeFetches < this.maxConcurrentFetches && this.queue.length) {
      const item = this.queue.shift();
      if (this.cache.has(item.chunk)) {
        item.resolve(this.#touch(item.chunk));
        this.inflight.delete(item.chunk);
        continue;
      }
      this.activeFetches++;
      fetchRange(this.url, item.start, item.end, this.packageLength, this.fetcher)
        .then(({ bytes }) => {
        if (bytes.byteLength > this.maxBytes) throw new Error('asset cache limit is smaller than one fetched chunk');
        if (this.cache.has(item.chunk)) return this.#touch(item.chunk);
        this.cache.set(item.chunk, bytes);
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
        .then(item.resolve, item.reject)
        .finally(() => {
          this.inflight.delete(item.chunk);
          this.activeFetches--;
          this.#pumpQueue();
        })
        .catch(() => {});
    }
  }
}
