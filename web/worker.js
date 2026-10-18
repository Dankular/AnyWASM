// Runs one Windows process inside a worker so the page stays responsive, and (if a canvas is
// supplied) replays the program's window/Direct3D command stream with WebGPU.
//
// In : {type:'start', wasm, bundle, canvas?, jitMinPrefix?}
//      {type:'input', kind, a, b, c}  {type:'stop'}  {type:'capture'}  {type:'readback-texture', id}
// Out: {type:'ready'}  {type:'out', stream, text}  {type:'window', width, height, title}  {type:'status', ...}
//      {type:'pixels', width, height, data}  {type:'texture-readback', ...}  {type:'gpu', draws, presents, pipelines, errors}
//      Terminal status includes JIT counters for runtime diagnostics.
import { AnyWasm, Status } from './anywasm.js';
import { ShaderTool } from './shaders.js';
import { GpuExecutor } from './gpu.js';
import { waitForPresentInterval } from './frame-pacing.js';
import { SharedCpuCompletion, SharedCpuWorkerPool } from './shared-cpu-pool.js';
import { InPlaceSharedCpuPool } from './shared-cpu-inplace-pool.js';
import { createRemoteDirectoryFiles, normalizeDirectoryPath } from './direct-assets.js';
import { encodeHostRegistrySnapshot } from './registry-snapshot.js';

let w = null, exec = null, stopped = false, disposeRequested = false, wake = null, testMode = false, sharedCpuPool = null, sharedCpuRequested = false;
let sharedNeedsCoordinatorTurn = false;
const pendingSharedInputs = [];
const pendingSharedCompletions = [];
const pendingMessageBoxes = new Map();
const pendingDialogBoxes = new Map();
let stderrCarry = '';
let sharedHandoffPendingFinish = false;
let sharedExecutionMode = 'cooperative';
let sharedExecutionReason = '';
let sharedExecutionWorkerCount = 0;
let sharedWorkerRetired = 0n;
let sharedParallelBatches = 0;
let sharedMaxConcurrentWorkerSlices = 0;
let graphicsUnavailableReason = '';
let surfacedGpuErrorCount = 0;
let userProfileStore = null;
let userProfileSavedGeneration = 0;
let userProfileFilesystemSavedGeneration = 0;
let userProfileNextFlushAt = 0;
let userProfileLastError = '';
let userProfileLastErrorAt = 0;
const dec = new TextDecoder();
const stderrDec = new TextDecoder();
let traceGuestDiagnostics = false;
const inPlaceCpuEnabled = new URL(self.location.href).searchParams.has('inPlaceCpu');
const sharedCpuWorkersEnabled = inPlaceCpuEnabled || new URL(self.location.href).searchParams.has('sharedCpuWorkers');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function readHostSystemParameters() {
  try {
    const response = await fetch(new URL('/__local/system-parameters', self.location.href));
    if (!response.ok) return { available: false, reason: `host settings returned HTTP ${response.status}` };
    const value = await response.json();
    if (value?.available === true
      && typeof value.cursorShadow === 'boolean'
      && typeof value.mouseSonar === 'boolean'
      && Number.isInteger(value.mouseTrails)
      && value.mouseTrails >= 0
      && Number.isInteger(value.wheelScrollLines)
      && value.wheelScrollLines >= 0) {
      const stickyKeysAvailable = value.stickyKeysAvailable === true
        && Number.isInteger(value.stickyKeysFlags)
        && value.stickyKeysFlags >= 0
        && value.stickyKeysFlags <= 0xffff_ffff;
      return {
        ...value,
        stickyKeysAvailable,
        stickyKeysFlags: stickyKeysAvailable ? value.stickyKeysFlags : 0,
      };
    }
    return { available: false, reason: value?.reason || 'Windows cursor-shadow setting is unavailable' };
  } catch (error) {
    return { available: false, reason: error.message || String(error) };
  }
}
async function readHostSims3Registry() {
  try {
    const response = await fetch(new URL('/__local/sims3/registry', self.location.href));
    if (!response.ok) return { available: false, reason: `host registry returned HTTP ${response.status}` };
    const value = await response.json();
    if (value?.available === true && typeof value.installPath === 'string' && Array.isArray(value.keys)) return value;
    return { available: false, reason: value?.reason || 'Windows Sims registry snapshot is unavailable' };
  } catch (error) {
    return { available: false, reason: error.message || String(error) };
  }
}

const USER_PROFILE_DB = 'anywasm-user-profile';
const USER_PROFILE_DB_VERSION = 1;
const USER_PROFILE_STORE = 'registry';
const USER_PROFILE_REGISTRY_RECORD = 'hkcu';
const USER_PROFILE_FILESYSTEM_RECORD = 'filesystem';

async function openUserProfileStore() {
  if (!globalThis.indexedDB) throw new Error('IndexedDB is unavailable in this worker');
  const db = await new Promise((resolve, reject) => {
    const request = indexedDB.open(USER_PROFILE_DB, USER_PROFILE_DB_VERSION);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(USER_PROFILE_STORE)) {
        request.result.createObjectStore(USER_PROFILE_STORE, { keyPath: 'key' });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error('could not open the virtual user profile'));
    request.onblocked = () => reject(new Error('virtual user profile database upgrade is blocked'));
  });
  db.onversionchange = () => db.close();
  const requestResult = (request) => new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error('virtual user profile request failed'));
  });
  const transactionDone = (transaction) => new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error || new Error('virtual user profile transaction failed'));
    transaction.onabort = () => reject(transaction.error || new Error('virtual user profile transaction was aborted'));
  });
  return {
    async load(key, maxBytes) {
      const transaction = db.transaction(USER_PROFILE_STORE, 'readonly');
      const done = transactionDone(transaction);
      const record = await requestResult(transaction.objectStore(USER_PROFILE_STORE).get(key));
      await done;
      if (!record) return null;
      const saved = record.snapshot;
      const bytes = saved instanceof ArrayBuffer
        ? new Uint8Array(saved)
        : ArrayBuffer.isView(saved)
          ? new Uint8Array(saved.buffer, saved.byteOffset, saved.byteLength)
          : null;
      if (!bytes || bytes.byteLength === 0 || bytes.byteLength > maxBytes) {
        throw new Error('saved virtual user profile has an invalid snapshot buffer');
      }
      return Uint8Array.from(bytes);
    },
    async save(key, snapshot) {
      const transaction = db.transaction(USER_PROFILE_STORE, 'readwrite');
      const done = transactionDone(transaction);
      transaction.objectStore(USER_PROFILE_STORE).put({ key, snapshot: Uint8Array.from(snapshot) });
      await done;
    },
    close() { db.close(); },
  };
}

function installUserProfileSnapshot(runtime, bytes) {
  if (typeof runtime.x.aw_set_user_profile_registry_snapshot !== 'function') {
    throw new Error('runtime is missing the virtual user-profile snapshot entry point');
  }
  const pointer = runtime.x.aw_alloc(bytes.byteLength) >>> 0;
  try {
    new Uint8Array(runtime.x.memory.buffer, pointer, bytes.byteLength).set(bytes);
    const result = runtime.x.aw_set_user_profile_registry_snapshot(pointer, bytes.byteLength);
    if (result !== 0) throw new Error(runtime.message() || `could not install virtual user profile (${result})`);
  } finally {
    runtime.x.aw_free(pointer, bytes.byteLength);
  }
}

function installUserProfileFilesystemSnapshot(runtime, bytes) {
  if (typeof runtime.x.aw_set_user_profile_filesystem_snapshot !== 'function') {
    throw new Error('runtime is missing the virtual user-profile filesystem snapshot entry point');
  }
  const pointer = runtime.x.aw_alloc(bytes.byteLength) >>> 0;
  try {
    new Uint8Array(runtime.x.memory.buffer, pointer, bytes.byteLength).set(bytes);
    const result = runtime.x.aw_set_user_profile_filesystem_snapshot(pointer, bytes.byteLength);
    if (result !== 0) throw new Error(runtime.message() || `could not install virtual user profile files (${result})`);
  } finally {
    runtime.x.aw_free(pointer, bytes.byteLength);
  }
}

async function initializeUserProfile(runtime) {
  userProfileStore?.close();
  userProfileStore = null;
  userProfileSavedGeneration = 0;
  userProfileFilesystemSavedGeneration = 0;
  userProfileNextFlushAt = 0;
  userProfileLastError = '';
  userProfileLastErrorAt = 0;
  let store = null;
  try {
    if (typeof runtime.x.aw_enable_user_profile_persistence !== 'function'
      || typeof runtime.x.aw_user_profile_registry_generation !== 'function'
      || typeof runtime.x.aw_user_profile_registry_snapshot_size !== 'function'
      || typeof runtime.x.aw_copy_user_profile_registry_snapshot !== 'function'
      || typeof runtime.x.aw_set_user_profile_filesystem_snapshot !== 'function'
      || typeof runtime.x.aw_user_profile_filesystem_generation !== 'function'
      || typeof runtime.x.aw_user_profile_filesystem_snapshot_size !== 'function'
      || typeof runtime.x.aw_copy_user_profile_filesystem_snapshot !== 'function') {
      throw new Error('runtime is missing virtual user-profile registry or filesystem persistence entry points');
    }
    store = await openUserProfileStore();
    const registrySnapshot = await store.load(USER_PROFILE_REGISTRY_RECORD, 32 * 1024 * 1024);
    const filesystemSnapshot = await store.load(USER_PROFILE_FILESYSTEM_RECORD, 512 * 1024 * 1024);
    if (registrySnapshot) installUserProfileSnapshot(runtime, registrySnapshot);
    if (filesystemSnapshot) installUserProfileFilesystemSnapshot(runtime, filesystemSnapshot);
    const rc = runtime.x.aw_enable_user_profile_persistence();
    if (rc !== 0) throw new Error(runtime.message() || `could not enable virtual user-profile persistence (${rc})`);
    userProfileStore = store;
    userProfileSavedGeneration = Number(runtime.x.aw_user_profile_registry_generation() || 0);
    userProfileFilesystemSavedGeneration = Number(runtime.x.aw_user_profile_filesystem_generation() || 0);
    postMessage({
      type: 'out',
      stream: 'stderr',
      text: registrySnapshot || filesystemSnapshot
        ? `[profile] loaded persistent virtual profile${registrySnapshot ? ' registry' : ''}${filesystemSnapshot ? ' files' : ''} from this browser origin\n`
        : '[profile] virtual registry and Documents storage are ready for this browser origin\n',
    });
  } catch (error) {
    store?.close();
    userProfileStore?.close();
    userProfileStore = null;
    postMessage({
      type: 'out',
      stream: 'stderr',
      text: `[profile] durable storage unavailable; profile update requests will fail with ERROR_NOT_SUPPORTED: ${error.message || error}\n`,
    });
  }
}

async function flushUserProfileUpdates(force = false) {
  if (!userProfileStore || !w) return null;
  const registryGeneration = Number(w.x.aw_user_profile_registry_generation?.() || 0);
  const filesystemGeneration = Number(w.x.aw_user_profile_filesystem_generation?.() || 0);
  if (registryGeneration === userProfileSavedGeneration
      && filesystemGeneration === userProfileFilesystemSavedGeneration) return null;
  const now = performance.now();
  if (!force && now < userProfileNextFlushAt) return null;
  userProfileNextFlushAt = now + 250;
  try {
    const snapshotRecord = async (key, sizeExport, copyExport, maximum) => {
      const size = Number(w.x[sizeExport]?.() || 0);
      if (size <= 0 || size > maximum) {
        throw new Error(w.message() || `virtual user-profile snapshot size is invalid (${size})`);
      }
      const pointer = w.x.aw_alloc(size) >>> 0;
      let snapshot;
      try {
        if (w.x[copyExport](pointer, size) !== 0) {
          throw new Error(w.message() || `could not serialize virtual user-profile ${key}`);
        }
        snapshot = new Uint8Array(w.x.memory.buffer, pointer, size).slice();
      } finally {
        w.x.aw_free(pointer, size);
      }
      await userProfileStore.save(key, snapshot);
    };
    if (registryGeneration !== userProfileSavedGeneration) {
      await snapshotRecord(USER_PROFILE_REGISTRY_RECORD,
        'aw_user_profile_registry_snapshot_size', 'aw_copy_user_profile_registry_snapshot', 32 * 1024 * 1024);
      userProfileSavedGeneration = registryGeneration;
    }
    if (filesystemGeneration !== userProfileFilesystemSavedGeneration) {
      await snapshotRecord(USER_PROFILE_FILESYSTEM_RECORD,
        'aw_user_profile_filesystem_snapshot_size', 'aw_copy_user_profile_filesystem_snapshot', 512 * 1024 * 1024);
      userProfileFilesystemSavedGeneration = filesystemGeneration;
    }
    userProfileLastError = '';
    postMessage({ type: 'out', stream: 'stderr', text: '[profile] saved virtual registry and filesystem changes\n' });
    return null;
  } catch (error) {
    const message = error.message || String(error);
    if (message !== userProfileLastError || now - userProfileLastErrorAt >= 5000) {
      userProfileLastError = message;
      userProfileLastErrorAt = now;
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[profile] could not persist virtual HKCU changes; will retry: ${message}\n`,
      });
    }
    userProfileNextFlushAt = now + 1000;
    return message;
  }
}

const MESSAGE_BOX_MARKER = '[anywasm-message-box]';
const DIALOG_BOX_MARKER = '[anywasm-dialog-box]';
const DIALOG_BOX_CLOSE_MARKER = '[anywasm-dialog-box-close]';
function decodeMarkerHex(hex) {
  if (hex.length > 2 * 1024 * 1024 || (hex.length & 1) !== 0 || !/^[0-9a-f]*$/i.test(hex)) return null;
  try {
    const bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < bytes.length; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

function parseMessageBoxMarker(line) {
  if (!line.startsWith(`${MESSAGE_BOX_MARKER}1|`)) return null;
  const fields = line.slice(`${MESSAGE_BOX_MARKER}1|`.length).split('|');
  if (fields.length !== 6) return null;
  const [ownerHex, flagsHex, defaultIdText, captionHex, textHex, buttonList] = fields;
  if (!/^[0-9a-f]{1,8}$/i.test(ownerHex) || !/^[0-9a-f]{1,8}$/i.test(flagsHex)) return null;
  if (!/^-?\d+$/.test(defaultIdText)) return null;
  const owner = Number.parseInt(ownerHex, 16) >>> 0;
  const flags = Number.parseInt(flagsHex, 16) >>> 0;
  const defaultId = Number(defaultIdText);
  if (!Number.isSafeInteger(defaultId)) return null;
  const caption = decodeMarkerHex(captionHex);
  const text = decodeMarkerHex(textHex);
  if (caption === null || text === null) return null;
  const buttons = [];
  if (buttonList) {
    for (const item of buttonList.split(',')) {
      const colon = item.indexOf(':');
      if (colon <= 0 || !/^-?\d+$/.test(item.slice(0, colon))) return null;
      const id = Number(item.slice(0, colon));
      if (!Number.isSafeInteger(id)) return null;
      buttons.push({ id, label: item.slice(colon + 1) });
    }
  }
  if (!buttons.length || new Set(buttons.map(({ id }) => id)).size !== buttons.length) return null;
  return { owner, flags, defaultId, caption, text, buttons };
}

function parseDialogBoxMarker(line) {
  if (!line.startsWith(`${DIALOG_BOX_MARKER}1|`)) return null;
  const fields = line.slice(`${DIALOG_BOX_MARKER}1|`.length).split('|');
  if (fields.length !== 13 && fields.length !== 18) return null;
  const readHex = (value, maxDigits = 8) => {
    if (!new RegExp(`^[0-9a-f]{1,${maxDigits}}$`, 'i').test(value)) return null;
    return Number.parseInt(value, 16) >>> 0;
  };
  const readDecimal = (value, signed = false) => {
    if (!(signed ? /^-?\d+$/ : /^\d+$/).test(value)) return null;
    const parsed = Number(value);
    return Number.isSafeInteger(parsed) ? parsed : null;
  };
  const hwnd = readHex(fields[0]);
  const owner = readHex(fields[1]);
  const style = readHex(fields[2]);
  const exStyle = readHex(fields[3]);
  const x = readDecimal(fields[4], true);
  const y = readDecimal(fields[5], true);
  const cx = readDecimal(fields[6], true);
  const cy = readDecimal(fields[7], true);
  const title = decodeMarkerHex(fields[8]);
  const extended = fields.length === 18;
  const font = extended ? {
    pointSize: readDecimal(fields[9]),
    weight: readDecimal(fields[10]),
    italic: readDecimal(fields[11]),
    charset: readDecimal(fields[12]),
    face: decodeMarkerHex(fields[13]),
  } : null;
  const initParamHex = fields[extended ? 14 : 9];
  const count = readDecimal(fields[extended ? 15 : 10]);
  const controlList = fields[extended ? 16 : 11];
  const callsiteHex = fields[extended ? 17 : 12];
  if ([hwnd, owner, style, exStyle, x, y, cx, cy, title, count].some((value) => value === null)
      || !/^[0-9a-f]{1,16}$/i.test(initParamHex)
      || !/^[0-9a-f]{1,16}$/i.test(callsiteHex)
      || count > 256
      || (font && Object.values(font).some((value) => value === null))) return null;
  const controls = [];
  if (controlList) {
    for (const serialized of controlList.split(';')) {
      const parts = serialized.split(',');
      if (parts.length !== 10) return null;
      const [idText, controlHwndHex, styleHex, exStyleHex, xText, yText, cxText, cyText, classHex, textHex] = parts;
      const control = {
        id: readDecimal(idText),
        hwnd: readHex(controlHwndHex),
        style: readHex(styleHex),
        exStyle: readHex(exStyleHex),
        x: readDecimal(xText, true),
        y: readDecimal(yText, true),
        cx: readDecimal(cxText, true),
        cy: readDecimal(cyText, true),
        className: decodeMarkerHex(classHex),
        text: decodeMarkerHex(textHex),
      };
      if (Object.values(control).some((value) => value === null)) return null;
      controls.push(control);
    }
  }
  if (controls.length !== count) return null;
  return {
    hwnd,
    owner,
    style,
    exStyle,
    x,
    y,
    cx,
    cy,
    title,
    initParam: Number.parseInt(initParamHex, 16),
    count,
    controls,
    callsite: Number.parseInt(callsiteHex, 16),
    font,
  };
}

function parseDialogBoxCloseMarker(line) {
  const prefix = `${DIALOG_BOX_CLOSE_MARKER}1|`;
  if (!line.startsWith(prefix)) return null;
  const fields = line.slice(prefix.length).split('|');
  if (fields.length !== 2 || !/^[0-9a-f]{1,8}$/i.test(fields[0]) || !/^[0-9a-f]{1,16}$/i.test(fields[1])) return null;
  const rawResult = BigInt(`0x${fields[1]}`);
  const result = rawResult <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(rawResult) : rawResult;
  return { hwnd: Number.parseInt(fields[0], 16) >>> 0, result };
}

function emitGuestStderr(text) {
  if (!text) return;
  const diagnostic = traceGuestDiagnostics
    ? text.split('\n').filter((line) => line.startsWith('[process-exit]') || line.startsWith('[gta-eventvol]') || line.startsWith('[gta-startup-return]') || line.startsWith('[gta-copy-trace]') || line.startsWith('[gta-copy-watch]') || line.startsWith('[gta-txd-trace]') || line.startsWith('[gta-file-open]') || line.startsWith('[gta-file-read]') || line.startsWith('[gta-file-read-result]') || line.startsWith('[gta-file-size]') || line.startsWith('[gta-file-seek]') || line.startsWith('[gta-map-view]') || line.startsWith('[anywasm-message-box]') || line.startsWith('[anywasm-dialog-box]') || line.startsWith('[anywasm-dialog-box-close]') || /PostQuitMessage|PostThreadMessage|PostMessage|DestroyWindow|ExitProcess|TerminateProcess/i.test(line)).join('\n')
    : text;
  if (diagnostic) postMessage({ type: 'out', stream: 'stderr', text: diagnostic });
}

function handleGuestStderr(text) {
  stderrCarry += text;
  const lines = stderrCarry.split('\n');
  stderrCarry = lines.pop() || '';
  for (const rawLine of lines) {
    const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
    const request = parseMessageBoxMarker(line);
    if (request) {
      const key = String(request.owner);
      if (!pendingMessageBoxes.has(key)) {
        pendingMessageBoxes.set(key, request);
        postMessage({ type: 'message-box-request', ...request });
      }
    } else {
      const dialogRequest = parseDialogBoxMarker(line);
      if (dialogRequest) {
        const key = String(dialogRequest.hwnd);
        const signature = JSON.stringify(dialogRequest);
        if (pendingDialogBoxes.get(key) !== signature) {
          pendingDialogBoxes.set(key, signature);
          postMessage({ type: 'dialog-box-request', ...dialogRequest });
        }
      } else {
        const dialogClosed = parseDialogBoxCloseMarker(line);
        if (dialogClosed) {
          pendingDialogBoxes.delete(String(dialogClosed.hwnd));
          postMessage({ type: 'dialog-box-close', ...dialogClosed });
        } else {
          emitGuestStderr(`${rawLine}\n`);
        }
      }
    }
  }
  // Only retain a partial line while it could still become a marker. Ordinary
  // stderr remains visible immediately even when the guest has not printed LF.
  const markerPrefixes = [
    `${MESSAGE_BOX_MARKER}1|`,
    `${DIALOG_BOX_MARKER}1|`,
    `${DIALOG_BOX_CLOSE_MARKER}1|`,
  ];
  const mayBeMarker = markerPrefixes.some((prefix) => prefix.startsWith(stderrCarry) || stderrCarry.startsWith(prefix));
  if (stderrCarry && !mayBeMarker) {
    emitGuestStderr(stderrCarry);
    stderrCarry = '';
  } else if (stderrCarry.startsWith(`${MESSAGE_BOX_MARKER}1|`)) {
    const request = parseMessageBoxMarker(stderrCarry);
    if (request) {
      const key = String(request.owner);
      if (!pendingMessageBoxes.has(key)) {
        pendingMessageBoxes.set(key, request);
        postMessage({ type: 'message-box-request', ...request });
      }
      stderrCarry = '';
    }
  } else if (stderrCarry.startsWith(`${DIALOG_BOX_MARKER}1|`)) {
    const request = parseDialogBoxMarker(stderrCarry);
    if (request) {
      const key = String(request.hwnd);
      const signature = JSON.stringify(request);
      if (pendingDialogBoxes.get(key) !== signature) {
        pendingDialogBoxes.set(key, signature);
        postMessage({ type: 'dialog-box-request', ...request });
      }
      stderrCarry = '';
    }
  } else if (stderrCarry.startsWith(`${DIALOG_BOX_CLOSE_MARKER}1|`)) {
    const dialogClosed = parseDialogBoxCloseMarker(stderrCarry);
    if (dialogClosed) {
      pendingDialogBoxes.delete(String(dialogClosed.hwnd));
      postMessage({ type: 'dialog-box-close', ...dialogClosed });
      stderrCarry = '';
    }
  }
}

function flushGuestStderr() {
  if (!stderrCarry) return;
  emitGuestStderr(stderrCarry);
  stderrCarry = '';
}

function applyInput(kind, a, b, c) {
  w.input(kind, a, b, c);
  if (kind === 12) pendingMessageBoxes.delete(String(Number(b) >>> 0));
  if (kind === 13) pendingDialogBoxes.delete(String(Number(b) >>> 0));
  if (kind === 14) pendingDialogBoxes.delete(String(Number(a) >>> 0));
}

function sameWindowsPath(left, right) {
  const normalize = (value) => value.replaceAll('/', '\\').replace(/[\\]+$/, '').toLowerCase();
  return typeof left === 'string' && typeof right === 'string' && normalize(left) === normalize(right);
}

function installHostRegistrySnapshot(runtime, snapshot) {
  if (typeof runtime.x.aw_set_host_registry_snapshot !== 'function') {
    throw new Error('runtime is missing the host registry snapshot entry point');
  }
  const bytes = encodeHostRegistrySnapshot(snapshot);
  const pointer = runtime.x.aw_alloc(bytes.byteLength) >>> 0;
  try {
    new Uint8Array(runtime.x.memory.buffer, pointer, bytes.byteLength).set(bytes);
    const result = runtime.x.aw_set_host_registry_snapshot(pointer, bytes.byteLength);
    if (result !== 0) throw new Error(runtime.message() || `could not supply host registry snapshot (${result})`);
  } finally {
    runtime.x.aw_free(pointer, bytes.byteLength);
  }
}
const waitForIdle = async (runtime) => {
  // Keep the virtual Windows clock moving while GetMessage is idle. Polling at 250 ms also
  // bounds input latency when the guest has no active user32 timer.
  const timerDelay = runtime.nextTimerDelayMs();
  const waitMs = Math.max(1, Math.min(timerDelay ?? 250, 250));
  const startedAt = performance.now();
  let timeout;
  await new Promise((resolve) => {
    let completed = false;
    const finish = () => {
      if (completed) return;
      completed = true;
      clearTimeout(timeout);
      if (wake === finish) wake = null;
      resolve();
    };
    wake = finish;
    timeout = setTimeout(finish, waitMs);
  });
  const elapsed100ns = Math.max(0, Math.round((performance.now() - startedAt) * 10_000));
  runtime.advanceTime100ns(BigInt(elapsed100ns));
};
const log = (m) => postMessage({ type: 'out', stream: 'stderr', text: `[gpu] ${m}\n` });

async function start({ wasm, bundle, directFiles, directSource, entryPath, assetUrl, assetPackage, canvas, audioEnabled, testMode: requestedTestMode, jitMinPrefix, traceGtaEventvol, traceImports, diagProgress }) {
  const directLaunch = !!directFiles || !!directSource;
  const directRootPath = typeof directSource?.rootPath === 'string' ? directSource.rootPath : null;
  let directChangeFeedUrl = null;
  let directChangeFeedId = null;
  let directChangeSequence = 0;
  let directChangeNextPollAt = 0;
  let directChangeLastError = '';
  let directChangeLastErrorAt = 0;
  let directChangeUnavailable = false;
  let directChangeOverflowReported = false;
  let directChangePartialAttributesReported = false;
  let directChangeNotificationsEnabled = false;
  let directChangeCoverageConfigured = false;
  let directChangeFeedReady = false;
  stopped = false;
  disposeRequested = false;
  wake = null;
  testMode = !!requestedTestMode;
  sharedCpuRequested = sharedCpuWorkersEnabled;
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedNeedsCoordinatorTurn = false;
  pendingSharedInputs.length = 0;
  pendingSharedCompletions.length = 0;
  pendingMessageBoxes.clear();
  pendingDialogBoxes.clear();
  stderrCarry = '';
  stderrDec.decode();
  traceGuestDiagnostics = !!traceGtaEventvol;
  sharedHandoffPendingFinish = false;
  sharedExecutionMode = 'cooperative';
  sharedExecutionReason = '';
  sharedExecutionWorkerCount = 0;
  sharedWorkerRetired = 0n;
  sharedParallelBatches = 0;
  sharedMaxConcurrentWorkerSlices = 0;
  surfacedGpuErrorCount = 0;
  exec?.device?.destroy();
  exec = null;
  graphicsUnavailableReason = canvas ? '' : 'OffscreenCanvas worker transfer is unsupported';
  const options = jitMinPrefix === undefined ? {} : { jitMinPrefix };
  if (inPlaceCpuEnabled) {
    // The runtime imports its linear memory so CPU workers can share guest frames in place.
    options.memory = new WebAssembly.Memory({ initial: 4096, maximum: 32768, shared: true });
  }
  w = await AnyWasm.create(wasm, options);
  w.hostIo.dsoundAudioSink = (commands) => postMessage(
    { type: 'dsound-audio', commands },
    [commands.buffer],
  );
  const setDirectChangeCoverage = (enabled) => {
    if (!directSource) return true;
    if (directChangeCoverageConfigured && directChangeNotificationsEnabled === enabled) return true;
    try {
      w.setHostChangeNotificationsEnabled(enabled);
      directChangeNotificationsEnabled = enabled;
      directChangeCoverageConfigured = true;
      return true;
    } catch (error) {
      directChangeUnavailable = true;
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[coverage] could not ${enabled ? 'enable' : 'disable'} host-folder change coverage: ${error.message || error}\n`,
      });
      return false;
    }
  };
  const hostSystemParameters = await readHostSystemParameters();
  try {
    let bootFiles = directFiles;
    if (directSource) {
      postMessage({ type: 'load-progress', message: `Scanning ${directSource.rootPath}…` });
      const response = await fetch(directSource.manifestUrl);
      if (!response.ok) {
        const detail = await response.json().catch(() => null);
        throw new Error(detail?.error || `${directSource.rootPath}: could not scan local install (HTTP ${response.status})`);
      }
      const manifest = await response.json();
      if (!Array.isArray(manifest.files) || !Array.isArray(manifest.directories)) {
        throw new Error(`${directSource.rootPath}: local install manifest is invalid`);
      }
      directChangeSequence = Number.isSafeInteger(manifest.changeSequence) && manifest.changeSequence >= 0
        ? manifest.changeSequence
        : 0;
      directChangeFeedId = typeof manifest.changeFeedId === 'string' ? manifest.changeFeedId : null;
      const changeUrl = new URL(directSource.manifestUrl);
      if (!changeUrl.pathname.endsWith('/manifest')) {
        throw new Error('local install manifest URL does not identify the Sims change feed');
      }
      changeUrl.pathname = `${changeUrl.pathname.slice(0, -'/manifest'.length)}/changes`;
      changeUrl.search = '';
      directChangeFeedUrl = changeUrl.href;
      try {
        const feedResponse = await fetch(`${directChangeFeedUrl}?since=${directChangeSequence}`, { cache: 'no-store' });
        if (feedResponse.ok) {
          const feed = await feedResponse.json();
          directChangeFeedReady = feed?.available === true
            && Number.isSafeInteger(feed.sequence)
            && Array.isArray(feed.changes);
        }
      } catch {
        directChangeFeedReady = false;
      }
      bootFiles = createRemoteDirectoryFiles([...manifest.files, ...manifest.directories], directSource.fileUrl);
      entryPath = directSource.entryPath || manifest.entryPath;
    }
    if (bootFiles) {
      const inventoryLabel = directSource ? directSource.rootPath : 'selected folder';
      postMessage({ type: 'load-progress', message: `Indexing ${bootFiles.length.toLocaleString()} files from ${inventoryLabel}…` });
      await w.loadDirectory(entryPath, bootFiles, (progress) => {
        if (progress.phase === 'boot') {
          postMessage({
            type: 'load-progress',
            message: `Loading ${progress.path} into runtime memory (${progress.fileIndex}/${progress.totalFiles} boot files)…`,
          });
        } else if (progress.phase === 'resolve') {
          postMessage({
            type: 'load-progress',
            message: 'Resolving executable imports and streaming required DLLs…',
          });
        } else {
          postMessage({
            type: 'asset-progress',
            fetchedBytes: progress.fetchedBytes,
            packageLength: Number(progress.packageLength),
          });
        }
      }, {
        afterLoadBegin: () => {
          if (directSource) setDirectChangeCoverage(directChangeFeedReady);
        },
      });
    } else {
      w.load(bundle);
    }
    w.setDsoundAudioEnabled(!!audioEnabled);
    if (directSource) {
      w.setVirtualRoot(directSource.rootPath);
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[host filesystem] mounted ${directSource.rootPath} as the streamed install root\n`,
      });
    }
    if (hostSystemParameters.available) {
      if (typeof w.x.aw_set_host_system_parameters !== 'function') {
        throw new Error('runtime is missing the host system-parameters entry point');
      }
      const rc = w.x.aw_set_host_system_parameters(
        hostSystemParameters.cursorShadow ? 1 : 0,
        hostSystemParameters.mouseTrails,
        hostSystemParameters.mouseSonar ? 1 : 0,
        hostSystemParameters.wheelScrollLines,
        hostSystemParameters.stickyKeysFlags,
        hostSystemParameters.stickyKeysAvailable ? 1 : 0,
      );
      if (rc !== 0) throw new Error(w.message() || `could not supply host system parameters (${rc})`);
    } else {
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[host] ${hostSystemParameters.reason}; SystemParametersInfo queries will remain untranslated.\n`,
      });
    }
    const hostRegistry = await readHostSims3Registry();
    if (directSource) {
      if (hostRegistry.available && sameWindowsPath(hostRegistry.installPath, directSource.rootPath)) {
        installHostRegistrySnapshot(w, hostRegistry);
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: `[host registry] loaded ${hostRegistry.keys.length} keys from the matching 32-bit Sims registration (read-only snapshot)\n`,
        });
      } else {
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: `[host registry] ${hostRegistry.reason || `snapshot install path ${hostRegistry.installPath} does not match ${directSource.rootPath}`}; registry reads outside translated data will stop explicitly.\n`,
        });
      }
    } else if (hostRegistry.available) {
      const shellFoldersPath = 'hkey_current_user\\software\\microsoft\\windows\\currentversion\\explorer\\shell folders';
      const shellFoldersKey = hostRegistry.keys.find((key) =>
        typeof key.path === 'string' && key.path.toLowerCase() === shellFoldersPath);
      if (shellFoldersKey) {
        installHostRegistrySnapshot(w, { available: true, keys: [shellFoldersKey] });
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: '[host registry] loaded the shared HKCU Shell Folders key (read-only snapshot)\n',
        });
      } else {
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: '[host registry] shared HKCU Shell Folders key is absent from the read-only snapshot; registry reads outside translated data will stop explicitly.\n',
        });
      }
    } else {
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[host registry] ${hostRegistry.reason || 'shared Shell Folders snapshot is unavailable'}; registry reads outside translated data will stop explicitly.\n`,
      });
    }
    await initializeUserProfile(w);
  } catch (e) {
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message, instructions: 0, mips: 0 });
    return;
  }
  if (traceGtaEventvol) w.x.aw_set_gta_eventvol_trace?.(1);
  if (traceImports) w.x.aw_set_import_trace?.(1);
  bundle = null;
  directFiles = null;
  directSource = null;
  if (assetUrl && !directLaunch) {
    postMessage({ type: 'load-progress', message: 'Loading game data index…' });
    let lastAssetProgress = 0;
    await w.attachAssets(assetUrl, ({ fetchedBytes, packageLength }) => {
      if (fetchedBytes - lastAssetProgress >= 4 * 1024 * 1024 || fetchedBytes === packageLength) {
        lastAssetProgress = fetchedBytes;
        postMessage({ type: 'asset-progress', fetchedBytes, packageLength: Number(packageLength) });
      }
    }, assetPackage || null);
  }
  if (sharedCpuRequested) {
    postMessage({ type: 'load-progress', message: 'Starting shared CPU worker…' });
    try {
      const response = await fetch(new URL(inPlaceCpuEnabled ? './anywasm_cpu_worker_inplace.wasm' : './anywasm_cpu_worker.wasm', import.meta.url));
      if (!response.ok) throw new Error(`worker module fetch returned HTTP ${response.status}`);
      const workerModule = await WebAssembly.compile(await response.arrayBuffer());
      const threadIds = w.sharedCpuContextThreadIds();
      if (!threadIds.length) throw new Error('the loaded process has no CPU contexts');
      if (inPlaceCpuEnabled) {
        sharedCpuPool = await InPlaceSharedCpuPool.create({
          workerWasm: workerModule,
          workerCount: 2,
          contextCount: Math.max(threadIds.length, 64),
          anyWasm: w,
        });
      } else {
        const pageCount = Number(w.sharedSnapshotPageCount());
        sharedCpuPool = await SharedCpuWorkerPool.create({
          workerWasm: workerModule,
          workerCount: 2,
          contextCount: Math.max(threadIds.length, 64),
          pageCount,
        });
      }
      sharedCpuPool.publishFrom(w);
      sharedExecutionMode = inPlaceCpuEnabled ? 'shared-cpu-workers-in-place' : 'shared-cpu-workers';
      sharedExecutionWorkerCount = sharedCpuPool.workerCount;
      postMessage({ type: 'out', stream: 'stderr', text: '[runtime] shared CPU worker mode enabled\n' });
    } catch (error) {
      sharedCpuPool?.dispose();
      sharedCpuPool = null;
      sharedCpuRequested = false;
      sharedExecutionReason = error.message || String(error);
      for (const input of pendingSharedInputs.splice(0)) {
        if (input.type === 'input') applyInput(input.kind, input.a, input.b, input.c);
        else w.pushStdin(new TextEncoder().encode(input.text));
      }
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[runtime] shared CPU worker unavailable; using cooperative execution: ${error.message || error}\n`,
      });
    }
  }
  const publishSharedCoordinatorState = () => {
    if (!sharedCpuPool) return;
    try {
      sharedCpuPool.publishFrom(w);
    } catch (error) {
      sharedCpuPool.dispose();
      sharedCpuPool = null;
      sharedCpuRequested = false;
      sharedNeedsCoordinatorTurn = false;
      sharedExecutionMode = 'cooperative';
      sharedExecutionReason = error.message || String(error);
      sharedExecutionWorkerCount = 0;
      pendingSharedCompletions.length = 0;
      sharedHandoffPendingFinish = false;
      for (const input of pendingSharedInputs.splice(0)) {
        if (input.type === 'input') applyInput(input.kind, input.a, input.b, input.c);
        else w.pushStdin(new TextEncoder().encode(input.text));
      }
      postMessage({
        type: 'out',
        stream: 'stderr',
        text: `[runtime] shared CPU layout changed; continuing cooperatively: ${error.message || error}\n`,
      });
    }
  };
  const traceGuestThreads = () => {
    const count = Math.min(Number(w.x.aw_debug_thread_count?.() || 0), 64);
    const threads = [];
    for (let i = 0; i < count; i++) {
      const id = Number(w.x.aw_debug_thread_at?.(i) || 0);
      if (!id) continue;
      const waitHandle = Number(w.x.aw_debug_thread_wait_handle?.(id) || 0);
      threads.push({
        id: `0x${id.toString(16)}`,
        rip: `0x${Number(w.x.aw_debug_thread_rip?.(id) || 0).toString(16)}`,
        state: Number(w.x.aw_debug_thread_state?.(id) || 0),
        waitHandle: waitHandle ? `0x${waitHandle.toString(16)}` : null,
        handleState: waitHandle ? Number(w.x.aw_debug_handle_state?.(BigInt(waitHandle)) || 0) : 0,
        sp: `0x${Number(w.x.aw_debug_thread_sp?.(id) || 0).toString(16)}`,
        stack: Array.from({ length: 8 }, (_, word) =>
          `0x${Number(w.x.aw_debug_thread_stack_word?.(id, word) || 0).toString(16)}`),
      });
    }
    const readGuestU16 = (address) => Number(w.x.aw_debug_guest_u16?.(address) || 0);
    const readGuestU32 = (address) => Number(w.x.aw_debug_guest_u32?.(address) || 0);
    const currentRip = Number(w.x.aw_debug_rip?.() || 0) >>> 0;
    const currentRegs = Object.fromEntries(['eax', 'ecx', 'edx', 'ebx', 'esp', 'ebp', 'esi', 'edi']
      .map((name, index) => [name, Number(w.x.aw_debug_gpr?.(index) || 0) >>> 0]));
    const readGuestString = (address, limit = 48) => {
      if (!address || address < 0x10000 || address > 0xfffffff0) return null;
      let result = '';
      for (let offset = 0; offset < limit; offset++) {
        const word = readGuestU32((address + (offset & ~3)) >>> 0) >>> 0;
        const byte = (word >>> ((offset & 3) * 8)) & 0xff;
        if (byte === 0) break;
        result += byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : `\\x${byte.toString(16).padStart(2, '0')}`;
      }
      return result;
    };
    const callFrames = [];
    const framePointerValid = (candidate) =>
      candidate >= currentRegs.esp && candidate <= (currentRegs.esp + 0x10000) >>> 0 && (candidate & 3) === 0;
    const framePointerSource = framePointerValid(currentRegs.ebp) ? 'ebp' : 'saved-ebp';
    let frame = framePointerSource === 'ebp'
      ? currentRegs.ebp
      : readGuestU32((currentRegs.esp + 8) >>> 0) >>> 0;
    for (let depth = 0; depth < 8 && frame >= 0x10000 && frame <= 0xfffffff0; depth++) {
      const previous = readGuestU32(frame) >>> 0;
      const returnAddress = readGuestU32((frame + 4) >>> 0) >>> 0;
      const args = Array.from({ length: 4 }, (_, index) => {
        const value = readGuestU32((frame + 8 + index * 4) >>> 0) >>> 0;
        return { value: `0x${value.toString(16)}`, text: readGuestString(value, depth === 0 ? 48 : 16) };
      });
      callFrames.push({ frame: `0x${frame.toString(16)}`, previous: `0x${previous.toString(16)}`, returnAddress: `0x${returnAddress.toString(16)}`, args });
      if (previous <= frame || previous > 0xfffffff0) break;
      frame = previous;
    }
    const stack32 = Array.from({ length: 128 }, (_, index) =>
      `0x${(readGuestU32((currentRegs.esp + index * 4) >>> 0) >>> 0).toString(16)}`);
    const stackStrings = Array.from({ length: 32 }, (_, index) => {
      const value = readGuestU32((currentRegs.esp + index * 4) >>> 0) >>> 0;
      const text = readGuestString(value);
      return text ? { offset: index * 4, value: `0x${value.toString(16)}`, text } : null;
    }).filter(Boolean);
    const memoryWindow = (address, words) => ({
      base: `0x${(address >>> 0).toString(16)}`,
      words: Array.from({ length: words }, (_, index) =>
        `0x${(readGuestU32((address + index * 4) >>> 0) >>> 0).toString(16)}`),
    });
    const queueCount = Math.min(Number(w.x.aw_debug_window_queue_len?.() || 0), 8);
    const factoryRoot = readGuestU32(0xc97b24) >>> 0;
    const factoryOffset = readGuestU32(0xc97b4c) >>> 0;
    const factoryMethods = (factoryRoot + factoryOffset) >>> 0;
    const resourceManager = readGuestU32(0x11af588) >>> 0;
    const resourcePath = readGuestU32((resourceManager + 0x28) >>> 0) >>> 0;
    const txdPoolBase = readGuestU32(0xc88018) >>> 0;
    const txdStorage = readGuestU32(0xc8800c) >>> 0;
    const txdRecords = readGuestU32(txdStorage) >>> 0;
    const txdIndex = readGuestU32(0xc88014) >>> 0;
    return {
      threadId: `0x${Number(w.x.aw_debug_thread_id?.() || 0).toString(16)}`,
      threads,
      regs: Object.fromEntries(Object.entries(currentRegs).map(([name, value]) => [name, `0x${value.toString(16)}`])),
      framePointerSource,
      callFrames,
      stack32,
      stackStrings,
      faultMemory: {
        object: memoryWindow((currentRegs.ebp - 0x10) >>> 0, 48),
        lockedBuffer: memoryWindow((currentRegs.edx - 0x10) >>> 0, 24),
        source: memoryWindow((currentRegs.esi - 0x10) >>> 0, 8),
        copyInput: memoryWindow(0x31b07318, 48),
        copiedRecord: memoryWindow(0x30c04090, 16),
      },
      resourceGlobals: {
        resourceSystem: {
          managerGlobal: memoryWindow(0x11af588, 4),
          manager: memoryWindow(resourceManager, 20),
          managerPath: memoryWindow(resourcePath, 12),
          workingDirectory: readGuestString(0x11af388, 260),
          driveList: memoryWindow(0x11af5a0, 27),
        },
        factoryBase: memoryWindow(0xc97b24, 12),
        rasterCreateCallback: `0x${readGuestU32((factoryRoot + 0x58) >>> 0).toString(16)}`,
        rasterCreateCallbackSlots: memoryWindow((factoryRoot + 0x58) >>> 0, 8),
        factoryAllocatorSlots: memoryWindow((factoryRoot + 0x130) >>> 0, 8),
        factoryAllocateCallback: `0x${readGuestU32((factoryRoot + 0x144) >>> 0).toString(16)}`,
        factoryVtable: memoryWindow(0xc97b4c, 16),
        factoryObject: memoryWindow(factoryMethods, 24),
        failedParticleRaster: memoryWindow(0x3005cc68, 16),
        factoryLookupCallback: `0x${readGuestU32((factoryMethods + 0x18) >>> 0).toString(16)}`,
        factoryCreateCallback: `0x${readGuestU32((factoryMethods + 0x14) >>> 0).toString(16)}`,
        txdTable: memoryWindow(0xc8800c, 12),
        txdIndices: memoryWindow(0xc88014, 8),
        txdStorage: memoryWindow(txdStorage, 8),
        txdRecords: memoryWindow(txdRecords, 36),
        currentTxdRecord: memoryWindow((txdRecords + txdIndex * 12) >>> 0, 6),
        txdPoolBase: `0x${txdPoolBase.toString(16)}`,
        txdPoolFirstWords: memoryWindow(txdPoolBase, 8),
        errorState: memoryWindow(0xc9a6f0, 4),
      },
      flags: `0x${Number(w.x.aw_debug_rflags?.() || 0).toString(16)}`,
      tickMs: Number(w.x.aw_debug_tick_ms?.() || 0),
      keyEvents: Array.from({ length: 12 }, (_, i) => readGuestU16(0xb731a8 + i * 2)),
      customEvents: Array.from({ length: 12 }, (_, i) => readGuestU16(0xb73190 + i * 2)),
      gameState: {
        startupPhase: readGuestU32(0xc8d4c0),
        splashFlag: readGuestU32(0xba67a4),
        windowFlag: readGuestU32(0xba67a8),
        activationFlag: readGuestU32(0xc87ffc),
        shutdownFlag: readGuestU32(0xc17050),
        returnSeed: readGuestU32(0xc17044),
        startupInputFlag: readGuestU32(0xba67a5),
      },
      windowQueue: Array.from({ length: queueCount }, (_, i) => ({
        hwnd: `0x${Number(w.x.aw_debug_window_message?.(i, 0) || 0).toString(16)}`,
        msg: `0x${Number(w.x.aw_debug_window_message?.(i, 1) || 0).toString(16)}`,
        wparam: `0x${Number(w.x.aw_debug_window_message?.(i, 2) || 0).toString(16)}`,
        lparam: `0x${Number(w.x.aw_debug_window_message?.(i, 3) || 0).toString(16)}`,
      })),
    };
  };
  const pollDirectHostChanges = async () => {
    if (!directChangeFeedUrl || directChangeUnavailable || performance.now() < directChangeNextPollAt) return;
    directChangeNextPollAt = performance.now() + 500;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 2_000);
    try {
      const response = await fetch(`${directChangeFeedUrl}?since=${directChangeSequence}`, {
        cache: 'no-store',
        signal: controller.signal,
      });
      if (!response.ok) throw new Error(`host-folder change feed returned HTTP ${response.status}`);
      const feed = await response.json();
      if (feed?.available !== true) {
        setDirectChangeCoverage(false);
        directChangeUnavailable = true;
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: `[coverage] host-folder change observation is unavailable: ${feed?.reason || 'local server disabled its recursive watcher'}\n`,
        });
        return;
      }
      if (!Number.isSafeInteger(feed.sequence) || feed.sequence < 0 || !Array.isArray(feed.changes)) {
        throw new Error('host-folder change feed returned an invalid sequence response');
      }
      if (!setDirectChangeCoverage(true)) return;
      if (!directChangePartialAttributesReported
          && Array.isArray(feed.partialFilters)
          && feed.partialFilters.includes(0x0004)) {
        directChangePartialAttributesReported = true;
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: '[coverage] host-folder FILE_NOTIFY_CHANGE_ATTRIBUTES is partial: read-only/mode changes are observed, but Windows Hidden, System, and Archive attribute changes are not represented by Node file metadata.\n',
        });
      }
      if (typeof feed.feedId === 'string') {
        if (directChangeFeedId && feed.feedId !== directChangeFeedId) {
          directChangeSequence = Number.isSafeInteger(feed.oldestSequence) && feed.oldestSequence > 0
            ? feed.oldestSequence - 1
            : 0;
          directChangeOverflowReported = true;
          postMessage({
            type: 'out',
            stream: 'stderr',
            text: '[coverage] host change server restarted; applying its retained events from the new sequence epoch\n',
          });
        }
        directChangeFeedId = feed.feedId;
      }
      if (feed.truncated === true && !directChangeOverflowReported) {
        directChangeOverflowReported = true;
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: '[coverage] host-folder change feed overflowed; applying retained events, but older notifications may have been missed\n',
        });
      }
      for (const change of feed.changes) {
        const sequence = Number(change?.sequence);
        if (!Number.isSafeInteger(sequence) || sequence <= directChangeSequence) continue;
        if (sequence !== directChangeSequence + 1 && feed.truncated !== true && !directChangeOverflowReported) {
          directChangeOverflowReported = true;
          postMessage({
            type: 'out',
            stream: 'stderr',
            text: `[coverage] host-folder change feed skipped sequence ${directChangeSequence + 1} through ${sequence - 1}\n`,
          });
        }
        let relativePath;
        try {
          relativePath = normalizeDirectoryPath(change.path);
        } catch {
          directChangeSequence = sequence;
          continue;
        }
        const filter = Number(change.filter);
        if (!Number.isInteger(filter) || filter <= 0 || (filter & ~0x007f) !== 0) {
          directChangeSequence = sequence;
          continue;
        }
        const rootPath = directRootPath.replace(/[\\/]+$/, '');
        const guestPath = `${rootPath}\\${relativePath}`;
        try {
          w.notifyVfsChange(guestPath, filter);
        } catch (error) {
          setDirectChangeCoverage(false);
          directChangeUnavailable = true;
          postMessage({
            type: 'out',
            stream: 'stderr',
            text: `[coverage] could not deliver host-folder change notifications: ${error.message || error}\n`,
          });
          return;
        }
        directChangeSequence = sequence;
      }
      if (feed.changes.length === 0 && feed.sequence > directChangeSequence) {
        directChangeSequence = feed.sequence;
      }
      directChangeLastError = '';
    } catch (error) {
      setDirectChangeCoverage(false);
      const message = error?.message || String(error);
      const now = performance.now();
      if (message !== directChangeLastError || now - directChangeLastErrorAt >= 5_000) {
        directChangeLastError = message;
        directChangeLastErrorAt = now;
        postMessage({
          type: 'out',
          stream: 'stderr',
          text: `[coverage] host-folder change polling failed; will retry: ${message}\n`,
        });
      }
      directChangeNextPollAt = now + 5_000;
    } finally {
      clearTimeout(timeout);
    }
  };
  if (canvas) {
    try {
      const tool = new ShaderTool(new URL('./shaders/', import.meta.url));
      await tool.init();
      exec = new GpuExecutor(canvas, tool, log, { noPresent: testMode, profile: new URL(self.location.href).searchParams.has('profileGpu') });
      exec.onWindow = (win) => postMessage({ type: 'window', ...win });
      await exec.init();
      w.hostIo.gpuExecutor = exec;
      postMessage({ type: 'gpuinfo', info: { ...exec.adapterInfo }, features: [...exec.features] });
    } catch (e) {
      graphicsUnavailableReason = `WebGPU unavailable: ${e.message}`;
      log(graphicsUnavailableReason);
      exec = null;
    }
  }
  postMessage({ type: 'ready' });
  const t0 = performance.now();
  let lastTraceProgress = t0;
  let status = Status.RUNNING;
  let terminalMessage = '';
  let frames = 0;
  let userProfileFlushError = '';
  const SHARED_PUMP_INSTRUCTION_BUDGET = 250_000n;
  let sharedInstructionBudgetRemaining = SHARED_PUMP_INSTRUCTION_BUDGET;
  const chargeSharedInstructionBudget = (before) => {
    const retired = w.retiredInstructionsExact() - before;
    if (retired < 0n) throw new Error('shared CPU retired-instruction counter moved backwards');
    if (retired > sharedInstructionBudgetRemaining) {
      sharedInstructionBudgetRemaining = 0n;
      throw new Error('shared CPU coordinator exceeded its pump instruction budget');
    }
    sharedInstructionBudgetRemaining -= retired;
    return retired;
  };
  while (!stopped && (status === Status.RUNNING || status === Status.FRAME || status === Status.IDLE)) {
    // Host I/O is polled only between completed guest/coordinator slices, never from inside a
    // Wasm call or while shared CPU workers are executing a batch.
    await pollDirectHostChanges();
    const sliceStart = performance.now();
    sharedInstructionBudgetRemaining = SHARED_PUMP_INSTRUCTION_BUDGET;
    status = Status.RUNNING;
    while (!stopped && status === Status.RUNNING && performance.now() - sliceStart < 12 && !w.hostWaitPending) {
      if (!sharedCpuPool) {
        status = w.run(250_000);
        continue;
      }
      if (sharedInstructionBudgetRemaining === 0n) break;
      if (w.callbackSuspended) {
        const before = w.retiredInstructionsExact();
        status = w.run(Number(sharedInstructionBudgetRemaining));
        chargeSharedInstructionBudget(before);
        if (!w.hostWaitPending && !w.callbackSuspended) {
          if (sharedHandoffPendingFinish) {
            w.finishSharedCpuCoordinatorTurn();
            sharedHandoffPendingFinish = false;
          }
          publishSharedCoordinatorState();
        }
        continue;
      }
      if (sharedNeedsCoordinatorTurn) {
        sharedCpuPool.importTo(w);
        for (const input of pendingSharedInputs.splice(0)) {
          if (input.type === 'input') applyInput(input.kind, input.a, input.b, input.c);
          else w.pushStdin(new TextEncoder().encode(input.text));
        }
        sharedNeedsCoordinatorTurn = false;
        if (pendingSharedCompletions.length) {
          const completion = pendingSharedCompletions.shift();
          if (completion.state === SharedCpuCompletion.FAILED) {
            throw new Error(`shared CPU worker failed at thread ${completion.threadId}`);
          }
          const before = w.retiredInstructionsExact();
          const switched = w.beginSharedCpuCoordinatorTurn(completion.threadId);
          try {
            status = w.run(1);
          } finally {
            if (switched && !w.hostWaitPending && !w.callbackSuspended) {
              w.finishSharedCpuCoordinatorTurn();
            } else if (switched) {
              sharedHandoffPendingFinish = true;
            }
          }
          chargeSharedInstructionBudget(before);
          sharedNeedsCoordinatorTurn = pendingSharedCompletions.length > 0;
        } else {
          const before = w.retiredInstructionsExact();
          status = w.run(1);
          chargeSharedInstructionBudget(before);
        }
        if (!w.hostWaitPending && !sharedHandoffPendingFinish) publishSharedCoordinatorState();
        continue;
      }
      const runnableThreadIds = w.sharedCpuRunnableThreadIds();
      if (!runnableThreadIds.length) {
        sharedCpuPool.importTo(w);
        const coordinatorBudget = Number(sharedInstructionBudgetRemaining);
        const before = w.retiredInstructionsExact();
        status = w.run(coordinatorBudget);
        chargeSharedInstructionBudget(before);
        if (!w.hostWaitPending) publishSharedCoordinatorState();
        continue;
      }
      const contexts = runnableThreadIds
        .slice(0, sharedCpuPool.workerCount)
        .map((threadId) => ({
          threadId,
          instructionBudget: w.sharedCpuThreadQuantumRemaining(threadId),
        }))
        .filter((context) => context.instructionBudget > 0);
      const coordinatorReserve = BigInt(contexts.length);
      const workerBudget = sharedInstructionBudgetRemaining > coordinatorReserve
        ? sharedInstructionBudgetRemaining - coordinatorReserve
        : 0n;
      if (workerBudget === 0n || contexts.length === 0) break;

      const before = w.retiredInstructionsExact();
      const completions = await sharedCpuPool.dispatchBatch(contexts, workerBudget);
      if (completions.some((completion) => completion.state === SharedCpuCompletion.FAILED)) {
        const failed = completions.find((completion) => completion.state === SharedCpuCompletion.FAILED);
        throw new Error(`shared CPU worker failed at thread ${failed.threadId}`);
      }
      const workerRetired = completions.reduce(
        (total, completion) => total + completion.retiredInstructions,
        0n,
      );
      sharedWorkerRetired += workerRetired;
      if (contexts.length >= 2) sharedParallelBatches++;
      sharedMaxConcurrentWorkerSlices = Math.max(
        sharedMaxConcurrentWorkerSlices,
        completions[0]?.maxConcurrentWorkerSlices || 0,
      );
      if (workerRetired > workerBudget) {
        throw new Error('shared CPU workers exceeded their combined instruction reservation');
      }
      sharedCpuPool.importTo(w);
      const importedRetired = chargeSharedInstructionBudget(before);
      if (importedRetired !== workerRetired) {
        throw new Error('shared CPU completion counts do not match coordinator accounting');
      }
      pendingSharedCompletions.push(
        ...completions.filter((completion) => completion.state !== SharedCpuCompletion.QUANTUM),
      );
      if (pendingSharedCompletions.length) sharedNeedsCoordinatorTurn = true;
    }
    userProfileFlushError = await flushUserProfileUpdates() || '';
    if (w.gpuPendingBytes()) {
      if (exec) w.consumeGpu((commands) => exec.execute(commands)); // executed in place: no copy
      else {
        w.takeGpu();
        status = Status.CRASHED;
        terminalMessage = `Graphics unavailable: ${graphicsUnavailableReason || 'no renderer is available'}`;
      }
    }
    if (exec && exec.stats.errors.length > surfacedGpuErrorCount) {
      const errors = exec.stats.errors.slice(surfacedGpuErrorCount);
      surfacedGpuErrorCount = exec.stats.errors.length;
      const detail = errors.slice(0, 4).join(' | ');
      const remainder = errors.length > 4 ? ` | and ${errors.length - 4} more renderer error(s)` : '';
      const rendererFailure = `WebGPU translation failed: ${detail}${remainder}`;
      if (status !== Status.CRASHED && status !== Status.UNIMPLEMENTED) {
        status = Status.CRASHED;
        terminalMessage = rendererFailure;
      } else {
        terminalMessage = [terminalMessage || w.message(), rendererFailure].filter(Boolean).join('\n');
      }
    }
    const o = w.takeStdout(); if (o.length) postMessage({ type: 'out', stream: 'stdout', text: dec.decode(o) });
    const e = w.takeStderr();
    if (e.length) handleGuestStderr(stderrDec.decode(e, { stream: true }));
    if ((traceGtaEventvol || diagProgress) && performance.now() - lastTraceProgress >= (diagProgress ? 1_000 : 5_000)) {
      lastTraceProgress = performance.now();
      const trace = traceGtaEventvol ? {
        rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
        instructions: w.retiredInstructions(),
        jitStats: w.jitStats,
        asyncify: w.asyncifyStats,
        ...(diagProgress ? {
          executionMode: sharedExecutionMode,
          executionWorkerCount: sharedExecutionWorkerCount,
          executionModeReason: sharedExecutionReason,
          sharedWorkerRetired: String(sharedWorkerRetired),
          sharedParallelBatches,
          maxConcurrentWorkerSlices: sharedMaxConcurrentWorkerSlices,
        } : {}),
        ...traceGuestThreads(),
      } : {
        rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
        instructions: w.retiredInstructions(),
        jitStats: w.jitStats,
        asyncify: w.asyncifyStats,
        ...(diagProgress ? {
          executionMode: sharedExecutionMode,
          executionWorkerCount: sharedExecutionWorkerCount,
          executionModeReason: sharedExecutionReason,
          sharedWorkerRetired: String(sharedWorkerRetired),
          sharedParallelBatches,
          maxConcurrentWorkerSlices: sharedMaxConcurrentWorkerSlices,
        } : {}),
        ...traceGuestThreads(),
        crtFile: Array.from({ length: 8 }, (_, index) =>
          `0x${(Number(w.x.aw_debug_guest_u32?.(0x8e2c90 + index * 4) || 0) >>> 0).toString(16)}`),
      };
      postMessage({
        type: 'trace-progress',
        trace,
      });
      if (diagProgress) console.info(`[gta-worker-progress]${JSON.stringify({
        rip: trace.rip,
        instructions: trace.instructions,
        jitStats: trace.jitStats,
        asyncify: trace.asyncify,
        frames,
        draws: exec?.stats?.draws ?? 0,
        presents: exec?.stats?.presents ?? 0,
        hostWaitPending: w.hostWaitPending,
        assetRange: w.hostIo.pendingRange,
        executionMode: trace.executionMode,
        executionWorkerCount: trace.executionWorkerCount,
        executionModeReason: trace.executionModeReason,
        sharedWorkerRetired: trace.sharedWorkerRetired,
        sharedParallelBatches: trace.sharedParallelBatches,
        regs: trace.regs,
        callFrames: trace.callFrames?.map(({ frame, returnAddress, args }) => ({ frame, returnAddress, args: args.slice(0, 2) })),
        gameState: trace.gameState,
        keyEvents: trace.keyEvents,
        customEvents: trace.customEvents,
      })}`);
    }
    if (w.hostWaitPending) {
      postMessage({
        type: 'load-progress',
        message: 'Fetching requested game data…',
        trace: traceGtaEventvol || diagProgress ? {
          rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
          instructions: w.retiredInstructions(),
          asyncify: w.asyncifyStats,
          assetRange: w.hostIo.pendingRange,
          ...(traceGtaEventvol ? traceGuestThreads() : {}),
        } : undefined,
      });
      try {
        await w.waitForHost();
        if (!w.callbackSuspended) publishSharedCoordinatorState();
      } catch (e) {
        status = Status.CRASHED;
        terminalMessage = `Game data range request failed: ${e.message}`;
      }
      continue;
    } else if (status === Status.FRAME) {
      frames++;
      if (exec && frames % 30 === 0) postMessage({
        type: 'gpu',
        ...exec.stats,
        asyncify: w.asyncifyStats,
        trace: traceGtaEventvol ? { rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`, instructions: w.retiredInstructions(), ...traceGuestThreads() } : undefined,
      });
      const syncInterval = exec?.lastPresentSyncInterval ?? 1;
      const waitStart = performance.now();
      const vblanksWaited = await waitForPresentInterval(syncInterval);
      const elapsed100ns = Math.max(0, Math.round((performance.now() - waitStart) * 10_000));
      w.advanceTime100ns(BigInt(elapsed100ns));
      if (testMode) postMessage({ type: 'frame-pacing', syncInterval, vblanksWaited, elapsed100ns, virtualTicksAdded: elapsed100ns * 10 });
      publishSharedCoordinatorState();
    } else if (status === Status.IDLE) {
      if (traceGtaEventvol) {
        postMessage({
          type: 'load-progress',
          message: 'Guest waiting for input',
          trace: { rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`, instructions: w.retiredInstructions(), ...traceGuestThreads() },
        });
      }
      await waitForIdle(w);
    }
    else if (status === Status.RUNNING) await sleep(0);
  }
  const stderrTail = stderrDec.decode();
  if (stderrTail) handleGuestStderr(stderrTail);
  flushGuestStderr();
  const dt = (performance.now() - t0) / 1000;
  userProfileFlushError = await flushUserProfileUpdates(true) || userProfileFlushError;
  const callbackSuspended = !!w?.callbackSuspended;
  const n = callbackSuspended ? 0 : w.instructions();
  if (exec) postMessage({ type: 'gpu', ...exec.stats, asyncify: w?.asyncifyStats });
  const finalMessage = [
    terminalMessage || (callbackSuspended ? 'stopped with a guest callback suspended' : w.message()),
    userProfileFlushError ? `Virtual user-profile persistence failed: ${userProfileFlushError}` : '',
  ].filter(Boolean).join('\n');
  const exceptionSp = Number(finalMessage.match(/\besp=0x([0-9a-f]+)/i)?.[1] || 0) >>> 0;
  const exceptionStack = diagProgress && exceptionSp
    ? Array.from({ length: 64 }, (_, index) =>
        `0x${(Number(w.x.aw_debug_guest_u32?.((exceptionSp + index * 4) >>> 0) || 0) >>> 0).toString(16)}`)
    : undefined;
  const finalTrace = traceGtaEventvol || diagProgress ? {
    rip: `0x${Number(w.x.aw_debug_rip?.() || 0).toString(16)}`,
    instructions: w.retiredInstructions(),
    ...traceGuestThreads(),
    ...(exceptionStack ? { exceptionSp: `0x${exceptionSp.toString(16)}`, exceptionStack } : {}),
  } : undefined;
  sharedCpuPool?.dispose();
  sharedCpuPool = null;
  sharedCpuRequested = false;
  userProfileStore?.close();
  userProfileStore = null;
  if (disposeRequested) {
    close();
    return;
  }
  postMessage({ type: 'status', status: stopped ? Status.RUNNING : status, exitCode: callbackSuspended || status === Status.CRASHED && terminalMessage ? -1 : w.exitCode(), message: finalMessage, instructions: n, mips: n / dt / 1e6, frames, jitStats: w.jitStats, trace: finalTrace, executionMode: sharedExecutionMode, executionWorkerCount: sharedExecutionWorkerCount, executionModeReason: sharedExecutionReason, sharedWorkerRetired: String(sharedWorkerRetired), sharedParallelBatches, maxConcurrentWorkerSlices: sharedMaxConcurrentWorkerSlices });
}

onmessage = async (ev) => {
  const m = ev.data;
  if (m.type === 'start') start(m).catch((e) => {
    sharedCpuPool?.dispose();
    sharedCpuPool = null;
    sharedCpuRequested = false;
    if (disposeRequested) {
      close();
      return;
    }
    postMessage({ type: 'status', status: Status.CRASHED, exitCode: -1, message: e.message || String(e), instructions: 0, mips: 0, executionMode: sharedExecutionMode, executionWorkerCount: sharedExecutionWorkerCount, executionModeReason: sharedExecutionReason });
  });
  else if (m.type === 'dispose') {
    disposeRequested = true;
    stopped = true;
    wake?.();
    if (!w) {
      sharedCpuPool?.dispose();
      sharedCpuPool = null;
      close();
    }
  }
  else if (m.type === 'input' && w) {
    if (sharedCpuRequested) {
      pendingSharedInputs.push({ type: 'input', kind: m.kind, a: m.a, b: m.b, c: m.c });
      sharedNeedsCoordinatorTurn = true;
    } else applyInput(m.kind, m.a, m.b, m.c);
    wake?.(); wake = null;
  }
  else if (m.type === 'stdin' && w) {
    if (sharedCpuRequested) {
      pendingSharedInputs.push({ type: 'stdin', text: m.text });
      sharedNeedsCoordinatorTurn = true;
    } else w.pushStdin(new TextEncoder().encode(m.text));
    wake?.(); wake = null;
  }
  else if (m.type === 'stop') { stopped = true; wake?.(); }
  else if (m.type === 'capture') {
    try {
      const px = exec ? await exec.readBackbuffer() : null;
      postMessage({ type: 'pixels', ...(px || { width: 0, height: 0, data: new Uint8Array(0) }) });
    } catch (e) {
      log('capture failed: ' + (e.stack || e.message));
      postMessage({ type: 'pixels', width: 0, height: 0, data: new Uint8Array(0), error: e.message });
    }
  } else if (m.type === 'readback-texture') {
    try {
      if (!testMode) throw new Error('texture readback is available only in test mode');
      if (!exec) throw new Error('WebGPU executor is unavailable');
      const result = await exec.readBackTexture(m.id);
      postMessage({ type: 'texture-readback', ...result });
    } catch (e) {
      postMessage({ type: 'texture-readback', id: m.id, error: e.message });
    }
  }
};
