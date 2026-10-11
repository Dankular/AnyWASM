// Shader toolchain: HLSL -> DXBC (vkd3d-shader) and DXBC -> SPIR-V -> WGSL (vkd3d-shader + Naga).
// vkd3d-shader is LGPL-2.1+ and Naga is MIT/Apache-2.0; both are shipped as separate WASM modules
// (see web/shaders/licenses). Works in browsers/workers and Node.
//
// Descriptor layout used for every translated shader (fixed so the host can build bind groups
// without per-shader reflection round trips):
//   group 0: constant buffers   binding = b# register
//   group 1: shader resources   binding = t# register  (textures / buffers)
//   group 2: samplers           binding = s# register
//   group 3: draw parameters     binding = 0 (Naga's hidden 4 x i32 uniform)
//            UAVs               binding = u# register + 1

const D = { SRV: 0, UAV: 1, CBV: 2, SAMPLER: 3 };
const GROUP_OF = { [D.CBV]: 0, [D.SRV]: 1, [D.SAMPLER]: 2, [D.UAV]: 3 };

// Naga 30 rejects non-finite SPIR-V float literals. DXBC uses untyped temp
// registers, so vkd3d can encode an integer register value such as -8 as an
// f32 NaN bit pattern before an integer operation. Re-express constants that
// are only stored to f32 pointers as integer constants, then load and bitcast
// them inside the function. The load prevents Naga from folding the bitcast
// back into an unrepresentable WGSL float literal.
function deferNonFiniteF32StoreConstants(spirv) {
  const bytes = new Uint8Array(spirv);
  if (bytes.byteLength < 20 || bytes.byteLength % 4 !== 0) return spirv;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== 0x07230203) return spirv;

  const words = Array.from({ length: bytes.byteLength / 4 }, (_, i) => view.getUint32(i * 4, true));
  const floatTypes = new Map(), intTypes = [], pointerTypes = new Map(), resultTypes = new Map();
  const constants = [], stores = [], entryBlocks = new Map();
  let functionId = 0, firstConstantAt = -1;
  for (let at = 5; at < words.length;) {
    const first = words[at], wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount > words.length) return spirv;
    if (opcode === 22 && wordCount === 3) floatTypes.set(words[at + 1], words[at + 2]); // OpTypeFloat
    if (opcode === 21 && wordCount === 4) intTypes.push({ id: words[at + 1], width: words[at + 2], signed: words[at + 3] }); // OpTypeInt
    if (opcode === 32 && wordCount === 4) pointerTypes.set(words[at + 1], { storage: words[at + 2], pointee: words[at + 3] }); // OpTypePointer
    if ([59, 65, 66, 67, 83].includes(opcode) && wordCount >= 3) resultTypes.set(words[at + 2], words[at + 1]);
    if (opcode === 43 && wordCount === 4) { // OpConstant with an f32 bit pattern
      if (firstConstantAt < 0) firstConstantAt = at;
      const typeId = words[at + 1], bits = words[at + 3];
      if (floatTypes.get(typeId) === 32 && (bits & 0x7f800000) === 0x7f800000) {
        constants.push({ at, id: words[at + 2], typeId });
      }
    }
    if (opcode === 54 && wordCount >= 3) functionId = words[at + 2]; // OpFunction
    if (opcode === 248 && wordCount === 2 && functionId && !entryBlocks.has(functionId)) entryBlocks.set(functionId, at); // first OpLabel
    if (opcode === 62 && wordCount >= 3) stores.push({ at, pointerId: words[at + 1], objectId: words[at + 2], functionId }); // OpStore
    if (opcode === 56) functionId = 0; // OpFunctionEnd
    at += wordCount;
  }
  if (!constants.length || firstConstantAt < 0 || !intTypes.some((t) => t.width === 32)) return spirv;

  const intType = intTypes.find((t) => t.width === 32 && t.signed === 1) || intTypes.find((t) => t.width === 32);
  const insertions = new Map();
  const insertionAt = (at) => {
    let group = insertions.get(at);
    if (!group) { group = { types: [], variables: [], initializers: [], operations: [] }; insertions.set(at, group); }
    return group;
  };
  let nextId = words[3];
  const intPointer = [...pointerTypes.entries()].find(([, type]) => type.storage === 7 && type.pointee === intType.id);
  const intPointerTypeId = intPointer ? intPointer[0] : nextId++;
  if (!intPointer) insertionAt(firstConstantAt).types.push((4 << 16) | 32, intPointerTypeId, 7, intType.id); // OpTypePointer Function i32

  for (const constant of constants) {
    const matchingStores = stores.filter((s) => s.objectId === constant.id);
    if (!matchingStores.length || matchingStores.some((s) => pointerTypes.get(resultTypes.get(s.pointerId))?.pointee !== constant.typeId)) continue;
    const functions = new Set(matchingStores.map((s) => s.functionId));
    if (functions.size !== 1 || functions.has(0)) continue;

    // Do not rewrite constants that also participate in some other SPIR-V
    // construct (including composite constants or arithmetic operations).
    const recognized = new Set([constant.at + 2, ...matchingStores.map((s) => s.at + 2)]);
    let hasOtherUse = false;
    for (let i = 5; i < words.length; i++) {
      if (words[i] === constant.id && !recognized.has(i)) { hasOtherUse = true; break; }
    }
    if (hasOtherUse) continue;

    const entryLabel = entryBlocks.get(matchingStores[0].functionId);
    if (entryLabel === undefined) continue;
    const variableAt = entryLabel + 2;
    let initializerAt = variableAt;
    while (initializerAt < words.length) {
      const first = words[initializerAt], wordCount = first >>> 16, opcode = first & 0xffff;
      if (opcode !== 59 || wordCount < 4) break;
      initializerAt += wordCount;
    }

    const variableId = nextId++;
    insertionAt(variableAt).variables.push((4 << 16) | 59, intPointerTypeId, variableId, 7); // OpVariable Function
    insertionAt(initializerAt).initializers.push((3 << 16) | 62, variableId, constant.id); // initialize the integer bits once
    words[constant.at + 1] = intType.id;
    for (const store of matchingStores) {
      const loadId = nextId++, bitcastId = nextId++;
      words[store.at + 2] = bitcastId;
      insertionAt(store.at).operations.push(
        (4 << 16) | 61, intType.id, loadId, variableId, // OpLoad
        (4 << 16) | 124, constant.typeId, bitcastId, loadId, // OpBitcast
      );
    }
  }
  if (!nextId || ![...insertions.values()].some((g) => g.variables.length)) return spirv;

  for (const [at, group] of [...insertions.entries()].sort((a, b) => b[0] - a[0])) {
    words.splice(at, 0, ...group.types, ...group.variables, ...group.initializers, ...group.operations);
  }
  words[3] = nextId;
  const out = new Uint8Array(words.length * 4), outView = new DataView(out.buffer);
  words.forEach((word, i) => outView.setUint32(i * 4, word, true));
  return out;
}

// D3D11's typed float UAV declaration does not carry its DXGI format in the scanned binding.
// AnyWASM's selected D3D11 path advertises R32_FLOAT as its typed UAV format, so specialize one
// formatless float storage image to R32f before Naga validates it for WebGPU.
function specializeSingleFloatUavImage(spirv, descriptors) {
  const uavs = descriptors.filter((d) => d.type === D.UAV);
  if (uavs.length !== 1 || uavs[0].dataType !== 5) return spirv;

  const bytes = new Uint8Array(spirv);
  const words = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 20 || words.getUint32(0, true) !== 0x07230203) return spirv;

  let storageImage = false;
  for (let at = 20; at < bytes.byteLength;) {
    const first = words.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount * 4 > bytes.byteLength) return spirv;
    if (opcode === 25 && wordCount >= 9 && words.getUint32(at + 7 * 4, true) === 2 && words.getUint32(at + 8 * 4, true) === 0) {
      words.setUint32(at + 8 * 4, 3, true); // OpTypeImage Unknown -> R32f
      storageImage = true;
    }
    at += wordCount * 4;
  }
  if (!storageImage) return spirv;

  for (let at = 20; at < bytes.byteLength;) {
    const first = words.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (opcode === 17 && wordCount === 2) {
      const capability = words.getUint32(at + 4, true);
      if (capability === 55 || capability === 56) words.setUint32(at + 4, 1, true); // formatless storage -> Shader
    }
    at += wordCount * 4;
  }
  return bytes;
}

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
    descriptors.forEach((d, i) => {
      const binding = d.register + (d.type === D.UAV ? 1 : 0);
      rec.set([d.type, d.space, d.register, d.resourceType, GROUP_OF[d.type], binding, d.count], i * 7);
    });
    const spirv = this._withBytes(dxbc, (p) => {
      const tbl = rec.length ? this.vk._malloc(rec.byteLength) : 0;
      if (tbl) this.vk.HEAPU8.set(new Uint8Array(rec.buffer), tbl);
      try {
        if (this.vk._wb_dxbc_compile_bound(p, dxbc.length, tbl, descriptors.length) !== 1) throw new Error('DXBC->SPIR-V: ' + this._msg());
        return this._result();
      } finally { if (tbl) this.vk._free(tbl); this.vk._wb_clear(); }
    });
    const normalizedSpirv = deferNonFiniteF32StoreConstants(spirv);
    const wgsl = this.naga.spirv_to_wgsl(specializeSingleFloatUavImage(normalizedSpirv, descriptors));
    return { spirv, wgsl, descriptors };
  }
}
