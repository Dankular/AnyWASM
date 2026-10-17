const ASSET_HEADER_SIZE = 24;
const MAX_ASSET_INDEX_SIZE = 64 * 1024 * 1024;
const MAX_U64 = 0xffff_ffff_ffff_ffffn;

function asciiLower(value) {
  return value.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

function normalizeRelativePath(value, { stripRoot = false } = {}) {
  if (typeof value !== 'string' || !value) throw new Error('unsafe empty file path');
  let path = value.replaceAll('/', '\\');
  if (path.startsWith('\\') || path.includes(':') || path.includes('\0')) {
    throw new Error(`unsafe absolute file path: ${value}`);
  }
  let components = path.split('\\');
  if (stripRoot && components.length > 1) components = components.slice(1);
  if (!components.length) throw new Error(`unsafe empty file path: ${value}`);
  const normalized = [];
  for (const component of components) {
    const trimmed = component.trimEnd();
    if (!trimmed || trimmed === '.' || trimmed === '..') {
      throw new Error(`unsafe path component in ${value}`);
    }
    const win32Component = trimmed.replace(/\.+$/, '');
    if (!win32Component || win32Component === '.' || win32Component === '..') {
      throw new Error(`unsafe path component in ${value}`);
    }
    normalized.push(asciiLower(win32Component));
  }
  return normalized.join('\\');
}

function sourcePaths(files) {
  const entries = Array.from(files ?? [], (file) => {
    const webkitPath = typeof file?.webkitRelativePath === 'string' && file.webkitRelativePath;
    const relativePath = webkitPath || file?.relativePath || file?.name;
    return { file, relativePath, webkitPath: !!webkitPath };
  });
  if (!entries.length) throw new Error('selected folder contains no files');

  const webkitEntries = entries.filter((entry) => entry.webkitPath);
  const firstComponents = webkitEntries.map(({ relativePath }) => relativePath.replaceAll('\\', '/').split('/')[0]);
  const root = webkitEntries.length === entries.length &&
    webkitEntries.every(({ relativePath }) => relativePath.replaceAll('\\', '/').includes('/')) &&
    firstComponents.every((component) => component.toLowerCase() === firstComponents[0].toLowerCase())
    ? firstComponents[0]
    : null;

  return entries.map(({ file, relativePath, webkitPath }) => {
    const isDirectory = file?.isDirectory === true;
    if (!file || typeof file.arrayBuffer !== 'function' ||
        !Number.isSafeInteger(file.size) || file.size < 0 ||
        (!isDirectory && typeof file.slice !== 'function') || (isDirectory && file.size !== 0)) {
      throw new Error(`selected file has invalid browser file data: ${relativePath ?? '(unknown path)'}`);
    }
    const path = normalizeRelativePath(relativePath, { stripRoot: !!root && webkitPath });
    return { path, file, size: BigInt(file.size), isDirectory };
  });
}

function inspectInventory(files) {
  const inventory = sourcePaths(files);
  const filesByPath = new Map();
  for (const { path, file } of inventory) {
    const key = path.toLowerCase();
    if (filesByPath.has(key)) throw new Error(`duplicate normalized file path: ${path}`);
    filesByPath.set(key, file);
  }
  const executables = inventory
    .map(({ path }) => path)
    .filter((path) => !filesByPath.get(path)?.isDirectory && path.toLowerCase().endsWith('.exe'))
    .sort();
  return { inventory, filesByPath, executables };
}

/** Validate a selected directory and list its normalized executable paths. */
export function inspectDirectory(files) {
  const { filesByPath, executables } = inspectInventory(files);
  return { filesByPath, executables };
}

/** Normalize an entry-point path using the same safety rules as selected files. */
export function normalizeDirectoryPath(path) {
  return normalizeRelativePath(path);
}

/** Adapt local-server file metadata to the same file interface used by folder selection. */
export function createRemoteDirectoryFiles(entries, fileUrl, fetcher = fetch) {
  if (!Array.isArray(entries) || typeof fileUrl !== 'string' || !fileUrl || typeof fetcher !== 'function') {
    throw new TypeError('remote directory source is invalid');
  }
  const endpoint = new URL(fileUrl, globalThis.location?.href || 'http://localhost/');
  return entries.map((entry) => {
    const path = normalizeRelativePath(entry?.path);
    const size = Number(entry?.size);
    const isDirectory = entry?.kind === 'directory';
    if (isDirectory && size !== 0) throw new Error(`invalid remote directory size: ${path}`);
    if (!Number.isSafeInteger(size) || size < 0) throw new Error(`invalid remote file size: ${path}`);
    const url = new URL(endpoint.href);
    url.searchParams.set('path', path.replaceAll('\\', '/'));

    const read = async (start, end, ranged) => {
      const expected = end - start;
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start || end > size) {
        throw new RangeError(`invalid remote file range for ${path}`);
      }
      if (expected === 0) return new ArrayBuffer(0);
      const response = await fetcher(url.href, ranged ? { headers: { Range: `bytes=${start}-${end - 1}` } } : {});
      if (!response.ok || (ranged ? response.status !== 206 : response.status !== 200)) {
        throw new Error(`failed to read ${path} (HTTP ${response.status})`);
      }
      if (ranged) {
        const contentRange = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get('content-range') || '');
        if (!contentRange || Number(contentRange[1]) !== start || Number(contentRange[2]) !== end - 1 || Number(contentRange[3]) !== size) {
          throw new Error(`server returned an invalid Content-Range for ${path}`);
        }
      }
      const bytes = await response.arrayBuffer();
      if (bytes.byteLength !== expected) throw new Error(`short read for ${path}: received ${bytes.byteLength} of ${expected} bytes`);
      return bytes;
    };

    return {
      name: path.split('\\').at(-1),
      size,
      relativePath: path,
      isDirectory,
      lastAccessTime: entry.lastAccessTime,
      lastWriteTime: entry.lastWriteTime,
      creationTime: entry.creationTime,
      mode: entry.mode,
      nlink: entry.nlink,
      lastModified: entry.lastModified,
      arrayBuffer: () => read(0, size, false),
      slice(start = 0, end = size) {
        return { arrayBuffer: () => read(start, end, true) };
      },
    };
  });
}

/** Build an AGP1-compatible virtual address map without creating a payload file. */
export function createDirectoryAssetIndex(files, bootPaths = []) {
  const { inventory, filesByPath, executables } = inspectInventory(files);

  // Keep metadata for boot files too. Their bytes are staged once at load time, then the VFS
  // overlay serves them; metadata reads do not fetch their asset ranges again.
  void bootPaths;
  const assetEntries = inventory.slice().sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  let indexLength = 0;
  const encodedPaths = assetEntries.map(({ path }) => {
    const encoded = new TextEncoder().encode(path);
    if (encoded.byteLength > 0xffff) throw new Error(`file path is too long for asset index: ${path}`);
    indexLength += 2 + encoded.byteLength + 16 + 31;
    return encoded;
  });
  if (indexLength > MAX_ASSET_INDEX_SIZE) throw new Error(`directory asset index is too large (${indexLength} bytes)`);
  if (assetEntries.length > 0xffff_ffff) throw new Error('directory contains too many files for the asset index');

  const indexEnd = ASSET_HEADER_SIZE + indexLength;
  let packageLength = BigInt(indexEnd);
  const records = [];
  for (let i = 0; i < assetEntries.length; i++) {
    const entry = assetEntries[i];
    const start = packageLength;
    if (!entry.isDirectory) packageLength += entry.size;
    if (packageLength > MAX_U64) throw new Error(`directory is too large to index: ${entry.path}`);
    records.push({ path: entry.path, file: entry.file, isDirectory: entry.isDirectory, start, end: packageLength, length: entry.size, encodedPath: encodedPaths[i] });
  }

  const bytes = new Uint8Array(indexEnd);
  bytes.set(new TextEncoder().encode('AGP2'), 0);
  const view = new DataView(bytes.buffer);
  view.setUint32(4, assetEntries.length, true);
  view.setBigUint64(8, BigInt(indexLength), true);
  view.setBigUint64(16, packageLength, true);
  let cursor = ASSET_HEADER_SIZE;
  for (const record of records) {
    view.setUint16(cursor, record.encodedPath.byteLength, true);
    cursor += 2;
    bytes.set(record.encodedPath, cursor);
    cursor += record.encodedPath.byteLength;
    view.setBigUint64(cursor, record.start, true);
    view.setBigUint64(cursor + 8, record.length, true);
    cursor += 16;
    view.setBigInt64(cursor, metadataTime(record.file.lastAccessTime), true);
    view.setBigInt64(cursor + 8, metadataTime(record.file.lastWriteTime ??
      (Number.isFinite(record.file.lastModified) ? Math.floor(record.file.lastModified / 1000) : undefined)), true);
    view.setBigInt64(cursor + 16, metadataTime(record.file.creationTime), true);
    view.setUint32(cursor + 24, validUint32(record.file.mode) ? record.file.mode : 0xffff_ffff, true);
    view.setUint16(cursor + 28, Number.isInteger(record.file.nlink) && record.file.nlink >= 0 && record.file.nlink < 0xffff ? record.file.nlink : 0xffff, true);
    view.setUint8(cursor + 30, record.isDirectory ? 1 : 0);
    cursor += 31;
  }

  return {
    bytes,
    packageLength,
    fileRanges: records
      .filter(({ length, isDirectory }) => !isDirectory && length > 0n)
      .map(({ path, file, start, end }) => ({ path, file, start, end })),
    filesByPath,
    executables,
  };
}

const UNKNOWN_TIME = -(1n << 63n);

function metadataTime(value) {
  return Number.isSafeInteger(value) ? BigInt(value) : UNKNOWN_TIME;
}

function validUint32(value) {
  return Number.isInteger(value) && value >= 0 && value < 0xffff_ffff;
}

/** Reads only the requested spans from the selected files. */
export class DirectoryAssetSource {
  constructor(index) {
    if (!index?.bytes || !(index.bytes instanceof Uint8Array) || !Array.isArray(index.fileRanges)) {
      throw new TypeError('directory asset index is invalid');
    }
    this.indexBytes = index.bytes;
    this.indexEnd = BigInt(index.bytes.byteLength);
    this.packageLength = BigInt(index.packageLength);
    this.fileRanges = index.fileRanges;
  }

  async readRange(offset, length) {
    const start = BigInt(offset);
    if (!Number.isSafeInteger(length) || length < 0) throw new RangeError('asset read length must be a nonnegative safe integer');
    const end = start + BigInt(length);
    if (start < 0n || end > this.packageLength) throw new RangeError('asset read is outside the directory');
    const output = new Uint8Array(length);
    let cursor = start;
    let written = 0;
    while (cursor < end) {
      if (cursor < this.indexEnd) {
        const count = Number((this.indexEnd < end ? this.indexEnd : end) - cursor);
        const indexOffset = Number(cursor);
        output.set(this.indexBytes.subarray(indexOffset, indexOffset + count), written);
        cursor += BigInt(count);
        written += count;
        continue;
      }

      const range = this.#findFileRange(cursor);
      if (!range) throw new Error(`directory asset range is not backed by a selected file at ${cursor}`);
      const count = Number((range.end < end ? range.end : end) - cursor);
      const fileOffset = Number(cursor - range.start);
      let bytes;
      try {
        bytes = new Uint8Array(await range.file.slice(fileOffset, fileOffset + count).arrayBuffer());
      } catch (error) {
        throw new Error(`failed to read ${range.path}: ${error?.message || error}`);
      }
      if (bytes.byteLength !== count) {
        throw new Error(`short read for ${range.path}: received ${bytes.byteLength} of ${count} requested bytes`);
      }
      output.set(bytes, written);
      cursor += BigInt(count);
      written += count;
    }
    return output;
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
}
