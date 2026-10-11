// Shader toolchain: HLSL -> DXBC (vkd3d-shader) and DXBC -> SPIR-V -> WGSL (vkd3d-shader + Naga).
// vkd3d-shader is LGPL-2.1+ and Naga is MIT/Apache-2.0; both are shipped as separate WASM modules
// (see web/shaders/licenses). Works in browsers/workers and Node.
//
// Descriptor layout used for every translated shader (fixed so the host can build bind groups
// without per-shader reflection round trips):
//   group 0: constant buffers   binding = b# register
//   group 1: shader resources   binding = t# register  (textures / buffers)
//   group 2: samplers           binding = s# register
//   group 3: UAVs               binding = u# register

const D = { SRV: 0, UAV: 1, CBV: 2, SAMPLER: 3 };
const GROUP_OF = { [D.CBV]: 0, [D.SRV]: 1, [D.SAMPLER]: 2, [D.UAV]: 3 };

export class ShaderTool {
  constructor(base = new URL('./shaders/', import.meta.url)) {
    this.base = base;
  }

  async init() {
    if (!this._init) {
      this._init = (async () => {
        const vk = new URL('vkd3d-shader.js', this.base).href;
        const ng = new URL('naga/winebrowser_shader_wgsl.js', this.base).href;
        const [vkMod, naga] = await Promise.all([import(vk), import(ng)]);
        this.vk = await vkMod.default({ locateFile: (p) => new URL(p, vk).href });
        // Naga's wasm-bindgen glue: in Node fetch() cannot read file: URLs, so pass bytes there.
        if (typeof process !== 'undefined' && process.versions?.node) {
          const { readFile } = await import('node:fs/promises');
          const { fileURLToPath } = await import('node:url');
          const bytes = await readFile(fileURLToPath(new URL('naga/winebrowser_shader_wgsl_bg.wasm', this.base)));
          await naga.default({ module_or_path: bytes });
        } else {
          await naga.default();
        }
        this.naga = naga;
      })();
    }
    await this._init;
  }

  _msg() {
    const p = this.vk._wb_messages_ptr();
    return p ? this.vk.UTF8ToString(p) : '';
  }

  _withBytes(bytes, fn) {
    const ptr = this.vk._malloc(bytes.length);
    this.vk.HEAPU8.set(bytes, ptr);
    try { return fn(ptr); } finally { this.vk._free(ptr); }
  }

  _result() {
    const p = this.vk._wb_result_ptr(), n = this.vk._wb_result_size();
    return this.vk.HEAPU8.slice(p, p + n);
  }

  /** Compile HLSL source to DXBC (SM4/SM5). */
  compileHlsl(source, entry, profile, name = 'shader.hlsl') {
    const enc = new TextEncoder();
    const parts = [enc.encode(source), ...[entry, profile, name].map((s) => enc.encode(s + '\0'))];
    const ptrs = parts.map((b) => {
      const p = this.vk._malloc(b.length);
      this.vk.HEAPU8.set(b, p);
      return p;
    });
    try {
      if (this.vk._wb_hlsl_compile(ptrs[0], parts[0].length, ptrs[1], ptrs[2], ptrs[3]) !== 1) throw new Error('HLSL: ' + this._msg());
      return this._result();
    } finally {
      this.vk._wb_clear();
      ptrs.forEach((p) => this.vk._free(p));
    }
  }

  /** Descriptors the shader declares: [{type, space, register, resourceType, count}]. */
  scan(dxbc) {
    return this._withBytes(dxbc, (p) => {
      if (this.vk._wb_dxbc_scan(p, dxbc.length) !== 1) throw new Error('DXBC scan: ' + this._msg());
      const n = this.vk._wb_dxbc_scan_count(), at = this.vk._wb_dxbc_scan_results();
      const w = new Uint32Array(this.vk.HEAPU8.buffer.slice(at, at + n * 28));
      const out = [];
      for (let i = 0; i < n; i++) out.push({ type: w[i * 7], space: w[i * 7 + 1], register: w[i * 7 + 2], resourceType: w[i * 7 + 3], dataType: w[i * 7 + 4], count: w[i * 7 + 6] || 1 });
      return out;
    });
  }

  /** DXBC -> { spirv, wgsl, descriptors } using the fixed descriptor layout above. */
  translate(dxbc) {
    const descriptors = this.scan(dxbc);
    const rec = new Uint32Array(descriptors.length * 7);
    descriptors.forEach((d, i) => rec.set([d.type, d.space, d.register, d.resourceType, GROUP_OF[d.type], d.register, d.count], i * 7));
    const spirv = this._withBytes(dxbc, (p) => {
      const tbl = rec.length ? this.vk._malloc(rec.byteLength) : 0;
      if (tbl) this.vk.HEAPU8.set(new Uint8Array(rec.buffer), tbl);
      try {
        if (this.vk._wb_dxbc_compile_bound(p, dxbc.length, tbl, descriptors.length) !== 1) throw new Error('DXBC->SPIR-V: ' + this._msg());
        return this._result();
      } finally { if (tbl) this.vk._free(tbl); this.vk._wb_clear(); }
    });
    const wgsl = this.naga.spirv_to_wgsl(spirv);
    return { spirv, wgsl, descriptors };
  }
}
