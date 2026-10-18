import { PackageManager } from './package-manager.js';
import { DSoundAudioOutput } from './dsound-audio.js';

const $ = (id) => document.getElementById(id);
const term = $('term');
const stat = $('stat');
const diagnostic = $('diagnostic');
const loading = $('loading');
const loadMessage = $('load-message');
let worker = null;
const dsoundAudio = new DSoundAudioOutput((error) => print(`[audio] ${error.message}\n`, 'err'));
document.addEventListener('pointerdown', () => { void dsoundAudio.activate(); }, { capture: true });
document.addEventListener('click', () => { void dsoundAudio.activate(); }, { capture: true });
const utf8Encoder = new TextEncoder();
let activeModal = null;
const queuedModals = [];
const knownMessageBoxOwners = new Set();
const knownDialogBoxes = new Map();
const messageBoxLayer = document.createElement('div');
messageBoxLayer.id = 'message-box-layer';
messageBoxLayer.hidden = true;
messageBoxLayer.setAttribute('aria-live', 'assertive');
document.body.append(messageBoxLayer);
const messageBoxStyles = document.createElement('style');
messageBoxStyles.textContent = `
  #message-box-layer { position:fixed; inset:0; z-index:10000; display:grid; place-items:center; padding:20px; background:rgb(0 0 0 / .58); }
  #message-box-layer[hidden] { display:none; }
  .message-box-panel { width:min(100%, 480px); max-height:min(90vh, 680px); overflow:auto; padding:20px; border:1px solid var(--line); border-radius:8px; background:var(--panel); color:var(--fg); box-shadow:0 20px 70px rgb(0 0 0 / .45); }
  .message-box-caption { margin:0 0 12px; font-size:17px; font-weight:600; }
  .message-box-text { margin:0; max-height:min(55vh, 480px); overflow:auto; white-space:pre-wrap; overflow-wrap:anywhere; }
  .message-box-actions { display:flex; flex-wrap:wrap; justify-content:flex-end; gap:8px; margin-top:20px; }
  .message-box-actions button:focus-visible { outline:2px solid var(--acc); outline-offset:2px; }
  .dialog-box-panel { width:max-content; min-width:min(100%, 280px); max-width:calc(100vw - 40px); max-height:calc(100vh - 40px); overflow:auto; padding:12px; border:1px solid var(--line); border-radius:8px; background:var(--panel); color:var(--fg); box-shadow:0 20px 70px rgb(0 0 0 / .45); }
  .dialog-box-header { display:flex; align-items:center; justify-content:space-between; gap:16px; margin-bottom:8px; }
  .dialog-box-title { margin:0; font-size:16px; font-weight:600; }
  .dialog-box-close { padding:0 8px; font-size:20px; line-height:1.3; }
  .dialog-box-client { position:relative; overflow:auto; }
  .dialog-box-control { position:absolute; margin:0; min-width:0; min-height:0; }
  .dialog-box-control.static { overflow:hidden; white-space:pre-wrap; overflow-wrap:anywhere; }
  .dialog-box-control.edit { width:auto; padding:2px 4px; border:1px solid var(--line); border-radius:3px; background:var(--bg); color:var(--fg); font:inherit; }
  textarea.dialog-box-control.edit { resize:none; }
  .dialog-box-control.button { padding:2px 10px; }
  .dialog-box-control:focus-visible { outline:2px solid var(--acc); outline-offset:1px; }
`;
document.head.append(messageBoxStyles);

function capabilities() {
  const caps = [
    ['WebAssembly', typeof WebAssembly === 'object', 'required'],
    ['Module workers', typeof Worker !== 'undefined', 'required'],
    ['WebGPU', !!navigator.gpu, 'needed for graphics programs'],
    ['OffscreenCanvas transfer', typeof HTMLCanvasElement.prototype.transferControlToOffscreen === 'function', 'needed for worker rendering'],
    ['SharedArrayBuffer workers', typeof SharedArrayBuffer !== 'undefined' && globalThis.crossOriginIsolated === true, 'optional parallel guest CPU execution'],
  ];
  $('caps').innerHTML = caps
    .map(([n, ok, why]) => `<div class="cap"><b>${n}</b><span class="${ok ? 'ok' : 'bad'}">${ok ? 'available' : 'missing'}</span> <span class="dim">· ${why}</span></div>`)
    .join('');
  return Object.fromEntries(caps.map(([n, ok]) => [n, ok]));
}

function print(text, cls) {
  const span = document.createElement('span');
  if (cls) span.className = cls;
  span.textContent = text.replace(/\r\n/g, '\n');
  term.appendChild(span);
  term.scrollTop = term.scrollHeight;
}

function showLoading(message) {
  loadMessage.textContent = message;
  loading.hidden = false;
}

function clearLoading() {
  loading.hidden = true;
  loadMessage.textContent = '';
}

function clearModalDialogs() {
  activeModal = null;
  queuedModals.length = 0;
  knownMessageBoxOwners.clear();
  knownDialogBoxes.clear();
  messageBoxLayer.replaceChildren();
  messageBoxLayer.hidden = true;
  const app = document.querySelector('main');
  if (app) app.inert = false;
}

function enqueueMessageBox(message) {
  if (!Number.isInteger(message.owner) || !Array.isArray(message.buttons) || !message.buttons.length) return;
  const request = {
    owner: Number(message.owner) >>> 0,
    flags: Number(message.flags) >>> 0,
    defaultId: Number(message.defaultId),
    caption: String(message.caption ?? ''),
    text: String(message.text ?? ''),
    buttons: message.buttons.filter((button) => button && Number.isSafeInteger(button.id) && typeof button.label === 'string'),
  };
  if (!request.buttons.length) return;
  const ownerKey = String(request.owner);
  if (knownMessageBoxOwners.has(ownerKey)) return;
  knownMessageBoxOwners.add(ownerKey);
  enqueueModal('message-box', request);
}

function enqueueDialogBox(message) {
  if (!Number.isInteger(message.hwnd) || !Array.isArray(message.controls)) return;
  const request = {
    hwnd: Number(message.hwnd) >>> 0,
    owner: Number(message.owner) >>> 0,
    style: Number(message.style) >>> 0,
    exStyle: Number(message.exStyle) >>> 0,
    x: Number(message.x) || 0,
    y: Number(message.y) || 0,
    cx: Number(message.cx) || 0,
    cy: Number(message.cy) || 0,
    title: String(message.title ?? ''),
    font: message.font || null,
    controls: message.controls.filter((control) => control && Number.isSafeInteger(control.id)
      && Number.isInteger(control.hwnd) && typeof control.className === 'string' && typeof control.text === 'string'),
  };
  const key = String(request.hwnd);
  knownDialogBoxes.set(key, request);
  if (activeModal?.kind === 'dialog-box' && activeModal.request.hwnd === request.hwnd) {
    refreshDialogBox(activeModal, request);
    return;
  }
  const queued = queuedModals.find((item) => item.kind === 'dialog-box' && item.request.hwnd === request.hwnd);
  if (queued) {
    queued.request = request;
    return;
  }
  enqueueModal('dialog-box', request);
}

function enqueueModal(kind, request, restoreFocus = undefined) {
  if (activeModal) queuedModals.push({ kind, request, restoreFocus });
  else showModal(kind, request, restoreFocus);
}

function showModal(kind, request, restoreFocusOverride = undefined) {
  const restoreFocus = restoreFocusOverride === undefined
    ? (document.activeElement instanceof HTMLElement ? document.activeElement : null)
    : restoreFocusOverride;
  if (kind === 'dialog-box') showDialogBox(request, restoreFocus);
  else showMessageBox(request, restoreFocus);
}

function showMessageBox(request, restoreFocus) {
  const panel = document.createElement('section');
  panel.className = 'message-box-panel';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-labelledby', 'message-box-caption');
  panel.setAttribute('aria-describedby', 'message-box-text');
  panel.tabIndex = -1;

  const title = document.createElement('h2');
  title.id = 'message-box-caption';
  title.className = 'message-box-caption';
  title.textContent = request.caption || 'Message';
  const body = document.createElement('p');
  body.id = 'message-box-text';
  body.className = 'message-box-text';
  body.textContent = request.text;
  const actions = document.createElement('div');
  actions.className = 'message-box-actions';
  const controls = request.buttons.map((choice) => {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = choice.label;
    button.dataset.buttonId = String(choice.id);
    button.addEventListener('click', () => chooseMessageBoxButton(choice.id));
    actions.append(button);
    return button;
  });
  panel.append(title, body, actions);
  messageBoxLayer.replaceChildren(panel);
  messageBoxLayer.hidden = false;
  const app = document.querySelector('main');
  if (app) app.inert = true;
  activeModal = { kind: 'message-box', request, panel, controls, restoreFocus };
  const defaultButton = request.buttons.find((choice) => choice.id === request.defaultId);
  const focusButton = defaultButton ? controls[request.buttons.indexOf(defaultButton)] : panel;
  queueMicrotask(() => focusButton?.focus());
  panel.addEventListener('keydown', (event) => {
    if (!activeModal || activeModal.panel !== panel || activeModal.kind !== 'message-box') return;
    if (event.key === 'Enter') {
      const focused = controls.find((button) => button === document.activeElement);
      const selected = focused || controls[request.buttons.findIndex((choice) => choice.id === request.defaultId)];
      event.preventDefault();
      if (selected) chooseMessageBoxButton(Number(selected.dataset.buttonId));
    } else if (event.key === 'Escape') {
      event.preventDefault();
      const cancel = request.buttons.find((choice) => choice.id === 2);
      if (cancel) chooseMessageBoxButton(cancel.id);
    } else if (event.key === 'Tab') {
      const focusedIndex = controls.indexOf(document.activeElement);
      if (event.shiftKey && focusedIndex <= 0) {
        event.preventDefault();
        controls.at(-1)?.focus();
      } else if (!event.shiftKey && focusedIndex === controls.length - 1) {
        event.preventDefault();
        controls[0]?.focus();
      }
    }
  });
}

function chooseMessageBoxButton(buttonId) {
  const active = activeModal;
  if (!active || active.kind !== 'message-box' || !active.request.buttons.some((choice) => choice.id === buttonId) || !worker) return;
  worker.postMessage({ type: 'input', kind: 12, a: buttonId, b: active.request.owner, c: 0 });
  knownMessageBoxOwners.delete(String(active.request.owner));
  finishActiveModal();
}

function dialogPixels(dlu, horizontal) {
  return Math.max(0, Number(dlu) * (horizontal ? 7 / 4 : 14 / 8));
}

function makeDialogControl(control, dialog) {
  const className = control.className.toLowerCase();
  let element;
  if (className === 'button') {
    element = document.createElement('button');
    element.type = 'button';
    element.addEventListener('click', () => chooseDialogControl(dialog, control.id));
    element.classList.add('button');
  } else if (className === 'edit') {
    element = (control.style & 0x0004) !== 0 ? document.createElement('textarea') : document.createElement('input');
    if (element instanceof HTMLInputElement) {
      element.type = 'text';
      element.autocomplete = 'off';
    }
    element.spellcheck = false;
    element.addEventListener('input', () => updateDialogEdit(dialog, control.id, element.value));
    element.classList.add('edit');
  } else if (className === 'static') {
    element = document.createElement('div');
    element.classList.add('static');
  } else {
    return null;
  }
  element.classList.add('dialog-box-control');
  element.dataset.controlId = String(control.id);
  element.setAttribute('aria-label', control.text || `${className} ${control.id}`);
  return element;
}

function refreshDialogBox(active, request) {
  active.request = request;
  active.actionPending = false;
  active.closePending = false;
  active.defaultButton = null;
  active.title.textContent = request.title || 'Dialog';
  const width = Math.max(120, dialogPixels(request.cx, true));
  const height = Math.max(40, dialogPixels(request.cy, false));
  active.client.style.width = `${width}px`;
  active.client.style.height = `${height}px`;
  active.panel.style.width = `min(calc(100vw - 40px), ${width + 28}px)`;
  if (request.font?.face) active.client.style.fontFamily = request.font.face;
  if (Number.isFinite(request.font?.pointSize) && request.font.pointSize > 0) {
    active.client.style.fontSize = `${Math.max(6, Math.min(48, request.font.pointSize * 4 / 3))}px`;
  }
  if (Number.isFinite(request.font?.weight) && request.font.weight > 0) {
    active.client.style.fontWeight = String(Math.max(100, Math.min(1000, request.font.weight)));
  }
  if (request.font?.italic) active.client.style.fontStyle = 'italic';
  const wanted = new Set();
  for (const control of request.controls) {
    const key = String(control.id);
    wanted.add(key);
    let record = active.controlElements.get(key);
    if (!record || record.className !== control.className.toLowerCase()) {
      record?.element.remove();
      active.controlElements.delete(key);
      const element = makeDialogControl(control, active);
      if (!element) continue;
      record = { element, className: control.className.toLowerCase() };
      active.controlElements.set(key, record);
      active.client.append(element);
    }
    const element = record.element;
    element.style.left = `${dialogPixels(control.x, true)}px`;
    element.style.top = `${dialogPixels(control.y, false)}px`;
    element.style.width = `${Math.max(8, dialogPixels(control.cx, true))}px`;
    element.style.height = `${Math.max(8, dialogPixels(control.cy, false))}px`;
    const visible = (control.style & 0x10000000) !== 0;
    element.hidden = !visible;
    if (record.className === 'edit') {
      let pending = active.pendingEdits.get(key);
      if (pending !== undefined && pending === control.text) {
        active.pendingEdits.delete(key);
        pending = undefined;
      }
      const value = pending === undefined ? control.text : pending;
      if (element.value !== value) element.value = value;
      element.readOnly = (control.style & 0x800) !== 0;
      element.disabled = (control.style & 0x08000000) !== 0 || active.actionPending || active.closePending;
    } else {
      element.textContent = control.text;
      element.setAttribute('aria-label', control.text || `${record.className} ${control.id}`);
      if (record.className === 'button') {
        element.disabled = (control.style & 0x08000000) !== 0 || active.actionPending || active.closePending;
      }
    }
  }
  for (const [key, record] of active.controlElements) {
    if (!wanted.has(key)) {
      record.element.remove();
      active.controlElements.delete(key);
    }
  }
  const visibleButtons = request.controls.filter((control) => control.className.toLowerCase() === 'button'
    && (control.style & 0x10000000) !== 0)
    .map((control) => ({ control, element: active.controlElements.get(String(control.id))?.element }))
    .filter(({ element }) => element instanceof HTMLButtonElement && !element.disabled);
  active.defaultButton = visibleButtons.find(({ control }) => (control.style & 0x0f) === 1)?.element
    || visibleButtons[0]?.element || null;
}

function showDialogBox(request, restoreFocus) {
  const panel = document.createElement('section');
  panel.className = 'dialog-box-panel';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-labelledby', 'dialog-box-title');
  panel.tabIndex = -1;
  const header = document.createElement('div');
  header.className = 'dialog-box-header';
  const title = document.createElement('h2');
  title.id = 'dialog-box-title';
  title.className = 'dialog-box-title';
  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'dialog-box-close';
  close.setAttribute('aria-label', 'Close dialog');
  close.textContent = '×';
  const client = document.createElement('div');
  client.className = 'dialog-box-client';
  header.append(title, close);
  panel.append(header, client);
  messageBoxLayer.replaceChildren(panel);
  messageBoxLayer.hidden = false;
  const app = document.querySelector('main');
  if (app) app.inert = true;
  const active = {
    kind: 'dialog-box', request, panel, title, client, closeButton: close, restoreFocus,
    controlElements: new Map(), pendingEdits: new Map(), actionPending: false,
    closePending: false, defaultButton: null,
  };
  activeModal = active;
  close.addEventListener('click', () => requestDialogClose(active));
  panel.addEventListener('keydown', (event) => {
    if (activeModal !== active) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      requestDialogClose(active);
    } else if (event.key === 'Enter' && !(document.activeElement instanceof HTMLTextAreaElement)) {
      const focusedButton = [...active.controlElements.values()].find(({ element }) => element === document.activeElement && element instanceof HTMLButtonElement);
      const target = focusedButton?.element
        || (active.defaultButton && !active.defaultButton.hidden && !active.defaultButton.disabled ? active.defaultButton : null);
      if (target && !target.disabled) {
        event.preventDefault();
        target.click();
      }
    } else if (event.key === 'Tab') {
      const focusable = [active.closeButton, ...[...active.controlElements.values()]
        .map(({ element }) => element)
        .filter((element) => !element.hidden && !element.disabled && (element instanceof HTMLButtonElement || element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement))];
      const focusedIndex = focusable.indexOf(document.activeElement);
      if (event.shiftKey && focusedIndex <= 0) {
        event.preventDefault();
        focusable.at(-1)?.focus();
      } else if (!event.shiftKey && focusedIndex === focusable.length - 1) {
        event.preventDefault();
        focusable[0]?.focus();
      }
    }
  });
  refreshDialogBox(active, request);
  queueMicrotask(() => {
    const initialFocus = [...active.controlElements.values()].find(({ element }) => !element.hidden && !element.disabled)?.element;
    const defaultFocus = active.defaultButton && !active.defaultButton.hidden && !active.defaultButton.disabled
      ? active.defaultButton : null;
    (defaultFocus || initialFocus || panel).focus();
  });
}

function updateDialogEdit(active, controlId, text) {
  if (!worker || activeModal !== active) return;
  let normalized = '';
  let bytes = 0;
  let scalarCount = 0;
  for (const scalar of Array.from(text)) {
    const codepoint = scalar.codePointAt(0);
    const safeScalar = codepoint >= 0xd800 && codepoint <= 0xdfff ? '\ufffd' : scalar;
    const scalarBytes = utf8Encoder.encode(safeScalar).length;
    if (scalarCount >= 32767 || bytes + scalarBytes > 65535) break;
    normalized += safeScalar;
    bytes += scalarBytes;
    scalarCount++;
  }
  const key = String(controlId);
  active.pendingEdits.set(key, normalized);
  const element = active.controlElements.get(key)?.element;
  if ((element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)
      && element.value !== normalized && normalized.length < text.length) {
    element.value = normalized;
  }
  worker.postMessage({ type: 'input', kind: 14, a: active.request.hwnd, b: controlId, c: 0 });
  for (const scalar of Array.from(normalized)) {
    worker.postMessage({ type: 'input', kind: 14, a: active.request.hwnd, b: controlId, c: scalar.codePointAt(0) });
  }
}

function chooseDialogControl(active, controlId) {
  if (!worker || activeModal !== active || active.actionPending || active.closePending) return;
  active.actionPending = true;
  for (const { element } of active.controlElements.values()) {
    if ('disabled' in element) element.disabled = true;
  }
  worker.postMessage({ type: 'input', kind: 13, a: controlId, b: active.request.hwnd, c: 0 });
}

function requestDialogClose(active) {
  if (!worker || activeModal !== active || active.closePending) return;
  active.closePending = true;
  for (const { element } of active.controlElements.values()) {
    if ('disabled' in element) element.disabled = true;
  }
  worker.postMessage({ type: 'input', kind: 13, a: 0, b: active.request.hwnd, c: 1 });
}

function closeDialogBox(message) {
  const key = String(Number(message.hwnd) >>> 0);
  knownDialogBoxes.delete(key);
  for (let index = queuedModals.length - 1; index >= 0; index--) {
    if (queuedModals[index].kind === 'dialog-box' && String(queuedModals[index].request.hwnd) === key) queuedModals.splice(index, 1);
  }
  if (activeModal?.kind === 'dialog-box' && String(activeModal.request.hwnd) === key) finishActiveModal();
}

function finishActiveModal() {
  const finished = activeModal;
  if (!finished) return;
  activeModal = null;
  messageBoxLayer.replaceChildren();
  if (queuedModals.length) {
    const next = queuedModals.shift();
    showModal(next.kind, next.request, finished.restoreFocus);
  } else {
    messageBoxLayer.hidden = true;
    const app = document.querySelector('main');
    if (app) app.inert = false;
    finished.restoreFocus?.focus?.({ preventScroll: true });
  }
}

function failLoading(label, message) {
  clearLoading();
  stat.textContent = `${label}: ${message}`;
  stat.dataset.status = '3';
  stat.dataset.exit = '-1';
  $('stop').disabled = true;
}

async function run(bundleBytes, label, assetUrl = null, assetPackage = null, directFolder = null) {
  const audioActivation = dsoundAudio.activate();
  dsoundAudio.stopAll();
  const reuseWorker = !!worker && stat.dataset.status !== undefined;
  if (worker && !reuseWorker) worker.postMessage({ type: 'dispose' });
  term.textContent = '';
  clearModalDialogs();
  stat.textContent = `Starting ${label}…`;
  diagnostic.hidden = true;
  diagnostic.replaceChildren();
  delete stat.dataset.status;
  $('stop').disabled = false;
  showLoading('Loading WebAssembly runtime…');
  let wasm;
  try {
    // ?inPlaceCpu selects the runtime linked against an imported shared memory (zero-copy workers).
    const runtimeFile = new URLSearchParams(location.search).has('inPlaceCpu') ? './anywasm_wasm_shared.wasm' : './anywasm_wasm.wasm';
    const response = await fetch(new URL(runtimeFile, import.meta.url));
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    wasm = await response.arrayBuffer();
  } catch (e) {
    failLoading(label, `could not load WebAssembly runtime (${e.message})`);
    return;
  }
  if (!reuseWorker) {
    const workerUrl = new URL('./worker.js', import.meta.url);
    const searchParams = new URLSearchParams(location.search);
    if (searchParams.has('sharedCpuWorkers') && searchParams.get('sharedCpuWorkers') !== '0') {
      workerUrl.searchParams.set('sharedCpuWorkers', '');
    }
    if (new URLSearchParams(location.search).has('profileGpu')) workerUrl.searchParams.set('profileGpu', '');
    if (new URLSearchParams(location.search).has('inPlaceCpu')) {
      workerUrl.searchParams.set('inPlaceCpu', '');
    }
    worker = new Worker(workerUrl, { type: 'module' });
    window.__worker = worker; // exposed for automated tests
  }

  // A fresh canvas per run: a canvas can only be transferred to a worker once.
  const old = $('screen');
  const canvas = document.createElement('canvas');
  canvas.id = 'screen'; canvas.width = 640; canvas.height = 480; canvas.hidden = true; canvas.tabIndex = 0;
  old.replaceWith(canvas);
  const setInputSize = attachInput(canvas);
  const off = typeof canvas.transferControlToOffscreen === 'function'
    ? canvas.transferControlToOffscreen()
    : null;

  worker.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'ready') clearLoading();
    else if (m.type === 'load-progress') {
      if (m.trace) window.__workerDiag = m.trace;
      showLoading(m.message);
    }
    else if (m.type === 'trace-progress') window.__workerDiag = m.trace;
    else if (m.type === 'asset-progress') {
      const fetched = (m.fetchedBytes / (1024 * 1024)).toFixed(1);
      const total = (Number(m.packageLength) / (1024 * 1024)).toFixed(1);
      showLoading(`Game data: ${fetched} MiB fetched on demand · ${total} MiB available`);
    }
    else if (m.type === 'message-box-request') enqueueMessageBox(m);
    else if (m.type === 'dialog-box-request') enqueueDialogBox(m);
    else if (m.type === 'dialog-box-close') closeDialogBox(m);
    else if (m.type === 'dsound-audio') dsoundAudio.consume(m.commands);
    else if (m.type === 'out') print(m.text, m.stream === 'stderr' ? 'err' : '');
    else if (m.type === 'window') {
      canvas.hidden = false;
      canvas.focus({ preventScroll: true });
      canvas.style.aspectRatio = `${m.width} / ${m.height}`;
      setInputSize(m.width, m.height);
      if (m.title) document.title = `${m.title} - AnyWASM`;
    } else if (m.type === 'gpuinfo') {
      window.__gpuInfo = m;
    } else if (m.type === 'gpu') {
      window.__gpuStats = m;
    } else if (m.type === 'pixels') {
      window.__pixels = m;
    } else if (m.type === 'texture-readback') {
      window.__textureReadback = m;
    } else if (m.type === 'status') {
      dsoundAudio.stopAll();
      if (m.trace) window.__workerDiag = m.trace;
      if (m.jitStats) window.__jitStats = m.jitStats;
      clearLoading();
      const names = { 0: 'stopped', 1: 'exited', 2: 'stopped at an unimplemented import', 3: 'crashed' };
      const detail = m.status === 1 ? `exit code ${m.exitCode}` : m.status === 2 ? '' : m.message;
      const executionMode = m.executionMode?.startsWith('shared-cpu-workers')
        ? `${m.executionWorkerCount} shared CPU workers`
        : 'cooperative CPU';
      const executionReason = m.executionModeReason ? ` (${m.executionModeReason})` : '';
      window.__executionMode = {
        mode: m.executionMode || 'cooperative',
        workerCount: m.executionWorkerCount || 0,
        reason: m.executionModeReason || '',
        workerRetired: m.sharedWorkerRetired || '0',
        parallelBatches: m.sharedParallelBatches || 0,
        maxConcurrentWorkerSlices: m.maxConcurrentWorkerSlices || 0,
      };
      stat.textContent = `${label}: ${names[m.status]}${detail ? ' (' + detail + ')' : ''} · ${m.instructions.toLocaleString()} instructions · ${m.mips.toFixed(1)} M instr/s · ${executionMode}${executionReason}${m.frames ? ' · ' + m.frames + ' frames' : ''}`;
      if (m.status === 2) {
        const title = document.createElement('strong');
        title.textContent = 'Stopped at an unimplemented import';
        const message = document.createElement('code');
        message.textContent = m.message || 'Import name unavailable.';
        const guidance = document.createElement('span');
        guidance.textContent = 'Run anywasm analyze <program.exe> to see all statically imported symbols.';
        diagnostic.replaceChildren(title, message, guidance);
        diagnostic.hidden = false;
      }
      stat.dataset.status = String(m.status);
      stat.dataset.exit = String(m.exitCode);
      $('stop').disabled = true;
    }
  };
  worker.onerror = (e) => {
    worker = null;
    window.__worker = null;
    failLoading(label, `worker failed to start (${e.message || 'unknown error'})`);
  };
  const testMode = new URLSearchParams(location.search).has('test');
  const traceGtaEventvol = new URLSearchParams(location.search).has('traceGtaEventvol');
  const traceImports = new URLSearchParams(location.search).has('traceImports');
  const diagProgress = new URLSearchParams(location.search).has('diagProgress');
  const jitMinPrefixValue = new URLSearchParams(location.search).get('jitMinPrefix');
  showLoading('Starting guest program…');
  const audioEnabled = await audioActivation;
  const message = { type: 'start', wasm, bundle: bundleBytes, assetUrl, assetPackage, audioEnabled, testMode, traceGtaEventvol, traceImports, diagProgress };
  if (directFolder) {
    if (directFolder.files) {
      message.directFiles = directFolder.files;
      message.entryPath = directFolder.entryPath;
    } else {
      message.directSource = directFolder;
    }
  }
  message.jitMinPrefix = jitMinPrefixValue === null ? 65 : Number(jitMinPrefixValue);
  const transfer = [wasm];
  if (bundleBytes) transfer.push(bundleBytes.buffer);
  if (off) {
    message.canvas = off;
    transfer.push(off);
  }
  worker.postMessage(message, transfer);
}

function attachInput(canvas) {
  let guestWidth = canvas.width;
  let guestHeight = canvas.height;
  const send = (kind, a = 0, b = 0, c = 0) => worker?.postMessage({ type: 'input', kind, a, b, c });
  const pos = (e) => {
    const r = canvas.getBoundingClientRect();
    const width = canvas.clientWidth || r.width;
    const height = canvas.clientHeight || r.height;
    return [
      Math.round((e.clientX - r.left - canvas.clientLeft) * guestWidth / width),
      Math.round((e.clientY - r.top - canvas.clientTop) * guestHeight / height),
    ];
  };
  canvas.addEventListener('keydown', (e) => { send(0, e.keyCode); if (e.key.length === 1) send(2, e.key.charCodeAt(0)); e.preventDefault(); });
  canvas.addEventListener('keyup', (e) => { send(1, e.keyCode); e.preventDefault(); });
  canvas.addEventListener('mousemove', (e) => { const [x, y] = pos(e); send(3, x, y); });
  canvas.addEventListener('mouseleave', () => send(10));
  canvas.addEventListener('mousedown', (e) => { canvas.focus(); const [x, y] = pos(e); send(4, e.button, x, y); });
  canvas.addEventListener('mouseup', (e) => { const [x, y] = pos(e); send(5, e.button, x, y); });
  canvas.addEventListener('wheel', (e) => { const [x, y] = pos(e); send(6, -Math.sign(e.deltaY) * 120, x, y); e.preventDefault(); }, { passive: false });
  canvas.addEventListener('contextmenu', (e) => e.preventDefault());
  canvas.addEventListener('focus', () => send(8, 1));
  canvas.addEventListener('blur', () => send(8, 0));
  return (width, height) => {
    if (Number.isFinite(width) && width > 0) guestWidth = Math.round(width);
    if (Number.isFinite(height) && height > 0) guestHeight = Math.round(height);
  };
}

async function demo(name) {
  const requestedExe = name === 'gta_sa' && new URLSearchParams(location.search).get('exe') === 'gta_sa.exe'
    ? 'gta_sa.exe'
    : null;
  const bundleName = requestedExe ? 'gta_sa_regular' : name;
  showLoading(`Loading demo bundle ${bundleName}.awb…`);
  try {
    const res = await fetch(new URL(`./demo/${bundleName}.awb`, import.meta.url));
    if (!res.ok) {
      clearLoading();
      stat.textContent = `demo ${bundleName}.awb not found - build it with: anywasm pack`;
      return;
    }
    const assetUrl = name === 'gta_sa' ? new URL('./demo/gta_sa.agp', import.meta.url).href : null;
    const entryName = requestedExe || (name === 'gta_sa' ? 'gta_sa_compact1.0.exe' : `${name}.exe`);
    await run(new Uint8Array(await res.arrayBuffer()), entryName, assetUrl);
  } catch (e) {
    failLoading(`${bundleName}.awb`, `could not load bundle (${e.message})`);
  }
}

capabilities();
$('file').addEventListener('change', async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  showLoading(`Reading bundle ${f.name}…`);
  try {
    await run(new Uint8Array(await f.arrayBuffer()), f.name);
  } catch (error) {
    failLoading(f.name, `could not read bundle (${error.message})`);
  }
});
$('demo-hello').addEventListener('click', () => demo('hello'));
$('demo-crt').addEventListener('click', () => demo('crt_test'));
$('demo-tri').addEventListener('click', () => demo('d3d11_triangle'));
$('demo-tex').addEventListener('click', () => demo('d3d11_textured'));
$('demo-gta').addEventListener('click', () => demo('gta_sa'));
$('demo-sims3').addEventListener('click', () => {
  const entryPath = 'Game\\Bin\\TS3W.exe';
  const directSource = {
    rootPath: 'D:\\The Sims 3',
    manifestUrl: new URL('/__local/sims3/manifest', location.origin).href,
    fileUrl: new URL('/__local/sims3/file', location.origin).href,
    entryPath,
  };
  void run(null, 'D:\\The Sims 3\\Game\\Bin\\TS3W.exe', null, null, directSource);
});
$('stop').addEventListener('click', () => { dsoundAudio.stopAll(); worker?.postMessage({ type: 'stop' }); });

const packageManager = new PackageManager(new URL('./catalog.json', import.meta.url).href);
const packageList = $('package-list');
const packageStatus = $('package-status');

async function showPackages() {
  packageList.replaceChildren();
  packageStatus.textContent = 'Loading package catalogs…';
  const [entries, installed] = await Promise.all([packageManager.refreshCatalogs(), packageManager.listInstalled()]);
  const installedByKey = new Map(installed.map((record) => [record.key, record]));
  if (packageManager.catalogErrors.length) {
    const error = document.createElement('p');
    error.className = 'bad';
    error.textContent = packageManager.catalogErrors.join('\n');
    packageList.append(error);
  }
  for (const entry of entries) {
    const card = document.createElement('article');
    card.className = 'package-card';
    const info = document.createElement('div');
    const title = document.createElement('h3');
    title.textContent = `${entry.name} · ${entry.version}`;
    const summary = document.createElement('p');
    summary.textContent = entry.summary || entry.id;
    info.append(title, summary);
    const actions = document.createElement('div');
    actions.className = 'package-actions';
    const record = installedByKey.get(`${entry.id}@${entry.version}`);
    if (record) {
      const launch = document.createElement('button');
      launch.type = 'button';
      launch.textContent = 'Launch';
      launch.addEventListener('click', () => {
        packageStatus.textContent = `Starting ${record.name}…`;
        void run(new Uint8Array(record.bundle), `${record.name} ${record.version}`, record.assetPackage?.url || null, record.assetPackage)
          .catch((error) => { packageStatus.textContent = `Launch failed: ${error.message || error}`; });
      });
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.textContent = 'Remove';
      remove.addEventListener('click', async () => {
        remove.disabled = true;
        try {
          await packageManager.remove(record.id, record.version);
          packageStatus.textContent = `Removed ${record.key}.`;
          await showPackages();
        } catch (error) {
          packageStatus.textContent = error.message || String(error);
          remove.disabled = false;
        }
      });
      actions.append(launch, remove);
    } else {
      const install = document.createElement('button');
      install.type = 'button';
      install.textContent = 'Install';
      install.addEventListener('click', async () => {
        install.disabled = true;
        packageStatus.textContent = `Resolving and downloading ${entry.id}@${entry.version}…`;
        try {
          await packageManager.install(entry.id, entry.version);
          packageStatus.textContent = `Installed ${entry.id}@${entry.version}.`;
          await showPackages();
        } catch (error) {
          packageStatus.textContent = `Install failed: ${error.message || error}`;
          install.disabled = false;
        }
      });
      actions.append(install);
    }
    card.append(info, actions);
    packageList.append(card);
  }
  if (!entries.length && !packageManager.catalogErrors.length) {
    const empty = document.createElement('p');
    empty.className = 'dim';
    empty.textContent = 'No packages are listed. Add a catalog URL to browse packages.';
    packageList.append(empty);
  }
  packageStatus.textContent = packageManager.catalogErrors.length
    ? `${entries.length} package version(s) loaded; some catalogs could not be read.`
    : `${entries.length} package version(s) available · ${installed.length} installed.`;
}

$('catalog-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const button = $('add-catalog');
  button.disabled = true;
  try {
    const catalog = await packageManager.addCatalog($('catalog-url').value);
    $('catalog-url').value = '';
    packageStatus.textContent = `Added ${catalog.name}.`;
    await showPackages();
  } catch (error) {
    packageStatus.textContent = `Could not add catalog: ${error.message || error}`;
  } finally {
    button.disabled = false;
  }
});
$('refresh-catalogs').addEventListener('click', () => { void showPackages(); });
window.addEventListener('pagehide', () => packageManager.close(), { once: true });
void showPackages().catch((error) => { packageStatus.textContent = `Package library unavailable: ${error.message || error}`; });

const auto = new URLSearchParams(location.search).get('run');
if (auto) demo(auto);
