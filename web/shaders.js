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

// vkd3d represents the fragment SV_Coverage output as a one-element array,
// while SPIR-V and WGSL define SampleMask as a scalar u32 output. Naga rejects
// the array-shaped BuiltIn variable, so lower its single element access to the
// scalar variable expected by the target. Leave unfamiliar shapes untouched.
function lowerArraySampleMaskOutput(spirv) {
  const bytes = new Uint8Array(spirv);
  if (bytes.byteLength < 20 || bytes.byteLength % 4 !== 0) return spirv;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== 0x07230203) return spirv;

  const words = Array.from({ length: bytes.byteLength / 4 }, (_, i) => view.getUint32(i * 4, true));
  const instructions = [], instructionOffsets = new Map(), intTypes = new Map(), constants = new Map();
  const arrayTypes = new Map(), pointerTypes = new Map(), builtins = new Set(), variables = new Map();
  let functionId = 0;
  for (let at = 5; at < words.length;) {
    const first = words[at], wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount > words.length) return spirv;
    const inst = words.slice(at, at + wordCount);
    instructions.push(inst);
    instructionOffsets.set(inst, at);
    if (opcode === 21 && wordCount === 4) intTypes.set(inst[1], { width: inst[2], signed: inst[3] }); // OpTypeInt
    if (opcode === 43 && wordCount === 4) constants.set(inst[2], { id: inst[2], type: inst[1], value: inst[3] }); // OpConstant
    if (opcode === 28 && wordCount === 4) arrayTypes.set(inst[1], { element: inst[2], length: inst[3] }); // OpTypeArray
    if (opcode === 32 && wordCount === 4) pointerTypes.set(inst[1], { storage: inst[2], pointee: inst[3] }); // OpTypePointer
    if (opcode === 71 && wordCount === 4 && inst[2] === 11 && inst[3] === 20) builtins.add(inst[1]); // BuiltIn SampleMask
    if (opcode === 54 && wordCount >= 3) functionId = inst[2]; // OpFunction
    if (opcode === 59 && wordCount >= 4) {
      variables.set(inst[2], { at, pointerType: inst[1], storage: inst[3], functionId });
    }
    if (opcode === 56) functionId = 0; // OpFunctionEnd
    at += wordCount;
  }

  const maskVariables = [...builtins].filter((id) => {
    const variable = variables.get(id), pointer = variable && pointerTypes.get(variable.pointerType);
    return variable?.storage === 3 && pointer && arrayTypes.has(pointer.pointee);
  });
  if (!maskVariables.length) return spirv;

  let nextId = words[3], changed = false;
  const rewrites = new Map(), removed = new Set(), newPointerTypes = new Map();
  const scalarPointers = new Map();
  for (const variableId of maskVariables) {
    const variable = variables.get(variableId), arrayPointer = pointerTypes.get(variable.pointerType);
    const array = arrayTypes.get(arrayPointer.pointee), length = constants.get(array.length);
    const integer = intTypes.get(length?.type);
    if (!array || !length || length.value !== 1 || integer?.width !== 32 || integer.signed !== 0
        || !intTypes.has(array.element) || intTypes.get(array.element).width !== 32
        || intTypes.get(array.element).signed !== 0) return spirv;

    const scalarTypeInst = instructions.find((inst) => (inst[0] & 0xffff) === 21 && inst[1] === array.element);
    if (!scalarTypeInst) return spirv;
    const insertionAt = instructionOffsets.get(scalarTypeInst) + scalarTypeInst.length;
    let scalarPointerId = scalarPointers.get(array.element);
    if (!scalarPointerId) {
      scalarPointerId = nextId++;
      scalarPointers.set(array.element, scalarPointerId);
      let declarations = newPointerTypes.get(insertionAt);
      if (!declarations) { declarations = []; newPointerTypes.set(insertionAt, declarations); }
      declarations.push([(4 << 16) | 32, scalarPointerId, 3, array.element]); // OpTypePointer Output u32
    }
    const variableInst = instructions.find((inst) => (inst[0] & 0xffff) === 59 && inst[2] === variableId);
    if (!variableInst || variableInst.length < 4 || variableInst[3] !== 3) return spirv;
    variableInst[1] = scalarPointerId;

    let foundAccess = false;
    for (const inst of instructions) {
      const opcode = inst[0] & 0xffff;
      if ((opcode !== 65 && opcode !== 66) || inst[3] !== variableId) continue; // AccessChain / InBoundsAccessChain
      const index = constants.get(inst[4]), indexType = intTypes.get(index?.type);
      if (inst.length !== 5 || index?.value !== 0 || indexType?.width !== 32) return spirv;
      const resultPointer = pointerTypes.get(inst[1]);
      if (!resultPointer || resultPointer.storage !== 3 || resultPointer.pointee !== array.element) return spirv;
      rewrites.set(inst[2], variableId);
      removed.add(inst);
      foundAccess = true;
    }
    if (!foundAccess) return spirv;
    changed = true;
  }
  if (!changed) return spirv;

  for (const inst of instructions) {
    if (removed.has(inst)) continue;
    const opcode = inst[0] & 0xffff;
    for (const [alias, variableId] of rewrites) {
      if (!inst.slice(1).includes(alias)) continue;
      if (opcode === 61 && inst[3] === alias) inst[3] = variableId; // OpLoad pointer
      else if (opcode === 62 && inst[1] === alias) inst[1] = variableId; // OpStore pointer
      else if ((opcode === 63 || opcode === 64) && (inst[1] === alias || inst[2] === alias)) {
        if (inst[1] === alias) inst[1] = variableId;
        if (inst[2] === alias) inst[2] = variableId;
      } else return spirv; // Unknown use: preserve the original module.
    }
  }

  // Insert the new pointer type immediately after its scalar pointee type,
  // which can appear after unrelated globals in vkd3d's declaration order.
  const output = words.slice(0, 5);
  let offset = 5;
  for (const inst of instructions) {
    if (!removed.has(inst)) output.push(...inst);
    offset += inst.length;
    for (const type of newPointerTypes.get(offset) || []) output.push(...type);
  }
  output[3] = nextId;
  const out = new Uint8Array(output.length * 4), outView = new DataView(out.buffer);
  output.forEach((word, i) => outView.setUint32(i * 4, word, true));
  return out;
}

// WebGPU has 1D WGSL sampled-texture types, but its native 1D texture resource is single-mip and
// cannot be an array. The GPU executor backs D3D11 1D resources with 2D textures (height 1), so
// lower SPIR-V 1D image types and coordinates to the matching 2D shape before Naga sees them.
function lowerOneDimensionalImagesToTwoDimensional(spirv) {
  const bytes = new Uint8Array(spirv);
  if (bytes.byteLength < 20 || bytes.byteLength % 4 !== 0) return spirv;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== 0x07230203) return spirv;

  const instructions = [];
  const imageTypes = new Map(), sampledImageTypes = new Map(), valueTypes = new Map();
  const scalarTypes = new Map(), vectorTypes = new Map(), constants = new Map();
  const typeResultOps = new Set([
    1, 12, 41, 42, 43, 44, 45, 46, 48, 49, 50, 51, 52, 55, 57, 59, 61,
    65, 66, 67, 68, 69, 70, 79, 80, 81, 82, 83, 84, 86,
  ]);
  let hasOneDimensionalImage = false;
  for (let at = 20; at < bytes.byteLength;) {
    const first = view.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (!wordCount || at + wordCount * 4 > bytes.byteLength) return spirv;
    const inst = Array.from({ length: wordCount }, (_, index) => view.getUint32(at + index * 4, true));
    instructions.push(inst);
    if (opcode === 21 && wordCount === 4) scalarTypes.set(inst[1], { kind: 'int', width: inst[2], signed: inst[3] });
    if (opcode === 22 && wordCount === 3) scalarTypes.set(inst[1], { kind: 'float', width: inst[2] });
    if (opcode === 23 && wordCount === 4) vectorTypes.set(inst[1], { componentType: inst[2], count: inst[3] });
    if (opcode === 25 && wordCount >= 9) {
      imageTypes.set(inst[1], { sampledType: inst[2], dimension: inst[3], arrayed: inst[5] !== 0 });
      if (inst[3] === 0) { inst[3] = 1; hasOneDimensionalImage = true; }
    }
    if (opcode === 27 && wordCount === 3) sampledImageTypes.set(inst[1], inst[2]);
    if (opcode === 43 && wordCount === 4) constants.set(`${inst[1]}:${inst[3] >>> 0}`, inst[2]);
    if ((typeResultOps.has(opcode) || opcode >= 109 && opcode <= 200 || opcode === 245) && inst.length >= 3) {
      valueTypes.set(inst[2], inst[1]);
    }
    at += wordCount * 4;
  }
  if (!hasOneDimensionalImage) return spirv;

  let nextId = view.getUint32(12, true);
  const newTypes = [], newConstants = [];
  const vectorTypeIds = new Map();
  for (const [typeId, vector] of vectorTypes) vectorTypeIds.set(`${vector.componentType}:${vector.count}`, typeId);
  const ensureVectorType = (componentType, count) => {
    const key = `${componentType}:${count}`;
    let id = vectorTypeIds.get(key);
    if (!id) {
      id = nextId++;
      vectorTypeIds.set(key, id);
      newTypes.push([(4 << 16) | 23, id, componentType, count]); // OpTypeVector
    }
    return id;
  };
  const ensureConstant = (typeId, value) => {
    const key = `${typeId}:${value >>> 0}`;
    let id = constants.get(key);
    if (!id) {
      id = nextId++;
      constants.set(key, id);
      newConstants.push([(4 << 16) | 43, typeId, id, value >>> 0]); // OpConstant
    }
    return id;
  };

  let changed = false, unsupported = false;
  const component = (id, typeId, index, before) => {
    const actualType = valueTypes.get(id);
    if (!actualType) return null;
    const vector = vectorTypes.get(actualType);
    if (!vector) return actualType === typeId && index === 0 ? id : null;
    if (vector.count <= index || vector.componentType !== typeId) return null;
    const result = nextId++;
    before.push([(5 << 16) | 81, typeId, result, id, index]); // OpCompositeExtract
    return result;
  };
  const construct = (typeId, components, before) => {
    const resultType = ensureVectorType(typeId, components.length), result = nextId++;
    before.push([((components.length + 3) << 16) | 80, resultType, result, ...components]); // OpCompositeConstruct
    valueTypes.set(result, resultType);
    return result;
  };
  const lowerCoordinate = (id, typeId, arrayed, projected, before) => {
    const x = component(id, typeId, 0, before);
    if (x === null) return null;
    const scalar = scalarTypes.get(typeId);
    const rowCenter = scalar?.kind === 'float' && scalar.width === 32 ? 0x3f000000 : 0;
    const components = [x, ensureConstant(typeId, rowCenter)];
    if (arrayed) {
      const layer = component(id, typeId, 1, before);
      if (layer === null) return null;
      components.push(layer);
    }
    if (projected) {
      const q = component(id, typeId, arrayed ? 2 : 1, before);
      if (q === null) return null;
      components.push(q);
    }
    return construct(typeId, components, before);
  };
  const lowerSpatialOperand = (id, typeId, before) => {
    const x = component(id, typeId, 0, before);
    if (x === null) return null;
    return construct(typeId, [x, ensureConstant(typeId, 0)], before);
  };
  const imageOperandCounts = new Map([
    [1, 1], [2, 1], [4, 2], [8, 1], [16, 1], [32, 1], [64, 1], [128, 1],
    [256, 1], [512, 1], [32768, 1],
  ]);
  const lowerImageOperands = (inst, maskIndex, typeId, before) => {
    if (maskIndex >= inst.length) return;
    const mask = inst[maskIndex];
    let operandIndex = maskIndex + 1;
    const offsetType = [...scalarTypes].find(([, type]) => type.kind === 'int' && type.width === 32 && type.signed === 1)?.[0]
      || [...scalarTypes].find(([, type]) => type.kind === 'int' && type.width === 32)?.[0];
    for (const [bit, count] of imageOperandCounts) {
      if (!(mask & bit)) continue;
      if (bit === 4) {
        for (let index = 0; index < count; index++) {
          const lowered = lowerSpatialOperand(inst[operandIndex + index], typeId, before);
          if (lowered === null) { unsupported = true; return; }
          inst[operandIndex + index] = lowered;
        }
      } else if (bit === 8 || bit === 16) {
        const lowered = offsetType === undefined ? null : lowerSpatialOperand(inst[operandIndex], offsetType, before);
        if (lowered === null) { unsupported = true; return; }
        inst[operandIndex] = lowered;
      } else if (bit === 32) {
        unsupported = true; // ConstOffsets is an array of dimension-specific vectors.
        return;
      }
      operandIndex += count;
    }
  };

  const output = [Array.from({ length: 5 }, (_, index) => view.getUint32(index * 4, true))];
  for (const original of instructions) {
    const inst = original.slice(), opcode = inst[0] & 0xffff;
    if (opcode === 103 || opcode === 104) { // OpImageQuerySizeLod / OpImageQuerySize
      const imageValue = inst[3];
      const image = imageTypes.get(valueTypes.get(imageValue));
      if (image?.dimension === 0) {
        const resultType = inst[1], resultId = inst[2];
        const resultVector = vectorTypes.get(resultType);
        const scalarType = resultVector?.componentType || resultType;
        const targetType = ensureVectorType(scalarType, image.arrayed ? 3 : 2);
        const targetId = nextId++;
        inst[1] = targetType;
        inst[2] = targetId;
        valueTypes.set(targetId, targetType);
        const tail = [];
        const widthId = nextId++;
        tail.push([(5 << 16) | 81, scalarType, widthId, targetId, 0]); // OpCompositeExtract width
        if (image.arrayed) {
          if (!resultVector || resultVector.count !== 2) { unsupported = true; break; }
          const layerId = nextId++;
          tail.push([(5 << 16) | 81, scalarType, layerId, targetId, 2]); // OpCompositeExtract array layers
          tail.push([(5 << 16) | 80, resultType, resultId, widthId, layerId]); // Repack 1DArray as (width, layers).
        } else {
          if (resultVector) { unsupported = true; break; }
          tail.push([(5 << 16) | 81, resultType, resultId, targetId, 0]);
        }
        valueTypes.set(resultId, resultType);
        output.push(inst, ...tail);
        changed = true;
        continue;
      }
    }
    let imageValue, coordinateIndex, maskIndex, projected = false;
    if (opcode >= 87 && opcode <= 98 || opcode === 105) {
      imageValue = inst[3]; coordinateIndex = 4; maskIndex = 5;
      projected = opcode >= 91 && opcode <= 94;
    } else if (opcode === 99) {
      imageValue = inst[1]; coordinateIndex = 2; maskIndex = 4;
    } else if (opcode === 143) { // OpImageTexelPointer
      imageValue = inst[3]; coordinateIndex = 4;
    }
    if (coordinateIndex !== undefined) {
      const valueType = valueTypes.get(imageValue);
      const imageType = sampledImageTypes.get(valueType) || valueType;
      const image = imageTypes.get(imageType);
      if (image?.dimension === 0) {
        const coordinate = inst[coordinateIndex];
        const coordinateType = valueTypes.get(coordinate);
        const coordinateVector = vectorTypes.get(coordinateType);
        const coordinateScalarType = coordinateVector?.componentType || coordinateType;
        if (!scalarTypes.has(coordinateScalarType)) { unsupported = true; break; }
        const before = [];
        const lowered = lowerCoordinate(coordinate, coordinateScalarType, image.arrayed, projected, before);
        if (lowered === null) { unsupported = true; break; }
        inst[coordinateIndex] = lowered;
        if (maskIndex !== undefined) lowerImageOperands(inst, maskIndex, coordinateScalarType, before);
        if (unsupported) break;
        output.push(...before, inst);
        changed = true;
        continue;
      }
    }
    output.push(inst);
  }
  if (unsupported || !changed) return unsupported ? spirv : bytes;

  let firstConstant = output.findIndex((inst, index) => index > 0 && [41, 42, 43, 44, 45, 46, 48, 49, 50, 51, 52].includes(inst[0] & 0xffff));
  if (firstConstant < 0) firstConstant = output.findIndex((inst, index) => index > 0 && [54, 59].includes(inst[0] & 0xffff));
  if (firstConstant < 0) return spirv;
  output.splice(firstConstant, 0, ...newTypes);
  let firstGlobal = output.findIndex((inst, index) => index > 0 && [54, 59].includes(inst[0] & 0xffff));
  if (firstGlobal < 0) firstGlobal = output.length;
  output.splice(firstGlobal, 0, ...newConstants);
  output[0][3] = nextId;
  const result = new Uint8Array(output.reduce((size, inst) => size + inst.length, 0) * 4), resultView = new DataView(result.buffer);
  let wordIndex = 0;
  for (const inst of output) for (const word of inst) resultView.setUint32(wordIndex++ * 4, word, true);
  return result;
}

// D3D11 typed UAV declarations do not carry their DXGI format in the scanned binding. The
// supported formats are inferred from the resource return type or supplied by the bound view, then
// specialized before Naga validates the formatless SPIR-V storage image for WebGPU.
function specializeSingleTypedUavImage(spirv, descriptors, dxgiFormat) {
  const uavs = descriptors.filter((d) => d.type === D.UAV);
  if (uavs.length !== 1) return spirv;
  const formats = {
    2: { dataType: 5, spirvFormat: 1 }, // R32G32B32A32_FLOAT
    3: { dataType: 4, spirvFormat: 30 }, // R32G32B32A32_UINT
    4: { dataType: 3, spirvFormat: 21 }, // R32G32B32A32_SINT
    10: { dataType: 5, spirvFormat: 2 }, // R16G16B16A16_FLOAT
    12: { dataType: 4, spirvFormat: 31 }, // R16G16B16A16_UINT
    14: { dataType: 3, spirvFormat: 22 }, // R16G16B16A16_SINT
    16: { dataType: 5, spirvFormat: 6 }, // R32G32_FLOAT
    17: { dataType: 4, spirvFormat: 35 }, // R32G32_UINT
    18: { dataType: 3, spirvFormat: 25 }, // R32G32_SINT
    28: { dataType: 5, spirvFormat: 4 }, // R8G8B8A8_UNORM
    30: { dataType: 4, spirvFormat: 32 }, // R8G8B8A8_UINT
    31: { dataType: 5, spirvFormat: 5 }, // R8G8B8A8_SNORM
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
  const needsExtendedFormats = [6, 22, 25, 31, 35].includes(spirvFormat);

  const bytes = new Uint8Array(spirv);
  const words = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 20 || words.getUint32(0, true) !== 0x07230203) return spirv;

  let storageImage = false;
  for (let at = 20; at < bytes.byteLength;) {
    const first = words.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (wordCount === 0 || at + wordCount * 4 > bytes.byteLength) return spirv;
    if (opcode === 25 && wordCount >= 9 && words.getUint32(at + 7 * 4, true) === 2 && words.getUint32(at + 8 * 4, true) === 0) {
      words.setUint32(at + 8 * 4, spirvFormat, true); // OpTypeImage Unknown -> selected DXGI format
      storageImage = true;
    }
    at += wordCount * 4;
  }
  if (!storageImage) return spirv;

  let extendedCapabilityInserted = false;
  let lastCapabilityEnd = 20;
  for (let at = 20; at < bytes.byteLength;) {
    const first = words.getUint32(at, true), wordCount = first >>> 16, opcode = first & 0xffff;
    if (opcode === 17 && wordCount === 2) {
      lastCapabilityEnd = at + wordCount * 4;
      const capability = words.getUint32(at + 4, true);
      if (capability === 49) extendedCapabilityInserted = true;
      if (capability === 55 || capability === 56) {
        const specializedCapability = needsExtendedFormats && !extendedCapabilityInserted ? 49 : 1;
        words.setUint32(at + 4, specializedCapability, true);
        extendedCapabilityInserted ||= specializedCapability === 49;
      }
    }
    at += wordCount * 4;
  }
  if (needsExtendedFormats && !extendedCapabilityInserted) {
    const extended = new Uint8Array(bytes.byteLength + 8);
    extended.set(bytes.subarray(0, lastCapabilityEnd));
    const capability = new DataView(extended.buffer);
    capability.setUint32(lastCapabilityEnd, (2 << 16) | 17, true); // OpCapability
    capability.setUint32(lastCapabilityEnd + 4, 49, true); // StorageImageExtendedFormats
    extended.set(bytes.subarray(lastCapabilityEnd), lastCapabilityEnd + 8);
    return extended;
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
    const normalizedSpirv = lowerArraySampleMaskOutput(deferNonFiniteF32StoreConstants(spirv));
    const twoDimensionalSpirv = lowerOneDimensionalImagesToTwoDimensional(normalizedSpirv);
    const bufferImageSpirv = lowerBufferImagesToStorageBuffers(twoDimensionalSpirv);
    const wgsl = this.naga.spirv_to_wgsl(specializeSingleTypedUavImage(bufferImageSpirv, descriptors, uavFormat));
    return { spirv, wgsl, descriptors };
  }
}
