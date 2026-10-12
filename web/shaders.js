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

// D3D11 typed UAV declarations do not carry their DXGI format in the scanned binding. The
// supported formats are inferred from the resource return type or supplied by the bound view, then
// specialized before Naga validates the formatless SPIR-V storage image for WebGPU.
function specializeSingleTypedUavImage(spirv, descriptors, dxgiFormat) {
  const uavs = descriptors.filter((d) => d.type === D.UAV);
  if (uavs.length !== 1) return spirv;
  const formats = {
    2: { dataType: 5, spirvFormat: 1 }, // R32G32B32A32_FLOAT
    10: { dataType: 5, spirvFormat: 2 }, // R16G16B16A16_FLOAT
    28: { dataType: 5, spirvFormat: 4 }, // R8G8B8A8_UNORM
    30: { dataType: 4, spirvFormat: 32 }, // R8G8B8A8_UINT
    32: { dataType: 3, spirvFormat: 23 }, // R8G8B8A8_SINT
    41: { dataType: 5, spirvFormat: 3 }, // R32_FLOAT
    42: { dataType: 4, spirvFormat: 33 }, // R32_UINT
    43: { dataType: 3, spirvFormat: 24 }, // R32_SINT
  };
  const inferredFormat = { 3: 43, 4: 42, 5: 41 }[uavs[0].dataType];
  const selected = formats[dxgiFormat ?? inferredFormat];
  if (!selected || selected.dataType !== uavs[0].dataType) return spirv;
  const spirvFormat = selected.spirvFormat;
  if (spirvFormat === undefined) return spirv;

  const bytes = new Uint8Array(spirv);
  const words = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 20 || words.getUint32(0, true) !== 0x07230203) return spirv;

  let storageImage = false;
  for (let at = 20; at < bytes.byteLength;) {
    const first = words.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount * 4 > bytes.byteLength) return spirv;
    if (opcode === 25 && wordCount >= 9 && words.getUint32(at + 7 * 4, true) === 2 && words.getUint32(at + 8 * 4, true) === 0) {
      words.setUint32(at + 8 * 4, spirvFormat, true); // OpTypeImage Unknown -> scalar R32 format
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

// WebGPU has no buffer-texture binding, and Naga rejects SPIR-V DimBuffer.
// Convert buffer images to runtime storage arrays; D3D buffer views already
// supply the exact byte range through a per-view GPUBuffer binding.
function lowerBufferImagesToStorageBuffers(spirv) {
  const bytes = new Uint8Array(spirv);
  if (bytes.byteLength < 20 || bytes.byteLength % 4 !== 0) return spirv;
  const input = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (input.getUint32(0, true) !== 0x07230203) return spirv;

  const instructions = [];
  const imageTypes = new Map(), pointerTypes = new Map(), variables = new Map();
  const scalarTypes = new Map(), vectorTypes = new Map(), constants = new Map();
  let hasFormatlessNonBufferStorageImage = false;
  for (let at = 20; at < bytes.byteLength;) {
    const first = input.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount * 4 > bytes.byteLength) return spirv;
    const inst = Array.from({ length: wordCount }, (_, i) => input.getUint32(at + 4 * i, true));
    instructions.push(inst);
    if (opcode === 25 && wordCount >= 9) {
      if (inst[3] === 5) imageTypes.set(inst[1], { id: inst[1], sampledType: inst[2], format: inst[8] });
      else if (inst[7] === 2 && inst[8] === 0) hasFormatlessNonBufferStorageImage = true;
    }
    if (opcode === 21 && wordCount === 4) scalarTypes.set(inst[1], { kind: 'int', width: inst[2], signed: inst[3] });
    if (opcode === 22 && wordCount === 3) scalarTypes.set(inst[1], { kind: 'float', width: inst[2] });
    if (opcode === 23 && wordCount === 4) vectorTypes.set(inst[1], { componentType: inst[2], count: inst[3] });
    if (opcode === 32 && wordCount === 4) pointerTypes.set(inst[1], { storage: inst[2], pointee: inst[3] });
    if (opcode === 59 && wordCount >= 4) variables.set(inst[2], { pointerType: inst[1], storage: inst[3] });
    if (opcode === 43 && wordCount === 4) {
      const key = `${inst[1]}:${inst[3] >>> 0}`;
      constants.set(key, inst[2]);
    }
    at += wordCount * 4;
  }
  if (!imageTypes.size) return spirv;

  let nextId = input.getUint32(12, true);
  const infoByType = new Map();
  const componentCount = new Map([[0, 1], [1, 4], [3, 1], [6, 2], [21, 4], [24, 1], [25, 2], [30, 4], [33, 1], [34, 2]]);
  for (const info of imageTypes.values()) {
    const components = componentCount.get(info.format);
    if (components === undefined || !scalarTypes.has(info.sampledType)) return spirv;
    info.components = components;
    info.arrayType = nextId++;
    info.structType = nextId++;
    info.elementType = info.sampledType;
    if (components > 1) {
      const existing = [...vectorTypes].find(([, v]) => v.componentType === info.sampledType && v.count === components);
      if (existing) info.elementType = existing[0];
      else {
        info.elementType = nextId++;
        info.vectorTypeDecl = [(4 << 16) | 23, info.elementType, info.sampledType, components];
      }
    }
    info.elementPointer = nextId++;
    info.pointerTypeIds = [];
    info.variables = [];
    infoByType.set(info.id, info);
  }

  const infoByPointer = new Map();
  for (const [pointerId, pointer] of pointerTypes) {
    const info = pointer.storage === 0 ? infoByType.get(pointer.pointee) : null;
    if (info) {
      infoByPointer.set(pointerId, info);
      info.pointerTypeIds.push(pointerId);
    }
  }
  if (![...infoByType.values()].some((info) => info.pointerTypeIds.length)) return spirv;

  const infoByVariable = new Map();
  for (const [variableId, variable] of variables) {
    const info = infoByPointer.get(variable.pointerType);
    if (info) {
      infoByVariable.set(variableId, info);
      info.variables.push(variableId);
    }
  }
  const infoByImageValue = new Map();
  for (const inst of instructions) {
    const opcode = inst[0] & 0xffff;
    if (opcode === 61 && inst.length >= 4) {
      const info = infoByType.get(inst[1]);
      if (info && infoByVariable.has(inst[3])) infoByImageValue.set(inst[2], { info, variableId: inst[3] });
    }
  }

  const newDecorations = [];
  const typeDeclsByPointer = new Map();
  for (const info of infoByType.values()) {
    const pointerId = info.pointerTypeIds[0];
    const declarations = [];
    if (info.vectorTypeDecl) declarations.push(info.vectorTypeDecl);
    declarations.push([(3 << 16) | 29, info.arrayType, info.elementType]); // OpTypeRuntimeArray
    declarations.push([(3 << 16) | 30, info.structType, info.arrayType]); // OpTypeStruct
    declarations.push([(4 << 16) | 32, info.elementPointer, 12, info.elementType]); // OpTypePointer StorageBuffer element
    typeDeclsByPointer.set(pointerId, declarations);
    newDecorations.push(
      [(4 << 16) | 71, info.arrayType, 6, info.components * 4], // ArrayStride
      [(3 << 16) | 71, info.structType, 2], // Block
      [(5 << 16) | 72, info.structType, 0, 35, 0], // MemberDecorate Offset 0
    );
  }

  let zeroIndexId = 0, zeroIndexType = 0;
  for (const [typeId, type] of scalarTypes) {
    if (type.kind === 'int' && type.width === 32) { zeroIndexType = typeId; break; }
  }
  if (!zeroIndexType) return spirv;
  zeroIndexId = constants.get(`${zeroIndexType}:0`) || 0;
  const newConstants = [];
  const ensureConstant = (typeId, value) => {
    const key = `${typeId}:${value >>> 0}`;
    let id = constants.get(key);
    if (!id) {
      id = nextId++;
      constants.set(key, id);
      newConstants.push([(4 << 16) | 43, typeId, id, value >>> 0]);
    }
    return id;
  };
  if (!zeroIndexId) zeroIndexId = ensureConstant(zeroIndexType, 0);
  for (const info of infoByType.values()) {
    const scalar = scalarTypes.get(info.sampledType);
    info.zeroValue = ensureConstant(info.sampledType, 0);
    info.oneValue = ensureConstant(info.sampledType, scalar.kind === 'float' ? 0x3f800000 : 1);
  }

  const valueTypes = new Map();
  for (const inst of instructions) {
    const opcode = inst[0] & 0xffff;
    if (([43, 44, 45, 61, 80, 81, 83].includes(opcode) || opcode >= 109 && opcode <= 200) && inst.length >= 3) valueTypes.set(inst[2], inst[1]);
  }
  const hasSameIntegerShape = (sourceTypeId, targetTypeId) => {
    const sourceVector = vectorTypes.get(sourceTypeId), targetVector = vectorTypes.get(targetTypeId);
    const sourceScalar = scalarTypes.get(sourceVector?.componentType || sourceTypeId);
    const targetScalar = scalarTypes.get(targetVector?.componentType || targetTypeId);
    return sourceScalar?.kind === 'int' && targetScalar?.kind === 'int'
      && sourceScalar.width === targetScalar.width
      && (sourceVector?.count || 1) === (targetVector?.count || 1);
  };
  const emittedTypeDeclarations = new Set();
  const output = Array.from({ length: 5 }, (_, i) => input.getUint32(i * 4, true));
  let annotationsEmitted = false;
  const pushInst = (inst) => output.push(...inst);
  const accessChain = (info, variableId, coordinateId) => {
    const pointerId = nextId++;
    return { pointerId, inst: [(6 << 16) | 65, info.elementPointer, pointerId, variableId, zeroIndexId, coordinateId] };
  };
  const extract = (typeId, compositeId, component) => {
    const resultId = nextId++;
    return { resultId, inst: [(5 << 16) | 81, typeId, resultId, compositeId, component] };
  };
  const vectorTypeFor = (typeId, count) => [...vectorTypes].find(([, v]) => v.componentType === typeId && v.count === count)?.[0]
    || [...infoByType.values()].find((i) => i.sampledType === typeId && i.components === count)?.elementType;

  for (const original of instructions) {
    const inst = original.slice(), opcode = inst[0] & 0xffff;
    if ([194, 195, 196].includes(opcode)) { // Normalize integer shift operand signedness for Naga.
      for (const operandIndex of [3, 4]) {
        const sourceId = inst[operandIndex], sourceTypeId = valueTypes.get(sourceId);
        if (!sourceTypeId || sourceTypeId === inst[1] || !hasSameIntegerShape(sourceTypeId, inst[1])) continue;
        const castId = nextId++;
        pushInst([(4 << 16) | 124, inst[1], castId, sourceId]); // OpBitcast to the shift result type.
        inst[operandIndex] = castId;
      }
    }
    if (!annotationsEmitted && opcode >= 19 && opcode <= 39) {
      for (const decoration of newDecorations) pushInst(decoration);
      annotationsEmitted = true;
    }
    if (opcode === 17 && (inst[1] === 46 || inst[1] === 47
        || (inst[1] === 55 || inst[1] === 56) && !hasFormatlessNonBufferStorageImage)) continue;
    if (opcode === 25 && infoByType.has(inst[1])) continue; // Remove the unsupported OpTypeImage.
    if (opcode === 71 && infoByVariable.has(inst[1]) && inst[2] === 25) continue; // WGSL storage buffers have no write-only access mode.
    if (opcode === 5 && infoByType.has(inst[1])) inst[1] = infoByType.get(inst[1]).structType; // OpName.
    if (opcode === 32 && infoByPointer.has(inst[1])) {
      const info = infoByPointer.get(inst[1]);
      if (!emittedTypeDeclarations.has(info.id)) {
        for (const declaration of typeDeclsByPointer.get(inst[1])) pushInst(declaration);
        emittedTypeDeclarations.add(info.id);
      }
      inst[2] = 12; inst[3] = info.structType; // UniformConstant image pointer -> StorageBuffer block pointer.
    }
    if (opcode === 59 && infoByVariable.has(inst[2])) inst[3] = 12; // OpVariable storage class.
    if (opcode === 61 && infoByType.has(inst[1])) continue; // Image handle load, replaced by the variable pointer.

    if ((opcode === 98 || opcode === 95) && inst.length >= 5) { // OpImageRead / OpImageFetch.
      const loaded = infoByImageValue.get(inst[3]);
      if (loaded) {
        const { info, variableId } = loaded, coordinateId = inst[4];
        const access = accessChain(info, variableId, coordinateId);
        pushInst(access.inst);
        const resultType = inst[1], resultId = inst[2];
        if (info.components === 4 && resultType === info.elementType) {
          pushInst([(4 << 16) | 61, resultType, resultId, access.pointerId]);
        } else {
          const loadedId = nextId++;
          pushInst([(4 << 16) | 61, info.elementType, loadedId, access.pointerId]);
          const scalarVector = vectorTypes.get(resultType);
          if (!scalarVector || scalarVector.count !== 4 || scalarVector.componentType !== info.sampledType) return spirv;
          const values = [];
          if (info.components === 1) values.push(loadedId, info.zeroValue, info.zeroValue, info.oneValue);
          else if (info.components === 2) {
            values.push(extract(info.sampledType, loadedId, 0), extract(info.sampledType, loadedId, 1));
            values.push(info.zeroValue, info.oneValue);
          } else return spirv;
          for (const item of values) if (typeof item !== 'number') pushInst(item.inst);
          const ids = values.map((item) => typeof item === 'number' ? item : item.resultId);
          pushInst([(ids.length + 3 << 16) | 80, resultType, resultId, ...ids]);
        }
        continue;
      }
    }
    if (opcode === 99 && inst.length >= 4) { // OpImageWrite.
      const loaded = infoByImageValue.get(inst[1]);
      if (loaded) {
        const { info, variableId } = loaded, coordinateId = inst[2], texelId = inst[3];
        const access = accessChain(info, variableId, coordinateId);
        pushInst(access.inst);
        let storedId = texelId;
        const texelType = valueTypes.get(texelId), texelVector = vectorTypes.get(texelType);
        if (info.components === 1 && texelVector) {
          const component = extract(info.sampledType, texelId, 0);
          pushInst(component.inst); storedId = component.resultId;
        } else if (info.components === 2 && texelVector?.count === 4) {
          const x = extract(info.sampledType, texelId, 0), y = extract(info.sampledType, texelId, 1);
          pushInst(x.inst); pushInst(y.inst);
          const typeId = vectorTypeFor(info.sampledType, 2);
          if (!typeId) return spirv;
          storedId = nextId++;
          pushInst([(5 << 16) | 80, typeId, storedId, x.resultId, y.resultId]);
        } else if (info.components === 4 && texelType !== info.elementType) {
          const ids = [];
          for (let component = 0; component < 4; component++) {
            const part = extract(info.sampledType, texelId, component);
            pushInst(part.inst); ids.push(part.resultId);
          }
          storedId = nextId++;
          pushInst([(7 << 16) | 80, info.elementType, storedId, ...ids]);
        }
        pushInst([(3 << 16) | 62, access.pointerId, storedId]); // OpStore.
        continue;
      }
    }

    pushInst(inst);
  }
  if (!annotationsEmitted) for (const decoration of newDecorations) output.unshift(...decoration);
  if (newConstants.length) {
    const firstFunction = output.findIndex((word) => (word & 0xffff) === 54);
    if (firstFunction >= 0) output.splice(firstFunction, 0, ...newConstants.flat());
  }
  // StorageBuffer became a core storage class in SPIR-V 1.3.
  output[1] = Math.max(output[1], 0x00010300);
  let capabilityAt = 5, hasDynamicBufferIndexing = false;
  while (capabilityAt < output.length) {
    const first = output[capabilityAt], opcode = first & 0xffff, wordCount = first >>> 16;
    if (opcode !== 17) break;
    if (output[capabilityAt + 1] === 30) hasDynamicBufferIndexing = true;
    capabilityAt += wordCount;
  }
  if (!hasDynamicBufferIndexing) output.splice(capabilityAt, 0, (2 << 16) | 17, 30); // StorageBufferArrayDynamicIndexing.
  output[3] = nextId;
  const result = new Uint8Array(output.length * 4), resultView = new DataView(result.buffer);
  output.forEach((word, i) => resultView.setUint32(i * 4, word, true));
  return result;
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
  translate(dxbc, { uavFormat } = {}) {
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
    const bufferImageSpirv = lowerBufferImagesToStorageBuffers(normalizedSpirv);
    const wgsl = this.naga.spirv_to_wgsl(specializeSingleTypedUavImage(bufferImageSpirv, descriptors, uavFormat));
    return { spirv, wgsl, descriptors };
  }
}
