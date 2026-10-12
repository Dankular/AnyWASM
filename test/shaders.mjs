// HLSL -> DXBC -> SPIR-V -> WGSL through the vendored toolchain.
import { readFile } from 'node:fs/promises';
import { ShaderTool } from '../web/shaders.js';
let failures = 0;
const check = (n, ok, d = '') => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${n}${ok ? '' : ' :: ' + d}`); if (!ok) failures++; };

const hlsl = `
cbuffer Frame : register(b0) { float4x4 mvp; float4 tint; };
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct VSIn { float3 pos : POSITION; float4 col : COLOR; float2 uv : TEXCOORD0; };
struct PSIn { float4 pos : SV_Position; float4 col : COLOR; float2 uv : TEXCOORD0; };
PSIn vs_main(VSIn i) { PSIn o; o.pos = mul(mvp, float4(i.pos, 1.0)); o.col = i.col * tint; o.uv = i.uv; return o; }
float4 ps_main(PSIn i) : SV_Target { return tex.Sample(smp, i.uv) * i.col; }
`;
const t = new ShaderTool();
await t.init();
const vs = t.compileHlsl(hlsl, 'vs_main', 'vs_5_0');
const ps = t.compileHlsl(hlsl, 'ps_main', 'ps_5_0');
check('vs DXBC container', vs.length > 64 && String.fromCharCode(...vs.slice(0, 4)) === 'DXBC', vs.length);
check('ps DXBC container', ps.length > 64 && String.fromCharCode(...ps.slice(0, 4)) === 'DXBC');

const v = t.translate(vs), p = t.translate(ps);
check('vs declares cbuffer b0', v.descriptors.some((d) => d.type === 2 && d.register === 0), JSON.stringify(v.descriptors));
check('ps declares t0 and s0', p.descriptors.some((d) => d.type === 0 && d.register === 0) && p.descriptors.some((d) => d.type === 3 && d.register === 0), JSON.stringify(p.descriptors));
check('vs WGSL uses @group(0) @binding(0) uniform', /@group\(0\)\s*@binding\(0\)\s*var<uniform>/.test(v.wgsl), v.wgsl.slice(0, 400));
check('ps WGSL uses texture @group(1) and sampler @group(2)', /@group\(1\)\s*@binding\(0\)\s*var\s+\w+:\s*texture_2d/.test(p.wgsl) && /@group\(2\)\s*@binding\(0\)\s*var\s+\w+:\s*sampler/.test(p.wgsl), p.wgsl.slice(0, 600));
check('vs has @vertex entry with position builtin', /@vertex/.test(v.wgsl) && /@builtin\(position\)/.test(v.wgsl));
check('ps has @fragment entry', /@fragment/.test(p.wgsl));
const depthPs = t.translate(t.compileHlsl(`
  struct PsOut { float4 color : SV_Target; float depth : SV_Depth; };
  PsOut main() {
    PsOut output;
    output.color = float4(1.0, 0.0, 0.0, 1.0);
    output.depth = 0.25;
    return output;
  }
`, 'main', 'ps_5_0'));
check('SV_Depth translates to the WGSL fragment-depth builtin',
  /@builtin\(frag_depth\)\s+\w+:\s*f32/.test(depthPs.wgsl), depthPs.wgsl);
const coveragePs = t.translate(t.compileHlsl(`
  struct PsOut { float4 color : SV_Target; uint coverage : SV_Coverage; };
  PsOut main() {
    PsOut output;
    output.color = float4(1.0, 0.0, 0.0, 1.0);
    output.coverage = 5;
    return output;
  }
`, 'main', 'ps_5_0'));
check('SV_Coverage translates to the WGSL sample-mask builtin',
  /@builtin\(sample_mask\)\s+\w+:\s*u32/.test(coveragePs.wgsl), coveragePs.wgsl);
const drawParamsVs = t.translate(t.compileHlsl(`
  float4 main(uint vertexId : SV_VertexID, uint instanceId : SV_InstanceID) : SV_Position {
    return float4(float(vertexId + instanceId), 0.0, 0.0, 1.0);
  }
`, 'main', 'vs_5_0'));
check('D3D draw parameters translate through the reserved group-3 binding 0 uniform',
  /@group\(3\)\s*@binding\(0\)\s*var<uniform>/.test(drawParamsVs.wgsl), drawParamsVs.wgsl);
const uavCs = t.translate(t.compileHlsl(`
  RWTexture2D<float> dst : register(u0);
  [numthreads(1, 1, 1)] void main(uint3 id : SV_DispatchThreadID) { dst[id.xy] = 0.25; }
`, 'main', 'cs_5_0'));
check('D3D UAV u0 translates to group-3 binding 1 while retaining logical register 0',
  uavCs.descriptors.some((d) => d.type === 1 && d.register === 0)
    && /@group\(3\)\s*@binding\(1\)\s*var\s+\w+\s*:\s*texture_storage_2d/.test(uavCs.wgsl),
  JSON.stringify(uavCs.descriptors) + '\n' + uavCs.wgsl);
const texture1dPs = t.translate(t.compileHlsl(`
  Texture1D<float4> image : register(t0);
  SamplerState smp : register(s0);
  float4 main(float4 position : SV_Position) : SV_Target {
    return image.SampleLevel(smp, position.x / 64.0, 1.0);
  }
`, 'main', 'ps_5_0'));
check('Texture1D samples lower to a 2D backing texture with a centered row coordinate',
  /var\s+t0_:\s*texture_2d<f32>/.test(texture1dPs.wgsl)
    && /textureSampleLevel\(t0_,\s*s0_,\s*vec2<f32>\(/.test(texture1dPs.wgsl)
    && !/texture_1d(?:<|_array)/.test(texture1dPs.wgsl),
  texture1dPs.wgsl);
const texture1dDimensionsPs = t.translate(t.compileHlsl(`
  Texture1D<float4> image : register(t0);
  float4 main() : SV_Target {
    uint width, levels;
    image.GetDimensions(0, width, levels);
    return float4(width, levels, 0.0, 1.0);
  }
`, 'main', 'ps_5_0'));
check('Texture1D GetDimensions reports the backing width and mip count',
  /var\s+t0_:\s*texture_2d<f32>/.test(texture1dDimensionsPs.wgsl)
    && /textureDimensions\(t0_,\s*0i\)/.test(texture1dDimensionsPs.wgsl)
    && /textureNumLevels\(t0_\)/.test(texture1dDimensionsPs.wgsl)
    && !/texture_1d(?:<|_array)/.test(texture1dDimensionsPs.wgsl),
  texture1dDimensionsPs.wgsl);
let texture1dArrayPs, texture1dArrayError = '';
try {
  texture1dArrayPs = t.translate(t.compileHlsl(`
    Texture1DArray<float4> image : register(t0);
    SamplerState smp : register(s0);
    float4 main(float4 position : SV_Position) : SV_Target {
      return image.SampleLevel(smp, float2(position.x / 64.0, 1.0), 1.0);
    }
  `, 'main', 'ps_5_0'));
} catch (error) { texture1dArrayError = String(error); }
check('Texture1DArray samples lower to a 2D array backing texture with centered row coordinates',
  /var\s+t0_:\s*texture_2d_array<f32>/.test(texture1dArrayPs?.wgsl || '')
    && /vec3<f32>\([^,]+,\s*0\.5f?,/.test(texture1dArrayPs?.wgsl || '')
    && /textureSampleLevel\(t0_,\s*s0_,\s*vec2<f32>\([^)]*\),\s*i32\(/.test(texture1dArrayPs?.wgsl || '')
    && !/texture_1d(?:<|_array)/.test(texture1dArrayPs?.wgsl || ''),
  texture1dArrayError || texture1dArrayPs?.wgsl || 'translation returned no WGSL');
let threw = false; try { t.compileHlsl('float4 x( {', 'x', 'ps_5_0'); } catch (e) { threw = /HLSL/.test(e.message); }
check('syntax errors reported', threw);
let sdfWgsl = '', sdfError = '';
try {
  const dxbc = new Uint8Array(await readFile(new URL('../fixtures/diligent_glfwdemo_sdf.dxbc', import.meta.url)));
  sdfWgsl = t.translate(dxbc).wgsl;
} catch (e) { sdfError = String(e); }
check('Diligent GLFW Demo SDF integer loop constants translate through runtime bitcasts',
  /local\s*=\s*-8i;/.test(sdfWgsl) && /let (\w+) = local;\s*r1_\[1u\] = bitcast<f32>\(\1\);/.test(sdfWgsl),
  sdfError || sdfWgsl.slice(0, 400));
let drawMapVs, drawMapPs, drawMapError = '';
try {
  const vsDxbc = new Uint8Array(await readFile(new URL('../fixtures/diligent_glfwdemo_drawmap_vs.dxbc', import.meta.url)));
  const psDxbc = new Uint8Array(await readFile(new URL('../fixtures/diligent_glfwdemo_drawmap_ps.dxbc', import.meta.url)));
  drawMapVs = t.translate(vsDxbc);
  drawMapPs = t.translate(psDxbc);
} catch (e) { drawMapError = String(e); }
check('Diligent DrawMap vertex shader translates with its constant buffer and position output',
  drawMapVs?.descriptors.some((d) => d.type === 2 && d.register === 0)
    && /@vertex\s+fn\s+main/.test(drawMapVs.wgsl) && /@builtin\(position\)/.test(drawMapVs.wgsl),
  drawMapError || JSON.stringify(drawMapVs?.descriptors));
check('Diligent DrawMap pixel shader translates its constant buffers, SDF texture, sampler, and ray loops',
  [0, 1].every((register) => drawMapPs?.descriptors.some((d) => d.type === 2 && d.register === register))
    && drawMapPs?.descriptors.some((d) => d.type === 0 && d.register === 0)
    && drawMapPs?.descriptors.some((d) => d.type === 3 && d.register === 0)
    && /@fragment\s+fn\s+main/.test(drawMapPs.wgsl) && (drawMapPs.wgsl.match(/loop\s*\{/g) || []).length >= 2,
  drawMapError || JSON.stringify(drawMapPs?.descriptors));
if (process.env.SHOW) { console.log(v.wgsl); console.log(p.wgsl); }
console.log(failures ? `\n${failures} FAILED` : '\nshader toolchain tests passed');
process.exit(failures ? 1 : 0);
