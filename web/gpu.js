// WebGPU executor: replays the binary command stream produced by the runtime's Direct3D layers
// (see crates/anywasm-rt/src/hle/gpu.rs for the wire format and opcodes).
//
// Ordering model: D3D11 is an immediate-mode API, WebGPU records command buffers. Every upload
// goes through a staging ring and is recorded as a copy in the same encoder as the draws, so a
// draw sees exactly the data that was uploaded before it. Render passes are kept open across
// consecutive draws and closed whenever a copy, clear or attachment change needs the encoder.

const OP = {
  WINDOW: 1, CREATE_BUFFER: 2, BUFFER_WRITE: 3, CREATE_TEXTURE2D: 4, TEXTURE_WRITE: 5, CREATE_SHADER: 6,
  CREATE_INPUT_LAYOUT: 7, CREATE_SAMPLER: 8, CREATE_BLEND: 9, CREATE_RASTER: 10, CREATE_DEPTH: 11, CREATE_VIEW: 12,
  SET_VERTEX_BUFFERS: 13, SET_INDEX_BUFFER: 14, SET_INPUT_LAYOUT: 15, SET_TOPOLOGY: 16, SET_SHADER: 17,
  SET_CBUFFERS: 18, SET_SRVS: 19, SET_SAMPLERS: 20, SET_RENDER_TARGETS: 21, SET_VIEWPORTS: 22, SET_SCISSORS: 23,
  SET_BLEND: 24, SET_RASTER: 25, SET_DEPTH: 26, CLEAR_RTV: 27, CLEAR_DSV: 28, DRAW: 29, DRAW_INDEXED: 30,
  DRAW_INSTANCED: 31, DRAW_INDEXED_INSTANCED: 32, COPY_RESOURCE: 33, PRESENT: 35, RELEASE: 36, SWAPCHAIN: 38,
  GENERATE_MIPS: 39, COPY_REGION: 40, RESIZE_SWAPCHAIN: 41, DISPATCH: 42, SET_UAVS: 43, SET_CBUFFERS1: 44,
  SET_FIXED_FUNCTION: 45, CREATE_TEXTURE3D: 46, RESOLVE_TEXTURE: 47, D3D9_RENDER_STATE: 48,
};

// DXGI_FORMAT -> GPUTextureFormat (colour and depth). Approximations are noted.
const TEX = {
  1: 'rgba32float', 2: 'rgba32float', 3: 'rgba32uint', 4: 'rgba32sint',
  9: 'rgba16float', 10: 'rgba16float', 11: 'rgba16float', 12: 'rgba16uint', 13: 'rgba16float', 14: 'rgba16sint',
  15: 'rg32float', 16: 'rg32float', 17: 'rg32uint', 18: 'rg32sint',
  23: 'rgb10a2unorm', 24: 'rgb10a2unorm', 25: 'rgb10a2uint', 26: 'rg11b10ufloat',
  27: 'rgba8unorm', 28: 'rgba8unorm', 29: 'rgba8unorm-srgb', 30: 'rgba8uint', 31: 'rgba8snorm', 32: 'rgba8sint',
  33: 'rg16float', 34: 'rg16float', 35: 'rg16float', 36: 'rg16uint', 37: 'rg16float', 38: 'rg16sint',
  39: 'r32float', 40: 'depth32float', 41: 'r32float', 42: 'r32uint', 43: 'r32sint',
  44: 'depth24plus-stencil8', 45: 'depth24plus-stencil8', 46: 'depth24plus-stencil8', 47: 'depth24plus-stencil8',
  20: 'depth32float-stencil8', 19: 'depth32float-stencil8', 21: 'depth32float-stencil8', 22: 'depth32float-stencil8',
  48: 'rg8unorm', 49: 'rg8unorm', 50: 'rg8uint', 51: 'rg8snorm', 52: 'rg8sint',
  53: 'r16float', 54: 'r16float', 55: 'depth16unorm', 56: 'r16float', 57: 'r16uint', 58: 'r16float', 59: 'r16sint',
  60: 'r8unorm', 61: 'r8unorm', 62: 'r8uint', 63: 'r8snorm', 64: 'r8sint', 65: 'r8unorm',
  70: 'bc1-rgba-unorm', 71: 'bc1-rgba-unorm', 72: 'bc1-rgba-unorm-srgb',
  73: 'bc2-rgba-unorm', 74: 'bc2-rgba-unorm', 75: 'bc2-rgba-unorm-srgb',
  76: 'bc3-rgba-unorm', 77: 'bc3-rgba-unorm', 78: 'bc3-rgba-unorm-srgb',
  79: 'bc4-r-unorm', 80: 'bc4-r-unorm', 81: 'bc4-r-snorm', 82: 'bc5-rg-unorm', 83: 'bc5-rg-unorm', 84: 'bc5-rg-snorm',
  87: 'bgra8unorm', 88: 'bgra8unorm', 90: 'bgra8unorm', 91: 'bgra8unorm-srgb', 92: 'bgra8unorm', 93: 'bgra8unorm-srgb',
  94: 'bc6h-rgb-ufloat', 95: 'bc6h-rgb-ufloat', 96: 'bc6h-rgb-float',
  97: 'bc7-rgba-unorm', 98: 'bc7-rgba-unorm', 99: 'bc7-rgba-unorm-srgb',
};
const TYPED_UAV_DATA_TYPES = { 2: 5, 10: 5, 28: 5, 30: 4, 32: 3, 41: 5, 42: 4, 43: 3 };

// DXGI_FORMAT -> GPUVertexFormat
const VTX = {
  2: 'float32x4', 6: 'float32x3', 16: 'float32x2', 41: 'float32',
  3: 'uint32x4', 7: 'uint32x3', 17: 'uint32x2', 42: 'uint32', 4: 'sint32x4', 8: 'sint32x3', 18: 'sint32x2', 43: 'sint32',
  10: 'float16x4', 34: 'float16x2', 11: 'unorm16x4', 12: 'uint16x4', 13: 'snorm16x4', 14: 'sint16x4',
  35: 'unorm16x2', 36: 'uint16x2', 37: 'snorm16x2', 38: 'sint16x2',
  28: 'unorm8x4', 30: 'uint8x4', 31: 'snorm8x4', 32: 'sint8x4', 87: 'unorm8x4-bgra',
  49: 'unorm8x2', 50: 'uint8x2', 51: 'snorm8x2', 52: 'sint8x2', 24: 'unorm10-10-10-2',
  61: 'unorm8', 62: 'uint8', 63: 'snorm8', 64: 'sint8',
};

const SRGB_PAIR = {
  'rgba8unorm': 'rgba8unorm-srgb', 'bgra8unorm': 'bgra8unorm-srgb', 'bc1-rgba-unorm': 'bc1-rgba-unorm-srgb',
  'bc2-rgba-unorm': 'bc2-rgba-unorm-srgb', 'bc3-rgba-unorm': 'bc3-rgba-unorm-srgb', 'bc7-rgba-unorm': 'bc7-rgba-unorm-srgb',
};
const UNSRGB = Object.fromEntries(Object.entries(SRGB_PAIR).map(([a, b]) => [b, a]));

const BLOCK = (f) => (f.startsWith('bc') ? [4, 4, f.startsWith('bc1') || f.startsWith('bc4') ? 8 : 16] : null);
const BYTES = (f) => {
  const b = BLOCK(f);
  if (b) return b[2];
  if (/^rgba32|^rg32.*x/.test(f)) return 16;
  if (f.startsWith('rgba32')) return 16;
  if (f.startsWith('rg32') || f.startsWith('rgba16')) return 8;
  if (f.startsWith('depth24') || f.startsWith('depth32float') && !f.includes('stencil')) return 4;
  if (f === 'depth32float-stencil8') return 8;
  if (f.startsWith('rg16') || f.startsWith('r32') || f.includes('8unorm') && f.startsWith('rgba') || f.startsWith('rgba8') || f.startsWith('bgra8') || f.startsWith('rgb10') || f.startsWith('rg11')) return 4;
  if (f.startsWith('rg8') || f.startsWith('r16') || f === 'depth16unorm') return 2;
  return 1;
};
const isDepth = (f) => f.startsWith('depth');
const hasStencil = (f) => f.includes('stencil');

const CMP = { 1: 'never', 2: 'less', 3: 'equal', 4: 'less-equal', 5: 'greater', 6: 'not-equal', 7: 'greater-equal', 8: 'always' };
const STENCIL_OP = { 1: 'keep', 2: 'zero', 3: 'replace', 4: 'increment-clamp', 5: 'decrement-clamp', 6: 'invert', 7: 'increment-wrap', 8: 'decrement-wrap' };
const BLEND_FACTOR = { 1: 'zero', 2: 'one', 3: 'src', 4: 'one-minus-src', 5: 'src-alpha', 6: 'one-minus-src-alpha', 7: 'dst-alpha', 8: 'one-minus-dst-alpha', 9: 'dst', 10: 'one-minus-dst', 11: 'src-alpha-saturated', 14: 'constant', 15: 'one-minus-constant' };
const BLEND_OP = { 1: 'add', 2: 'subtract', 3: 'reverse-subtract', 4: 'min', 5: 'max' };
const ADDRESS = { 1: 'repeat', 2: 'mirror-repeat', 3: 'clamp-to-edge', 4: 'clamp-to-edge', 5: 'mirror-repeat' };
const TOPOLOGY = { 1: 'point-list', 2: 'line-list', 3: 'line-strip', 4: 'triangle-list', 5: 'triangle-strip' };

const BLIT_WGSL = `
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
  var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
  var o: VOut;
  o.pos = vec4(p[i], 0.0, 1.0);
  o.uv = vec2((p[i].x + 1.0) * 0.5, (1.0 - p[i].y) * 0.5);
  return o;
}
@fragment fn fs(v: VOut) -> @location(0) vec4<f32> { return textureSampleLevel(src, smp, v.uv, 0.0); }
`;

export class GpuExecutor {
  /**
   * @param {OffscreenCanvas|HTMLCanvasElement} canvas
   * @param {import('./shaders.js').ShaderTool} tool
   */
  constructor(canvas, tool, log = () => {}, { noPresent = false } = {}) {
    this.noPresent = noPresent; // skip the canvas blit (headless test mode; see docs/STATUS.md)
    this.canvas = canvas;
    this.tool = tool;
    this.log = log;
    this.reset();
  }

  reset() {
    this.res = new Map();      // id -> {kind:'buffer'|'texture', gpu, ...}
    this.views = new Map();    // id -> view description
    this.shaders = new Map();
    this.layouts = new Map();  // input layouts
    this.samplers = new Map();
    this.fixedFunctionShaders = new Map();
    this.fixedFunctionPixelVariants = new Map();
    this.fixedFunctionNextId = 0xfffffff0;
    this.blends = new Map();
    this.rasters = new Map();
    this.depths = new Map();
    this.swapchains = new Map();
    this.pipelines = new Map();
    this.computePipelines = new Map();
    this.bglCache = new Map();
    this.emptyBindGroupCache = new WeakMap();
    this.dummyBuffers = new Map();
    this.transientBuffers = [];
    this.instanceExpansionPipeline = null;
    this.enc = null;
    this.pass = null;
    this.passKey = '';
    this.ring = null;
    this.ringOff = 0;
    this.stats = { draws: 0, dispatches: 0, presents: 0, passes: 0, pipelines: 0, lastComputeUavs: [], errors: [] };
    this.st = {
      vb: [], ib: { buf: 0, format: 57, offset: 0 }, layout: 0, topology: 4, shader: [0, 0, 0],
      cb: [[], [], []], cbRange: [[], [], []], srv: [[], [], []], smp: [[], [], []], uav: [], rtvs: [], dsv: 0,
      viewports: [], scissors: [], blend: { id: 0, factor: [1, 1, 1, 1], mask: 0xffffffff }, raster: 0, depth: { id: 0, ref: 0 },
      d3d9RenderStates: new Map(),
    };
  }

  async init() {
    if (!navigator.gpu) throw new Error('WebGPU is not available in this browser');
    // Keep the GPU instance alive: in workers Chromium drops the underlying instance when this
    // object is collected, which breaks every later mapAsync/popErrorScope on the device.
    this.gpuInstance = navigator.gpu;
    const adapter = await this.gpuInstance.requestAdapter({ powerPreference: 'high-performance' });
    if (!adapter) throw new Error('No WebGPU adapter');
    this.adapter = adapter; // keep alive: the device's instance reference is tied to the adapter wrapper
    const want = ['texture-compression-bc', 'depth32float-stencil8', 'float32-filterable', 'rg11b10ufloat-renderable', 'bgra8unorm-storage', 'indirect-first-instance', 'depth-clip-control', 'float32-blendable', 'dual-source-blending', 'clip-distances'];
    const features = want.filter((f) => adapter.features.has(f));
    const L = adapter.limits;
    this.device = await adapter.requestDevice({
      requiredFeatures: features,
      requiredLimits: { maxBufferSize: L.maxBufferSize, maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxUniformBufferBindingSize: L.maxUniformBufferBindingSize, maxColorAttachmentBytesPerSample: L.maxColorAttachmentBytesPerSample, maxVertexBuffers: L.maxVertexBuffers, maxVertexAttributes: L.maxVertexAttributes },
    });
    this.features = this.device.features;
    this.device.addEventListener?.('uncapturederror', (e) => {
      const m = e.error?.message || String(e.error);
      this.stats.errors.push(m);
      this.log('WebGPU error: ' + m);
    });
    this.adapterInfo = adapter.info || {};
    this.canvasFormat = this.gpuInstance.getPreferredCanvasFormat();
    this.ctx = this.canvas.getContext('webgpu');
    this.ctx.configure({ device: this.device, format: this.canvasFormat, alphaMode: 'opaque', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
    const mod = this.device.createShaderModule({ code: BLIT_WGSL });
    this.blitBGL = this.device.createBindGroupLayout({ entries: [
      { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: 'float' } },
      { binding: 1, visibility: GPUShaderStage.FRAGMENT, sampler: { type: 'filtering' } },
    ] });
    this.blit = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.blitBGL] }),
      vertex: { module: mod, entryPoint: 'vs' }, fragment: { module: mod, entryPoint: 'fs', targets: [{ format: this.canvasFormat }] }, primitive: { topology: 'triangle-list' },
    });
    this.blitSampler = this.device.createSampler({ magFilter: 'linear', minFilter: 'linear' });
    this.defaultSampler = this.device.createSampler({});
    this.dummyTex = this.device.createTexture({ size: [1, 1], format: 'rgba8unorm', usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
    this.dummyView = this.dummyTex.createView();
  }

  // ── encoder / pass management ────────────────────────────────────────────
  encoder() {
    if (!this.enc) this.enc = this.device.createCommandEncoder();
    return this.enc;
  }
  endPass() {
    if (this.pass) { this.pass.end(); this.pass = null; this.passKey = ''; }
  }
  flush() {
    this.endPass();
    if (this.enc) {
      this.device.queue.submit([this.enc.finish()]);
      this.enc = null;
    }
    for (const buffer of this.transientBuffers) buffer.destroy();
    this.transientBuffers = [];
  }

  /** Copy `bytes` into the staging ring; returns {buffer, offset}. */
  stage(bytes) {
    const need = (bytes.byteLength + 255) & ~255;
    if (!this.ring || this.ringOff + need > this.ringSize) {
      const size = Math.max(16 << 20, need);
      this.ring = this.device.createBuffer({ size, usage: GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST | GPUBufferUsage.UNIFORM });
      this.ringSize = size;
      this.ringOff = 0;
    }
    const off = this.ringOff;
    this.ringOff += need;
    const pad = (bytes.byteLength + 3) & ~3;
    if (pad === bytes.byteLength) this.device.queue.writeBuffer(this.ring, off, bytes);
    else { const t = new Uint8Array(pad); t.set(bytes); this.device.queue.writeBuffer(this.ring, off, t); }
    return { buffer: this.ring, offset: off, size: pad };
  }

  // ── formats ──────────────────────────────────────────────────────────────
  texFormat(dxgi, bind = 0) {
    if ((dxgi === 39) && (bind & 0x40)) return 'depth32float';
    if ((dxgi === 44) && (bind & 0x40)) return 'depth24plus-stencil8';
    if (dxgi === 53 && (bind & 0x40)) return 'depth16unorm';
    let f = TEX[dxgi];
    if (!f) { this.log(`unsupported DXGI format ${dxgi}, using rgba8unorm`); f = 'rgba8unorm'; }
    if (f.startsWith('bc') && !this.features.has('texture-compression-bc')) { this.log('BC texture requested but unsupported; substituting rgba8unorm'); return 'rgba8unorm'; }
    if (f === 'depth32float-stencil8' && !this.features.has('depth32float-stencil8')) return 'depth24plus-stencil8';
    return f;
  }

  // ── command dispatch ─────────────────────────────────────────────────────
  /** Execute a command stream (Uint8Array). */
  execute(buf) {
    let p = 0;
    const dvAll = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    while (p + 8 <= buf.length) {
      const op = dvAll.getUint32(p, true), len = dvAll.getUint32(p + 4, true);
      const dv = new DataView(buf.buffer, buf.byteOffset + p + 8, len);
      const bytes = buf.subarray(p + 8, p + 8 + len);
      try { this.dispatch(op, dv, bytes); } catch (e) { this.stats.errors.push(`op ${op}: ${e.message}`); this.log(`op ${op} failed: ${e.stack || e.message}`); }
      p += 8 + len;
    }
    this.flush();
  }

  dispatch(op, dv, bytes) {
    const u = (i) => dv.getUint32(i * 4, true);
    const i32 = (i) => dv.getInt32(i * 4, true);
    const f = (i) => dv.getFloat32(i * 4, true);
    const st = this.st;
    switch (op) {
      case OP.WINDOW: {
        const w = u(1), h = u(2);
        if (!this.windowSize || this.windowSize[0] !== w || this.windowSize[1] !== h) {
          this.windowSize = [w, h];
          this.onWindow?.({ id: u(0), width: w, height: h, title: bytes.length > 12 ? new TextDecoder().decode(bytes.subarray(12)) : undefined });
        }
        break;
      }
      case OP.CREATE_BUFFER: {
        const size = Math.max(16, (u(1) + 15) & ~15);
        const gpu = this.device.createBuffer({ size, usage: GPUBufferUsage.VERTEX | GPUBufferUsage.INDEX | GPUBufferUsage.UNIFORM | GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
        this.res.set(u(0), { kind: 'buffer', gpu, size, d3dSize: u(1), bind: u(2), misc: u(4), stride: u(5), shadow: new Uint8Array(size) });
        break;
      }
      case OP.BUFFER_WRITE: {
        const r = this.res.get(u(0));
        if (!r) break;
        const off = u(1), n = u(2);
        const data = bytes.subarray(12, 12 + n);
        const aligned = off & ~3;
        let payload = data;
        if (aligned !== off) { const t = new Uint8Array(n + (off - aligned)); t.set(data, off - aligned); payload = t; }
        if (r.shadow && off + data.length <= r.shadow.length) r.shadow.set(data, off);
        this.endPass();
        const s = this.stage(payload);
        this.encoder().copyBufferToBuffer(s.buffer, s.offset, r.gpu, aligned, Math.min(s.size, r.size - aligned));
        break;
      }
      case OP.CREATE_TEXTURE2D: this.createTexture(u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7), u(9), '2d', u(8)); break;
      case OP.CREATE_TEXTURE3D: this.createTexture(u(0), u(1), u(2), u(4), u(3), u(5), u(6), u(7), u(8), '3d'); break;
      case OP.TEXTURE_WRITE: this.textureWrite(u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7), u(8), bytes.subarray(36, 36 + u(8))); break;
      case OP.CREATE_SHADER: this.createShader(u(0), u(1), bytes.slice(12, 12 + u(2))); break;
      case OP.CREATE_INPUT_LAYOUT: {
        const n = u(1), els = [];
        for (let i = 0; i < n; i++) els.push({ location: u(2 + i * 6), format: u(3 + i * 6), slot: u(4 + i * 6), offset: u(5 + i * 6), cls: u(6 + i * 6), step: u(7 + i * 6) });
        this.layouts.set(u(0), els);
        break;
      }
      case OP.CREATE_SAMPLER: this.samplers.set(u(0), this.makeSampler(new DataView(bytes.buffer, bytes.byteOffset + 4, bytes.length - 4))); break;
      case OP.CREATE_BLEND: this.blends.set(u(0), new DataView(bytes.slice(4).buffer)); break;
      case OP.CREATE_RASTER: this.rasters.set(u(0), new DataView(bytes.slice(4).buffer)); break;
      case OP.CREATE_DEPTH: this.depths.set(u(0), new DataView(bytes.slice(4).buffer)); break;
      case OP.CREATE_VIEW: this.views.set(u(0), { kind: u(1), resource: u(2), format: u(3), dim: u(4), mip: u(5), mips: u(6), slice: u(7), slices: u(8), cache: null }); break;
      case OP.SET_VERTEX_BUFFERS: { const s = u(0), n = u(1); for (let i = 0; i < n; i++) st.vb[s + i] = { buf: u(2 + i * 3), stride: u(3 + i * 3), offset: u(4 + i * 3) }; break; }
      case OP.SET_INDEX_BUFFER: st.ib = { buf: u(0), format: u(1), offset: u(2) }; break;
      case OP.SET_INPUT_LAYOUT: st.layout = u(0); break;
      case OP.SET_TOPOLOGY: st.topology = u(0); break;
      case OP.D3D9_RENDER_STATE: st.d3d9RenderStates.set(u(0), u(1)); break;
      case OP.SET_FIXED_FUNCTION:
        this.setFixedFunction(
          u(0), u(1), u(2),
          dv.byteLength >= 16 ? u(3) : 4,
          dv.byteLength >= 20 ? u(4) : 2,
          dv.byteLength >= 24 ? u(5) : 1,
          dv.byteLength >= 28 ? u(6) : 2,
          dv.byteLength >= 32 ? u(7) : 2,
          dv.byteLength >= 36 ? u(8) : 1,
        );
        break;
      case OP.SET_SHADER: st.shader[u(0)] = u(1); break;
      case OP.SET_CBUFFERS: case OP.SET_SRVS: case OP.SET_SAMPLERS: {
        const arr = (op === OP.SET_CBUFFERS ? st.cb : op === OP.SET_SRVS ? st.srv : st.smp)[u(0)];
        const s = u(1), n = u(2);
        for (let i = 0; i < n; i++) {
          arr[s + i] = u(3 + i);
          if (op === OP.SET_CBUFFERS) st.cbRange[u(0)][s + i] = { first: 0, count: 0 };
        }
        break;
      }
      case OP.SET_CBUFFERS1: {
        const stage = u(0), s = u(1), n = u(2);
        for (let i = 0; i < n; i++) {
          const at = 3 + i * 3;
          st.cb[stage][s + i] = u(at);
          st.cbRange[stage][s + i] = { first: u(at + 1), count: u(at + 2) };
        }
        break;
      }
      case OP.SET_UAVS: {
        const s = u(0), n = u(1);
        for (let i = 0; i < n; i++) st.uav[s + i] = { id: u(2 + i * 2), initialCount: u(3 + i * 2) };
        break;
      }
      case OP.SET_RENDER_TARGETS: { const n = u(0); st.rtvs = []; for (let i = 0; i < n; i++) st.rtvs.push(u(1 + i)); st.dsv = u(1 + n); break; }
      case OP.SET_VIEWPORTS: { const n = u(0); st.viewports = []; for (let i = 0; i < n; i++) st.viewports.push([f(1 + i * 6), f(2 + i * 6), f(3 + i * 6), f(4 + i * 6), f(5 + i * 6), f(6 + i * 6)]); break; }
      case OP.SET_SCISSORS: { const n = u(0); st.scissors = []; for (let i = 0; i < n; i++) st.scissors.push([i32(1 + i * 4), i32(2 + i * 4), i32(3 + i * 4), i32(4 + i * 4)]); break; }
      case OP.SET_BLEND: st.blend = { id: u(0), factor: [f(1), f(2), f(3), f(4)], mask: u(5) }; break;
      case OP.SET_RASTER: st.raster = u(0); break;
      case OP.SET_DEPTH: st.depth = { id: u(0), ref: u(1) }; break;
      case OP.CLEAR_RTV: this.clearRtv(u(0), [f(1), f(2), f(3), f(4)]); break;
      case OP.CLEAR_DSV: this.clearDsv(u(0), u(1), f(2), u(3)); break;
      case OP.DRAW: this.draw(false, u(0), 1, u(1), 0, 0, 0); break;
      case OP.DRAW_INDEXED: this.draw(true, u(0), 1, u(1), i32(2), 0, 0); break;
      case OP.DRAW_INSTANCED: this.draw(false, u(0), u(1), u(2), 0, u(3), 0); break;
      case OP.DRAW_INDEXED_INSTANCED: this.draw(true, u(0), u(1), u(2), i32(3), u(4), 0); break;
      case OP.COPY_RESOURCE: this.copyResource(u(0), u(1)); break;
      case OP.COPY_REGION: this.copyRegion(dv); break;
      case OP.RESOLVE_TEXTURE: this.resolveTexture(u(0), u(1), u(2), u(3), u(4)); break;
      case OP.SWAPCHAIN: this.createSwapchain(u(0), u(2), u(3), u(4), u(5)); break;
      case OP.RESIZE_SWAPCHAIN: this.resizeSwapchain(u(0), u(1), u(2), u(3), u(4)); break;
      case OP.PRESENT: this.present(u(0), u(1)); break;
      case OP.GENERATE_MIPS: this.generateMips(u(0)); break;
      case OP.RELEASE: this.release(u(0)); break;
      case OP.DISPATCH: this.dispatchCompute(u(0), u(1), u(2)); break;
      default: this.log(`unknown opcode ${op}`);
    }
  }

  release(id) {
    const r = this.res.get(id);
    if (r) { this.flush(); r.gpu.destroy?.(); this.res.delete(id); }
    this.views.delete(id); this.shaders.delete(id); this.layouts.delete(id); this.samplers.delete(id);
    this.blends.delete(id); this.rasters.delete(id); this.depths.delete(id);
  }

  // ── resources ────────────────────────────────────────────────────────────
  createTexture(id, w, h, mips, array, dxgi, bind, usage, misc, dimension = '2d', sampleCount = 1) {
    let format = this.texFormat(dxgi, bind);
    const viewFormats = [];
    if (SRGB_PAIR[format] && !format.startsWith('bc')) viewFormats.push(SRGB_PAIR[format]);
    else if (UNSRGB[format] && !format.startsWith('bc')) viewFormats.push(UNSRGB[format]);
    sampleCount = Math.max(1, sampleCount);
    const multisampled = sampleCount > 1;
    let u = multisampled ? 0 : GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_SRC | GPUTextureUsage.COPY_DST;
    if (bind & (0x20 | 0x40)) u |= GPUTextureUsage.RENDER_ATTACHMENT;
    if (bind & 0x80) u |= GPUTextureUsage.STORAGE_BINDING;
    if (dimension === '3d' && (format === 'rgba8unorm' || format === 'r32float')) u |= GPUTextureUsage.STORAGE_BINDING;
    const cube = dimension === '2d' && (misc & 0x4) !== 0;
    const gpu = this.device.createTexture({ size: [w, h, Math.max(1, array)], dimension, mipLevelCount: Math.max(1, mips), sampleCount, format, usage: u, viewFormats });
    this.res.set(id, {
      kind: 'texture', gpu, width: w, height: h, depth: dimension === '3d' ? Math.max(1, array) : 1,
      mips: Math.max(1, mips), array: dimension === '3d' ? 1 : Math.max(1, array), samples: sampleCount, format, dxgi, bind, cube, dimension,
    });
  }

  resolveTexture(dstId, dstSub, srcId, srcSub, dxgiFormat) {
    const dst = this.res.get(dstId), src = this.res.get(srcId);
    if (!dst || !src || dst.kind !== 'texture' || src.kind !== 'texture'
      || src.samples <= 1 || dst.samples !== 1 || src.dimension !== '2d' || dst.dimension !== '2d'
      || src.format !== dst.format || (dxgiFormat && dxgiFormat !== src.dxgi) || isDepth(src.format)) {
      this.log(`ResolveSubresource skipped: source ${srcId} and destination ${dstId} are incompatible`);
      return;
    }
    const srcMip = srcSub % src.mips, srcSlice = Math.floor(srcSub / src.mips);
    const dstMip = dstSub % dst.mips, dstSlice = Math.floor(dstSub / dst.mips);
    if (src.width >> srcMip !== dst.width >> dstMip || src.height >> srcMip !== dst.height >> dstMip
      || srcSlice >= src.array || dstSlice >= dst.array) {
      this.log(`ResolveSubresource skipped: source ${srcId} and destination ${dstId} extents differ`);
      return;
    }
    this.endPass();
    this.device.pushErrorScope('validation');
    try {
      const srcView = src.gpu.createView({ dimension: '2d', baseMipLevel: srcMip, mipLevelCount: 1, baseArrayLayer: srcSlice, arrayLayerCount: 1 });
      const dstView = dst.gpu.createView({ dimension: '2d', baseMipLevel: dstMip, mipLevelCount: 1, baseArrayLayer: dstSlice, arrayLayerCount: 1 });
      const pass = this.encoder().beginRenderPass({ colorAttachments: [{
        view: srcView, resolveTarget: dstView, loadOp: 'load', storeOp: 'discard',
      }] });
      pass.end();
    } catch (error) {
      this.stats.errors.push(`ResolveSubresource: ${error.message}`);
      this.log(`ResolveSubresource: ${error.message}`);
    }
    this.device.popErrorScope().then((error) => {
      if (error) {
        this.stats.errors.push(`ResolveSubresource: ${error.message}`);
        this.log(`ResolveSubresource: ${error.message}`);
      }
    });
  }

  textureWrite(id, mip, slice, x, y, w, h, rowPitch, len, data) {
    const r = this.res.get(id);
    if (!r) return;
    const blk = BLOCK(r.format) || [1, 1, BYTES(r.format)];
    const rows = Math.ceil(h / blk[1]);
    const rowBytes = Math.ceil(w / blk[0]) * blk[2];
    const bpr = (rowBytes + 255) & ~255;
    const packed = new Uint8Array(bpr * rows);
    for (let row = 0; row < rows; row++) packed.set(data.subarray(row * rowPitch, row * rowPitch + rowBytes), row * bpr);
    this.endPass();
    const s = this.stage(packed);
    const cw = Math.ceil(w / blk[0]) * blk[0], ch = Math.ceil(h / blk[1]) * blk[1];
    this.encoder().copyBufferToTexture({ buffer: s.buffer, offset: s.offset, bytesPerRow: bpr, rowsPerImage: rows }, { texture: r.gpu, mipLevel: mip, origin: { x, y, z: slice } }, { width: cw, height: ch, depthOrArrayLayers: 1 });
  }

  makeSampler(dv) {
    const filter = dv.getUint32(0, true), au = dv.getUint32(4, true), av = dv.getUint32(8, true), aw = dv.getUint32(12, true);
    const bias = dv.getFloat32(16, true), aniso = dv.getUint32(20, true), cmp = dv.getUint32(24, true);
    const minLod = dv.getFloat32(44, true), maxLod = dv.getFloat32(48, true);
    const isAniso = (filter & 0x40) !== 0, comparison = ((filter >> 7) & 3) === 1;
    const lin = (bit) => ((filter >> bit) & 1 ? 'linear' : 'nearest');
    const d = {
      minFilter: lin(4), magFilter: lin(2), mipmapFilter: lin(0),
      addressModeU: ADDRESS[au] || 'repeat', addressModeV: ADDRESS[av] || 'repeat', addressModeW: ADDRESS[aw] || 'repeat',
      lodMinClamp: Math.max(0, minLod), lodMaxClamp: Math.min(32, Math.max(maxLod, minLod)),
    };
    if (isAniso) { d.minFilter = d.magFilter = d.mipmapFilter = 'linear'; d.maxAnisotropy = Math.max(1, Math.min(16, aniso)); }
    if (comparison) d.compare = CMP[cmp] || 'always';
    void bias;
    return { gpu: this.device.createSampler(d), comparison, maxAnisotropy: d.maxAnisotropy || 1 };
  }

  shaderInfo(dxbc, uavFormat) {
    const translated = this.tool.translate(dxbc, uavFormat === undefined ? undefined : { uavFormat });
    const { wgsl, descriptors } = translated;
    const entry = (/@(vertex|fragment|compute)(?:\s+@\w+(?:\([^)]*\))?)*\s*fn\s+(\w+)/.exec(wgsl) || [])[2] || 'main';
    const usesDrawParameters = /@group\(3\)\s*@binding\(0\)\s*var<uniform>/.test(wgsl);
    const cbSizes = {};
    for (const m of wgsl.matchAll(/@group\(0\)\s*@binding\((\d+)\)\s*var<uniform>\s+\w+:\s*(\w+);/g)) {
      const s = new RegExp(`struct\\s+${m[2]}\\s*\\{\\s*\\w+:\\s*array<vec4<\\w+>,\\s*(\\d+)>`).exec(wgsl);
      if (s) cbSizes[+m[1]] = +s[1] * 16;
    }
    const storageTextures = {};
    for (const m of wgsl.matchAll(/@group\(3\)\s*@binding\((\d+)\)\s*var\s+\w+\s*:\s*texture_storage_(1d|2d|2d_array|3d)<\s*(\w+)\s*,\s*(write|read|read_write)\s*>/g)) {
      storageTextures[+m[1]] = {
        dimension: m[2] === '2d_array' ? '2d-array' : m[2],
        format: m[3],
        access: ({ write: 'write-only', read: 'read-only', read_write: 'read-write' })[m[4]],
      };
    }
    const storageBuffers = { 1: {}, 3: {} };
    for (const m of wgsl.matchAll(/@group\((1|3)\)\s*@binding\((\d+)\)\s*var<storage,\s*(read|read_write)>\s+\w+\s*:/g)) {
      storageBuffers[+m[1]][+m[2]] = { access: m[3] };
    }
    const module = this.device.createShaderModule({ code: wgsl });
    return { module, entry, descriptors, cbSizes, storageTextures, storageBuffers, usesDrawParameters, wgsl };
  }

  createShader(id, stage, dxbc) {
    this.shaders.set(id, { ...this.shaderInfo(dxbc), stage, dxbc, variants: new Map() });
  }

  shaderVariant(shader, dxgiFormat) {
    const defaultFormat = { 3: 43, 4: 42, 5: 41 }[shader.descriptors.find((d) => d.type === 1)?.dataType];
    if (dxgiFormat === defaultFormat) return shader;
    let variant = shader.variants.get(dxgiFormat);
    if (!variant) {
      variant = this.shaderInfo(shader.dxbc, dxgiFormat);
      shader.variants.set(dxgiFormat, variant);
    }
    return { ...shader, ...variant };
  }

  // ── views ────────────────────────────────────────────────────────────────
  setFixedFunction(fvf, width, height, colorOp = 4, colorArg1 = 2, colorArg2 = 1, alphaOp = 2, alphaArg1 = 2, alphaArg2 = 1) {
    if (fvf === 0) {
      this.st.shader[0] = 0;
      this.st.shader[1] = 0;
      this.st.layout = 0;
      return;
    }
    if (![0x42, 0x44, 0x142, 0x144, 0x152].includes(fvf)) {
      this.st.shader[0] = 0;
      this.st.shader[1] = 0;
      this.st.layout = 0;
      this.log(`unsupported D3D9 FVF 0x${fvf.toString(16)}`);
      return;
    }
    const key = `${fvf}/${width}/${height}/${colorOp}/${colorArg1}/${colorArg2}/${alphaOp}/${alphaArg1}/${alphaArg2}`;
    let fixed = this.fixedFunctionShaders.get(key);
    if (!fixed) {
      const transformed = fvf === 0x44 || fvf === 0x144;
      const normal = fvf === 0x152;
      const textured = fvf === 0x142 || fvf === 0x144 || normal;
      const positionFormat = transformed ? 2 : 6;
      const positionSize = transformed ? 16 : 12;
      const normalInput = normal ? '@location(1) normal: vec3<f32>,' : '';
      const colorLocation = normal ? 2 : 1;
      const texcoordLocation = normal ? 3 : 2;
      const colorOffset = positionSize + (normal ? 12 : 0);
      const inputPosition = transformed ? 'vec4<f32>' : 'vec3<f32>';
      const texcoordInput = textured ? `@location(${texcoordLocation}) uv: vec2<f32>,` : '';
      const texcoordOutput = textured ? '@location(2) uv: vec2<f32>,' : '';
      const texcoordAssign = textured ? 'output.uv = input.uv;' : '';
      const positionCode = transformed
        ? `let invW = 1.0 / max(input.position.w, 0.000001);
           output.position = vec4<f32>(
             (input.position.x * (2.0 / ${Math.max(width, 1)}.0) - 1.0) * invW,
             (1.0 - input.position.y * (2.0 / ${Math.max(height, 1)}.0)) * invW,
             input.position.z * invW, invW);`
        : 'output.position = vec4<f32>(input.position, 1.0);';
      const code = `
        struct VertexInput {
          @location(0) position: ${inputPosition},
          ${normalInput}
          @location(${colorLocation}) color: vec4<f32>,
          ${texcoordInput}
        };
        struct VertexOutput {
          @builtin(position) position: vec4<f32>,
          @location(0) color: vec4<f32>,
          ${texcoordOutput}
        };
        @vertex fn vs(input: VertexInput) -> VertexOutput {
          var output: VertexOutput;
          ${positionCode}
          output.color = input.color.bgra;
          ${texcoordAssign}
          return output;
        }
        @fragment fn fs(input: VertexOutput) -> @location(0) vec4<f32> {
          return input.color;
        }
      `;
      const module = this.device.createShaderModule({ code });
      const channelArg = (argument, channel) => argument === 2 ? `texel.${channel}` : `input.color.${channel}`;
      const colorA = channelArg(colorArg1, 'rgb'), colorB = channelArg(colorArg2, 'rgb');
      const alphaA = channelArg(alphaArg1, 'a'), alphaB = channelArg(alphaArg2, 'a');
      const colorExpr = colorOp === 1 ? 'input.color.rgb'
        : colorOp === 2 ? colorA
        : colorOp === 3 ? colorB
        : colorOp === 4 ? `(${colorA} * ${colorB})`
        : colorOp === 5 ? `((${colorA} * ${colorB}) * 2.0)`
        : colorOp === 6 ? `((${colorA} * ${colorB}) * 4.0)`
        : colorOp === 7 ? `(${colorA} + ${colorB})`
        : colorOp === 10 ? `(${colorA} - ${colorB})`
        : 'input.color.rgb';
      const alphaExpr = alphaOp === 2 ? alphaA
        : alphaOp === 3 ? alphaB
        : alphaOp === 4 ? `(${alphaA} * ${alphaB})`
        : 'input.color.a';
      const texturedCode = textured ? `${code}
        @group(1) @binding(0) var d3d9Texture: texture_2d<f32>;
        @group(2) @binding(0) var d3d9Sampler: sampler;
        @fragment fn fsTextured(input: VertexOutput) -> @location(0) vec4<f32> {
          let texel = textureSample(d3d9Texture, d3d9Sampler, input.uv);
          return vec4<f32>(${colorExpr}, ${alphaExpr});
        }
      ` : null;
      const texturedModule = texturedCode ? this.device.createShaderModule({ code: texturedCode }) : null;
      const texturedPsId = textured ? this.fixedFunctionNextId-- : 0;
      fixed = {
        vsId: this.fixedFunctionNextId--,
        psId: this.fixedFunctionNextId--,
        texturedPsId,
        layoutId: this.fixedFunctionNextId--,
        layout: [
          { location: 0, format: positionFormat, slot: 0, offset: 0, cls: 0, step: 0 },
          ...(normal ? [{ location: 1, format: 6, slot: 0, offset: positionSize, cls: 0, step: 0 }] : []),
          { location: colorLocation, format: 28, slot: 0, offset: colorOffset, cls: 0, step: 0 },
          ...(textured ? [{ location: texcoordLocation, format: 16, slot: 0, offset: colorOffset + 4, cls: 0, step: 0 }] : []),
        ],
        vs: { stage: 0, module, entry: 'vs', descriptors: [], cbSizes: {}, usesDrawParameters: false },
        ps: {
          stage: 1, module, entry: 'fs', descriptors: [], cbSizes: {}, usesDrawParameters: false,
          fixedFunction: { source: code, output: 'input.color' },
          texturedVariant: texturedPsId || undefined,
        },
        texturedPs: texturedModule ? {
          stage: 1,
          module: texturedModule,
          entry: 'fsTextured',
          descriptors: [{ type: 0, register: 0, resourceType: 4 }, { type: 3, register: 0 }],
          cbSizes: {},
          usesDrawParameters: false,
          fixedFunction: { source: texturedCode, output: `vec4<f32>(${colorExpr}, ${alphaExpr})` },
        } : null,
      };
      this.fixedFunctionShaders.set(key, fixed);
    }
    this.shaders.set(fixed.vsId, fixed.vs);
    this.shaders.set(fixed.psId, fixed.ps);
    if (fixed.texturedPs) this.shaders.set(fixed.texturedPsId, fixed.texturedPs);
    this.layouts.set(fixed.layoutId, fixed.layout);
    this.st.shader[0] = fixed.vsId;
    this.st.shader[1] = fixed.psId;
    this.st.layout = fixed.layoutId;
  }

  fixedFunctionAlphaTest(ps, psId, alphaFunc, alphaRef) {
    if (!ps?.fixedFunction) return ps;
    const compare = {
      1: 'false', 2: 'outputColor.a < REFERENCE', 3: 'outputColor.a == REFERENCE',
      4: 'outputColor.a <= REFERENCE', 5: 'outputColor.a > REFERENCE',
      6: 'outputColor.a != REFERENCE', 7: 'outputColor.a >= REFERENCE', 8: 'true',
    }[alphaFunc] || 'true';
    const referenceValue = (alphaRef & 0xff) / 255;
    const reference = referenceValue.toFixed(9);
    const key = `${psId}/${alphaFunc}/${alphaRef & 0xff}`;
    let variant = this.fixedFunctionPixelVariants.get(key);
    if (variant) return variant;
    const statement = `return ${ps.fixedFunction.output};`;
    if (!ps.fixedFunction.source.includes(statement)) {
      this.log('D3D9 alpha test skipped: fixed-function pixel shader output was not found');
      return ps;
    }
    const expression = compare.replaceAll('REFERENCE', reference);
    const replacement = `let outputColor = ${ps.fixedFunction.output};\n          if (!(${expression})) { discard; }\n          return outputColor;`;
    const source = ps.fixedFunction.source.replace(statement, replacement);
    variant = { ...ps, module: this.device.createShaderModule({ code: source }) };
    this.fixedFunctionPixelVariants.set(key, variant);
    return variant;
  }

  viewFormat(dxgi, tex) {
    const f = dxgi ? this.texFormat(dxgi, tex.bind) : tex.format;
    return f === tex.format || f === SRGB_PAIR[tex.format] || f === UNSRGB[tex.format] ? f : tex.format;
  }

  getView(id) {
    const v = this.views.get(id);
    if (!v) return null;
    if (v.cache) return v.cache;
    const tex = this.res.get(v.resource);
    if (!tex) return null;
    if (tex.kind === 'buffer') {
      const raw = (v.slice & 1) !== 0 || (tex.misc & 0x20) !== 0;
      const structured = (tex.misc & 0x40) !== 0;
      const stride = raw ? 4 : structured ? tex.stride : v.format ? BYTES(this.texFormat(v.format, tex.bind)) : 0;
      const offset = v.mip * stride;
      const size = Math.min(v.mips * stride, tex.d3dSize - offset);
      if (!stride || offset < 0 || offset % 4 !== 0 || size <= 0 || size % 4 !== 0) return null;
      v.cache = { buffer: tex.gpu, bufferResource: tex, offset, size, dim: 'buffer', format: v.format };
      return v.cache;
    }
    if (tex.kind !== 'texture') return null;
    const format = this.viewFormat(v.format, tex);
    const d = { format };
    if (isDepth(format)) d.aspect = hasStencil(format) && v.kind === 2 ? 'depth-only' : 'all';
    if (v.kind === 0 || v.kind === 1) { // render attachments: one mip, one layer, '2d'
      d.dimension = '2d'; d.baseMipLevel = v.mip; d.mipLevelCount = 1; d.baseArrayLayer = v.dim === 5 || v.dim === 4 && v.slices > 1 ? v.slice : 0; d.arrayLayerCount = 1;
    } else {
      d.baseMipLevel = v.mip; d.mipLevelCount = Math.min(v.mips || tex.mips, tex.mips - v.mip);
      if (v.dim === 8 || tex.dimension === '3d') { d.dimension = '3d'; }
      else if (v.dim === 9 || (tex.cube && v.dim !== 5)) { d.dimension = 'cube'; d.baseArrayLayer = 0; d.arrayLayerCount = 6; }
      else if (v.dim === 5) { d.dimension = '2d-array'; d.baseArrayLayer = v.slice; d.arrayLayerCount = Math.min(v.slices, tex.array - v.slice); }
      else { d.dimension = '2d'; d.baseArrayLayer = 0; d.arrayLayerCount = 1; }
    }
    v.cache = { view: tex.gpu.createView(d), format, tex, dim: d.dimension };
    return v.cache;
  }

  // ── clears ───────────────────────────────────────────────────────────────
  clearRtv(id, color) {
    const v = this.getView(id);
    if (!v) return;
    this.endPass();
    const pass = this.encoder().beginRenderPass({ colorAttachments: [{ view: v.view, loadOp: 'clear', storeOp: 'store', clearValue: { r: color[0], g: color[1], b: color[2], a: color[3] } }] });
    pass.end();
    this.stats.passes++;
  }

  clearDsv(id, flags, depth, stencil) {
    const v = this.getView(id);
    if (!v) return;
    this.endPass();
    const a = { view: v.view };
    a.depthLoadOp = flags & 1 ? 'clear' : 'load'; a.depthStoreOp = 'store'; a.depthClearValue = depth;
    if (hasStencil(v.format)) { a.stencilLoadOp = flags & 2 ? 'clear' : 'load'; a.stencilStoreOp = 'store'; a.stencilClearValue = stencil; }
    const pass = this.encoder().beginRenderPass({ colorAttachments: [], depthStencilAttachment: a });
    pass.end();
    this.stats.passes++;
  }

  // ── draws ────────────────────────────────────────────────────────────────
  attachments() {
    const st = this.st;
    const colors = st.rtvs.map((id) => (id ? this.getView(id) : null));
    const depth = st.dsv ? this.getView(st.dsv) : null;
    return { colors, depth };
  }

  ensurePass(att) {
    const key = JSON.stringify([this.st.rtvs, this.st.dsv]);
    if (this.pass && this.passKey === key) return;
    this.endPass();
    const desc = { colorAttachments: att.colors.map((c) => (c ? { view: c.view, loadOp: 'load', storeOp: 'store' } : null)) };
    if (att.depth) {
      const a = { view: att.depth.view, depthLoadOp: 'load', depthStoreOp: 'store' };
      if (hasStencil(att.depth.format)) { a.stencilLoadOp = 'load'; a.stencilStoreOp = 'store'; }
      desc.depthStencilAttachment = a;
    }
    this.pass = this.encoder().beginRenderPass(desc);
    this.passKey = key;
    this.stats.passes++;
  }

  bindGroupLayout(group, entries) {
    const key = group + JSON.stringify(entries);
    let l = this.bglCache.get(key);
    if (!l) { l = this.device.createBindGroupLayout({ entries }); this.bglCache.set(key, l); }
    return l;
  }

  emptyBindGroup(layout) {
    let group = this.emptyBindGroupCache.get(layout);
    if (!group) {
      group = this.device.createBindGroup({ layout, entries: [] });
      this.emptyBindGroupCache.set(layout, group);
    }
    return group;
  }

  resolveBindGroups(layouts, resolved) {
    return resolved.map((entries, group) => entries.length
      ? this.device.createBindGroup({ layout: layouts[group], entries })
      : this.emptyBindGroup(layouts[group]));
  }

  sampleTypeOf(format) {
    if (!format) return 'float';
    if (isDepth(format)) return 'depth';
    if (/uint$/.test(format)) return 'uint';
    if (/sint$/.test(format)) return 'sint';
    if (/32float$/.test(format) && !this.features.has('float32-filterable')) return 'unfilterable-float';
    return 'float';
  }

  uniformBinding(stages, reg, declared) {
    const stage = stages.find((candidate) => this.st.cb[candidate][reg]) ?? -1;
    const id = stage >= 0 ? this.st.cb[stage][reg] : 0;
    const range = stage >= 0 ? this.st.cbRange[stage][reg] || { first: 0, count: 0 } : { first: 0, count: 0 };
    const resource = this.res.get(id);
    const offset = range.first * 16;
    const requested = range.count ? range.count * 16 : declared;
    const available = resource ? Math.max(0, resource.size - offset) : 0;
    const availableSize = Math.min(requested, available);
    let buffer = resource?.gpu, bindingOffset = offset;
    const copyToAlignedBuffer = !resource || offset % 256 !== 0 || availableSize < declared;
    if (copyToAlignedBuffer) {
      // WebGPU requires 256-byte uniform offsets. Copy misaligned or short D3D ranges to offset zero.
      buffer = this.paddedBuffer(resource, Math.max(declared, requested), offset);
      bindingOffset = 0;
    }
    const size = copyToAlignedBuffer ? Math.max(declared, requested) : Math.min(requested, available);
    return { buffer, offset: bindingOffset, size: Math.max(16, (size + 15) & ~15) };
  }

  draw(indexed, count, instances, first, baseVertex, firstInstance) {
    const st = this.st;
    const vs = this.shaders.get(st.shader[0]);
    let psId = st.shader[1];
    let ps = this.shaders.get(psId);
    if (ps?.texturedVariant && st.srv[1][0]) {
      psId = ps.texturedVariant;
      ps = this.shaders.get(psId);
    }
    if (ps && st.d3d9RenderStates.get(15)) {
      ps = this.fixedFunctionAlphaTest(ps, psId, st.d3d9RenderStates.get(25) ?? 8, st.d3d9RenderStates.get(24) ?? 0);
    }
    if (!vs) return;
    let topology = st.topology;
    let transientIndexBuffer = null;
    const rasterState = this.rasters.get(st.raster);
    const d3d9FillMode = st.d3d9RenderStates.get(8);
    const wireframe = d3d9FillMode === undefined
      ? rasterState?.getUint32(0, true) === 2
      : d3d9FillMode === 2;
    if (wireframe && (topology === 4 || topology === 5 || topology === 6)) {
      const indices = this.wireframeIndices(indexed, count, first, baseVertex);
      if (!indices) return;
      if (indices.length === 0) return;
      transientIndexBuffer = this.device.createBuffer({ size: indices.byteLength, usage: GPUBufferUsage.INDEX | GPUBufferUsage.COPY_DST });
      this.device.queue.writeBuffer(transientIndexBuffer, 0, indices);
      this.transientBuffers.push(transientIndexBuffer);
      indexed = true;
      count = indices.length;
      first = 0;
      baseVertex = 0;
      topology = 2; // D3D11 triangle wireframe -> WebGPU line-list
    } else if (topology === 6) {
      const indices = this.triangleFanIndices(indexed, count, first, baseVertex);
      if (!indices) return;
      if (indices.length === 0) return;
      transientIndexBuffer = this.device.createBuffer({ size: indices.byteLength, usage: GPUBufferUsage.INDEX | GPUBufferUsage.COPY_DST });
      this.device.queue.writeBuffer(transientIndexBuffer, 0, indices);
      this.transientBuffers.push(transientIndexBuffer);
      indexed = true;
      count = indices.length;
      first = 0;
      baseVertex = 0;
      topology = 4; // WebGPU has no triangle-fan topology; expand it to triangle-list indices.
    }
    const att = this.attachments();
    if (!att.colors.some(Boolean) && !att.depth) return;

    // Descriptor tables from shader reflection. UAVs use binding register + 1 so
    // group 3 binding 0 remains available for Naga's hidden draw-parameter uniform.
    const GROUP = { 2: 0, 0: 1, 3: 2, 1: 3 };
    const groups = [new Map(), new Map(), new Map(), new Map()];
    for (const sh of [vs, ps]) {
      if (!sh) continue;
      for (const d of sh.descriptors) {
        const g = GROUP[d.type];
        const key = d.register;
        if (!groups[g].has(key)) groups[g].set(key, { d, vis: 0 });
        groups[g].get(key).vis |= sh === vs ? GPUShaderStage.VERTEX : GPUShaderStage.FRAGMENT;
      }
    }
    if (groups[3].size) {
      const registers = [...groups[3].keys()].map((reg) => `u${reg}`).join(', ');
      this.log(`draw skipped: graphics UAV binding is unsupported (${registers})`);
      return;
    }
    const layoutEntries = [[], [], [], []];
    const resolved = [[], [], [], []];
    for (const [reg, { d, vis }] of groups[0]) {
      const declared = Math.max(vs.cbSizes[reg] || 0, ps?.cbSizes[reg] || 0) || 65536;
      layoutEntries[0].push({ binding: reg, visibility: vis, buffer: { type: 'uniform' } });
      const stages = (vis & GPUShaderStage.VERTEX) ? [0, 1] : [1, 0];
      resolved[0].push({ binding: reg, resource: this.uniformBinding(stages, reg, declared) });
    }
    const samplerComparison = [];
    for (const [reg, { d, vis }] of groups[2]) {
      const sid = this.st.smp[(vis & GPUShaderStage.VERTEX) ? 0 : 1][reg] || this.st.smp[1][reg] || this.st.smp[0][reg] || 0;
      const s = this.samplers.get(sid);
      samplerComparison[reg] = !!s?.comparison;
      layoutEntries[2].push({ binding: reg, visibility: vis, sampler: { type: s?.comparison ? 'comparison' : 'filtering' } });
      resolved[2].push({ binding: reg, resource: s ? s.gpu : this.defaultSampler });
    }
    for (const [reg, { d, vis }] of groups[1]) {
      const vid = this.st.srv[(vis & GPUShaderStage.FRAGMENT) ? 1 : 0][reg] || this.st.srv[1][reg] || this.st.srv[0][reg] || 0;
      const view = this.getView(vid);
      const bufferDeclaration = vs.storageBuffers?.[1]?.[reg] || ps?.storageBuffers?.[1]?.[reg];
      if (bufferDeclaration) {
        const prepared = view?.dim === 'buffer' && this.prepareStorageBufferView(view, false);
        if (!prepared) {
          this.log(`draw skipped: buffer SRV t${reg} is unavailable or has an unsupported range`);
          return;
        }
        layoutEntries[1].push({ binding: reg, visibility: vis, buffer: { type: bufferDeclaration.access === 'read' ? 'read-only-storage' : 'storage' } });
        resolved[1].push({ binding: reg, resource: prepared.resource });
        continue;
      }
      const dim = d.resourceType === 4 ? '2d' : d.resourceType === 5 ? '2d-array' : d.resourceType === 7 ? 'cube' : d.resourceType === 3 ? '2d' : '2d';
      const sampleType = this.sampleTypeOf(view?.format);
      layoutEntries[1].push({ binding: reg, visibility: vis, texture: { sampleType, viewDimension: view?.dim || dim } });
      resolved[1].push({ binding: reg, resource: view ? view.view : this.dummyView });
    }
    const drawParamVisibility = (vs.usesDrawParameters ? GPUShaderStage.VERTEX : 0)
      | (ps?.usesDrawParameters ? GPUShaderStage.FRAGMENT : 0);
    if (drawParamVisibility) {
      const data = new Uint8Array(16);
      const values = new DataView(data.buffer);
      values.setInt32(0, baseVertex, true);
      values.setInt32(4, firstInstance, true);
      const allocation = this.stage(data);
      layoutEntries[3].push({ binding: 0, visibility: drawParamVisibility, buffer: { type: 'uniform' } });
      resolved[3].push({ binding: 0, resource: { buffer: allocation.buffer, offset: allocation.offset, size: 16 } });
    }

    // Pipeline.
    const layoutDesc = this.layouts.get(st.layout) || [];
    const strides = layoutDesc.map((e) => st.vb[e.slot]?.stride || 0);
    const stepRates = new Map();
    for (const e of layoutDesc) {
      if (e.cls !== 1 || e.step <= 1) continue;
      const prior = stepRates.get(e.slot);
      if (prior !== undefined && prior !== e.step) {
        this.log(`draw skipped: input slot ${e.slot} has conflicting instance step rates`);
        return;
      }
      stepRates.set(e.slot, e.step);
    }
    const boundVertexBuffers = new Map();
    for (const slot of new Set(layoutDesc.map((e) => e.slot))) {
      const binding = st.vb[slot];
      const resource = binding && this.res.get(binding.buf);
      if (!resource) continue;
      let gpu = resource.gpu, offset = binding.offset;
      const stepRate = stepRates.get(slot) || 1;
      if (stepRate > 1) {
        gpu = this.expandInstanceBuffer(resource, binding.offset, binding.stride, stepRate, firstInstance + instances);
        if (!gpu) return;
        offset = 0;
      }
      boundVertexBuffers.set(slot, { gpu, offset });
    }
    const rtFormats = att.colors.map((c) => c?.format || null);
    const sampleCount = att.colors.find(Boolean)?.tex.samples || att.depth?.tex.samples || 1;
    const d3d9PipelineStates = [8, 7, 14, 23, 22, 15, 24, 25, 27, 19, 20, 171, 206, 207, 208, 209, 168]
      .map((state) => st.d3d9RenderStates.get(state) ?? null);
    const key = JSON.stringify([st.shader[0], psId, st.layout, strides, topology, st.blend.id, st.raster, st.depth.id, d3d9PipelineStates, rtFormats, att.depth?.format || null, sampleCount, layoutEntries, indexed && (topology === 3 || topology === 5) ? st.ib.format : 0]);
    let pipe = this.pipelines.get(key);
    if (!pipe) {
      pipe = this.buildPipeline(vs, ps, layoutDesc, strides, att, layoutEntries, indexed, topology);
      this.pipelines.set(key, pipe);
      this.stats.pipelines++;
    }
    if (!pipe) return;

    this.ensurePass(att);
    const pass = this.pass;
    pass.setPipeline(pipe.pipeline);
    const bindGroups = this.resolveBindGroups(pipe.bgls, resolved);
    for (let g = 0; g < 4; g++) {
      pass.setBindGroup(g, bindGroups[g]);
    }
    // Vertex buffers.
    for (const [slot, buffer] of boundVertexBuffers) {
      pass.setVertexBuffer(slot, buffer.gpu, buffer.offset);
    }
    if (indexed) {
      if (transientIndexBuffer) {
        pass.setIndexBuffer(transientIndexBuffer, 'uint32', 0);
      } else {
        const r = this.res.get(st.ib.buf);
        if (!r) return;
        pass.setIndexBuffer(r.gpu, st.ib.format === 42 ? 'uint32' : 'uint16', st.ib.offset);
      }
    }
    // Viewport / scissor.
    const first0 = att.colors.find(Boolean) || att.depth;
    const tex = first0.tex;
    const mipW = Math.max(1, tex.width >> (this.views.get(this.st.rtvs.find(Boolean) || st.dsv)?.mip || 0));
    const mipH = Math.max(1, tex.height >> (this.views.get(this.st.rtvs.find(Boolean) || st.dsv)?.mip || 0));
    const vp = st.viewports[0] || [0, 0, mipW, mipH, 0, 1];
    pass.setViewport(vp[0], vp[1], Math.min(vp[2], mipW - vp[0]), Math.min(vp[3], mipH - vp[1]), vp[4], vp[5]);
    const raster = this.rasters.get(st.raster);
    const d3d9Scissor = st.d3d9RenderStates.get(174);
    if (d3d9Scissor === undefined ? !!raster?.getUint32(28, true) : !!d3d9Scissor) {
      const sc = st.scissors[0] || [0, 0, mipW, mipH];
      const l = Math.max(0, sc[0]), t = Math.max(0, sc[1]);
      pass.setScissorRect(l, t, Math.max(0, Math.min(sc[2], mipW) - l), Math.max(0, Math.min(sc[3], mipH) - t));
    } else pass.setScissorRect(0, 0, mipW, mipH);
    pass.setBlendConstant({ r: st.blend.factor[0], g: st.blend.factor[1], b: st.blend.factor[2], a: st.blend.factor[3] });
    pass.setStencilReference(st.depth.ref);

    if (indexed) pass.drawIndexed(count, instances, first, baseVertex, firstInstance);
    else pass.draw(count, instances, first, firstInstance);
    this.stats.draws++;
  }

  wireframeIndices(indexed, count, first, baseVertex) {
    const st = this.st;
    const source = [];
    if (indexed) {
      const buffer = this.res.get(st.ib.buf);
      const indexBytes = st.ib.format === 42 ? 4 : 2;
      const start = st.ib.offset + first * indexBytes;
      if (!buffer?.shadow || start + count * indexBytes > buffer.shadow.length) {
        this.log('wireframe draw skipped: index data is not available on the CPU');
        return null;
      }
      const view = new DataView(buffer.shadow.buffer, buffer.shadow.byteOffset, buffer.shadow.byteLength);
      for (let i = 0; i < count; i++) {
        const raw = indexBytes === 4 ? view.getUint32(start + i * indexBytes, true) : view.getUint16(start + i * indexBytes, true);
        const restart = indexBytes === 4 ? 0xffffffff : 0xffff;
        if (raw === restart && (st.topology === 5)) {
          source.push(restart);
          continue;
        }
        const index = raw + baseVertex;
        if (index < 0 || index > 0xffffffff) {
          this.log('wireframe draw skipped: base vertex produces an invalid index');
          return null;
        }
        source.push(index);
      }
    } else {
      for (let i = 0; i < count; i++) source.push(first + i);
    }

    const triangles = [];
    if (st.topology === 4) {
      for (let i = 0; i + 2 < source.length; i += 3) triangles.push([source[i], source[i + 1], source[i + 2]]);
    } else if (st.topology === 5) {
      const restart = indexed ? (st.ib.format === 42 ? 0xffffffff : 0xffff) : -1;
      let strip = [];
      const flush = () => {
        for (let i = 0; i + 2 < strip.length; i++) {
          triangles.push(i & 1 ? [strip[i + 1], strip[i], strip[i + 2]] : [strip[i], strip[i + 1], strip[i + 2]]);
        }
        strip = [];
      };
      for (const index of source) {
        if (index === restart) flush();
        else strip.push(index);
      }
      flush();
    } else if (st.topology === 6) {
      for (let i = 1; i + 1 < source.length; i++) {
        triangles.push([source[0], source[i], source[i + 1]]);
      }
    }
    const edges = [];
    for (const [a, b, c] of triangles) edges.push(a, b, b, c, c, a);
    return new Uint32Array(edges);
  }

  triangleFanIndices(indexed, count, first, baseVertex) {
    const st = this.st;
    const source = [];
    if (indexed) {
      const buffer = this.res.get(st.ib.buf);
      const indexBytes = st.ib.format === 42 ? 4 : 2;
      const start = st.ib.offset + first * indexBytes;
      if (!buffer?.shadow || start + count * indexBytes > buffer.shadow.length) {
        this.log('triangle-fan draw skipped: index data is not available on the CPU');
        return null;
      }
      const view = new DataView(buffer.shadow.buffer, buffer.shadow.byteOffset, buffer.shadow.byteLength);
      for (let i = 0; i < count; i++) {
        const raw = indexBytes === 4 ? view.getUint32(start + i * indexBytes, true) : view.getUint16(start + i * indexBytes, true);
        const index = raw + baseVertex;
        if (index < 0 || index > 0xffffffff) {
          this.log('triangle-fan draw skipped: base vertex produces an invalid index');
          return null;
        }
        source.push(index);
      }
    } else {
      for (let i = 0; i < count; i++) source.push(first + i);
    }
    const triangles = [];
    for (let i = 1; i + 1 < source.length; i++) triangles.push(source[0], source[i], source[i + 1]);
    return new Uint32Array(triangles);
  }

  expandInstanceBuffer(source, sourceOffset, stride, stepRate, requiredInstances) {
    if (stride <= 0 || stride % 4 !== 0 || sourceOffset % 4 !== 0 || requiredInstances <= 0) {
      this.log('draw skipped: instance step-rate expansion requires aligned nonempty vertex data');
      return null;
    }
    const strideWords = stride / 4;
    const outputWords = requiredInstances * strideWords;
    const outputBytes = outputWords * 4;
    const storageAlignment = this.device.limits.minStorageBufferOffsetAlignment || 256;
    const sourceBindingOffset = Math.floor(sourceOffset / storageAlignment) * storageAlignment;
    const sourceOffsetWords = (sourceOffset - sourceBindingOffset) / 4;
    const sourceAvailableWords = Math.floor((source.size - sourceBindingOffset) / 4);
    const sourceRequiredWords = sourceOffsetWords + Math.ceil(requiredInstances / stepRate) * strideWords;
    const sourceWords = Math.min(
      sourceAvailableWords,
      sourceRequiredWords,
      Math.floor(this.device.limits.maxStorageBufferBindingSize / 4),
    );
    if (!Number.isSafeInteger(outputWords) || outputWords <= 0 || outputBytes > this.device.limits.maxStorageBufferBindingSize
        || outputBytes > this.device.limits.maxBufferSize || sourceOffsetWords >= sourceWords) {
      this.log('draw skipped: instance step-rate expansion exceeds WebGPU buffer limits');
      return null;
    }
    if (!this.instanceExpansionPipeline) {
      const module = this.device.createShaderModule({ code: `
        struct Params {
          sourceWords: u32,
          strideWords: u32,
          stepRate: u32,
          sourceOffsetWords: u32,
          outputWords: u32,
          baseWord: u32,
          _pad0: u32,
          _pad1: u32,
        };
        @group(0) @binding(0) var<storage, read> source: array<u32>;
        @group(0) @binding(1) var<storage, read_write> expanded: array<u32>;
        @group(0) @binding(2) var<uniform> params: Params;
        @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
          let word = params.baseWord + id.x;
          if (word >= params.outputWords) { return; }
          let row = word / params.strideWords;
          let field = word % params.strideWords;
          let sourceWord = params.sourceOffsetWords + (row / params.stepRate) * params.strideWords + field;
          if (sourceWord < params.sourceWords) {
            expanded[word] = source[sourceWord];
          } else {
            expanded[word] = 0u;
          }
        }
      ` });
      this.instanceExpansionPipeline = this.device.createComputePipeline({
        layout: 'auto',
        compute: { module, entryPoint: 'main' },
      });
    }
    const expanded = this.device.createBuffer({
      size: outputBytes,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.VERTEX,
    });
    this.transientBuffers.push(expanded);
    this.endPass();
    const pass = this.encoder().beginComputePass();
    pass.setPipeline(this.instanceExpansionPipeline);
    const maxGroups = Math.max(1, this.device.limits.maxComputeWorkgroupsPerDimension || 65535);
    const wordsPerDispatch = maxGroups * 64;
    for (let baseWord = 0; baseWord < outputWords; baseWord += wordsPerDispatch) {
      const params = this.device.createBuffer({ size: 32, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
      this.transientBuffers.push(params);
      this.device.queue.writeBuffer(params, 0, new Uint32Array([
        sourceWords, strideWords, stepRate, sourceOffsetWords, outputWords, baseWord, 0, 0,
      ]));
      const bindGroup = this.device.createBindGroup({
        layout: this.instanceExpansionPipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: { buffer: source.gpu, offset: sourceBindingOffset, size: sourceWords * 4 } },
          { binding: 1, resource: { buffer: expanded } },
          { binding: 2, resource: { buffer: params, size: 32 } },
        ],
      });
      const chunkWords = Math.min(wordsPerDispatch, outputWords - baseWord);
      pass.setBindGroup(0, bindGroup);
      pass.dispatchWorkgroups(Math.ceil(chunkWords / 64));
    }
    pass.end();
    return expanded;
  }

  prepareStorageBufferView(view, writable) {
    const size = view.size & ~3;
    if (!view.bufferResource || size <= 0 || size !== view.size || view.offset % 4 !== 0) return null;
    const scratch = this.device.createBuffer({
      size: Math.max(4, size),
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST,
    });
    this.transientBuffers.push(scratch);
    this.endPass();
    this.encoder().copyBufferToBuffer(view.bufferResource.gpu, view.offset, scratch, 0, size);
    return { resource: { buffer: scratch, offset: 0, size }, copyBack: writable ? { scratch, view, size } : null };
  }

  dispatchCompute(x, y, z) {
    const st = this.st;
    let shader = this.shaders.get(st.shader[2]);
    if (!shader || shader.stage !== 2) {
      this.log(`compute dispatch skipped: compute shader ${st.shader[2]} is unavailable`);
      return;
    }

    const textureUavs = shader.descriptors.filter((d) => d.type === 1 && !shader.storageBuffers?.[3]?.[d.register + 1]);
    if (textureUavs.length === 1) {
      const declaration = textureUavs[0];
      const uav = st.uav[declaration.register];
      const viewDesc = uav?.id && this.views.get(uav.id);
      const view = viewDesc && this.getView(uav.id);
      const texture = viewDesc && this.res.get(viewDesc.resource);
      const dxgiFormat = viewDesc?.format || texture?.dxgi;
      if (view?.dim !== 'buffer' && TYPED_UAV_DATA_TYPES[dxgiFormat] === declaration.dataType) {
        shader = this.shaderVariant(shader, dxgiFormat);
      }
    }

    const GROUP = { 2: 0, 0: 1, 3: 2, 1: 3 };
    const groups = [new Map(), new Map(), new Map(), new Map()];
    for (const d of shader.descriptors) {
      const group = GROUP[d.type];
      if (group === undefined) continue;
      groups[group].set(d.register, d);
    }
    const layoutEntries = [[], [], [], []];
    const resolved = [[], [], [], []];
    const storageBufferCopies = [];

    for (const [reg] of groups[0]) {
      const declared = shader.cbSizes[reg] || 65536;
      layoutEntries[0].push({ binding: reg, visibility: GPUShaderStage.COMPUTE, buffer: { type: 'uniform' } });
      resolved[0].push({ binding: reg, resource: this.uniformBinding([2], reg, declared) });
    }
    for (const [reg, d] of groups[1]) {
      const view = this.getView(st.srv[2][reg] || 0);
      const bufferDeclaration = shader.storageBuffers?.[1]?.[reg];
      if (bufferDeclaration) {
        const prepared = view?.dim === 'buffer' && this.prepareStorageBufferView(view, false);
        if (!prepared) {
          this.log(`compute dispatch skipped: buffer SRV t${reg} is unavailable or has an unsupported range`);
          return;
        }
        layoutEntries[1].push({ binding: reg, visibility: GPUShaderStage.COMPUTE, buffer: { type: bufferDeclaration.access === 'read' ? 'read-only-storage' : 'storage' } });
        resolved[1].push({ binding: reg, resource: prepared.resource });
        continue;
      }
      const fallbackDimension = d.resourceType === 5 ? '2d-array' : d.resourceType === 7 ? 'cube' : '2d';
      const viewDimension = view?.dim || fallbackDimension;
      const sampleType = this.sampleTypeOf(view?.format);
      layoutEntries[1].push({ binding: reg, visibility: GPUShaderStage.COMPUTE, texture: { sampleType, viewDimension } });
      resolved[1].push({ binding: reg, resource: view ? view.view : this.dummyView });
    }
    for (const [reg] of groups[2]) {
      const sampler = this.samplers.get(st.smp[2][reg] || 0);
      layoutEntries[2].push({ binding: reg, visibility: GPUShaderStage.COMPUTE, sampler: { type: sampler?.comparison ? 'comparison' : 'filtering' } });
      resolved[2].push({ binding: reg, resource: sampler ? sampler.gpu : this.defaultSampler });
    }
    for (const [reg] of groups[3]) {
      const binding = reg + 1;
      const bufferDeclaration = shader.storageBuffers?.[3]?.[binding];
      const declaration = shader.storageTextures[binding];
      const uav = st.uav[reg];
      const view = uav && this.getView(uav.id);
      if (bufferDeclaration) {
        const prepared = view?.dim === 'buffer' && this.prepareStorageBufferView(view, bufferDeclaration.access === 'read_write');
        if (!prepared) {
          this.log(`compute dispatch skipped: buffer UAV u${reg} is unavailable or has an unsupported range`);
          return;
        }
        layoutEntries[3].push({ binding, visibility: GPUShaderStage.COMPUTE, buffer: { type: bufferDeclaration.access === 'read' ? 'read-only-storage' : 'storage' } });
        resolved[3].push({ binding, resource: prepared.resource });
        if (prepared.copyBack) storageBufferCopies.push(prepared.copyBack);
        continue;
      }
      if (!declaration || !view || declaration.format !== view.format || declaration.dimension !== view.dim) {
        this.log(`compute dispatch skipped: UAV u${reg} declaration=${JSON.stringify(declaration || null)} view=${JSON.stringify(view && { format: view.format, dimension: view.dim })}`);
        return;
      }
      layoutEntries[3].push({ binding, visibility: GPUShaderStage.COMPUTE, storageTexture: {
        access: declaration.access,
        format: declaration.format,
        viewDimension: declaration.dimension,
      } });
      resolved[3].push({ binding, resource: view.view });
    }

    this.stats.lastComputeUavs = [...groups[3].keys()]
      .map((reg) => st.uav[reg]?.id)
      .filter((id) => id !== undefined && id !== 0);

    const key = JSON.stringify([st.shader[2], layoutEntries]);
    let cached = this.computePipelines.get(key);
    if (!cached) {
      const bgls = layoutEntries.map((entries, group) => this.bindGroupLayout(group, entries));
      let pipeline;
      this.device.pushErrorScope('validation');
      try {
        pipeline = this.device.createComputePipeline({
          layout: this.device.createPipelineLayout({ bindGroupLayouts: bgls }),
          compute: { module: shader.module, entryPoint: shader.entry },
        });
      } catch (error) {
        this.device.popErrorScope();
        this.stats.errors.push(`createComputePipeline: ${error.message}`);
        this.log(`createComputePipeline: ${error.message}`);
        return;
      }
      this.device.popErrorScope().then((error) => {
        if (error) {
          this.stats.errors.push('compute pipeline: ' + error.message);
          this.log('compute pipeline validation: ' + error.message);
        }
      });
      cached = { pipeline, bgls };
      this.computePipelines.set(key, cached);
      this.stats.pipelines++;
    }

    const bindGroups = this.resolveBindGroups(cached.bgls, resolved);
    this.endPass();
    const pass = this.encoder().beginComputePass();
    pass.setPipeline(cached.pipeline);
    for (let group = 0; group < bindGroups.length; group++) pass.setBindGroup(group, bindGroups[group]);
    pass.dispatchWorkgroups(x, y, z);
    pass.end();
    for (const copy of storageBufferCopies) {
      this.encoder().copyBufferToBuffer(copy.scratch, 0, copy.view.bufferResource.gpu, copy.view.offset, copy.size);
    }
    this.stats.dispatches++;
  }

  paddedBuffer(src, size, sourceOffset = 0) {
    const sz = Math.max(16, (size + 15) & ~15);
    const key = `${src ? 'b' : 'z'}${sz}`;
    if (!src) {
      let b = this.dummyBuffers.get(key);
      if (!b) { b = this.device.createBuffer({ size: sz, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST }); this.dummyBuffers.set(key, b); }
      return b;
    }
    const b = this.device.createBuffer({ size: sz, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    this.endPass();
    const available = Math.max(0, src.size - sourceOffset);
    if (available) this.encoder().copyBufferToBuffer(src.gpu, sourceOffset, b, 0, Math.min(available, sz));
    return b;
  }

  buildPipeline(vs, ps, layoutDesc, strides, att, layoutEntries, indexed, topology = this.st.topology) {
    const st = this.st;
    const bgls = layoutEntries.map((e, g) => this.bindGroupLayout(g, e));
    const pl = this.device.createPipelineLayout({ bindGroupLayouts: bgls });

    // Vertex buffers grouped by slot.
    const bySlot = new Map();
    layoutDesc.forEach((e, i) => {
      if (!bySlot.has(e.slot)) bySlot.set(e.slot, { arrayStride: strides[i] || 16, stepMode: e.cls === 1 ? 'instance' : 'vertex', attributes: [] });
      bySlot.get(e.slot).attributes.push({ shaderLocation: e.location, offset: e.offset, format: VTX[e.format] || 'float32x4' });
    });
    const maxSlot = Math.max(-1, ...bySlot.keys());
    const buffers = [];
    for (let s = 0; s <= maxSlot; s++) buffers.push(bySlot.get(s) || null);

    // Output merger.
    const blend = this.blends.get(st.blend.id);
    const d3d9States = st.d3d9RenderStates;
    const d3d9BlendEnabled = d3d9States.has(27);
    const targets = att.colors.map((c, i) => {
      if (!c) return null;
      const t = { format: c.format, writeMask: 0xf };
      if (d3d9States.has(168)) t.writeMask = d3d9States.get(168) & 0xf;
      if (d3d9BlendEnabled) {
        if (d3d9States.get(27)) {
          const src = d3d9States.get(19) ?? 2, dst = d3d9States.get(20) ?? 1;
          const op = d3d9States.get(171) ?? 1;
          const separate = !!d3d9States.get(206);
          const srcAlpha = separate ? (d3d9States.get(207) ?? 2) : src;
          const dstAlpha = separate ? (d3d9States.get(208) ?? 1) : dst;
          const opAlpha = separate ? (d3d9States.get(209) ?? 1) : op;
          const factor = (n) => BLEND_FACTOR[n] || 'one';
          const operation = (n) => BLEND_OP[n] || 'add';
          const color = { srcFactor: factor(src), dstFactor: factor(dst), operation: operation(op) };
          const alpha = { srcFactor: factor(srcAlpha), dstFactor: factor(dstAlpha), operation: operation(opAlpha) };
          for (const channel of [color, alpha]) if (channel.operation === 'min' || channel.operation === 'max') { channel.srcFactor = 'one'; channel.dstFactor = 'one'; }
          t.blend = { color, alpha };
        }
      } else if (blend) {
        const o = 8 + 32 * (blend.getUint32(4, true) ? i : 0);
        if (blend.getUint32(o, true)) {
          const fac = (n) => BLEND_FACTOR[n] || 'one';
          const color = { srcFactor: fac(blend.getUint32(o + 4, true)), dstFactor: fac(blend.getUint32(o + 8, true)), operation: BLEND_OP[blend.getUint32(o + 12, true)] || 'add' };
          const alpha = { srcFactor: fac(blend.getUint32(o + 16, true)), dstFactor: fac(blend.getUint32(o + 20, true)), operation: BLEND_OP[blend.getUint32(o + 24, true)] || 'add' };
          for (const c2 of [color, alpha]) if (c2.operation === 'min' || c2.operation === 'max') { c2.srcFactor = 'one'; c2.dstFactor = 'one'; }
          t.blend = { color, alpha };
        }
        t.writeMask = blend.getUint8(o + 28);
      }
      return t;
    });

    const desc = {
      layout: pl,
      vertex: { module: vs.module, entryPoint: vs.entry, buffers },
      primitive: { topology: TOPOLOGY[topology] || 'triangle-list' },
    };
    desc.multisample = { count: att.colors.find(Boolean)?.tex.samples || att.depth?.tex.samples || 1 };
    if (ps) desc.fragment = { module: ps.module, entryPoint: ps.entry, targets };
    else if (targets.some(Boolean)) desc.fragment = undefined;
    if (indexed && (topology === 3 || topology === 5)) desc.primitive.stripIndexFormat = st.ib.format === 42 ? 'uint32' : 'uint16';
    const r = this.rasters.get(st.raster);
    if (st.d3d9RenderStates.has(22)) {
      const cull = st.d3d9RenderStates.get(22);
      if (cull === 1) desc.primitive.cullMode = 'none';
      else if (cull === 2 || cull === 3) {
        desc.primitive.cullMode = 'front';
        desc.primitive.frontFace = cull === 2 ? 'cw' : 'ccw';
      }
    } else if (r) {
      desc.primitive.cullMode = ({ 1: 'none', 2: 'front', 3: 'back' })[r.getUint32(4, true)] || 'back';
      desc.primitive.frontFace = r.getUint32(8, true) ? 'ccw' : 'cw';
    } else { desc.primitive.cullMode = 'back'; desc.primitive.frontFace = 'cw'; }
    const dsd = this.depths.get(st.depth.id);
    if (att.depth) {
      const fmt = att.depth.format;
      const d = { format: fmt };
      const hasD3d9DepthState = st.d3d9RenderStates.has(7) || st.d3d9RenderStates.has(14) || st.d3d9RenderStates.has(23);
      const enable = hasD3d9DepthState ? (st.d3d9RenderStates.get(7) ?? 1) : dsd ? dsd.getUint32(0, true) : 1;
      const write = hasD3d9DepthState
        ? (st.d3d9RenderStates.get(14) ?? 1)
        : dsd ? dsd.getUint32(4, true) : 1;
      const compare = hasD3d9DepthState
        ? (st.d3d9RenderStates.get(23) ?? 2)
        : dsd ? dsd.getUint32(8, true) : 2;
      d.depthWriteEnabled = !!(enable && write);
      d.depthCompare = enable ? CMP[compare] || 'less' : 'always';
      if (r) { d.depthBias = r.getInt32(12, true); d.depthBiasSlopeScale = r.getFloat32(20, true); d.depthBiasClamp = r.getFloat32(16, true); }
      if (dsd && dsd.getUint32(12, true) && hasStencil(fmt)) {
        const face = (o) => ({ failOp: STENCIL_OP[dsd.getUint32(o, true)] || 'keep', depthFailOp: STENCIL_OP[dsd.getUint32(o + 4, true)] || 'keep', passOp: STENCIL_OP[dsd.getUint32(o + 8, true)] || 'keep', compare: CMP[dsd.getUint32(o + 12, true)] || 'always' });
        d.stencilFront = face(20); d.stencilBack = face(36);
        d.stencilReadMask = dsd.getUint8(16); d.stencilWriteMask = dsd.getUint8(17);
      }
      desc.depthStencil = d;
    }
    const before = this.stats.errors.length;
    this.device.pushErrorScope('validation');
    let pipeline = null;
    try { pipeline = this.device.createRenderPipeline(desc); } catch (e) { this.stats.errors.push('createRenderPipeline: ' + e.message); this.log('createRenderPipeline: ' + e.message); }
    this.device.popErrorScope().then((e) => { if (e) { this.stats.errors.push('pipeline: ' + e.message); this.log('pipeline validation: ' + e.message); } });
    void before;
    return pipeline ? { pipeline, bgls } : null;
  }

  // ── copies ───────────────────────────────────────────────────────────────
  copyResource(dstId, srcId) {
    const d = this.res.get(dstId), s = this.res.get(srcId);
    if (!d || !s) return;
    this.endPass();
    const enc = this.encoder();
    if (d.kind === 'buffer' && s.kind === 'buffer') {
      const size = Math.min(s.size, d.size);
      enc.copyBufferToBuffer(s.gpu, 0, d.gpu, 0, size);
      if (d.shadow && s.shadow) d.shadow.set(s.shadow.subarray(0, size), 0);
    }
    else if (d.kind === 'texture' && s.kind === 'texture') {
      for (let m = 0; m < Math.min(d.mips, s.mips); m++) {
        const sourceLayers = s.dimension === '3d' ? Math.max(1, s.depth >> m) : s.array;
        const destLayers = d.dimension === '3d' ? Math.max(1, d.depth >> m) : d.array;
        enc.copyTextureToTexture({ texture: s.gpu, mipLevel: m }, { texture: d.gpu, mipLevel: m }, { width: Math.max(1, s.width >> m), height: Math.max(1, s.height >> m), depthOrArrayLayers: Math.min(sourceLayers, destLayers) });
      }
    }
  }

  copyRegion(dv) {
    const u = (i) => dv.getUint32(i * 4, true);
    const [dst, dsub, dx, dy, dz, src, ssub, hasBox] = [u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7)];
    const d = this.res.get(dst), s = this.res.get(src);
    if (!d || !s) return;
    this.endPass();
    if (d.kind === 'buffer' && s.kind === 'buffer') {
      const l = hasBox ? u(8) : 0, r = hasBox ? u(11) : s.size;
      const sourceOffset = l & ~3, destOffset = dx & ~3;
      const size = Math.min(r - l, d.size - dx) & ~3;
      this.encoder().copyBufferToBuffer(s.gpu, sourceOffset, d.gpu, destOffset, size);
      if (d.shadow && s.shadow && sourceOffset + size <= s.shadow.length && destOffset + size <= d.shadow.length) {
        d.shadow.set(s.shadow.subarray(sourceOffset, sourceOffset + size), destOffset);
      }
      return;
    }
    const smip = ssub % s.mips, sslice = Math.floor(ssub / s.mips), dmip = dsub % d.mips, dslice = Math.floor(dsub / d.mips);
    const box = hasBox ? { l: u(8), t: u(9), f: u(10), r: u(11), b: u(12), back: u(13) } : { l: 0, t: 0, f: 0, r: Math.max(1, s.width >> smip), b: Math.max(1, s.height >> smip), back: s.dimension === '3d' ? Math.max(1, s.depth >> smip) : 1 };
    const sourceZ = s.dimension === '3d' ? box.f : sslice;
    const destZ = d.dimension === '3d' ? dz : dslice;
    const depth = s.dimension === '3d' || d.dimension === '3d' ? box.back - box.f : 1;
    this.encoder().copyTextureToTexture({ texture: s.gpu, mipLevel: smip, origin: { x: box.l, y: box.t, z: sourceZ } }, { texture: d.gpu, mipLevel: dmip, origin: { x: dx, y: dy, z: destZ } }, { width: box.r - box.l, height: box.b - box.t, depthOrArrayLayers: depth });
  }

  generateMips(viewId) {
    const v = this.views.get(viewId);
    const tex = v && this.res.get(v.resource);
    if (!tex || tex.mips < 2) return;
    this.endPass();
    // Simple box-filter chain using the blit pipeline (colour textures only).
    if (isDepth(tex.format) || BLOCK(tex.format)) return;
    if (tex.dimension === '3d') {
      this.generateMips3d(tex);
      return;
    }
    if (!this.mipPipelines) this.mipPipelines = new Map();
    let pipe = this.mipPipelines.get(tex.format);
    if (!pipe) {
      const mod = this.device.createShaderModule({ code: BLIT_WGSL });
      pipe = this.device.createRenderPipeline({ layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.blitBGL] }), vertex: { module: mod, entryPoint: 'vs' }, fragment: { module: mod, entryPoint: 'fs', targets: [{ format: tex.format }] } });
      this.mipPipelines.set(tex.format, pipe);
    }
    for (let layer = 0; layer < tex.array; layer++) {
      for (let m = 1; m < tex.mips; m++) {
        const src = tex.gpu.createView({ baseMipLevel: m - 1, mipLevelCount: 1, baseArrayLayer: layer, arrayLayerCount: 1, dimension: '2d' });
        const dst = tex.gpu.createView({ baseMipLevel: m, mipLevelCount: 1, baseArrayLayer: layer, arrayLayerCount: 1, dimension: '2d' });
        const pass = this.encoder().beginRenderPass({ colorAttachments: [{ view: dst, loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 0 } }] });
        pass.setPipeline(pipe);
        pass.setBindGroup(0, this.device.createBindGroup({ layout: this.blitBGL, entries: [{ binding: 0, resource: src }, { binding: 1, resource: this.blitSampler }] }));
        pass.draw(3);
        pass.end();
      }
    }
  }

  generateMips3d(tex) {
    if (tex.format !== 'rgba8unorm' && tex.format !== 'r32float') {
      this.log(`GenerateMips skipped: 3D ${tex.format} storage textures are unsupported`);
      return;
    }
    if (!this.mip3dPipelines) this.mip3dPipelines = new Map();
    let pipe = this.mip3dPipelines.get(tex.format);
    if (!pipe) {
      const code = `
        @group(0) @binding(0) var source: texture_3d<f32>;
        @group(0) @binding(1) var destination: texture_storage_3d<${tex.format}, write>;
        @compute @workgroup_size(4, 4, 4)
        fn main(@builtin(global_invocation_id) id: vec3<u32>) {
          let sourceSize = textureDimensions(source, 0);
          let destinationSize = max(sourceSize / vec3<u32>(2u), vec3<u32>(1u));
          if (any(id >= destinationSize)) { return; }
          let base = id * vec3<u32>(2u);
          var sum = vec4<f32>(0.0);
          for (var z = 0u; z < 2u; z++) {
            for (var y = 0u; y < 2u; y++) {
              for (var x = 0u; x < 2u; x++) {
                let coord = min(base + vec3<u32>(x, y, z), sourceSize - vec3<u32>(1u));
                sum += textureLoad(source, vec3<i32>(coord), 0);
              }
            }
          }
          textureStore(destination, vec3<i32>(id), sum * 0.125);
        }
      `;
      const module = this.device.createShaderModule({ code });
      pipe = this.device.createComputePipeline({ layout: 'auto', compute: { module, entryPoint: 'main' } });
      this.mip3dPipelines.set(tex.format, pipe);
    }
    for (let mip = 1; mip < tex.mips; mip++) {
      const source = tex.gpu.createView({ dimension: '3d', baseMipLevel: mip - 1, mipLevelCount: 1 });
      const destination = tex.gpu.createView({ dimension: '3d', baseMipLevel: mip, mipLevelCount: 1 });
      const bindGroup = this.device.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: [
        { binding: 0, resource: source }, { binding: 1, resource: destination },
      ] });
      const pass = this.encoder().beginComputePass();
      pass.setPipeline(pipe);
      pass.setBindGroup(0, bindGroup);
      pass.dispatchWorkgroups(Math.ceil(Math.max(1, tex.width >> mip) / 4), Math.ceil(Math.max(1, tex.height >> mip) / 4), Math.ceil(Math.max(1, tex.depth >> mip) / 4));
      pass.end();
      this.stats.dispatches++;
    }
  }

  // ── swap chains ──────────────────────────────────────────────────────────
  createSwapchain(id, texId, w, h, dxgi) {
    this.swapchains.set(id, { texId });
    const base = this.texFormat(dxgi, 0x20);
    const unsrgb = UNSRGB[base] || base;
    const view = SRGB_PAIR[unsrgb];
    const gpu = this.device.createTexture({ size: [w, h], format: unsrgb, usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_SRC | GPUTextureUsage.COPY_DST, viewFormats: view ? [view] : [] });
    this.res.set(texId, { kind: 'texture', gpu, width: w, height: h, mips: 1, array: 1, format: unsrgb, dxgi, bind: 0x28, cube: false, swapchain: id });
    // The backbuffer is an application-visible texture; if the app asked for SRGB the RTV view picks the sRGB format.
    if (this.canvas.width !== w || this.canvas.height !== h) { this.canvas.width = w; this.canvas.height = h; }
  }

  resizeSwapchain(id, texId, w, h, dxgi) {
    this.flush();
    this.res.get(texId)?.gpu.destroy?.();
    for (const v of this.views.values()) if (v.resource === texId) v.cache = null;
    this.createSwapchain(id, texId, w, h, dxgi);
  }

  present(scId, texId) {
    const t = this.res.get(texId);
    if (!t) return;
    this.endPass();
    if (this.noPresent) { this.flush(); this.stats.presents++; return; } // test mode: leave the canvas untouched
    const target = this.ctx.getCurrentTexture();
    const src = t.gpu.createView({ format: UNSRGB[t.format] || t.format });
    const pass = this.encoder().beginRenderPass({ colorAttachments: [{ view: target.createView(), loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 1 } }] });
    pass.setPipeline(this.blit);
    pass.setBindGroup(0, this.device.createBindGroup({ layout: this.blitBGL, entries: [{ binding: 0, resource: src }, { binding: 1, resource: this.blitSampler }] }));
    pass.draw(3);
    pass.end();
    this.flush();
    this.stats.presents++;
  }

  /** Read the most recent back buffer as tightly packed RGBA8 (for tests/screenshots). */
  async readBackbuffer(scId) {
    const sc = this.swapchains.get(scId ?? [...this.swapchains.keys()][0]);
    const t = sc && this.res.get(sc.texId);
    if (!t) return null;
    this.flush();
    const bpr = (t.width * 4 + 255) & ~255;
    const buf = this.device.createBuffer({ size: bpr * t.height, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const enc = this.device.createCommandEncoder();
    enc.copyTextureToBuffer({ texture: t.gpu }, { buffer: buf, bytesPerRow: bpr, rowsPerImage: t.height }, { width: t.width, height: t.height });
    this.device.queue.submit([enc.finish()]);
    await buf.mapAsync(GPUMapMode.READ);
    const raw = new Uint8Array(buf.getMappedRange());
    const out = new Uint8Array(t.width * t.height * 4);
    const bgra = t.format.startsWith('bgra');
    for (let y = 0; y < t.height; y++) {
      const row = raw.subarray(y * bpr, y * bpr + t.width * 4);
      if (!bgra) out.set(row, y * t.width * 4);
      else for (let x = 0; x < t.width; x++) { const i = y * t.width * 4 + x * 4; out[i] = row[x * 4 + 2]; out[i + 1] = row[x * 4 + 1]; out[i + 2] = row[x * 4]; out[i + 3] = row[x * 4 + 3]; }
    }
    buf.unmap(); buf.destroy();
    return { width: t.width, height: t.height, data: out };
  }

  /** Read the raw bytes of a D3D11 resource subresource for guest Map(READ). */
  async readBackResource(id, subresource = 0) {
    const resource = this.res.get(id);
    if (!resource) throw new Error(`resource ${id} is unavailable`);
    if (resource.kind === 'buffer') {
      const byteLength = resource.d3dSize ?? resource.size;
      if (!byteLength) throw new Error(`buffer ${id} has no readable bytes`);
      const copyLength = (byteLength + 3) & ~3;
      const buffer = this.device.createBuffer({
        size: copyLength,
        usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
      });
      try {
        this.flush();
        const encoder = this.device.createCommandEncoder();
        encoder.copyBufferToBuffer(resource.gpu, 0, buffer, 0, copyLength);
        this.device.queue.submit([encoder.finish()]);
        await buffer.mapAsync(GPUMapMode.READ);
        return new Uint8Array(buffer.getMappedRange()).slice(0, byteLength);
      } finally {
        if (buffer.mapState === 'mapped') buffer.unmap();
        buffer.destroy();
      }
    }
    if (resource.kind !== 'texture') throw new Error(`resource ${id} has an unsupported kind`);
    if (resource.samples !== 1) throw new Error(`resource ${id} is multisampled and cannot be mapped for read`);
    if (hasStencil(resource.format)) throw new Error(`stencil texture readback is unsupported for ${resource.format}`);

    const mip = resource.dimension === '3d' ? subresource : subresource % resource.mips;
    const slice = resource.dimension === '3d' ? 0 : Math.floor(subresource / resource.mips);
    if (mip < 0 || mip >= resource.mips || slice >= resource.array) {
      throw new Error(`texture ${id} subresource ${subresource} is out of range`);
    }
    const width = Math.max(1, resource.width >> mip);
    const height = Math.max(1, resource.height >> mip);
    const depth = resource.dimension === '3d' ? Math.max(1, resource.depth >> mip) : 1;
    const block = BLOCK(resource.format) || [1, 1, BYTES(resource.format)];
    const rowBytes = Math.ceil(width / block[0]) * block[2];
    const rows = Math.ceil(height / block[1]);
    const bytesPerRow = (rowBytes + 255) & ~255;
    const readback = this.device.createBuffer({
      size: bytesPerRow * rows * depth,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });
    try {
      this.flush();
      const encoder = this.device.createCommandEncoder();
      encoder.copyTextureToBuffer(
        {
          texture: resource.gpu,
          mipLevel: mip,
          origin: { x: 0, y: 0, z: slice },
          ...(isDepth(resource.format) ? { aspect: 'depth-only' } : {}),
        },
        { buffer: readback, bytesPerRow, rowsPerImage: rows },
        { width, height, depthOrArrayLayers: depth },
      );
      this.device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);
      const mapped = new Uint8Array(readback.getMappedRange());
      const output = new Uint8Array(rowBytes * rows * depth);
      const sourceImagePitch = bytesPerRow * rows;
      const outputImagePitch = rowBytes * rows;
      for (let z = 0; z < depth; z++) {
        for (let y = 0; y < rows; y++) {
          output.set(
            mapped.subarray(z * sourceImagePitch + y * bytesPerRow, z * sourceImagePitch + y * bytesPerRow + rowBytes),
            z * outputImagePitch + y * rowBytes,
          );
        }
      }
      return output;
    } finally {
      if (readback.mapState === 'mapped') readback.unmap();
      readback.destroy();
    }
  }

  /** Read a supported typed texture, optionally through a D3D11 view. */
  async readBackTexture(id) {
    const view = this.views.get(id);
    const resourceId = view?.resource ?? id;
    const texture = this.res.get(resourceId);
    if (!texture || texture.kind !== 'texture') throw new Error(`texture ${resourceId} is unavailable`);
    const mip = view?.mip ?? 0;
    const slice = view && (view.dim === 5 || view.dim === 4 && view.slices > 1) ? view.slice : 0;
    const format = view ? this.viewFormat(view.format, texture) : texture.format;
    const scalar32 = ['r32float', 'r32uint', 'r32sint'].includes(format);
    const scalarArray = format === 'r32uint' ? Uint32Array : format === 'r32sint' ? Int32Array : Float32Array;
    const rgba32float = format === 'rgba32float';
    const rgba16float = format === 'rgba16float';
    const rgba8 = ['rgba8unorm', 'rgba8unorm-srgb', 'bgra8unorm', 'bgra8unorm-srgb'].includes(format);
    const rgba8Integer = format === 'rgba8uint' || format === 'rgba8sint';
    if (!scalar32 && !rgba32float && !rgba16float && !rgba8 && !rgba8Integer) {
      throw new Error(`texture readback does not support format ${format}`);
    }
    const width = Math.max(1, texture.width >> mip);
    const height = Math.max(1, texture.height >> mip);
    const bytesPerTexel = rgba32float ? 16 : rgba16float ? 8 : 4;
    const bytesPerRow = (width * bytesPerTexel + 255) & ~255;
    const buffer = this.device.createBuffer({
      size: bytesPerRow * height,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });
    this.flush();
    const encoder = this.device.createCommandEncoder();
    encoder.copyTextureToBuffer(
      { texture: texture.gpu, mipLevel: mip, origin: { x: 0, y: 0, z: slice } },
      { buffer, bytesPerRow, rowsPerImage: height },
      { width, height, depthOrArrayLayers: 1 },
    );
    this.device.queue.submit([encoder.finish()]);
    await buffer.mapAsync(GPUMapMode.READ);
    const bytes = new Uint8Array(buffer.getMappedRange());
    let data;
    if (rgba32float) {
      data = new Float32Array(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new Float32Array(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
      }
    } else if (rgba16float) {
      data = new Float32Array(width * height * 4);
      for (let row = 0; row < height; row++) {
        const halfs = new Uint16Array(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4);
        for (let i = 0; i < halfs.length; i++) {
          const half = halfs[i];
          const sign = half & 0x8000 ? -1 : 1;
          const exponent = (half >>> 10) & 0x1f;
          const fraction = half & 0x03ff;
          const value = exponent === 0
            ? sign * Math.pow(2, -14) * (fraction / 1024)
            : exponent === 0x1f
              ? (fraction ? NaN : sign * Infinity)
              : sign * Math.pow(2, exponent - 15) * (1 + fraction / 1024);
          data[row * width * 4 + i] = value;
        }
      }
    } else if (scalar32) {
      data = new scalarArray(width * height);
      for (let row = 0; row < height; row++) {
        data.set(new scalarArray(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width), row * width);
      }
    } else if (rgba8Integer) {
      const ArrayType = format === 'rgba8uint' ? Uint8Array : Int8Array;
      data = new ArrayType(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new ArrayType(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
      }
    } else {
      data = new Uint8Array(width * height * 4);
      const bgra = format.startsWith('bgra');
      for (let y = 0; y < height; y++) {
        const row = bytes.subarray(y * bytesPerRow, y * bytesPerRow + width * 4);
        if (!bgra) {
          data.set(row, y * width * 4);
        } else {
          for (let x = 0; x < width; x++) {
            const src = x * 4, dst = (y * width + x) * 4;
            data[dst] = row[src + 2];
            data[dst + 1] = row[src + 1];
            data[dst + 2] = row[src];
            data[dst + 3] = row[src + 3];
          }
        }
      }
    }
    buffer.unmap();
    buffer.destroy();
    return { id, resourceId, width, height, format, data };
  }
}
