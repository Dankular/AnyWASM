import { fetchAssetIndex, sha256Hex } from './assets.js';

const DB_NAME = 'anywasm-packages';
const DB_VERSION = 1;
const STORE_INSTALLS = 'installs';
const STORE_CATALOGS = 'catalogs';
const MAX_CATALOG_PACKAGES = 10000;
const MAX_BUNDLE_BYTES = 512 * 1024 * 1024;
const INSTALL_CONCURRENCY = 4;
const ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/i;

function installKey(id, version) {
  return `${id}@${version}`;
}

function requireString(value, label) {
  if (typeof value !== 'string' || !value.trim()) throw new Error(`${label} must be a nonempty string`);
  return value.trim();
}

function safeUrl(value, base, label) {
  let url;
  try { url = new URL(requireString(value, label), base); }
  catch { throw new Error(`${label} is not a valid URL`); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
    throw new Error(`${label} must use HTTP or HTTPS and contain no credentials`);
  }
  url.hash = '';
  return url.href;
}

function validateIdVersion(id, version, label = 'package') {
  if (typeof id !== 'string' || !ID_PATTERN.test(id)) throw new Error(`${label} has an invalid id`);
  if (typeof version !== 'string' || !VERSION_PATTERN.test(version)) throw new Error(`${label} has an invalid version`);
}

function validateCatalog(data, url) {
  if (!data || data.schemaVersion !== 1 || !Array.isArray(data.packages)) {
    throw new Error('catalog must use schemaVersion 1 and contain a packages array');
  }
  if (data.packages.length > MAX_CATALOG_PACKAGES) throw new Error('catalog contains too many package versions');
  const name = typeof data.name === 'string' && data.name.trim() ? data.name.trim() : new URL(url).host;
  const seen = new Set();
  const packages = data.packages.map((item, index) => {
    if (!item || typeof item !== 'object') throw new Error(`catalog package ${index} is invalid`);
    validateIdVersion(item.id, item.version, `catalog package ${index}`);
    const key = installKey(item.id, item.version);
    if (seen.has(key)) throw new Error(`catalog contains duplicate package ${key}`);
    seen.add(key);
    return {
      id: item.id,
      version: item.version,
      name: typeof item.name === 'string' && item.name.trim() ? item.name.trim() : item.id,
      summary: typeof item.summary === 'string' ? item.summary : '',
      manifestUrl: safeUrl(item.manifest, url, `manifest URL for ${key}`),
      catalogUrl: url,
    };
  });
  return { url, name, packages };
}

async function validateManifest(data, entry, manifestUrl) {
  if (!data || data.schemaVersion !== 1) throw new Error(`manifest for ${entry.id}@${entry.version} has an unsupported schema`);
  validateIdVersion(data.id, data.version, 'manifest');
  if (data.id !== entry.id || data.version !== entry.version) {
    throw new Error(`manifest identity does not match catalog entry ${entry.id}@${entry.version}`);
  }
  const name = requireString(data.name, `name for ${entry.id}@${entry.version}`);
  const appEntry = requireString(data.entry, `entry for ${entry.id}@${entry.version}`);
  const pathParts = appEntry.replaceAll('\\', '/').split('/');
  if (appEntry.startsWith('/') || appEntry.includes(':') || pathParts.some((part) => part === '..' || part === '')) {
    throw new Error(`manifest entry path is not relative: ${appEntry}`);
  }
  if (!data.bundle || !Number.isSafeInteger(data.bundle.size) || data.bundle.size <= 0 || data.bundle.size > MAX_BUNDLE_BYTES ||
      !SHA256_PATTERN.test(data.bundle.sha256 || '')) {
    throw new Error(`manifest bundle metadata is invalid for ${entry.id}@${entry.version}`);
  }
  const bundle = {
    url: safeUrl(data.bundle.url, manifestUrl, 'bundle URL'),
    size: data.bundle.size,
    sha256: data.bundle.sha256.toLowerCase(),
  };
  let assets = null;
  if (data.assets !== undefined && data.assets !== null) {
    if (!Number.isSafeInteger(data.assets.size) || data.assets.size < 24 || !SHA256_PATTERN.test(data.assets.indexSha256 || '')) {
      throw new Error(`manifest asset metadata is invalid for ${entry.id}@${entry.version}`);
    }
    assets = {
      url: safeUrl(data.assets.url, manifestUrl, 'asset package URL'),
      size: data.assets.size,
      indexSha256: data.assets.indexSha256.toLowerCase(),
    };
  }
  if (!Array.isArray(data.dependencies)) throw new Error(`manifest dependencies must be an array for ${entry.id}@${entry.version}`);
  const dependencyKeys = new Set();
  const dependencies = data.dependencies.map((dependency) => {
    if (!dependency || typeof dependency !== 'object') throw new Error(`manifest contains an invalid dependency for ${entry.id}@${entry.version}`);
    validateIdVersion(dependency.id, dependency.version, 'dependency');
    const key = installKey(dependency.id, dependency.version);
    if (dependencyKeys.has(key)) throw new Error(`manifest contains duplicate dependency ${key}`);
    dependencyKeys.add(key);
    return { id: dependency.id, version: dependency.version };
  });
  const manifest = { schemaVersion: 1, id: data.id, version: data.version, name, entry: appEntry, dependencies, bundle, assets, manifestUrl };
  manifest.fingerprint = await sha256Hex(new TextEncoder().encode(JSON.stringify(manifest)));
  return manifest;
}

function openDatabase() {
  if (!globalThis.indexedDB) return Promise.reject(new Error('IndexedDB is unavailable in this browser'));
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_INSTALLS)) db.createObjectStore(STORE_INSTALLS, { keyPath: 'key' });
      if (!db.objectStoreNames.contains(STORE_CATALOGS)) db.createObjectStore(STORE_CATALOGS, { keyPath: 'url' });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error('could not open package database'));
    request.onblocked = () => reject(new Error('package database upgrade is blocked by another open tab'));
  });
}

function requestResult(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error('package database request failed'));
  });
}

function transactionDone(transaction) {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error || new Error('package database transaction failed'));
    transaction.onabort = () => reject(transaction.error || new Error('package database transaction was aborted'));
  });
}

async function boundedMap(items, limit, fn) {
  const results = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (true) {
      const index = next++;
      if (index >= items.length) return;
      results[index] = await fn(items[index], index);
    }
  });
  await Promise.all(workers);
  return results;
}

async function fetchJson(url, label) {
  const response = await fetch(url, { cache: 'no-store' });
  if (!response.ok) throw new Error(`${label} returned HTTP ${response.status}`);
  try { return await response.json(); }
  catch { throw new Error(`${label} did not contain valid JSON`); }
}

async function fetchBundle(manifest) {
  const response = await fetch(manifest.bundle.url, { cache: 'no-store' });
  if (!response.ok) throw new Error(`bundle for ${manifest.id}@${manifest.version} returned HTTP ${response.status}`);
  const bytes = await response.arrayBuffer();
  if (bytes.byteLength !== manifest.bundle.size) throw new Error(`bundle size mismatch for ${manifest.id}@${manifest.version}`);
  if (await sha256Hex(bytes) !== manifest.bundle.sha256) throw new Error(`bundle SHA-256 mismatch for ${manifest.id}@${manifest.version}`);
  return bytes;
}

async function validateAssets(manifest) {
  if (!manifest.assets) return null;
  const index = await fetchAssetIndex(manifest.assets.url);
  if (Number(index.packageLength) !== manifest.assets.size) throw new Error(`asset package size mismatch for ${manifest.id}@${manifest.version}`);
  if (await sha256Hex(index.bytes) !== manifest.assets.indexSha256) {
    throw new Error(`asset index SHA-256 mismatch for ${manifest.id}@${manifest.version}`);
  }
  return { url: manifest.assets.url, size: manifest.assets.size, indexSha256: manifest.assets.indexSha256 };
}

export class PackageManager {
  constructor(defaultCatalogUrl) {
    this.defaultCatalogUrl = safeUrl(defaultCatalogUrl, globalThis.location?.href || 'http://localhost/', 'default catalog URL');
    this.dbPromise = openDatabase();
    this.catalogs = [];
    this.entries = new Map();
    this.catalogErrors = [];
  }

  async listCatalogs() {
    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_CATALOGS, 'readonly');
    const done = transactionDone(transaction);
    const saved = await requestResult(transaction.objectStore(STORE_CATALOGS).getAll());
    await done;
    return [{ url: this.defaultCatalogUrl, isDefault: true }, ...saved.filter((item) => item.url !== this.defaultCatalogUrl)];
  }

  async addCatalog(rawUrl) {
    const url = safeUrl(rawUrl, globalThis.location?.href || this.defaultCatalogUrl, 'catalog URL');
    const catalog = await this.#fetchCatalog(url);
    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_CATALOGS, 'readwrite');
    const done = transactionDone(transaction);
    transaction.objectStore(STORE_CATALOGS).put({ url, name: catalog.name, addedAt: Date.now() });
    await done;
    return catalog;
  }

  async refreshCatalogs() {
    this.catalogs = [];
    this.entries = new Map();
    this.catalogErrors = [];
    const sources = await this.listCatalogs();
    const results = await Promise.allSettled(sources.map((source) => this.#fetchCatalog(source.url)));
    const conflicts = new Set();
    for (let i = 0; i < results.length; i++) {
      const result = results[i];
      if (result.status === 'rejected') {
        this.catalogErrors.push(`${sources[i].url}: ${result.reason?.message || result.reason}`);
        continue;
      }
      this.catalogs.push(result.value);
      for (const entry of result.value.packages) {
        const key = installKey(entry.id, entry.version);
        if (conflicts.has(key)) continue;
        const previous = this.entries.get(key);
        if (previous && previous.manifestUrl !== entry.manifestUrl) {
          this.catalogErrors.push(`Conflicting catalog entries for ${key}`);
          this.entries.delete(key);
          conflicts.add(key);
          continue;
        }
        this.entries.set(key, entry);
      }
    }
    return [...this.entries.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  }

  async #fetchCatalog(url) {
    const data = await fetchJson(url, 'catalog');
    return validateCatalog(data, url);
  }

  async listInstalled() {
    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_INSTALLS, 'readonly');
    const done = transactionDone(transaction);
    const records = await requestResult(transaction.objectStore(STORE_INSTALLS).getAll());
    await done;
    return records;
  }

  async getInstalled(id, version) {
    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_INSTALLS, 'readonly');
    const done = transactionDone(transaction);
    const record = await requestResult(transaction.objectStore(STORE_INSTALLS).get(installKey(id, version)));
    await done;
    return record || null;
  }

  async #resolve(root) {
    const resolved = new Map();
    const selectedVersions = new Map();
    const visiting = new Set();
    const visit = async (entry) => {
      const key = installKey(entry.id, entry.version);
      if (visiting.has(key)) throw new Error(`dependency cycle includes ${key}`);
      const chosen = selectedVersions.get(entry.id);
      if (chosen && chosen !== entry.version) throw new Error(`dependency conflict for ${entry.id}: ${chosen} and ${entry.version}`);
      selectedVersions.set(entry.id, entry.version);
      if (resolved.has(key)) return;
      if (resolved.size >= 128) throw new Error('dependency graph exceeds 128 packages');
      visiting.add(key);
      const raw = await fetchJson(entry.manifestUrl, `manifest for ${key}`);
      const manifest = await validateManifest(raw, entry, entry.manifestUrl);
      resolved.set(key, { entry, manifest });
      for (const dependency of manifest.dependencies) {
        const depKey = installKey(dependency.id, dependency.version);
        const depEntry = this.entries.get(depKey);
        if (!depEntry) throw new Error(`missing dependency ${depKey} required by ${key}`);
        await visit(depEntry);
      }
      visiting.delete(key);
    };
    await visit(root);
    return [...resolved.values()];
  }

  async install(id, version) {
    const root = this.entries.get(installKey(id, version));
    if (!root) throw new Error(`package ${id}@${version} is not in a loaded catalog`);
    const graph = await this.#resolve(root);
    const current = new Map((await this.listInstalled()).map((record) => [record.key, record]));
    const pending = graph.filter(({ entry, manifest }) => {
      const record = current.get(installKey(entry.id, entry.version));
      return !record || record.manifestUrl !== entry.manifestUrl || record.manifestFingerprint !== manifest.fingerprint;
    });
    if (pending.length === 0) return [...current.values()].filter((record) => graph.some(({ entry }) => record.key === installKey(entry.id, entry.version)));

    const artifactJobs = pending.flatMap(({ entry, manifest }) => [
      { key: installKey(entry.id, entry.version), manifest, kind: 'bundle' },
      ...(manifest.assets ? [{ key: installKey(entry.id, entry.version), manifest, kind: 'assets' }] : []),
    ]);
    const artifacts = new Map();
    await boundedMap(artifactJobs, INSTALL_CONCURRENCY, async (job) => {
      const value = job.kind === 'bundle' ? await fetchBundle(job.manifest) : await validateAssets(job.manifest);
      const record = artifacts.get(job.key) || {};
      record[job.kind] = value;
      artifacts.set(job.key, record);
    });
    const prepared = pending.map(({ entry, manifest }) => {
      const artifact = artifacts.get(installKey(entry.id, entry.version));
      return {
        key: installKey(entry.id, entry.version),
        id: entry.id,
        version: entry.version,
        name: manifest.name,
        entry: manifest.entry,
        dependencies: manifest.dependencies,
        manifestUrl: manifest.manifestUrl,
        manifestFingerprint: manifest.fingerprint,
        bundle: artifact.bundle,
        assetPackage: artifact.assets || null,
        installedAt: Date.now(),
      };
    });

    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_INSTALLS, 'readwrite');
    const done = transactionDone(transaction);
    for (const record of prepared) transaction.objectStore(STORE_INSTALLS).put(record);
    await done;
    return prepared;
  }

  async remove(id, version) {
    const key = installKey(id, version);
    const records = await this.listInstalled();
    const dependent = records.find((record) => record.dependencies.some((item) => installKey(item.id, item.version) === key));
    if (dependent) throw new Error(`${key} is required by ${dependent.key}`);
    const db = await this.dbPromise;
    const transaction = db.transaction(STORE_INSTALLS, 'readwrite');
    const done = transactionDone(transaction);
    transaction.objectStore(STORE_INSTALLS).delete(key);
    await done;
  }

  close() {
    this.dbPromise.then((db) => db.close()).catch(() => {});
  }
}
