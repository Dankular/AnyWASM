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
  SET_FIXED_FUNCTION: 45, CREATE_TEXTURE3D: 46, RESOLVE_TEXTURE: 47, D3D9_RENDER_STATE: 48, CLEAR_STATE: 49, CLEAR_UAV: 50, PUSH_STATE: 51, POP_STATE: 52, CREATE_TEXTURE1D: 53,
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
  85: 'rgba16float', 86: 'rgba16float', 115: 'rgba16float',
  87: 'bgra8unorm', 88: 'bgra8unorm', 90: 'bgra8unorm', 91: 'bgra8unorm-srgb', 92: 'bgra8unorm', 93: 'bgra8unorm-srgb',
  94: 'bc6h-rgb-ufloat', 95: 'bc6h-rgb-ufloat', 96: 'bc6h-rgb-float',
  97: 'bc7-rgba-unorm', 98: 'bc7-rgba-unorm', 99: 'bc7-rgba-unorm-srgb',
};
// DXGI formats expose the named channels to shaders; channels omitted by the
// view receive D3D's format defaults when a sampler supplies a border texel.
const TEX_CHANNELS = {
  1: 'rgba', 2: 'rgba', 3: 'rgba', 4: 'rgba',
  9: 'rgba', 10: 'rgba', 11: 'rgba', 12: 'rgba', 13: 'rgba', 14: 'rgba',
  15: 'rg', 16: 'rg', 17: 'rg', 18: 'rg',
  19: 'rg', 20: 'rg', 21: 'r', 22: 'g',
  23: 'rgba', 24: 'rgba', 25: 'rgba', 26: 'rgb',
  27: 'rgba', 28: 'rgba', 29: 'rgba', 30: 'rgba', 31: 'rgba', 32: 'rgba',
  33: 'rg', 34: 'rg', 35: 'rg', 36: 'rg', 37: 'rg', 38: 'rg',
  39: 'r', 40: 'r', 41: 'r', 42: 'r', 43: 'r',
  44: 'rg', 45: 'r', 46: 'r', 47: 'g',
  48: 'rg', 49: 'rg', 50: 'rg', 51: 'rg', 52: 'rg',
  53: 'r', 54: 'r', 55: 'r', 56: 'r', 57: 'r', 58: 'r', 59: 'r',
  60: 'r', 61: 'r', 62: 'r', 63: 'r', 64: 'r', 65: 'a',
  70: 'rgba', 71: 'rgba', 72: 'rgba', 73: 'rgba', 74: 'rgba', 75: 'rgba', 76: 'rgba', 77: 'rgba', 78: 'rgba',
  79: 'r', 80: 'r', 81: 'r', 82: 'rg', 83: 'rg', 84: 'rg',
  85: 'rgb', 86: 'rgba', 87: 'rgba', 88: 'rgb', 90: 'rgba', 91: 'rgba', 92: 'rgb', 93: 'rgb',
  94: 'rgb', 95: 'rgb', 96: 'rgb', 97: 'rgba', 98: 'rgba', 99: 'rgba', 115: 'rgba',
};
// DXGI packed BGR formats use the first named component in the least-significant bits.
// RGBA16_FLOAT is the portable rendering/sampling backing; conversion back to the packed DXGI
// texel happens only at the guest-visible staging boundary.
const LEGACY_PACKED_16 = {
  85: { r: [5, 11], g: [6, 5], b: [5, 0], a: null },
  86: { r: [5, 10], g: [5, 5], b: [5, 0], a: [1, 15] },
  115: { r: [4, 8], g: [4, 4], b: [4, 0], a: [4, 12] },
};
const TYPED_UAV_DATA_TYPES = { 2: 5, 3: 4, 4: 3, 10: 5, 12: 4, 14: 3, 16: 5, 17: 4, 18: 3, 28: 5, 30: 4, 31: 5, 32: 3, 41: 5, 42: 4, 43: 3 };
const UAV_CLEAR_FORMATS = {
  2: { channels: 4, bits: 32, type: 'float' }, 3: { channels: 4, bits: 32, type: 'uint' }, 4: { channels: 4, bits: 32, type: 'sint' },
  10: { channels: 4, bits: 16, type: 'float' }, 12: { channels: 4, bits: 16, type: 'uint' }, 14: { channels: 4, bits: 16, type: 'sint' },
  16: { channels: 2, bits: 32, type: 'float' }, 17: { channels: 2, bits: 32, type: 'uint' }, 18: { channels: 2, bits: 32, type: 'sint' },
  28: { channels: 4, bits: 8, type: 'unorm' }, 30: { channels: 4, bits: 8, type: 'uint' },
  31: { channels: 4, bits: 8, type: 'snorm' }, 32: { channels: 4, bits: 8, type: 'sint' },
  41: { channels: 1, bits: 32, type: 'float' }, 42: { channels: 1, bits: 32, type: 'uint' }, 43: { channels: 1, bits: 32, type: 'sint' },
};

const roundToEven = (value) => {
  const floor = Math.floor(value), fraction = value - floor;
  return fraction < 0.5 ? floor : fraction > 0.5 ? floor + 1 : floor % 2 === 0 ? floor : floor + 1;
};

function halfBitsToFloat(half) {
  const sign = half & 0x8000 ? -1 : 1;
  const exponent = (half >>> 10) & 0x1f;
  const fraction = half & 0x03ff;
  return exponent === 0
    ? sign * Math.pow(2, -14) * (fraction / 1024)
    : exponent === 0x1f
      ? (fraction ? NaN : sign * Infinity)
      : sign * Math.pow(2, exponent - 15) * (1 + fraction / 1024);
}

function unpackLegacyPacked16(value, dxgi) {
  const channels = LEGACY_PACKED_16[dxgi];
  return ['r', 'g', 'b', 'a'].map((channel) => {
    const spec = channels[channel];
    if (!spec) return 1;
    const [bits, shift] = spec;
    return ((value >>> shift) & ((1 << bits) - 1)) / ((1 << bits) - 1);
  });
}

function packLegacyPacked16(channels, dxgi) {
  const layout = LEGACY_PACKED_16[dxgi];
  let value = 0;
  for (const [index, channel] of ['r', 'g', 'b', 'a'].entries()) {
    const spec = layout[channel];
    if (!spec) continue;
    const [bits, shift] = spec;
    const max = (1 << bits) - 1;
    const normalized = Number.isNaN(channels[index]) ? 0 : Math.min(1, Math.max(0, channels[index]));
    value |= roundToEven(normalized * max) << shift;
  }
  return value;
}

function floatToHalfBits(value) {
  const bits = new Uint32Array(new Float32Array([value]).buffer)[0];
  const sign = (bits >>> 16) & 0x8000;
  const exponent = (bits >>> 23) & 0xff;
  const fraction = bits & 0x7fffff;
  if (exponent === 0xff) return sign | (fraction ? 0x7e00 : 0x7c00);
  let halfExponent = exponent - 127 + 15;
  if (halfExponent >= 31) return sign | 0x7c00;
  if (halfExponent <= 0) {
    if (halfExponent < -10) return sign;
    const significand = fraction | 0x800000;
    const shift = 14 - halfExponent;
    const truncated = significand >>> shift;
    const remainder = significand & ((1 << shift) - 1);
    const halfway = 1 << (shift - 1);
    return sign | (truncated + (remainder > halfway || remainder === halfway && (truncated & 1) ? 1 : 0));
  }
  let mantissa = fraction >>> 13;
  const remainder = fraction & 0x1fff;
  if (remainder > 0x1000 || remainder === 0x1000 && (mantissa & 1)) {
    mantissa++;
    if (mantissa === 0x400) {
      mantissa = 0;
      halfExponent++;
      if (halfExponent >= 31) return sign | 0x7c00;
    }
  }
  return sign | (halfExponent << 10) | mantissa;
}

function uavClearPattern(format, isFloat, uintValues, floatValues) {
  const info = UAV_CLEAR_FORMATS[format];
  if (!info || isFloat && !['float', 'unorm', 'snorm'].includes(info.type)) return null;
  const pattern = new Uint8Array(info.channels * info.bits / 8);
  const view = new DataView(pattern.buffer);
  for (let channel = 0; channel < info.channels; channel++) {
    const offset = channel * info.bits / 8;
    if (!isFloat) {
      const value = uintValues[channel] >>> 0;
      if (info.bits === 8) view.setUint8(offset, value & 0xff);
      else if (info.bits === 16) view.setUint16(offset, value & 0xffff, true);
      else view.setUint32(offset, value, true);
      continue;
    }
    if (info.type === 'float') {
      if (info.bits === 16) view.setUint16(offset, floatToHalfBits(floatValues[channel]), true);
      else view.setFloat32(offset, floatValues[channel], true);
    } else if (info.type === 'unorm') {
      const value = Number.isNaN(floatValues[channel]) ? 0 : floatValues[channel];
      const max = (2 ** info.bits) - 1;
      const normalized = Math.min(1, Math.max(0, value));
      const packed = roundToEven(normalized * max);
      if (info.bits === 8) view.setUint8(offset, packed);
      else view.setUint16(offset, packed, true);
    } else {
      const value = Number.isNaN(floatValues[channel]) ? 0 : floatValues[channel];
      const max = (2 ** (info.bits - 1)) - 1;
      const normalized = Math.min(1, Math.max(-1, value));
      const packed = roundToEven(normalized * max);
      if (info.bits === 8) view.setInt8(offset, packed);
      else view.setInt16(offset, packed, true);
    }
  }
  return pattern;
}

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
const depthAspectFormat = (f) => f === 'depth24plus-stencil8' ? 'depth24plus'
  : f === 'depth32float-stencil8' ? 'depth32float' : f;

const CMP = { 1: 'never', 2: 'less', 3: 'equal', 4: 'less-equal', 5: 'greater', 6: 'not-equal', 7: 'greater-equal', 8: 'always' };
const STENCIL_OP = { 1: 'keep', 2: 'zero', 3: 'replace', 4: 'increment-clamp', 5: 'decrement-clamp', 6: 'invert', 7: 'increment-wrap', 8: 'decrement-wrap' };
const BLEND_FACTOR = { 1: 'zero', 2: 'one', 3: 'src', 4: 'one-minus-src', 5: 'src-alpha', 6: 'one-minus-src-alpha', 7: 'dst-alpha', 8: 'one-minus-dst-alpha', 9: 'dst', 10: 'one-minus-dst', 11: 'src-alpha-saturated', 14: 'constant', 15: 'one-minus-constant', 16: 'src1', 17: 'one-minus-src1', 18: 'src1-alpha', 19: 'one-minus-src1-alpha' };
const BLEND_OP = { 1: 'add', 2: 'subtract', 3: 'reverse-subtract', 4: 'min', 5: 'max' };
function dualSourceFragmentWgsl(wgsl) {
  const entry = /@fragment\s+fn\s+\w+\s*\([^)]*\)\s*->\s*(\w+)/.exec(wgsl);
  if (!entry) return null;
  const structStart = wgsl.indexOf(`struct ${entry[1]} {`);
  if (structStart < 0) return null;
  const structEnd = wgsl.indexOf('\n}', structStart);
  if (structEnd < 0) return null;
  const bodyStart = wgsl.indexOf('\n', structStart) + 1;
  const body = wgsl.slice(bodyStart, structEnd);
  const locations = [...body.matchAll(/@location\((\d+)\)/g)].map((match) => match[1]);
  if (locations.length !== 2 || locations.filter((location) => location === '0').length !== 1
      || locations.filter((location) => location === '1').length !== 1) return null;
  const dualBody = body
    .replace('@location(0)', '@location(0) @blend_src(0)')
    .replace('@location(1)', '@location(0) @blend_src(1)');
  return `enable dual_source_blending;\n${wgsl.slice(0, bodyStart)}${dualBody}${wgsl.slice(structEnd)}`;
}
const ADDRESS = { 1: 'repeat', 2: 'mirror-repeat', 3: 'clamp-to-edge', 4: 'clamp-to-edge', 5: 'clamp-to-edge' };
const TOPOLOGY = { 1: 'point-list', 2: 'line-list', 3: 'line-strip', 4: 'triangle-list', 5: 'triangle-strip' };

function splitWgslArguments(text) {
  const args = [];
  let start = 0, parentheses = 0, brackets = 0, braces = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '(') parentheses++;
    else if (c === ')') parentheses--;
    else if (c === '[') brackets++;
    else if (c === ']') brackets--;
    else if (c === '{') braces++;
    else if (c === '}') braces--;
    else if (c === ',' && parentheses === 0 && brackets === 0 && braces === 0) {
      args.push({ text: text.slice(start, i), start, end: i });
      start = i + 1;
    }
  }
  args.push({ text: text.slice(start), start, end: text.length });
  return args;
}

function mirrorOnceHelper(dimension) {
  const coordType = dimension === 1 ? 'f32' : `vec${dimension}<f32>`;
  const maskType = dimension === 1 ? 'bool' : `vec${dimension}<bool>`;
  const zero = dimension === 1 ? '0.0' : `vec${dimension}<f32>(0.0)`;
  const one = dimension === 1 ? '1.0' : `vec${dimension}<f32>(1.0)`;
  return `fn mirror_once_${dimension}(coord: ${coordType}, mask: ${maskType}) -> ${coordType} {
  let zero = ${zero};
  let one = ${one};
  let mirroredOnce = abs(coord);
  return select(coord, clamp(mirroredOnce, zero, one), mask);
}
`;
}

function samplerAddressVariantKey(samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats = new Map()) {
  const samplers = [...samplerAddressModes]
    .filter(([, modes]) => modes?.some((mode) => mode === 4 || mode === 5))
    .sort(([a], [b]) => a - b)
    .map(([register, modes]) => [register, modes,
      modes.includes(4) ? samplerBorderColors.get(register) || [0, 0, 0, 0] : null,
      modes.includes(4) ? samplerSettings.get(register) || null : null]);
  if (!samplers.length) return '[]';
  return JSON.stringify([samplers, [...textureFormats].sort(([a], [b]) => a - b)]);
}

function borderSampleLevelWgsl(textureName, textureType, samplerIndex, addressModes, borderColor, settings, arrayTexture, textureFormat) {
  const kind = textureType.kind;
  const dimension = kind === 'texture_1d' ? 1
    : ['texture_2d', 'texture_2d_array', 'texture_depth_2d', 'texture_depth_2d_array'].includes(kind) ? 2
      : kind === 'texture_3d' ? 3 : 0;
  const depthTexture = kind.startsWith('texture_depth_');
  const componentType = depthTexture ? 'f32' : textureType.component;
  const comparison = !!settings?.comparison;
  if (!dimension || !['f32', 'i32', 'u32'].includes(componentType) || (comparison && !depthTexture)) return null;
  if (arrayTexture !== (kind === 'texture_2d_array' || kind === 'texture_depth_2d_array')) return null;

  const name = `border_sample_${textureName}_${samplerIndex}`;
  const coordType = dimension === 1 ? 'f32' : `vec${dimension}<f32>`;
  const resultType = depthTexture ? 'f32' : `vec4<${componentType}>`;
  const zero = depthTexture ? '0.0' : `vec4<${componentType}>(0.0)`;
  const lodBias = settings?.mipBias || 0;
  const minLod = settings?.lodMinClamp || 0;
  const maxLod = settings?.lodMaxClamp ?? 32;
  const floatLiteral = (value) => Number.isInteger(value) ? `${value}.0` : String(value);
  const channels = TEX_CHANNELS[textureFormat] || 'rgba';
  const formattedBorderColor = ['r', 'g', 'b', 'a'].map((channel, index) => channels.includes(channel)
    ? borderColor[index]
    : index === 3 ? 1 : 0);
  const borderValue = depthTexture
    ? floatLiteral(borderColor[0])
    : `vec4<${componentType}>(${formattedBorderColor.map((value) => componentType === 'f32' ? floatLiteral(value) : `${componentType}(${floatLiteral(value)})`).join(', ')})`;
  const compareOperator = {
    never: null,
    less: '<',
    equal: '==',
    'less-equal': '<=',
    greater: '>',
    'not-equal': '!=',
    'greater-equal': '>=',
    always: null,
  }[settings?.compare];
  const compareAlways = settings?.compare === 'always';
  const axes = ['x', 'y', 'z'].slice(0, dimension);
  const levelSize = dimension === 1
    ? `i32(textureDimensions(${textureName}, level))`
    : `vec${dimension}<i32>(textureDimensions(${textureName}, level))`;
  const asFloatSize = dimension === 1 ? 'f32(_size)' : `vec${dimension}<f32>(_size)`;
  const half = dimension === 1 ? '0.5' : `vec${dimension}<f32>(0.5)`;
  const indexExpr = (name, axis) => dimension === 1 ? name : `${name}.${axes[axis]}`;
  const coordParams = `coord: ${coordType}${arrayTexture ? ', layer: i32' : ''}`;
  const sampleArgs = (indices, levelName) => {
    const address = dimension === 1 ? indices[0] : `vec${dimension}<i32>(${indices.join(', ')})`;
    return arrayTexture
      ? `${textureName}, ${address}, layer, ${levelName}`
      : `${textureName}, ${address}, ${levelName}`;
  };
  const levelFunction = `${name}_at_level`;
  const makeAddress = (tap, axis, index) => {
    const size = dimension === 1 ? '_size' : `_size.${axes[axis]}`;
    const addressName = `_addr_${tap}_${axis}`;
    const lines = [];
    let expr;
    switch (addressModes[axis]) {
      case 1:
        expr = `((_index_${tap}_${axis} % ${size}) + ${size}) % ${size}`;
        break;
      case 2: {
        const period = `_period_${tap}_${axis}`, wrapped = `_wrapped_${tap}_${axis}`;
        lines.push(`let ${period} = ${size} * 2;`);
        lines.push(`let ${wrapped} = ((_index_${tap}_${axis} % ${period}) + ${period}) % ${period};`);
        expr = `select(${wrapped}, ${period} - 1 - ${wrapped}, ${wrapped} >= ${size})`;
        break;
      }
      case 5: {
        const mirrored = `_mirrored_${tap}_${axis}`;
        lines.push(`let ${mirrored} = select(_index_${tap}_${axis}, -_index_${tap}_${axis} - 1, _index_${tap}_${axis} < 0);`);
        expr = `clamp(${mirrored}, 0, ${size} - 1)`;
        break;
      }
      case 3:
      case 4:
      default:
        expr = `clamp(_index_${tap}_${axis}, 0, ${size} - 1)`;
        break;
    }
    lines.push(`let ${addressName} = ${expr};`);
    return { lines, addressName, size };
  };
  const makeTap = (tap, bits, linear) => {
    const lines = [];
    const indices = [];
    const borderTerms = [];
    const weights = [];
    for (let axis = 0; axis < dimension; axis++) {
      const base = indexExpr('_base', axis);
      const indexName = `_index_${tap}_${axis}`;
      lines.push(`let ${indexName} = ${base}${linear && bits[axis] ? ' + 1' : ''};`);
      indices.push(indexName);
      const { lines: addressLines, addressName, size } = makeAddress(tap, axis, indexName);
      lines.push(...addressLines);
      if (addressModes[axis] === 4) borderTerms.push(`(${indexName} < 0 || ${indexName} >= ${size})`);
      if (linear) {
        const fraction = indexExpr('_fraction', axis);
        weights.push(bits[axis] ? fraction : `(1.0 - ${fraction})`);
      }
      indices[axis] = addressName;
    }
    if (linear) lines.push(`let _weight_${tap} = ${weights.join(' * ')};`);
    const loaded = `textureLoad(${sampleArgs(indices, 'level')})`;
    const value = borderTerms.length ? `select(${loaded}, ${borderValue}, ${borderTerms.join(' || ')})` : loaded;
    let sampleValue = value;
    if (comparison) {
      const valueName = `_cmp_depth_${tap}`;
      const resultName = `_cmp_result_${tap}`;
      lines.push(`let ${valueName} = ${value};`);
      const expression = compareAlways ? 'true'
        : compareOperator ? `reference ${compareOperator} ${valueName}` : 'false';
      lines.push(`let ${resultName} = select(0.0, 1.0, ${expression});`);
      sampleValue = resultName;
    }
    if (linear) lines.push(`_sum = _sum + (${sampleValue}) * _weight_${tap};`);
    else lines.push(`return ${sampleValue};`);
    return lines;
  };
  const linearSampling = componentType === 'f32';
  const atLevelBody = [`let _size = ${levelSize};`];
  if (linearSampling) {
    atLevelBody.push(`if (linear) {`);
    atLevelBody.push(`  let _position = coord * ${asFloatSize} - ${half};`);
    atLevelBody.push(`  let _base = ${dimension === 1 ? 'i32(floor(_position))' : `vec${dimension}<i32>(floor(_position))`};`);
    atLevelBody.push(`  let _fraction = fract(_position);`);
    atLevelBody.push(`  var _sum: ${resultType} = ${zero};`);
    for (let tap = 0; tap < 1 << dimension; tap++) {
      const bits = axes.map((_, axis) => (tap >> axis) & 1);
      atLevelBody.push(...makeTap(tap, bits, true).map((line) => `  ${line}`));
    }
    atLevelBody.push('  return _sum;', '}');
  }
  atLevelBody.push(`let _base = ${dimension === 1 ? 'i32(floor(coord * f32(_size)))' : `vec${dimension}<i32>(floor(coord * ${asFloatSize}))`};`);
  atLevelBody.push(...makeTap('point', axes.map(() => 0), false));

  const compareParam = comparison ? ', reference: f32' : '';
  const params = `${coordParams}, level: i32, linear: bool${compareParam}`;
  const atLevel = `fn ${levelFunction}(${params}) -> ${resultType} {\n${atLevelBody.map((line) => `  ${line}`).join('\n')}\n}`;
  const mainParams = `${coordParams}, requestedLod: f32${compareParam}`;
  const mainBody = [
    `let _maxLevel = f32(textureNumLevels(${textureName}) - 1u);`,
    `let _lod = clamp(requestedLod + ${floatLiteral(lodBias)}, ${floatLiteral(minLod)}, min(${floatLiteral(maxLod)}, _maxLevel));`,
    `let _sampleLod = clamp(_lod, 0.0, _maxLevel);`,
    `let _linear = select(${settings?.minFilter === 'linear'}, ${settings?.magFilter === 'linear'}, _lod <= 0.0);`,
  ];
  if (settings?.mipmapFilter === 'linear' && componentType === 'f32') {
    mainBody.push(`let _mip0 = i32(floor(_sampleLod));`);
    mainBody.push(`let _mip1 = min(_mip0 + 1, i32(textureNumLevels(${textureName})) - 1);`);
    mainBody.push(`let _mipWeight = fract(_sampleLod);`);
    const atArgs = (mip) => `${arrayTexture ? 'coord, layer' : 'coord'}, ${mip}, _linear${comparison ? ', reference' : ''}`;
    mainBody.push(`return mix(${levelFunction}(${atArgs('_mip0')}), ${levelFunction}(${atArgs('_mip1')}), _mipWeight);`);
  } else {
    mainBody.push(`let _mip = i32(floor(_sampleLod + 0.5));`);
    mainBody.push(`return ${levelFunction}(${arrayTexture ? 'coord, layer' : 'coord'}, _mip, _linear${comparison ? ', reference' : ''});`);
  }
  return { name, source: `${atLevel}\nfn ${name}(${mainParams}) -> ${resultType} {\n${mainBody.map((line) => `  ${line}`).join('\n')}\n}` };
}

function borderSampleLodWgsl(name, textureName, textureType, arrayTexture, mode) {
  const dimension = textureType.kind === 'texture_1d' ? 1
    : ['texture_2d', 'texture_2d_array', 'texture_depth_2d', 'texture_depth_2d_array'].includes(textureType.kind) ? 2
      : textureType.kind === 'texture_3d' ? 3 : 0;
  if (!dimension) return null;
  const coordType = dimension === 1 ? 'f32' : `vec${dimension}<f32>`;
  const size = dimension === 1
    ? `f32(textureDimensions(${textureName}, 0))`
    : `vec${dimension}<f32>(textureDimensions(${textureName}, 0))`;
  const coordParams = `coord: ${coordType}${arrayTexture ? ', layer: i32' : ''}`;
  const callArgs = (lod) => `${arrayTexture ? 'coord, layer' : 'coord'}, ${lod}${mode === 'compare' ? ', reference' : ''}`;
  const gradientLength = (gradient) => dimension === 1 ? `abs(${gradient})` : `length(${gradient})`;
  let params = coordParams;
  let derivativeX, derivativeY, lodBias = '0.0';
  if (mode === 'grad') {
    params += `, derivativeX: ${coordType}, derivativeY: ${coordType}`;
    derivativeX = 'derivativeX'; derivativeY = 'derivativeY';
  } else {
    derivativeX = 'dpdx(coord)'; derivativeY = 'dpdy(coord)';
    if (mode === 'bias') {
      params += ', sourceBias: f32';
      lodBias = 'sourceBias';
    }
    if (mode === 'compare') params += ', reference: f32';
  }
  const source = `fn ${name}_${mode}(${params}) -> ${textureType.kind.startsWith('texture_depth_') ? 'f32' : `vec4<${textureType.component}>`} {
  let _size = ${size};
  let _dx = (${derivativeX}) * _size;
  let _dy = (${derivativeY}) * _size;
  let _lod = log2(max(${gradientLength('_dx')}, ${gradientLength('_dy')})) + ${lodBias};
  return ${name}(${callArgs('_lod')});
}`;
  return { name: `${name}_${mode}`, source };
}

function anisotropicBorderSampleWgsl(sampleName, textureName, textureType, samplerIndex,
  addressModes, borderColor, settings, arrayTexture, textureFormat, fixedLevelZero = false) {
  const comparison = !!settings?.comparison;
  const depthTexture = textureType.kind.startsWith('texture_depth_');
  if (!['texture_2d', 'texture_2d_array', 'texture_depth_2d', 'texture_depth_2d_array'].includes(textureType.kind)
      || arrayTexture !== (textureType.kind === 'texture_2d_array' || textureType.kind === 'texture_depth_2d_array')
      || comparison !== depthTexture || (!comparison && textureType.component !== 'f32')) return null;
  const maxAnisotropy = Math.max(1, Math.min(16, settings?.maxAnisotropy || 1));
  if (maxAnisotropy <= 1) return null;
  const channels = TEX_CHANNELS[textureFormat] || 'rgba';
  const borderValues = ['r', 'g', 'b', 'a'].map((channel, index) => channels.includes(channel)
    ? borderColor[index]
    : index === 3 ? 1 : 0);
  const floatLiteral = (value) => Number.isInteger(value) ? `${value}.0` : String(value);
  const borderValue = `vec4<f32>(${borderValues.map(floatLiteral).join(', ')})`;
  const compareOperator = {
    never: null,
    less: '<',
    equal: '==',
    'less-equal': '<=',
    greater: '>',
    'not-equal': '!=',
    'greater-equal': '>=',
    always: null,
  }[settings?.compare];
  const borderComparison = settings?.compare === 'always' ? 'true'
    : settings?.compare === 'never' ? 'false'
      : compareOperator ? `reference ${compareOperator} ${floatLiteral(borderColor[0])}` : 'false';
  const borderResult = comparison ? `select(0.0, 1.0, ${borderComparison})` : borderValue;
  const resultType = comparison ? 'f32' : 'vec4<f32>';
  const zero = comparison ? '0.0' : 'vec4<f32>(0.0)';
  const outside = addressModes.slice(0, 2).flatMap((mode, axis) => mode === 4
    ? [`(_tapCoord.${axis === 0 ? 'x' : 'y'} < 0.0)`, `(_tapCoord.${axis === 0 ? 'x' : 'y'} > 1.0)`]
    : []);
  const outsideExpr = outside.length ? outside.join(' || ') : 'false';
  const arrayParams = arrayTexture ? ', layer: i32' : '';
  const arrayArgs = arrayTexture ? ', layer' : '';
  const compareParam = comparison ? ', reference: f32' : '';
  const compareArg = comparison ? ', reference' : '';
  const sampleLod = fixedLevelZero ? '' : `let _lod = log2(max(_minorLength, 0.000001)) + sourceBias;`;
  const sampleCall = fixedLevelZero
    ? `${sampleName}(_tapCoord${arrayArgs}, 0, true${compareArg})`
    : `${sampleName}(_tapCoord${arrayArgs}, _lod${compareArg})`;
  const name = `aniso_border_sample_${textureName}_${samplerIndex}`;
  const source = `fn ${name}(coord: vec2<f32>${arrayParams}, derivativeX: vec2<f32>, derivativeY: vec2<f32>, sourceBias: f32${compareParam}) -> ${resultType} {
  let _size = vec2<f32>(textureDimensions(${textureName}, 0));
  let _lengthX = length(derivativeX * _size);
  let _lengthY = length(derivativeY * _size);
  let _majorLength = max(_lengthX, _lengthY);
  let _minorLength = min(_lengthX, _lengthY);
  let _majorGradient = select(derivativeY, derivativeX, _lengthX >= _lengthY);
  let _tapCount = clamp(i32(ceil(_majorLength / max(_minorLength, 0.000001))), 1, ${maxAnisotropy});
  ${sampleLod}
  var _sum: ${resultType} = ${zero};
  for (var _tap = 0; _tap < ${maxAnisotropy}; _tap = _tap + 1) {
    if (_tap < _tapCount) {
      let _offset = (f32(_tap) + 0.5) / f32(_tapCount) - 0.5;
      let _tapCoord = coord + _majorGradient * _offset;
      let _sample = ${sampleCall};
      _sum = _sum + select(_sample, ${borderResult}, ${outsideExpr});
    }
  }
  return _sum / f32(_tapCount);
}`;
  return { name, source };
}

function borderGatherWgsl(textureName, textureType, samplerIndex, addressModes, borderColor, settings, arrayTexture, textureFormat, component, comparison) {
  const kind = textureType.kind;
  const depthTexture = kind.startsWith('texture_depth_');
  if (!['texture_2d', 'texture_2d_array', 'texture_depth_2d', 'texture_depth_2d_array'].includes(kind)
      || arrayTexture !== (kind === 'texture_2d_array' || kind === 'texture_depth_2d_array')
      || settings?.anisotropic || (comparison !== !!settings?.comparison)
      || (comparison && !depthTexture) || (!comparison && depthTexture)
      || (!comparison && textureType.component !== 'f32')) return null;

  const name = `border_gather_${textureName}_${samplerIndex}`;
  const channels = TEX_CHANNELS[textureFormat] || 'rgba';
  const formattedBorderColor = ['r', 'g', 'b', 'a'].map((channel, index) => channels.includes(channel)
    ? borderColor[index]
    : index === 3 ? 1 : 0);
  const floatLiteral = (value) => Number.isInteger(value) ? `${value}.0` : String(value);
  const borderValue = depthTexture ? floatLiteral(borderColor[0])
    : `vec4<f32>(${formattedBorderColor.map(floatLiteral).join(', ')})[${component}]`;
  const compareOperator = {
    never: null,
    less: '<',
    equal: '==',
    'less-equal': '<=',
    greater: '>',
    'not-equal': '!=',
    'greater-equal': '>=',
    always: null,
  }[settings?.compare];
  const compareAlways = settings?.compare === 'always';
  const compareExpression = (depth) => compareAlways ? 'true'
    : compareOperator ? `reference ${compareOperator} ${depth}` : 'false';
  const axes = ['x', 'y'];
  const makeAddress = (tap, axis) => {
    const size = `_size.${axes[axis]}`;
    const index = `_index_${tap}_${axes[axis]}`;
    const address = `_addr_${tap}_${axes[axis]}`;
    let expression;
    const lines = [];
    switch (addressModes[axis]) {
      case 1:
        expression = `((${index} % ${size}) + ${size}) % ${size}`;
        break;
      case 2: {
        const period = `_period_${tap}_${axes[axis]}`;
        const wrapped = `_wrapped_${tap}_${axes[axis]}`;
        lines.push(`let ${period} = ${size} * 2;`);
        lines.push(`let ${wrapped} = ((${index} % ${period}) + ${period}) % ${period};`);
        expression = `select(${wrapped}, ${period} - 1 - ${wrapped}, ${wrapped} >= ${size})`;
        break;
      }
      case 5: {
        const mirrored = `_mirrored_${tap}_${axes[axis]}`;
        lines.push(`let ${mirrored} = select(${index}, -${index} - 1, ${index} < 0);`);
        expression = `clamp(${mirrored}, 0, ${size} - 1)`;
        break;
      }
      case 3:
      case 4:
      default:
        expression = `clamp(${index}, 0, ${size} - 1)`;
        break;
    }
    lines.push(`let ${address} = ${expression};`);
    return { lines, address, borderTerm: addressModes[axis] === 4 ? `(${index} < 0 || ${index} >= ${size})` : null };
  };
  const makeTap = (tap, xOffset, yOffset) => {
    const lines = [
      `let _index_${tap}_x = _base.x + ${xOffset};`,
      `let _index_${tap}_y = _base.y + ${yOffset};`,
    ];
    const x = makeAddress(tap, 0), y = makeAddress(tap, 1);
    lines.push(...x.lines, ...y.lines);
    const address = `vec2<i32>(${x.address}, ${y.address})`;
    const loaded = comparison
      ? `textureLoad(${textureName}, ${address}${arrayTexture ? ', layer' : ''}, 0)`
      : `textureLoad(${textureName}, ${address}${arrayTexture ? ', layer' : ''}, 0)[${component}]`;
    const borderTerms = [x.borderTerm, y.borderTerm].filter(Boolean);
    const value = borderTerms.length ? `select(${loaded}, ${borderValue}, ${borderTerms.join(' || ')})` : loaded;
    if (comparison) {
      lines.push(`let _depth_${tap} = ${value};`);
      lines.push(`let _value_${tap} = select(0.0, 1.0, ${compareExpression(`_depth_${tap}`)});`);
    } else {
      lines.push(`let _value_${tap} = ${value};`);
    }
    return lines;
  };
  const params = `coord: vec2<f32>${arrayTexture ? ', layer: i32' : ''}, offset: vec2<i32>${comparison ? ', reference: f32' : ''}`;
  const body = [
    `let _size = vec2<i32>(textureDimensions(${textureName}));`,
    'let _position = coord * vec2<f32>(_size) - vec2<f32>(0.5);',
    'let _base = vec2<i32>(floor(_position)) + offset;',
    ...makeTap('00', 0, 0),
    ...makeTap('10', 1, 0),
    ...makeTap('01', 0, 1),
    ...makeTap('11', 1, 1),
    'return vec4<f32>(_value_01, _value_11, _value_10, _value_00);',
  ];
  return { name, source: `fn ${name}(${params}) -> vec4<f32> {\n${body.map((line) => `  ${line}`).join('\n')}\n}` };
}

function samplerAddressSampleWgsl(wgsl, samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats = new Map()) {
  const textureTypes = new Map();
  const texturePattern = /\bvar\s+(\w+)\s*:\s*(texture_(?:1d|2d|2d_array|3d|cube|cube_array|depth_2d|depth_2d_array|depth_cube|depth_cube_array))(?:\s*<([^;]*)>)?\s*;/g;
  for (const match of wgsl.matchAll(texturePattern)) textureTypes.set(match[1], { kind: match[2], component: match[3]?.trim() });

  const callPattern = /\b(textureSample(?:BaseClampToEdge|CompareLevel|Compare|Level|Grad|Bias)?|textureGather(?:Compare)?)\s*\(/g;
  let result = '', cursor = 0, changed = false;
  const unsupported = new Set();
  const dimensions = new Set();
  const borderHelpers = new Map();
  for (const match of wgsl.matchAll(callPattern)) {
    const open = match.index + match[0].lastIndexOf('(');
    let depth = 1, close = open + 1;
    for (; close < wgsl.length && depth; close++) {
      if (wgsl[close] === '(') depth++;
      else if (wgsl[close] === ')') depth--;
    }
    if (depth) continue;
    close--;
    const args = splitWgslArguments(wgsl.slice(open + 1, close));
    if (args.length < 3) continue;
    const gatherCompare = match[1] === 'textureGatherCompare';
    const gather = gatherCompare || match[1] === 'textureGather';
    const textureArgIndex = gather ? (gatherCompare ? 0 : 1) : 0;
    const samplerArgIndex = gather ? (gatherCompare ? 1 : 2) : 1;
    const coordArgIndex = gather ? (gatherCompare ? 2 : 3) : 2;
    const textureName = args[textureArgIndex].text.trim(), samplerName = args[samplerArgIndex].text.trim();
    const samplerRegister = /^s(\d+)_$/.exec(samplerName);
    const samplerIndex = samplerRegister ? Number(samplerRegister[1]) : samplerName === 'd3d9Sampler' ? 0 : null;
    const addressModes = samplerIndex === null ? undefined : samplerAddressModes.get(samplerIndex);
    if (!addressModes?.some((mode) => mode === 4 || mode === 5)) continue;

    const textureType = textureTypes.get(textureName);
    const dimension = textureType?.kind === 'texture_1d' ? 1
      : ['texture_2d', 'texture_2d_array', 'texture_depth_2d', 'texture_depth_2d_array'].includes(textureType?.kind) ? 2
        : textureType?.kind === 'texture_3d' ? 3 : 0;
    if (!dimension) {
      unsupported.add(`${samplerName}: no supported sampled-texture dimension`);
      continue;
    }

    const spatialAxes = dimension === 1 ? 1 : dimension === 2 ? 2 : 3;
    const mirrorMask = addressModes.slice(0, spatialAxes).map((mode) => mode === 5);
    const borderMask = addressModes.slice(0, spatialAxes).map((mode) => mode === 4);
    if (!mirrorMask.some(Boolean) && !borderMask.some(Boolean)) continue;
    const coord = args[coordArgIndex];
    const coordStart = open + 1 + coord.start;
    const coordEnd = open + 1 + coord.end;
    let call = wgsl.slice(match.index, close + 1);

    if (borderMask.some(Boolean)) {
      const arrayTexture = textureType.kind === 'texture_2d_array' || textureType.kind === 'texture_depth_2d_array';
      const settings = samplerSettings.get(samplerIndex);
      if (gather) {
        const expectedArgs = arrayTexture ? 5 : 4;
        const textureRegister = /^t(\d+)_$/.exec(textureName);
        const textureFormat = textureRegister ? textureFormats.get(Number(textureRegister[1])) : undefined;
        const helper = args.length >= expectedArgs && args.length <= expectedArgs + 1
          ? borderGatherWgsl(textureName, textureType, samplerIndex, addressModes,
            samplerBorderColors.get(samplerIndex) || [0, 0, 0, 0], settings, arrayTexture, textureFormat,
            gatherCompare ? '0' : args[0].text.trim(), gatherCompare)
          : null;
        if (helper) {
          borderHelpers.set(helper.name, helper.source);
          const helperArgs = [coord.text.trim()];
          if (arrayTexture) helperArgs.push(args[gatherCompare ? 3 : 4].text.trim());
          helperArgs.push(args.length > expectedArgs ? args[expectedArgs].text.trim() : 'vec2<i32>(0, 0)');
          if (gatherCompare) helperArgs.push(args[arrayTexture ? 4 : 3].text.trim());
          const orderedArgs = gatherCompare
            ? [helperArgs[0], helperArgs[1], helperArgs[2], ...(arrayTexture ? [helperArgs[3]] : [])]
            : [helperArgs[0], helperArgs[1], ...(arrayTexture ? [helperArgs[2]] : [])];
          const helperCall = `${helper.name}(${orderedArgs.join(', ')})`;
          result += wgsl.slice(cursor, match.index) + helperCall;
          cursor = close + 1;
          changed = true;
          continue;
        }
      }
      const mode = ({
        textureSample: 'implicit',
        textureSampleLevel: 'level',
        textureSampleCompare: 'compare',
        textureSampleCompareLevel: 'compareLevel',
        textureSampleGrad: 'grad',
        textureSampleBias: 'bias',
      })[match[1]];
      const expectedArgs = mode === 'level' || mode === 'compareLevel' || mode === 'compare' ? (arrayTexture ? 5 : 4)
        : mode === 'grad' ? (arrayTexture ? 6 : 5)
          : mode === 'bias' ? (arrayTexture ? 5 : 4)
            : mode === 'implicit' ? (arrayTexture ? 4 : 3) : 0;
      const borderColor = samplerBorderColors.get(samplerIndex) || [0, 0, 0, 0];
      const textureRegister = /^t(\d+)_$/.exec(textureName);
      const textureFormat = textureRegister ? textureFormats.get(Number(textureRegister[1])) : undefined;
      const helper = mode && args.length === expectedArgs
        ? borderSampleLevelWgsl(textureName, textureType, samplerIndex, addressModes, borderColor, settings, arrayTexture, textureFormat)
        : null;
      if (helper) {
        const fixedLevelZero = settings?.comparison && mode === 'compareLevel';
        const anisotropicSample = settings?.comparison
          ? mode === 'compare' || fixedLevelZero
          : ['implicit', 'grad', 'bias'].includes(mode);
        if (settings?.anisotropic && anisotropicSample) {
          const anisotropicHelper = anisotropicBorderSampleWgsl(fixedLevelZero ? `${helper.name}_at_level` : helper.name,
            textureName, textureType, samplerIndex, addressModes, borderColor, settings, arrayTexture, textureFormat, fixedLevelZero);
          if (anisotropicHelper) {
            borderHelpers.set(helper.name, helper.source);
            borderHelpers.set(anisotropicHelper.name, anisotropicHelper.source);
            const coordExpression = coord.text.trim();
            const anisotropicArgs = [coordExpression];
            if (arrayTexture) anisotropicArgs.push(args[3].text.trim());
            let derivativeX, derivativeY, sourceBias = '0.0';
            if (mode === 'grad') {
              const derivativeStart = arrayTexture ? 4 : 3;
              derivativeX = args[derivativeStart].text.trim();
              derivativeY = args[derivativeStart + 1].text.trim();
            } else {
              derivativeX = `dpdx(${coordExpression})`;
              derivativeY = `dpdy(${coordExpression})`;
              if (mode === 'bias') sourceBias = args[arrayTexture ? 4 : 3].text.trim();
            }
            anisotropicArgs.push(derivativeX, derivativeY, sourceBias);
            if (settings?.comparison) anisotropicArgs.push(args[arrayTexture ? 4 : 3].text.trim());
            const helperCall = `${anisotropicHelper.name}(${anisotropicArgs.join(', ')})`;
            result += wgsl.slice(cursor, match.index) + helperCall;
            cursor = close + 1;
            changed = true;
            continue;
          }
        }
        borderHelpers.set(helper.name, helper.source);
        const sampleArgs = [coord.text.trim()];
        if (arrayTexture) sampleArgs.push(args[3].text.trim());
        let helperCall;
        if (mode === 'compareLevel') {
          sampleArgs.push('0.0', args[arrayTexture ? 4 : 3].text.trim());
          helperCall = `${helper.name}(${sampleArgs.join(', ')})`;
        } else if (mode === 'compare') {
          const wrapper = borderSampleLodWgsl(helper.name, textureName, textureType, arrayTexture, mode);
          if (wrapper) {
            borderHelpers.set(wrapper.name, wrapper.source);
            sampleArgs.push(args[arrayTexture ? 4 : 3].text.trim());
            helperCall = `${wrapper.name}(${sampleArgs.join(', ')})`;
          }
        } else if (mode === 'level') {
          sampleArgs.push(args[arrayTexture ? 4 : 3].text.trim());
          helperCall = `${helper.name}(${sampleArgs.join(', ')})`;
        } else {
          const wrapper = borderSampleLodWgsl(helper.name, textureName, textureType, arrayTexture, mode);
          if (wrapper) {
            borderHelpers.set(wrapper.name, wrapper.source);
            const restStart = arrayTexture ? 4 : 3;
            for (let index = restStart; index < args.length; index++) sampleArgs.push(args[index].text.trim());
            helperCall = `${wrapper.name}(${sampleArgs.join(', ')})`;
          }
        }
        if (helperCall) {
          result += wgsl.slice(cursor, match.index) + helperCall;
          cursor = close + 1;
          changed = true;
          continue;
        }
      }
      if (settings?.anisotropic && !settings.comparison && mode && textureType.component === 'f32'
        && dimension > 0 && dimension <= 3) {
        const coordExpr = coord.text.trim();
        const outside = borderMask.map((isBorder, axis) => {
          if (!isBorder) return null;
          const value = dimension === 1 ? `(${coordExpr})` : `(${coordExpr}).${['x', 'y', 'z'][axis]}`;
          return `(${value} < 0.0 || ${value} > 1.0)`;
        }).filter(Boolean).join(' || ');
        if (outside) {
          const borderColor = samplerBorderColors.get(samplerIndex) || [0, 0, 0, 0];
          const channels = TEX_CHANNELS[textureFormat] || 'rgba';
          const borderValues = ['r', 'g', 'b', 'a'].map((channel, index) => channels.includes(channel)
            ? borderColor[index]
            : index === 3 ? 1 : 0);
          const floatLiteral = (value) => Number.isInteger(value) ? `${value}.0` : String(value);
          const borderValue = `vec4<f32>(${borderValues.map(floatLiteral).join(', ')})`;
          call = `select(${call}, ${borderValue}, ${outside})`;
          result += wgsl.slice(cursor, match.index) + call;
          cursor = close + 1;
          changed = true;
          continue;
        }
      }
      unsupported.add(`${samplerName}: BORDER ${match[1]} could not be specialized for ${textureType.kind}`);
    }

    if (mirrorMask.some(Boolean)) {
      const maskExpr = dimension === 1 ? String(mirrorMask[0]) : `vec${spatialAxes}<bool>(${mirrorMask.join(', ')})`;
      const localStart = coordStart - match.index, localEnd = coordEnd - match.index;
      call = call.slice(0, localStart) + `mirror_once_${dimension}(${coord.text.trim()}, ${maskExpr})` + call.slice(localEnd);
      dimensions.add(dimension);
    }

    if (borderMask.some(Boolean)) {
      unsupported.add(`${samplerName}: BORDER is unsupported for ${match[1]} on ${textureType.kind}`);
    }

    result += wgsl.slice(cursor, match.index) + call;
    cursor = close + 1;
    changed = true;
  }
  if (!changed) return { wgsl, changed: false, unsupported };
  result += wgsl.slice(cursor);
  const helpers = [...dimensions].sort().map(mirrorOnceHelper).join('\n');
  const specialized = [...borderHelpers.values()].join('\n');
  const prelude = [helpers, specialized].filter(Boolean).join('\n');
  return { wgsl: prelude ? `${prelude}\n${result}` : result, changed: true, unsupported };
}

const defaultPipelineState = () => ({
  vb: [], ib: { buf: 0, format: 57, offset: 0 }, layout: 0, topology: 4, shader: [0, 0, 0],
  cb: [[], [], []], cbRange: [[], [], []], srv: [[], [], []], smp: [[], [], []], uav: [], rtvs: [], dsv: 0,
  viewports: [], scissors: [], blend: { id: 0, factor: [1, 1, 1, 1], mask: 0xffffffff }, raster: 0, depth: { id: 0, ref: 0 },
  d3d9RenderStates: new Map(),
});

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
    this.dualSourceShaderVariants = new WeakMap();
    this.samplerAddressVariants = new WeakMap();
    this.computePipelines = new Map();
    this.depth24ReadbackPipeline = null;
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
    this.lastPresentSyncInterval = 1;
    this.st = defaultPipelineState();
    this.stateStack = [];
  }

  clearState() {
    this.endPass();
    this.st = defaultPipelineState();
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
      case OP.CREATE_TEXTURE1D: this.createTexture(u(0), u(1), 1, u(2), u(3), u(4), u(5), u(6), u(7), '1d'); break;
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
      case OP.CLEAR_STATE: this.clearState(); break;
      case OP.PUSH_STATE: this.endPass(); this.stateStack.push(structuredClone(this.st)); break;
      case OP.POP_STATE: this.endPass(); this.st = this.stateStack.pop() || defaultPipelineState(); break;
      case OP.SET_FIXED_FUNCTION:
        this.setFixedFunction(
          u(0), u(1), u(2),
          dv.byteLength >= 16 ? u(3) : 4,
          dv.byteLength >= 20 ? u(4) : 2,
          dv.byteLength >= 24 ? u(5) : 1,
          dv.byteLength >= 28 ? u(6) : 2,
          dv.byteLength >= 32 ? u(7) : 2,
          dv.byteLength >= 36 ? u(8) : 1,
          dv.byteLength >= 40 ? u(9) : 0xffffffff,
          dv.byteLength >= 44 ? u(10) : 1,
          dv.byteLength >= 48 ? u(11) : 1,
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
      case OP.CLEAR_UAV: this.clearUav(u(0), u(1) !== 0, [u(2), u(3), u(4), u(5)], [f(2), f(3), f(4), f(5)]); break;
      case OP.DRAW: this.draw(false, u(0), 1, u(1), 0, 0, 0); break;
      case OP.DRAW_INDEXED: this.draw(true, u(0), 1, u(1), i32(2), 0, 0); break;
      case OP.DRAW_INSTANCED: this.draw(false, u(0), u(1), u(2), 0, u(3), 0); break;
      case OP.DRAW_INDEXED_INSTANCED: this.draw(true, u(0), u(1), u(2), i32(3), u(4), 0); break;
      case OP.COPY_RESOURCE: this.copyResource(u(0), u(1)); break;
      case OP.COPY_REGION: this.copyRegion(dv); break;
      case OP.RESOLVE_TEXTURE: this.resolveTexture(u(0), u(1), u(2), u(3), u(4)); break;
      case OP.SWAPCHAIN: this.createSwapchain(u(0), u(2), u(3), u(4), u(5)); break;
      case OP.RESIZE_SWAPCHAIN: this.resizeSwapchain(u(0), u(1), u(2), u(3), u(4)); break;
      case OP.PRESENT: this.present(u(0), u(1), bytes.byteLength >= 12 ? u(2) : 1); break;
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
    const backingDimension = dimension === '1d' ? '2d' : dimension;
    const backingHeight = dimension === '1d' ? 1 : h;
    const gpu = this.device.createTexture({ size: [w, backingHeight, Math.max(1, array)], dimension: backingDimension, mipLevelCount: Math.max(1, mips), sampleCount, format, usage: u, viewFormats });
    this.res.set(id, {
      kind: 'texture', gpu, width: w, height: backingHeight, depth: dimension === '3d' ? Math.max(1, array) : 1,
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
    let uploadData = data, uploadRowPitch = rowPitch;
    if (LEGACY_PACKED_16[r.dxgi]) {
      uploadData = new Uint8Array(w * h * 8);
      uploadRowPitch = w * 8;
      const source = new DataView(data.buffer, data.byteOffset, data.byteLength);
      const destination = new DataView(uploadData.buffer);
      for (let row = 0; row < h; row++) {
        for (let column = 0; column < w; column++) {
          const channels = unpackLegacyPacked16(source.getUint16(row * rowPitch + column * 2, true), r.dxgi);
          for (let channel = 0; channel < 4; channel++) {
            destination.setUint16((row * w + column) * 8 + channel * 2, floatToHalfBits(channels[channel]), true);
          }
        }
      }
    }
    const blk = BLOCK(r.format) || [1, 1, BYTES(r.format)];
    const rows = Math.ceil(h / blk[1]);
    const rowBytes = Math.ceil(w / blk[0]) * blk[2];
    const bpr = (rowBytes + 255) & ~255;
    const packed = new Uint8Array(bpr * rows);
    for (let row = 0; row < rows; row++) packed.set(uploadData.subarray(row * uploadRowPitch, row * uploadRowPitch + rowBytes), row * bpr);
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
    const borderColor = [0, 1, 2, 3].map((channel) => dv.getFloat32(28 + channel * 4, true));
    if (isAniso) { d.minFilter = d.magFilter = d.mipmapFilter = 'linear'; d.maxAnisotropy = Math.max(1, Math.min(16, aniso)); }
    if (comparison) d.compare = CMP[cmp] || 'always';
    const sampling = {
      minFilter: d.minFilter, magFilter: d.magFilter, mipmapFilter: d.mipmapFilter,
      mipBias: bias, lodMinClamp: d.lodMinClamp, lodMaxClamp: d.lodMaxClamp,
      maxAnisotropy: d.maxAnisotropy || 1,
      anisotropic: isAniso, comparison, compare: d.compare,
    };
    return { gpu: this.device.createSampler(d), comparison, maxAnisotropy: d.maxAnisotropy || 1, addressModes: [au, av, aw], borderColor, sampling };
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

  samplerAddressVariant(shader, samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats) {
    if (!shader) return shader;
    const key = samplerAddressVariantKey(samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats);
    if (key === '[]') return shader;
    let variants = this.samplerAddressVariants.get(shader);
    if (!variants) {
      variants = new Map();
      this.samplerAddressVariants.set(shader, variants);
    }
    let variant = variants.get(key);
    if (variant) return variant;

    const source = shader.wgsl || shader.fixedFunction?.source;
    if (!source) return shader;
    const transformed = samplerAddressSampleWgsl(source, samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats);
    for (const message of transformed.unsupported) {
      this.log(`sampler addressing unsupported: ${message}`);
    }
    if (!transformed.changed) return shader;
    variant = {
      ...shader,
      ...(shader.fixedFunction ? { fixedFunction: { ...shader.fixedFunction, source: transformed.wgsl } } : {}),
      wgsl: transformed.wgsl,
      module: this.device.createShaderModule({ code: transformed.wgsl }),
    };
    variants.set(key, variant);
    return variant;
  }

  dualSourceShaderVariant(shader) {
    let variant = this.dualSourceShaderVariants.get(shader);
    if (variant) return variant;
    const wgsl = dualSourceFragmentWgsl(shader.wgsl);
    if (!wgsl) {
      this.log('draw skipped: dual-source blending requires SV_Target0 and SV_Target1 outputs');
      return null;
    }
    variant = { ...shader, wgsl, module: this.device.createShaderModule({ code: wgsl }) };
    this.dualSourceShaderVariants.set(shader, variant);
    return variant;
  }

  // ── views ────────────────────────────────────────────────────────────────
  setFixedFunction(fvf, width, height, colorOp = 4, colorArg1 = 2, colorArg2 = 1, alphaOp = 2, alphaArg1 = 2, alphaArg2 = 1, textureFactor = 0xffffffff, colorArg0 = 1, alphaArg0 = 1) {
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
    const key = `${fvf}/${width}/${height}/${colorOp}/${colorArg0}/${colorArg1}/${colorArg2}/${alphaOp}/${alphaArg0}/${alphaArg1}/${alphaArg2}/${textureFactor >>> 24}`;
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
      const color0 = channelArg(colorArg0, 'rgb');
      const colorA = channelArg(colorArg1, 'rgb'), colorB = channelArg(colorArg2, 'rgb');
      const colorAlphaA = channelArg(colorArg1, 'a');
      const alpha0 = channelArg(alphaArg0, 'a');
      const alphaA = channelArg(alphaArg1, 'a'), alphaB = channelArg(alphaArg2, 'a');
      const textureFactorAlpha = (((textureFactor >>> 24) & 0xff) / 255).toFixed(8);
      const colorExpr = colorOp === 1 ? 'input.color.rgb'
        : colorOp === 2 ? colorA
        : colorOp === 3 ? colorB
        : colorOp === 4 ? `(${colorA} * ${colorB})`
        : colorOp === 5 ? `((${colorA} * ${colorB}) * 2.0)`
        : colorOp === 6 ? `((${colorA} * ${colorB}) * 4.0)`
        : colorOp === 7 ? `(${colorA} + ${colorB})`
        : colorOp === 8 ? `(${colorA} + ${colorB} - 0.5)`
        : colorOp === 9 ? `((${colorA} + ${colorB} - 0.5) * 2.0)`
        : colorOp === 10 ? `(${colorA} - ${colorB})`
        : colorOp === 11 ? `(${colorA} + ${colorB} - (${colorA} * ${colorB}))`
        : colorOp === 12 || colorOp === 16 ? `((${colorA} * input.color.a) + (${colorB} * (1.0 - input.color.a)))`
        : colorOp === 13 ? `((${colorA} * texel.a) + (${colorB} * (1.0 - texel.a)))`
        : colorOp === 14 ? `((${colorA} * ${textureFactorAlpha}) + (${colorB} * (1.0 - ${textureFactorAlpha})))`
        : colorOp === 15 ? `(${colorA} + (${colorB} * (1.0 - texel.a)))`
        : colorOp === 18 ? `(${colorA} + (${colorAlphaA} * ${colorB}))`
        : colorOp === 19 ? `((${colorA} * ${colorB}) + vec3<f32>(${colorAlphaA}))`
        : colorOp === 20 ? `(${colorA} + ((1.0 - ${colorAlphaA}) * ${colorB}))`
        : colorOp === 21 ? `((vec3<f32>(1.0) - ${colorA}) * ${colorB} + vec3<f32>(${colorAlphaA}))`
        : colorOp === 24 ? `vec3<f32>(clamp(dot((${colorA} * 2.0 - vec3<f32>(1.0)), (${colorB} * 2.0 - vec3<f32>(1.0))), 0.0, 1.0))`
        : colorOp === 25 ? `(${color0} + (${colorA} * ${colorB}))`
        : colorOp === 26 ? `((${color0} * ${colorA}) + ((1.0 - ${color0}) * ${colorB}))`
        : 'input.color.rgb';
      const alphaExpr = alphaOp === 2 ? alphaA
        : alphaOp === 3 ? alphaB
        : alphaOp === 4 ? `(${alphaA} * ${alphaB})`
        : alphaOp === 5 ? `((${alphaA} * ${alphaB}) * 2.0)`
        : alphaOp === 6 ? `((${alphaA} * ${alphaB}) * 4.0)`
        : alphaOp === 7 ? `(${alphaA} + ${alphaB})`
        : alphaOp === 8 ? `(${alphaA} + ${alphaB} - 0.5)`
        : alphaOp === 9 ? `((${alphaA} + ${alphaB} - 0.5) * 2.0)`
        : alphaOp === 10 ? `(${alphaA} - ${alphaB})`
        : alphaOp === 11 ? `(${alphaA} + ${alphaB} - (${alphaA} * ${alphaB}))`
        : alphaOp === 12 || alphaOp === 16 ? `((${alphaA} * input.color.a) + (${alphaB} * (1.0 - input.color.a)))`
        : alphaOp === 13 ? `((${alphaA} * texel.a) + (${alphaB} * (1.0 - texel.a)))`
        : alphaOp === 14 ? `((${alphaA} * ${textureFactorAlpha}) + (${alphaB} * (1.0 - ${textureFactorAlpha})))`
        : alphaOp === 15 ? `(${alphaA} + (${alphaB} * (1.0 - texel.a)))`
        : alphaOp === 25 ? `(${alpha0} + (${alphaA} * ${alphaB}))`
        : alphaOp === 26 ? `((${alpha0} * ${alphaA}) + ((1.0 - ${alpha0}) * ${alphaB}))`
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
    const sourceFormat = this.viewFormat(v.format, tex);
    const depthOnly = hasStencil(sourceFormat) && v.kind === 2;
    const format = depthOnly ? depthAspectFormat(sourceFormat) : sourceFormat;
    const d = { format };
    if (isDepth(sourceFormat)) d.aspect = depthOnly ? 'depth-only' : 'all';
    if (v.kind === 0 || v.kind === 1) { // render attachments: one mip, one layer, '2d'
      d.dimension = '2d'; d.baseMipLevel = v.mip; d.mipLevelCount = 1;
      const oneDArray = tex.dimension === '1d' && (v.kind === 1 ? v.dim === 2 : v.dim === 3);
      const arrayAttachment = v.dim === 5 || v.dim === 4 && (v.slice !== 0 || v.slices > 1);
      d.baseArrayLayer = oneDArray || arrayAttachment ? v.slice : 0;
      d.arrayLayerCount = 1;
    } else {
      d.baseMipLevel = v.mip; d.mipLevelCount = Math.min(v.mips || tex.mips, tex.mips - v.mip);
      if (v.dim === 8 || tex.dimension === '3d') { d.dimension = '3d'; }
      else if (v.dim === 10) {
        d.dimension = 'cube-array'; d.baseArrayLayer = v.slice;
        d.arrayLayerCount = Math.min(v.slices * 6, tex.array - v.slice);
      }
      else if (v.dim === 9 || (tex.cube && v.dim !== 5)) { d.dimension = 'cube'; d.baseArrayLayer = 0; d.arrayLayerCount = 6; }
      else if (tex.dimension === '1d') {
        const arrayView = v.dim === 3;
        d.dimension = arrayView ? '2d-array' : '2d';
        d.baseArrayLayer = arrayView ? v.slice : 0;
        d.arrayLayerCount = arrayView ? Math.min(v.slices || tex.array, tex.array - v.slice) : 1;
      }
      else if (v.dim === 5) { d.dimension = '2d-array'; d.baseArrayLayer = v.slice; d.arrayLayerCount = Math.min(v.slices, tex.array - v.slice); }
      else { d.dimension = '2d'; d.baseArrayLayer = 0; d.arrayLayerCount = 1; }
    }
    v.cache = { view: tex.gpu.createView(d), format, dxgi: v.format || tex.dxgi, tex, dim: d.dimension };
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

  clearUav(id, isFloat, uintValues, floatValues) {
    const desc = this.views.get(id);
    if (!desc || desc.kind !== 3) return;
    const view = this.getView(id);
    if (!view) { this.log(`clear UAV skipped: view ${id} is unavailable`); return; }

    if (view.dim === 'buffer') {
      const rawOrStructured = (desc.slice & 1) !== 0 || (view.bufferResource.misc & (0x20 | 0x40)) !== 0;
      if (rawOrStructured && !isFloat) {
        const pattern = new Uint8Array(4);
        new DataView(pattern.buffer).setUint32(0, uintValues[0] >>> 0, true);
        this.clearUavBuffer(view, pattern);
        return;
      }
      if (rawOrStructured || !desc.format) { this.log(`clear UAV skipped: buffer view ${id} has no supported typed format`); return; }
      const pattern = uavClearPattern(desc.format, isFloat, uintValues, floatValues);
      if (!pattern) { this.log(`clear UAV skipped: unsupported buffer format ${desc.format}`); return; }
      this.clearUavBuffer(view, pattern);
      return;
    }

    const pattern = uavClearPattern(desc.format, isFloat, uintValues, floatValues);
    if (!pattern) { this.log(`clear UAV skipped: unsupported texture format ${desc.format}`); return; }
    this.clearUavTexture(view, desc, pattern);
  }

  clearUavBuffer(view, pattern) {
    const byteCount = view.size;
    if (byteCount <= 0 || byteCount % pattern.byteLength !== 0 || view.offset % 4 !== 0) {
      this.log('clear UAV skipped: buffer view range is not aligned to its element format');
      return;
    }
    this.flush();
    const chunkSize = Math.max(pattern.byteLength, Math.floor((1 << 20) / pattern.byteLength) * pattern.byteLength);
    for (let offset = 0; offset < byteCount; offset += chunkSize) {
      const length = Math.min(chunkSize, byteCount - offset);
      const chunk = new Uint8Array(length);
      for (let at = 0; at < length; at += pattern.byteLength) chunk.set(pattern, at);
      this.device.queue.writeBuffer(view.buffer, view.offset + offset, chunk);
    }
  }

  clearUavTexture(view, desc, pattern) {
    const { tex } = view;
    if (!tex || tex.samples !== 1) { this.log('clear UAV skipped: multisampled textures are unsupported'); return; }
    const mip = desc.mip;
    const width = Math.max(1, tex.width >> mip), height = Math.max(1, tex.height >> mip);
    let firstSlice = 0, sliceCount = 1;
    if (tex.dimension === '3d') {
      const depth = Math.max(1, tex.depth >> mip);
      firstSlice = Math.min(desc.slice, depth);
      sliceCount = Math.min(desc.slices || depth, depth - firstSlice);
    } else if (desc.dim === 5 || tex.dimension === '1d' && desc.dim === 3) {
      firstSlice = Math.min(desc.slice, tex.array);
      sliceCount = Math.min(desc.slices || tex.array, tex.array - firstSlice);
    }
    if (!sliceCount) return;

    const rowBytes = width * pattern.byteLength;
    const bytesPerRow = (rowBytes + 255) & ~255;
    const chunkRows = Math.max(1, Math.floor((1 << 20) / bytesPerRow));
    this.flush();
    for (let slice = 0; slice < sliceCount; slice++) {
      for (let y = 0; y < height; y += chunkRows) {
        const rows = Math.min(chunkRows, height - y);
        const data = new Uint8Array(bytesPerRow * rows);
        for (let row = 0; row < rows; row++) {
          const rowStart = row * bytesPerRow;
          for (let x = 0; x < width; x++) data.set(pattern, rowStart + x * pattern.byteLength);
        }
        this.device.queue.writeTexture(
          { texture: tex.gpu, mipLevel: mip, origin: { x: 0, y, z: firstSlice + slice } },
          data,
          { offset: 0, bytesPerRow, rowsPerImage: rows },
          { width, height: rows, depthOrArrayLayers: 1 },
        );
      }
    }
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
    let vs = this.shaders.get(st.shader[0]);
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
    const samplerAddressModes = new Map();
    const samplerBorderColors = new Map();
    const samplerSettings = new Map();
    const textureFormats = new Map();
    for (const [reg, { d, vis }] of groups[2]) {
      const sid = this.st.smp[(vis & GPUShaderStage.VERTEX) ? 0 : 1][reg] || this.st.smp[1][reg] || this.st.smp[0][reg] || 0;
      const s = this.samplers.get(sid);
      samplerComparison[reg] = !!s?.comparison;
      if (s?.addressModes) {
        samplerAddressModes.set(reg, s.addressModes);
        if (s.addressModes.includes(4)) samplerBorderColors.set(reg, s.borderColor || [0, 0, 0, 0]);
        samplerSettings.set(reg, s.sampling || {});
      }
      layoutEntries[2].push({ binding: reg, visibility: vis, sampler: { type: s?.comparison ? 'comparison' : 'filtering' } });
      resolved[2].push({ binding: reg, resource: s ? s.gpu : this.defaultSampler });
    }
    for (const [reg, { vis }] of groups[1]) {
      const vid = this.st.srv[(vis & GPUShaderStage.FRAGMENT) ? 1 : 0][reg] || this.st.srv[1][reg] || this.st.srv[0][reg] || 0;
      const view = this.getView(vid);
      if (view?.dim !== 'buffer' && view) textureFormats.set(reg, view.dxgi ?? view.tex?.dxgi);
    }
    const samplerAddressKey = samplerAddressVariantKey(samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats);
    vs = this.samplerAddressVariant(vs, samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats);
    ps = this.samplerAddressVariant(ps, samplerAddressModes, samplerBorderColors, samplerSettings, textureFormats);
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
    const key = JSON.stringify([st.shader[0], psId, samplerAddressKey, st.layout, strides, topology, st.blend.id, st.blend.mask, st.raster, st.depth.id, d3d9PipelineStates, rtFormats, att.depth?.format || null, sampleCount, layoutEntries, indexed && (topology === 3 || topology === 5) ? st.ib.format : 0]);
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
    const usesDualSourceBlend = !d3d9BlendEnabled && !!blend?.getUint32(8, true)
      && [12, 16, 24, 28].some((offset) => {
        const factor = blend.getUint32(offset, true);
        return factor >= 16 && factor <= 19;
      });
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
    const sampleCount = att.colors.find(Boolean)?.tex.samples || att.depth?.tex.samples || 1;
    desc.multisample = {
      count: sampleCount,
      mask: st.blend.mask,
      alphaToCoverageEnabled: sampleCount > 1 && !!blend?.getUint32(0, true),
    };
    if (ps) {
      let fragmentShader = ps;
      if (usesDualSourceBlend) {
        if (targets.length !== 1) {
          this.log('draw skipped: dual-source blending requires exactly one render target');
          return null;
        }
        fragmentShader = this.dualSourceShaderVariant(ps);
        if (!fragmentShader) return null;
      }
      desc.fragment = { module: fragmentShader.module, entryPoint: fragmentShader.entry, targets };
    }
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

  copyBufferRegion(src, dst, sourceOffset, destinationOffset, size) {
    if (size <= 0) return;
    let source = src.gpu;
    if (src === dst) {
      const scratch = this.device.createBuffer({ size: src.gpu.size, usage: GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST | GPUBufferUsage.STORAGE });
      this.encoder().copyBufferToBuffer(src.gpu, 0, scratch, 0, src.gpu.size);
      this.transientBuffers.push(scratch);
      source = scratch;
    }

    if ((sourceOffset | destinationOffset | size) % 4 === 0) {
      this.encoder().copyBufferToBuffer(source, sourceOffset, dst.gpu, destinationOffset, size);
      return;
    }

    if (source.size > this.device.limits.maxStorageBufferBindingSize || dst.gpu.size > this.device.limits.maxStorageBufferBindingSize) {
      throw new Error('unaligned buffer CopySubresourceRegion exceeds the WebGPU storage binding limit');
    }
    if (!this.bufferByteCopyPipeline) {
      const module = this.device.createShaderModule({ code: `
        struct CopyParams {
          sourceOffset: u32,
          destinationOffset: u32,
          byteCount: u32,
          firstWord: u32,
          wordCount: u32,
        };
        @group(0) @binding(0) var<storage, read> sourceData: array<u32>;
        @group(0) @binding(1) var<storage, read_write> destinationData: array<u32>;
        @group(0) @binding(2) var<uniform> params: CopyParams;
        @compute @workgroup_size(64)
        fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
          if (invocation.x >= params.wordCount) { return; }
          let destinationWord = params.firstWord + invocation.x;
          var result = destinationData[destinationWord];
          for (var lane = 0u; lane < 4u; lane += 1u) {
            let destinationByte = destinationWord * 4u + lane;
            if (destinationByte >= params.destinationOffset && destinationByte < params.destinationOffset + params.byteCount) {
              let sourceByte = params.sourceOffset + destinationByte - params.destinationOffset;
              let sourceWord = sourceData[sourceByte / 4u];
              let value = (sourceWord >> ((sourceByte % 4u) * 8u)) & 0xffu;
              let shift = lane * 8u;
              result = (result & ~(0xffu << shift)) | (value << shift);
            }
          }
          destinationData[destinationWord] = result;
        }
      ` });
      const visibility = GPUShaderStage.COMPUTE;
      const layout = this.device.createBindGroupLayout({ entries: [
        { binding: 0, visibility, buffer: { type: 'read-only-storage' } },
        { binding: 1, visibility, buffer: { type: 'storage' } },
        { binding: 2, visibility, buffer: { type: 'uniform', minBindingSize: 20 } },
      ] });
      this.bufferByteCopyLayout = layout;
      this.bufferByteCopyPipeline = this.device.createComputePipeline({
        layout: this.device.createPipelineLayout({ bindGroupLayouts: [layout] }),
        compute: { module, entryPoint: 'main' },
      });
    }

    const firstWord = destinationOffset >>> 2;
    const wordCount = ((destinationOffset + size + 3) >>> 2) - firstWord;
    const params = this.device.createBuffer({ size: 32, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    this.device.queue.writeBuffer(params, 0, new Uint32Array([sourceOffset, destinationOffset, size, firstWord, wordCount]));
    this.transientBuffers.push(params);
    const bindGroup = this.device.createBindGroup({
      layout: this.bufferByteCopyLayout,
      entries: [
        { binding: 0, resource: { buffer: source } },
        { binding: 1, resource: { buffer: dst.gpu } },
        { binding: 2, resource: { buffer: params, size: 20 } },
      ],
    });
    const pass = this.encoder().beginComputePass();
    pass.setPipeline(this.bufferByteCopyPipeline);
    pass.setBindGroup(0, bindGroup);
    pass.dispatchWorkgroups(Math.ceil(wordCount / 64));
    pass.end();
  }

  copyRegion(dv) {
    const u = (i) => dv.getUint32(i * 4, true);
    const [dst, dsub, dx, dy, dz, src, ssub, hasBox] = [u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7)];
    const d = this.res.get(dst), s = this.res.get(src);
    if (!d || !s) return;
    this.endPass();
    if (d.kind === 'buffer' && s.kind === 'buffer') {
      const l = hasBox ? u(8) : 0, r = hasBox ? u(11) : s.size;
      const sourceOffset = l, destOffset = dx;
      const sourceSize = s.d3dSize ?? s.size, destinationSize = d.d3dSize ?? d.size;
      const size = Math.max(0, Math.min(r - l, sourceSize - sourceOffset, destinationSize - destOffset));
      this.copyBufferRegion(s, d, sourceOffset, destOffset, size);
      if (d.shadow && s.shadow && sourceOffset + size <= s.shadow.length && destOffset + size <= d.shadow.length) {
        if (s.shadow === d.shadow) d.shadow.copyWithin(destOffset, sourceOffset, sourceOffset + size);
        else d.shadow.set(s.shadow.subarray(sourceOffset, sourceOffset + size), destOffset);
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

  present(scId, texId, syncInterval = 1) {
    this.lastPresentSyncInterval = Math.min(syncInterval >>> 0, 4);
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

    const mip = resource.dimension === '3d' ? subresource : subresource % resource.mips;
    const slice = resource.dimension === '3d' ? 0 : Math.floor(subresource / resource.mips);
    if (mip < 0 || mip >= resource.mips || slice >= resource.array) {
      throw new Error(`texture ${id} subresource ${subresource} is out of range`);
    }
    const width = Math.max(1, resource.width >> mip);
    const height = Math.max(1, resource.height >> mip);
    const depth = resource.dimension === '3d' ? Math.max(1, resource.depth >> mip) : 1;
    if (resource.dxgi === 45 && resource.format === 'depth24plus-stencil8') {
      if (resource.dimension !== '2d' || depth !== 1) throw new Error(`D24_UNORM_S8 readback does not support ${resource.dimension} textures`);
      return this.readBackDepth24Stencil(resource, mip, slice, width, height);
    }
    if (resource.format === 'depth32float-stencil8') {
      const depthBytesPerRow = (width * 4 + 255) & ~255;
      const stencilBytesPerRow = (width + 255) & ~255;
      const imageCount = depthBytesPerRow * height * depth;
      const depthBuffer = this.device.createBuffer({ size: imageCount, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
      const stencilBuffer = this.device.createBuffer({ size: stencilBytesPerRow * height * depth, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
      try {
        this.flush();
        const encoder = this.device.createCommandEncoder();
        const source = { texture: resource.gpu, mipLevel: mip, origin: { x: 0, y: 0, z: slice } };
        const copySize = { width, height, depthOrArrayLayers: depth };
        encoder.copyTextureToBuffer({ ...source, aspect: 'depth-only' },
          { buffer: depthBuffer, bytesPerRow: depthBytesPerRow, rowsPerImage: height }, copySize);
        encoder.copyTextureToBuffer({ ...source, aspect: 'stencil-only' },
          { buffer: stencilBuffer, bytesPerRow: stencilBytesPerRow, rowsPerImage: height }, copySize);
        this.device.queue.submit([encoder.finish()]);
        await Promise.all([depthBuffer.mapAsync(GPUMapMode.READ), stencilBuffer.mapAsync(GPUMapMode.READ)]);
        const depthBytes = new Uint8Array(depthBuffer.getMappedRange());
        const stencilBytes = new Uint8Array(stencilBuffer.getMappedRange());
        const output = new Uint8Array(width * height * depth * 8);
        for (let z = 0; z < depth; z++) {
          for (let y = 0; y < height; y++) {
            for (let x = 0; x < width; x++) {
              const destination = ((z * height + y) * width + x) * 8;
              const depthOffset = z * depthBytesPerRow * height + y * depthBytesPerRow + x * 4;
              const stencilOffset = z * stencilBytesPerRow * height + y * stencilBytesPerRow + x;
              output.set(depthBytes.subarray(depthOffset, depthOffset + 4), destination);
              output[destination + 4] = stencilBytes[stencilOffset];
            }
          }
        }
        return output;
      } finally {
        if (depthBuffer.mapState === 'mapped') depthBuffer.unmap();
        if (stencilBuffer.mapState === 'mapped') stencilBuffer.unmap();
        depthBuffer.destroy();
        stencilBuffer.destroy();
      }
    }
    if (hasStencil(resource.format)) throw new Error(`stencil texture readback is unsupported for ${resource.format}`);
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
      if (LEGACY_PACKED_16[resource.dxgi]) {
        const packed = new Uint8Array(width * height * depth * 2);
        const source = new DataView(output.buffer, output.byteOffset, output.byteLength);
        const destination = new DataView(packed.buffer);
        for (let z = 0; z < depth; z++) {
          for (let y = 0; y < height; y++) {
            for (let x = 0; x < width; x++) {
              const pixel = ((z * height + y) * width + x);
              const channels = Array.from({ length: 4 }, (_, channel) => halfBitsToFloat(source.getUint16(pixel * 8 + channel * 2, true)));
              destination.setUint16(pixel * 2, packLegacyPacked16(channels, resource.dxgi), true);
            }
          }
        }
        return packed;
      }
      return output;
    } finally {
      if (readback.mapState === 'mapped') readback.unmap();
      readback.destroy();
    }
  }

  async readBackDepth24Stencil(resource, mip, slice, width, height) {
    if (!this.depth24ReadbackPipeline) {
      const module = this.device.createShaderModule({ code: `
        @group(0) @binding(0) var depthTexture: texture_depth_2d;
        @group(0) @binding(1) var<storage, read_write> depthValues: array<f32>;
        @compute @workgroup_size(8, 8, 1)
        fn main(@builtin(global_invocation_id) id: vec3<u32>) {
          let size = textureDimensions(depthTexture, 0);
          if (id.x >= size.x || id.y >= size.y) { return; }
          let value = textureLoad(depthTexture, vec2<i32>(id.xy), 0);
          depthValues[id.y * size.x + id.x] = value;
        }
      ` });
      this.depth24ReadbackPipeline = this.device.createComputePipeline({
        layout: 'auto',
        compute: { module, entryPoint: 'main' },
      });
    }
    const depthByteLength = width * height * 4;
    const stencilBytesPerRow = (width + 255) & ~255;
    const depthStorage = this.device.createBuffer({ size: depthByteLength, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const depthReadback = this.device.createBuffer({ size: depthByteLength, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const stencilReadback = this.device.createBuffer({ size: stencilBytesPerRow * height, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    try {
      this.flush();
      const depthView = resource.gpu.createView({
        dimension: '2d', aspect: 'depth-only', baseMipLevel: mip, mipLevelCount: 1,
        baseArrayLayer: slice, arrayLayerCount: 1,
      });
      const bindGroup = this.device.createBindGroup({
        layout: this.depth24ReadbackPipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: depthView },
          { binding: 1, resource: { buffer: depthStorage, size: depthByteLength } },
        ],
      });
      const encoder = this.device.createCommandEncoder();
      const pass = encoder.beginComputePass();
      pass.setPipeline(this.depth24ReadbackPipeline);
      pass.setBindGroup(0, bindGroup);
      pass.dispatchWorkgroups(Math.ceil(width / 8), Math.ceil(height / 8));
      pass.end();
      encoder.copyBufferToBuffer(depthStorage, 0, depthReadback, 0, depthByteLength);
      encoder.copyTextureToBuffer(
        { texture: resource.gpu, mipLevel: mip, origin: { x: 0, y: 0, z: slice }, aspect: 'stencil-only' },
        { buffer: stencilReadback, bytesPerRow: stencilBytesPerRow, rowsPerImage: height },
        { width, height, depthOrArrayLayers: 1 },
      );
      this.device.queue.submit([encoder.finish()]);
      await Promise.all([depthReadback.mapAsync(GPUMapMode.READ), stencilReadback.mapAsync(GPUMapMode.READ)]);
      const depthValues = new Float32Array(depthReadback.getMappedRange());
      const stencilValues = new Uint8Array(stencilReadback.getMappedRange());
      const packed = new Uint8Array(width * height * 4);
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          const pixel = y * width + x;
          const d24 = roundToEven(Math.min(1, Math.max(0, depthValues[pixel])) * 0xffffff);
          const output = pixel * 4;
          packed[output] = d24 & 0xff;
          packed[output + 1] = (d24 >>> 8) & 0xff;
          packed[output + 2] = (d24 >>> 16) & 0xff;
          packed[output + 3] = stencilValues[y * stencilBytesPerRow + x];
        }
      }
      return packed;
    } finally {
      for (const buffer of [depthStorage, depthReadback, stencilReadback]) {
        if (buffer.mapState === 'mapped') buffer.unmap();
        buffer.destroy();
      }
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
    const rg32 = ['rg32float', 'rg32uint', 'rg32sint'].includes(format);
    const rg32Array = format === 'rg32uint' ? Uint32Array : format === 'rg32sint' ? Int32Array : Float32Array;
    const rgba32float = format === 'rgba32float';
    const rgba32Integer = format === 'rgba32uint' || format === 'rgba32sint';
    const rgba16float = format === 'rgba16float';
    const rgba16Integer = format === 'rgba16uint' || format === 'rgba16sint';
    const rgba8Snorm = format === 'rgba8snorm';
    const rgba8 = ['rgba8unorm', 'rgba8unorm-srgb', 'bgra8unorm', 'bgra8unorm-srgb'].includes(format);
    const rgba8Integer = format === 'rgba8uint' || format === 'rgba8sint';
    if (!scalar32 && !rg32 && !rgba32float && !rgba32Integer && !rgba16float && !rgba16Integer && !rgba8Snorm && !rgba8 && !rgba8Integer) {
      throw new Error(`texture readback does not support format ${format}`);
    }
    const width = Math.max(1, texture.width >> mip);
    const height = Math.max(1, texture.height >> mip);
    const bytesPerTexel = rgba32float || rgba32Integer ? 16 : rg32 || rgba16float || rgba16Integer ? 8 : 4;
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
    } else if (rgba32Integer) {
      const ArrayType = format === 'rgba32uint' ? Uint32Array : Int32Array;
      data = new ArrayType(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new ArrayType(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
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
    } else if (rg32) {
      data = new rg32Array(width * height * 2);
      for (let row = 0; row < height; row++) {
        data.set(new rg32Array(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 2), row * width * 2);
      }
    } else if (rgba16Integer) {
      const ArrayType = format === 'rgba16uint' ? Uint16Array : Int16Array;
      data = new ArrayType(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new ArrayType(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
      }
    } else if (rgba8Integer) {
      const ArrayType = format === 'rgba8uint' ? Uint8Array : Int8Array;
      data = new ArrayType(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new ArrayType(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
      }
    } else if (rgba8Snorm) {
      data = new Int8Array(width * height * 4);
      for (let row = 0; row < height; row++) {
        data.set(new Int8Array(bytes.buffer, bytes.byteOffset + row * bytesPerRow, width * 4), row * width * 4);
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
