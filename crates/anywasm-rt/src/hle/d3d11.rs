//! Direct3D 11: device creation, resources, state objects, immediate context.
//!
//! Nothing is rendered here. Every call is validated and translated into the binary GPU command
//! stream (`gpu.rs`) that the browser executes with WebGPU. DXBC shaders are forwarded untouched;
//! the browser compiles them (DXBC -> SPIR-V -> WGSL).

use super::com::*;
use super::dxgi;
use super::gpu::{op, Pk};
use super::*;
use crate::process::*;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    fn gpu_readback(
        commands: *const u8,
        commands_len: u32,
        resource: u32,
        subresource: u32,
        destination: *mut u8,
        destination_len: u32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
fn request_gpu_readback(commands: &[u8], id: u32, sub: u32, destination: &mut [u8]) -> i32 {
    // SAFETY: the imported host function only reads `commands` and writes exactly `destination`.
    unsafe {
        gpu_readback(
            commands.as_ptr(),
            commands.len() as u32,
            id,
            sub,
            destination.as_mut_ptr(),
            destination.len() as u32,
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn request_gpu_readback(_commands: &[u8], _id: u32, _sub: u32, _destination: &mut [u8]) -> i32 {
    0
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "D3D11CreateDevice" => |p| create_device(p, false),
        "D3D11CreateDeviceAndSwapChain" => |p| create_device(p, true),
        _ => return None,
    })
}

const D3D_FEATURE_LEVEL_11_0: u32 = 0xb000;
const D3D_FEATURE_LEVEL_11_1: u32 = 0xb100;
const D3D11_QUERY_EVENT: u32 = 0;
const D3D11_QUERY_TYPE_COUNT: u32 = 16;
const DXGI_FORMAT_UNKNOWN: u32 = 0;
const DXGI_FORMAT_R32_FLOAT: u32 = 41;
const DXGI_FORMAT_R32_UINT: u32 = 42;
const DXGI_FORMAT_R32_SINT: u32 = 43;
const DXGI_FORMAT_R32G32_FLOAT: u32 = 16;
const DXGI_FORMAT_R32G32_UINT: u32 = 17;
const DXGI_FORMAT_R32G32_SINT: u32 = 18;
const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = 29;
const DXGI_FORMAT_R8G8B8A8_SNORM: u32 = 31;
const DXGI_FORMAT_R8G8B8A8_UINT: u32 = 30;
const DXGI_FORMAT_R8G8B8A8_SINT: u32 = 32;
const DXGI_FORMAT_R16G16B16A16_FLOAT: u32 = 10;
const DXGI_FORMAT_R16G16B16A16_UINT: u32 = 12;
const DXGI_FORMAT_R16G16B16A16_SINT: u32 = 14;
const DXGI_FORMAT_R10G10B10A2_UNORM: u32 = 24;
const DXGI_FORMAT_R11G11B10_FLOAT: u32 = 26;
const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
const DXGI_FORMAT_R32G32B32A32_UINT: u32 = 3;
const DXGI_FORMAT_R32G32B32A32_SINT: u32 = 4;
const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
const DXGI_FORMAT_B8G8R8A8_UNORM_SRGB: u32 = 91;
const DXGI_FORMAT_R32_TYPELESS: u32 = 39;
const DXGI_FORMAT_D32_FLOAT: u32 = 40;
const DXGI_FORMAT_R24G8_TYPELESS: u32 = 44;
const DXGI_FORMAT_D24_UNORM_S8_UINT: u32 = 45;
const DXGI_FORMAT_R16_TYPELESS: u32 = 53;
const DXGI_FORMAT_D16_UNORM: u32 = 55;
const D3D11_FORMAT_SUPPORT_BUFFER: u32 = 0x1;
const D3D11_FORMAT_SUPPORT_CPU_LOCKABLE: u32 = 0x0002_0000;
const D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW: u32 = 0x0200_0000;
const D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE: u32 = 0x80;
const D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE: u32 = 0x0004_0000;
const D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET: u32 = 0x0020_0000;

fn supports_4x_multisample_render_target(format: u32) -> bool {
    matches!(
        format,
        DXGI_FORMAT_R8G8B8A8_UNORM
            | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            | DXGI_FORMAT_B8G8R8A8_UNORM
            | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
            | DXGI_FORMAT_R16G16B16A16_FLOAT
            | DXGI_FORMAT_R10G10B10A2_UNORM
            | DXGI_FORMAT_R11G11B10_FLOAT
    )
}

fn supports_4x_multisample_depth_stencil(format: u32) -> bool {
    matches!(
        format,
        DXGI_FORMAT_D32_FLOAT | DXGI_FORMAT_D24_UNORM_S8_UINT | DXGI_FORMAT_D16_UNORM
    )
}

fn supports_4x_multisample_depth_resource(format: u32) -> bool {
    matches!(
        format,
        DXGI_FORMAT_R32_TYPELESS
            | DXGI_FORMAT_D32_FLOAT
            | DXGI_FORMAT_R24G8_TYPELESS
            | DXGI_FORMAT_D24_UNORM_S8_UINT
            | DXGI_FORMAT_R16_TYPELESS
            | DXGI_FORMAT_D16_UNORM
    )
}

fn supports_4x_multisample_format(format: u32) -> bool {
    supports_4x_multisample_render_target(format) || supports_4x_multisample_depth_stencil(format)
}

fn format_support_flags(format: u32) -> u32 {
    if format == DXGI_FORMAT_UNKNOWN {
        return D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_CPU_LOCKABLE;
    }
    let mut flags = 0x0003_ffff; // common sampling, render-target and texture capabilities
    if supports_4x_multisample_render_target(format) {
        flags |= D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE
            | D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET;
    }
    if matches!(
        format,
        DXGI_FORMAT_R8G8B8A8_UNORM
            | DXGI_FORMAT_R8G8B8A8_SNORM
            | DXGI_FORMAT_R8G8B8A8_UINT
            | DXGI_FORMAT_R8G8B8A8_SINT
            | DXGI_FORMAT_R16G16B16A16_FLOAT
            | DXGI_FORMAT_R16G16B16A16_UINT
            | DXGI_FORMAT_R16G16B16A16_SINT
            | DXGI_FORMAT_R32G32B32A32_FLOAT
            | DXGI_FORMAT_R32G32B32A32_UINT
            | DXGI_FORMAT_R32G32B32A32_SINT
            | DXGI_FORMAT_R32G32_FLOAT
            | DXGI_FORMAT_R32G32_UINT
            | DXGI_FORMAT_R32G32_SINT
            | DXGI_FORMAT_R32_FLOAT
            | DXGI_FORMAT_R32_UINT
            | DXGI_FORMAT_R32_SINT
    ) {
        flags |= D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW;
    }
    flags
}

fn ensure_device(p: &mut Process) {
    dxgi::ensure_singletons(p);
    if p.extra.com.device == 0 {
        p.extra.com.device = create(p, "ID3D11Device1", Kind::Device);
        p.extra.com.context = create(p, "ID3D11DeviceContext1", Kind::Context);
    }
    if p.extra.com.dxgi_device == 0 {
        p.extra.com.dxgi_device = create(p, "IDXGIDevice1", Kind::Device);
    }
}

fn create_device(p: &mut Process, with_swap_chain: bool) -> R {
    // D3D11CreateDevice(pAdapter, DriverType, Software, Flags, pFeatureLevels, FeatureLevels, SDKVersion,
    //                   ppDevice, pFeatureLevel, ppImmediateContext)
    // ...AndSwapChain(..., SDKVersion, pSwapChainDesc, ppSwapChain, ppDevice, pFeatureLevel, ppContext)
    let (pdev, pfl, pctx, pdesc, psc) = if with_swap_chain {
        (p.arg(9), p.arg(10), p.arg(11), p.arg(7), p.arg(8))
    } else {
        (p.arg(7), p.arg(8), p.arg(9), 0, 0)
    };
    let Some(feature_level) = requested_feature_level(p)? else {
        return Ok(E_INVALIDARG);
    };
    ensure_device(p);
    let (dev, ctx) = (p.extra.com.device, p.extra.com.context);
    if pdev != 0 {
        p.wr64(pdev, dev)?;
        p.extra.com.objs.get_mut(&dev).map(|o| o.refs += 1);
    }
    if pctx != 0 {
        p.wr64(pctx, ctx)?;
        p.extra.com.objs.get_mut(&ctx).map(|o| o.refs += 1);
    }
    if pfl != 0 {
        p.wr32(pfl, feature_level)?;
    }
    if with_swap_chain && psc != 0 && pdesc != 0 {
        let (w, h, fmt) = (p.rd32(pdesc)?, p.rd32(pdesc + 4)?, p.rd32(pdesc + 16)?);
        let (buffers, hwnd) = (p.rd32(pdesc + 40)?, p.rd64(pdesc + 48)?);
        let sc = dxgi::make_swap_chain(p, hwnd, w, h, fmt, buffers.max(1));
        p.wr64(psc, sc)?;
    }
    Ok(S_OK)
}

fn requested_feature_level(p: &Process) -> Result<Option<u32>, Stop> {
    let (levels, count) = (p.arg(4), p.arg(5) as u32);
    if levels == 0 {
        return Ok((count == 0).then_some(D3D_FEATURE_LEVEL_11_0));
    }
    if count == 0 {
        return Ok(None);
    }

    let mut selected = None;
    for index in 0..count {
        let level = p.rd32(levels + 4 * index as u64)?;
        // D3D11 runtimes without 11.1 reject a request containing 11.1 rather than falling back.
        if level == D3D_FEATURE_LEVEL_11_1 {
            return Ok(None);
        }
        if level == D3D_FEATURE_LEVEL_11_0 && selected.is_none() {
            selected = Some(level);
        }
    }
    Ok(selected)
}

fn clear_unordered_access_view(p: &mut Process, float_values: bool) -> R {
    let Some(Kind::View {
        vk: ViewKind::Uav,
        id,
        ..
    }) = kind_of(p, p.arg(1))
    else {
        return Ok(0);
    };
    let values = p.arg(2);
    let mut command = Pk::new().u32(id).u32(u32::from(float_values));
    for index in 0..4 {
        command = command.u32(p.rd32(values + 4 * index)?);
    }
    p.extra.gpu.emit(op::CLEAR_UAV, command);
    Ok(0)
}

pub fn method(iface: &str, m: &str) -> Option<HleFn> {
    let _ = iface;
    Some(match m {
        // ── shared by several interfaces ──
        "GetDesc" => get_desc,
        "GetParent" => get_parent,
        "GetResource" => |p| {
            let (this, out) = (p.arg(0), p.arg(1));
            if let Some(Kind::View { resource, .. }) = kind_of(p, this) {
                let obj = p
                    .extra
                    .com
                    .objs
                    .iter()
                    .find(|(_, o)| {
                        o.kind.host_id() == Some(resource)
                            && matches!(
                                o.kind,
                                Kind::Buffer { .. }
                                    | Kind::Texture1D { .. }
                                    | Kind::Texture2D { .. }
                                    | Kind::Texture3D { .. }
                            )
                    })
                    .map(|(k, _)| *k);
                if let Some(o) = obj {
                    p.extra.com.objs.get_mut(&o).map(|x| x.refs += 1);
                    p.wr64(out, o)?;
                    return Ok(0);
                }
            }
            p.wr64(out, 0)?;
            Ok(0)
        },
        "GetType" => |p| {
            let o = p.arg(1);
            let this = p.arg(0);
            let kind = kind_of(p, this);
            if matches!(kind, Some(Kind::Context)) {
                return Ok(u64::from(p.extra.com.deferred_contexts.contains_key(&this)));
            }
            let dimension = match kind {
                Some(Kind::Buffer { .. }) => 1,
                Some(Kind::Texture1D { .. }) => 2,
                Some(Kind::Texture2D { .. }) => 3,
                Some(Kind::Texture3D { .. }) => 4,
                _ => 0,
            };
            p.wr32(o, dimension)?;
            Ok(0)
        },
        // ── ID3D11Device ──
        "CreateBuffer" => create_buffer,
        "CreateDeferredContext" => |p| create_deferred_context(p, "ID3D11DeviceContext"),
        "CreateDeferredContext1" => |p| create_deferred_context(p, "ID3D11DeviceContext1"),
        "CreateDeferredContext2" => |p| create_deferred_context(p, "ID3D11DeviceContext2"),
        "CreateDeferredContext3" => |p| create_deferred_context(p, "ID3D11DeviceContext3"),
        "CreateTexture1D" => create_texture1d,
        "CreateTexture2D" => create_texture2d,
        "CreateTexture3D" | "CreateTexture3D1" => create_texture3d,
        "CreateRenderTargetView" => |p| create_view(p, ViewKind::Rtv),
        "CreateDepthStencilView" => |p| create_view(p, ViewKind::Dsv),
        "CreateShaderResourceView" => |p| create_view(p, ViewKind::Srv),
        "CreateUnorderedAccessView" => |p| create_view(p, ViewKind::Uav),
        "CreateInputLayout" => create_input_layout,
        "CreateVertexShader" => |p| create_shader(p, 0, "ID3D11VertexShader"),
        "CreatePixelShader" => |p| create_shader(p, 1, "ID3D11PixelShader"),
        "CreateComputeShader" => |p| create_shader(p, 2, "ID3D11ComputeShader"),
        "CreateSamplerState" => |p| create_state(p, op::CREATE_SAMPLER, 52, "ID3D11SamplerState"),
        "CreateBlendState" | "CreateBlendState1" => {
            |p| create_state(p, op::CREATE_BLEND, 264, "ID3D11BlendState")
        }
        "CreateRasterizerState" | "CreateRasterizerState1" => {
            |p| create_state(p, op::CREATE_RASTER, 40, "ID3D11RasterizerState")
        }
        "CreateDepthStencilState" => {
            |p| create_state(p, op::CREATE_DEPTH, 52, "ID3D11DepthStencilState")
        }
        "CreateQuery" => |p| create_query(p, false),
        "CreatePredicate" => |p| create_query(p, true),
        "GetImmediateContext" | "GetImmediateContext1" => |p| {
            let out = p.arg(1);
            let c = p.extra.com.context;
            p.extra.com.objs.get_mut(&c).map(|o| o.refs += 1);
            p.wr64(out, c)?;
            Ok(0)
        },
        "CheckFormatSupport" => |p| {
            let (format, out) = (p.arg(1) as u32, p.arg(2));
            p.wr32(out, format_support_flags(format))?;
            Ok(S_OK)
        },
        "CheckMultisampleQualityLevels" => |p| {
            let (format, samples, out) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3));
            p.wr32(
                out,
                if supports_4x_multisample_format(format) && matches!(samples, 1 | 4) {
                    1
                } else {
                    0
                },
            )?;
            Ok(S_OK)
        },
        "CheckFeatureSupport" => check_feature_support,
        "GetFeatureLevel" => |_| Ok(D3D_FEATURE_LEVEL_11_0 as u64),
        "GetCreationFlags" => |_| Ok(0),
        "GetContextFlags" => |p| match kind_of(p, p.arg(0)) {
            Some(Kind::CommandList { flags, .. }) => Ok(flags as u64),
            Some(Kind::Context) => Ok(p
                .extra
                .com
                .deferred_contexts
                .get(&p.arg(0))
                .map_or(0, |context| context.flags) as u64),
            _ => Ok(0),
        },
        "GetDeviceRemovedReason" => |_| Ok(S_OK),
        "SetExceptionMode" => |_| Ok(S_OK),
        "GetExceptionMode" => |_| Ok(0),
        "ClearState" => |p| {
            if let Some(state) = p.extra.com.deferred_contexts.get_mut(&p.arg(0)) {
                state.bound_rtv.clear();
                state.bound_dsv = 0;
            } else {
                p.extra.com.bound_rtv.clear();
                p.extra.com.bound_dsv = 0;
            }
            p.extra.gpu.emit(op::CLEAR_STATE, Pk::new());
            Ok(0)
        },
        // ── ID3D11DeviceContext: pipeline state ──
        "IASetInputLayout" => |p| {
            let id = obj_id(p, p.arg(1));
            p.extra.gpu.emit(op::SET_INPUT_LAYOUT, Pk::new().u32(id));
            Ok(0)
        },
        "IASetVertexBuffers" => ia_set_vertex_buffers,
        "IASetIndexBuffer" => |p| {
            let (b, f, o) = (obj_id(p, p.arg(1)), p.arg(2) as u32, p.arg(3) as u32);
            p.extra
                .gpu
                .emit(op::SET_INDEX_BUFFER, Pk::new().u32(b).u32(f).u32(o));
            Ok(0)
        },
        "IASetPrimitiveTopology" => |p| {
            let t = p.arg(1) as u32;
            p.extra.gpu.emit(op::SET_TOPOLOGY, Pk::new().u32(t));
            Ok(0)
        },
        "VSSetShader" => |p| set_shader(p, 0),
        "PSSetShader" => |p| set_shader(p, 1),
        "CSSetShader" => |p| set_shader(p, 2),
        "VSSetConstantBuffers" => |p| set_slots(p, op::SET_CBUFFERS, 0),
        "PSSetConstantBuffers" => |p| set_slots(p, op::SET_CBUFFERS, 1),
        "CSSetConstantBuffers" => |p| set_slots(p, op::SET_CBUFFERS, 2),
        "VSSetConstantBuffers1" => |p| set_slots1(p, 0),
        "PSSetConstantBuffers1" => |p| set_slots1(p, 1),
        "CSSetConstantBuffers1" => |p| set_slots1(p, 2),
        "VSSetShaderResources" => |p| set_slots(p, op::SET_SRVS, 0),
        "PSSetShaderResources" => |p| set_slots(p, op::SET_SRVS, 1),
        "CSSetShaderResources" => |p| set_slots(p, op::SET_SRVS, 2),
        "VSSetSamplers" => |p| set_slots(p, op::SET_SAMPLERS, 0),
        "PSSetSamplers" => |p| set_slots(p, op::SET_SAMPLERS, 1),
        "CSSetSamplers" => |p| set_slots(p, op::SET_SAMPLERS, 2),
        "CSSetUnorderedAccessViews" => set_uavs,
        "OMSetRenderTargets" => om_set_render_targets,
        "OMSetBlendState" => |p| {
            let (b, f, mask) = (obj_id(p, p.arg(1)), p.arg(2), p.arg(3) as u32);
            let mut pk = Pk::new().u32(b);
            for i in 0..4 {
                pk = pk.f32(if f != 0 {
                    f32::from_bits(p.rd32(f + 4 * i)?)
                } else {
                    1.0
                });
            }
            p.extra.gpu.emit(op::SET_BLEND, pk.u32(mask));
            Ok(0)
        },
        "OMSetDepthStencilState" => |p| {
            let (d, r) = (obj_id(p, p.arg(1)), p.arg(2) as u32);
            p.extra.gpu.emit(op::SET_DEPTH, Pk::new().u32(d).u32(r));
            Ok(0)
        },
        "RSSetState" => |p| {
            let id = obj_id(p, p.arg(1));
            p.extra.gpu.emit(op::SET_RASTER, Pk::new().u32(id));
            Ok(0)
        },
        "RSSetViewports" => |p| {
            let (n, v) = (p.arg(1) as u32, p.arg(2));
            let mut pk = Pk::new().u32(n);
            for i in 0..n as u64 * 6 {
                pk = pk.u32(p.rd32(v + 4 * i)?);
            }
            p.extra.gpu.emit(op::SET_VIEWPORTS, pk);
            Ok(0)
        },
        "RSSetScissorRects" => |p| {
            let (n, v) = (p.arg(1) as u32, p.arg(2));
            let mut pk = Pk::new().u32(n);
            for i in 0..n as u64 * 4 {
                pk = pk.u32(p.rd32(v + 4 * i)?);
            }
            p.extra.gpu.emit(op::SET_SCISSORS, pk);
            Ok(0)
        },
        // ── draws ──
        "Draw" => |p| {
            let (a, b) = (p.arg(1) as u32, p.arg(2) as u32);
            p.extra.gpu.emit(op::DRAW, Pk::new().u32(a).u32(b));
            Ok(0)
        },
        "DrawIndexed" => |p| {
            let (a, b, c) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as i32);
            p.extra
                .gpu
                .emit(op::DRAW_INDEXED, Pk::new().u32(a).u32(b).i32(c));
            Ok(0)
        },
        "DrawInstanced" => |p| {
            let (a, b, c, d) = (
                p.arg(1) as u32,
                p.arg(2) as u32,
                p.arg(3) as u32,
                p.arg(4) as u32,
            );
            p.extra
                .gpu
                .emit(op::DRAW_INSTANCED, Pk::new().u32(a).u32(b).u32(c).u32(d));
            Ok(0)
        },
        "DrawIndexedInstanced" => |p| {
            let (a, b, c, d, e) = (
                p.arg(1) as u32,
                p.arg(2) as u32,
                p.arg(3) as u32,
                p.arg(4) as i32,
                p.arg(5) as u32,
            );
            p.extra.gpu.emit(
                op::DRAW_INDEXED_INSTANCED,
                Pk::new().u32(a).u32(b).u32(c).i32(d).u32(e),
            );
            Ok(0)
        },
        "Dispatch" => |p| {
            let (a, b, c) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32);
            p.extra
                .gpu
                .emit(op::DISPATCH, Pk::new().u32(a).u32(b).u32(c));
            Ok(0)
        },
        "ClearRenderTargetView" => |p| {
            let (v, c) = (obj_id(p, p.arg(1)), p.arg(2));
            let mut pk = Pk::new().u32(v);
            for i in 0..4 {
                pk = pk.u32(p.rd32(c + 4 * i)?);
            }
            p.extra.gpu.emit(op::CLEAR_RTV, pk);
            Ok(0)
        },
        "ClearUnorderedAccessViewFloat" => |p| clear_unordered_access_view(p, true),
        "ClearUnorderedAccessViewUint" => |p| clear_unordered_access_view(p, false),
        "ClearDepthStencilView" => |p| {
            let (v, flags) = (obj_id(p, p.arg(1)), p.arg(2) as u32);
            let depth = f32::from_bits(p.cpu.xmm[3] as u32);
            let stencil = p.arg(4) as u32 & 0xff;
            p.extra.gpu.emit(
                op::CLEAR_DSV,
                Pk::new().u32(v).u32(flags).f32(depth).u32(stencil),
            );
            Ok(0)
        },
        "GenerateMips" => |p| {
            let v = obj_id(p, p.arg(1));
            p.extra.gpu.emit(op::GENERATE_MIPS, Pk::new().u32(v));
            Ok(0)
        },
        "CopyResource" => |p| {
            let (d, s) = (p.arg(1), p.arg(2));
            copy_resource(p, d, s)?;
            Ok(0)
        },
        "CopySubresourceRegion" => copy_subresource_region,
        "UpdateSubresource" | "UpdateSubresource1" => update_subresource,
        "Map" => map,
        "Unmap" => unmap,
        "Flush1" => |p| {
            // Guest GPU commands are serialized in order by the browser executor. Signal the
            // optional idle event so Diligent's Flush1 + WaitForSingleObject sequence can
            // continue.
            let event = p.arg(2);
            if let Some(Handle::Event { signaled, .. }) = p.handles.get_mut(&event) {
                *signaled = true;
            }
            Ok(0)
        },
        "Begin" => |_| Ok(0), // D3D11_QUERY_EVENT is End-only; other query types are rejected.
        "End" => set_query_complete,
        "GetData" => get_query_data,
        "ResolveSubresource" => resolve_subresource,
        "FinishCommandList" => finish_command_list,
        "ExecuteCommandList" => execute_command_list,
        "Flush" | "SetPredication" | "DiscardResource" | "DiscardView" => |_| Ok(0),
        "OMGetRenderTargets" => |p| {
            let (n, rtvs, dsv) = (p.arg(1) as usize, p.arg(2), p.arg(3));
            let (bound, bound_dsv) = p
                .extra
                .com
                .deferred_contexts
                .get(&p.arg(0))
                .map(|state| (state.bound_rtv.clone(), state.bound_dsv))
                .unwrap_or_else(|| (p.extra.com.bound_rtv.clone(), p.extra.com.bound_dsv));
            for i in 0..n {
                if rtvs != 0 {
                    p.wr64(rtvs + 8 * i as u64, bound.get(i).copied().unwrap_or(0))?;
                }
            }
            if dsv != 0 {
                p.wr64(dsv, bound_dsv)?;
            }
            Ok(0)
        },
        _ => return None,
    })
}

fn create_deferred_context(p: &mut Process, iface: &'static str) -> R {
    let (flags, out) = (p.arg(1) as u32, p.arg(2));
    if flags != 0 {
        out_ptr(p, out, 0)?;
        return Ok(E_INVALIDARG);
    }
    if out != 0 {
        let context = create(p, iface, Kind::Context);
        p.extra.com.deferred_contexts.insert(
            context,
            DeferredContextState {
                flags,
                ..DeferredContextState::default()
            },
        );
        out_ptr(p, out, context)?;
    }
    Ok(S_OK)
}

fn finish_command_list(p: &mut Process) -> R {
    let (context, restore, out) = (p.arg(0), p.arg(1) != 0, p.arg(2));
    out_ptr(p, out, 0)?;
    let Some(state) = p.extra.com.deferred_contexts.get_mut(&context) else {
        return Ok(DXGI_ERROR_INVALID_CALL);
    };
    let commands = std::mem::take(&mut state.commands);
    let flags = state.flags;
    state.commands = if restore {
        state.state_commands.clone()
    } else {
        state.state_commands.clear();
        state.bound_rtv.clear();
        state.bound_dsv = 0;
        Vec::new()
    };
    if out != 0 {
        let command_list = create(
            p,
            "ID3D11CommandList",
            Kind::CommandList { commands, flags },
        );
        out_ptr(p, out, command_list)?;
    }
    Ok(S_OK)
}

fn execute_command_list(p: &mut Process) -> R {
    let (context, list, restore) = (p.arg(0), p.arg(1), p.arg(2) != 0);
    if p.extra.com.deferred_contexts.contains_key(&context) {
        return Ok(0);
    }
    let Some(Kind::CommandList { commands, .. }) = kind_of(p, list) else {
        return Ok(0);
    };
    if restore {
        p.extra.gpu.emit(op::PUSH_STATE, Pk::new());
    }
    p.extra.gpu.emit(op::CLEAR_STATE, Pk::new());
    p.extra.gpu.buf.extend(commands);
    if restore {
        p.extra.gpu.emit(op::POP_STATE, Pk::new());
    } else {
        p.extra.gpu.emit(op::CLEAR_STATE, Pk::new());
    }
    Ok(0)
}

fn create_query(p: &mut Process, predicate: bool) -> R {
    let (desc, out) = (p.arg(1), p.arg(2));
    if out != 0 {
        p.wr64(out, 0)?;
    }
    if desc == 0 {
        return Ok(E_INVALIDARG);
    }

    let query_type = p.rd32(desc)?;
    if query_type >= D3D11_QUERY_TYPE_COUNT {
        return Ok(E_INVALIDARG);
    }
    if predicate {
        if !matches!(query_type, 5 | 7 | 9 | 11 | 13 | 15) {
            return Ok(E_INVALIDARG);
        }
        return Ok(E_NOTIMPL);
    }
    if query_type != D3D11_QUERY_EVENT {
        return Ok(E_NOTIMPL);
    }
    if out == 0 {
        return Ok(S_OK);
    }

    let query = create(
        p,
        "ID3D11Query",
        Kind::Query {
            query_type,
            complete: false,
        },
    );
    p.wr64(out, query)?;
    Ok(S_OK)
}

fn set_query_complete(p: &mut Process) -> R {
    let query = p.arg(1);
    if let Some(Kind::Query {
        query_type: D3D11_QUERY_EVENT,
        complete: state,
    }) = p.extra.com.objs.get_mut(&query).map(|obj| &mut obj.kind)
    {
        *state = true;
    }
    Ok(0)
}

fn get_query_data(p: &mut Process) -> R {
    let query = p.arg(1);
    let (data, size) = (p.arg(2), p.arg(3) as u32);
    if data == 0 && size != 0 {
        return Ok(E_INVALIDARG);
    }
    let complete = matches!(
        kind_of(p, query),
        Some(Kind::Query {
            query_type: D3D11_QUERY_EVENT,
            complete: true
        })
    );
    if !complete {
        return Ok(S_FALSE);
    }
    if data != 0 && size >= 4 {
        p.wr32(data, 1)?;
    }
    Ok(S_OK)
}

fn check_feature_support(p: &mut Process) -> R {
    const FEATURE_THREADING: u64 = 0;
    const FEATURE_DOUBLES: u64 = 1;
    const FEATURE_FORMAT_SUPPORT: u64 = 2;
    const FEATURE_FORMAT_SUPPORT2: u64 = 3;
    const FEATURE_D3D10_X_HARDWARE_OPTIONS: u64 = 4;
    const FEATURE_D3D11_OPTIONS1: u64 = 10;

    let (feature, data, size) = (p.arg(1), p.arg(2), p.arg(3) as u32);
    let expected_size = match feature {
        FEATURE_THREADING | FEATURE_FORMAT_SUPPORT | FEATURE_FORMAT_SUPPORT2 => 8,
        FEATURE_DOUBLES | FEATURE_D3D10_X_HARDWARE_OPTIONS => 4,
        FEATURE_D3D11_OPTIONS1 => 16,
        _ => return Ok(E_INVALIDARG),
    };
    if data == 0 || size != expected_size {
        return Ok(E_INVALIDARG);
    }

    match feature {
        FEATURE_THREADING | FEATURE_DOUBLES => p.write_bytes(data, &vec![0; size as usize])?,
        // This executor has no tiled resources, min/max filtering, depth-only ClearView, or
        // mappable default buffers. D3D11_FEATURE_DATA_D3D11_OPTIONS1 is four 32-bit fields;
        // zero reports TILED_RESOURCES_NOT_SUPPORTED and FALSE for the remaining capabilities.
        FEATURE_D3D11_OPTIONS1 => p.write_bytes(data, &vec![0; size as usize])?,
        FEATURE_FORMAT_SUPPORT => {
            let format = p.rd32(data)?;
            p.wr32(data + 4, format_support_flags(format))?;
        }
        FEATURE_FORMAT_SUPPORT2 => {
            let format = p.rd32(data)?;
            let flags = if format_support_flags(format)
                & D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW
                != 0
            {
                D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE
            } else {
                0
            };
            p.wr32(data + 4, flags)?;
        }
        FEATURE_D3D10_X_HARDWARE_OPTIONS => p.wr32(data, 1)?,
        _ => unreachable!(),
    }
    Ok(S_OK)
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Host id of the COM object at `ptr` (0 for null / unknown).
fn obj_id(p: &Process, ptr: u64) -> u32 {
    kind_of(p, ptr).and_then(|k| k.host_id()).unwrap_or(0)
}

/// (block width, block height, bytes per block) for a DXGI_FORMAT.
pub fn format_info(f: u32) -> (u32, u32, u32) {
    match f {
        1..=4 => (1, 1, 16),
        5..=8 => (1, 1, 12),
        9..=22 => (1, 1, 8),  // R16G16B16A16, R32G32, R32G8X24
        23..=47 => (1, 1, 4), // R10G10B10A2, R11G11B10, R8G8B8A8, R16G16, R32, R24G8
        48..=59 => (1, 1, 2), // R8G8, R16
        60..=65 => (1, 1, 1), // R8, A8
        67..=69 => (1, 1, 4),
        70..=72 | 79..=81 => (4, 4, 8),            // BC1, BC4
        73..=78 | 82..=84 | 94..=99 => (4, 4, 16), // BC2, BC3, BC5, BC6H, BC7
        85 | 86 | 115 => (1, 1, 2),                // B5G6R5, B5G5R5A1, B4G4R4A4
        87..=93 => (1, 1, 4),                      // B8G8R8A8 / X8
        _ => (1, 1, 4),
    }
}

fn mip_dim(base: u32, mip: u32) -> u32 {
    (base >> mip).max(1)
}

fn row_bytes(format: u32, width: u32) -> u32 {
    let (bw, _, bb) = format_info(format);
    width.div_ceil(bw) * bb
}

fn rows(format: u32, height: u32) -> u32 {
    let (_, bh, _) = format_info(format);
    height.div_ceil(bh)
}

fn set_shader(p: &mut Process, stage: u32) -> R {
    let id = obj_id(p, p.arg(1));
    p.extra
        .gpu
        .emit(op::SET_SHADER, Pk::new().u32(stage).u32(id));
    Ok(0)
}

fn set_slots(p: &mut Process, opcode: u32, stage: u32) -> R {
    let (start, n, arr) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3));
    let mut pk = Pk::new().u32(stage).u32(start).u32(n);
    for i in 0..n as u64 {
        let ptr = if arr != 0 { p.rd64(arr + 8 * i)? } else { 0 };
        pk = pk.u32(obj_id(p, ptr));
    }
    p.extra.gpu.emit(opcode, pk);
    Ok(0)
}

fn set_slots1(p: &mut Process, stage: u32) -> R {
    // (this, StartSlot, NumBuffers, ppBuffers, pFirstConstant, pNumConstants)
    let (start, n, buffers, first, count) = (
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4),
        p.arg(5),
    );
    let mut pk = Pk::new().u32(stage).u32(start).u32(n);
    for i in 0..n as u64 {
        let buffer = if buffers != 0 {
            p.rd64(buffers + 8 * i)?
        } else {
            0
        };
        let first_constant = if first != 0 {
            p.rd32(first + 4 * i)?
        } else {
            0
        };
        let num_constants = if count != 0 {
            p.rd32(count + 4 * i)?
        } else {
            0
        };
        pk = pk
            .u32(obj_id(p, buffer))
            .u32(first_constant)
            .u32(num_constants);
    }
    p.extra.gpu.emit(op::SET_CBUFFERS1, pk);
    Ok(0)
}

fn set_uavs(p: &mut Process) -> R {
    // (this, StartSlot, NumUAVs, ppUAVs, pUAVInitialCounts)
    let (start, n, views, counts) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3), p.arg(4));
    let mut pk = Pk::new().u32(start).u32(n);
    for i in 0..n as u64 {
        let view = if views != 0 {
            p.rd64(views + 8 * i)?
        } else {
            0
        };
        let initial_count = if counts != 0 {
            p.rd32(counts + 4 * i)?
        } else {
            u32::MAX
        };
        pk = pk.u32(obj_id(p, view)).u32(initial_count);
    }
    p.extra.gpu.emit(op::SET_UAVS, pk);
    Ok(0)
}

fn ia_set_vertex_buffers(p: &mut Process) -> R {
    // (this, StartSlot, NumBuffers, ppVertexBuffers, pStrides, pOffsets)
    let (start, n, bufs, strides, offsets) = (
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4),
        p.arg(5),
    );
    let mut pk = Pk::new().u32(start).u32(n);
    for i in 0..n as u64 {
        let ptr = if bufs != 0 { p.rd64(bufs + 8 * i)? } else { 0 };
        let (s, o) = (
            if strides != 0 {
                p.rd32(strides + 4 * i)?
            } else {
                0
            },
            if offsets != 0 {
                p.rd32(offsets + 4 * i)?
            } else {
                0
            },
        );
        pk = pk.u32(obj_id(p, ptr)).u32(s).u32(o);
    }
    p.extra.gpu.emit(op::SET_VERTEX_BUFFERS, pk);
    Ok(0)
}

fn om_set_render_targets(p: &mut Process) -> R {
    let (n, rtvs, dsv) = (p.arg(1) as u32, p.arg(2), p.arg(3));
    let mut pk = Pk::new().u32(n);
    let mut bound = Vec::new();
    for i in 0..n as u64 {
        let ptr = if rtvs != 0 { p.rd64(rtvs + 8 * i)? } else { 0 };
        bound.push(ptr);
        pk = pk.u32(obj_id(p, ptr));
    }
    if let Some(state) = p.extra.com.deferred_contexts.get_mut(&p.arg(0)) {
        state.bound_rtv = bound;
        state.bound_dsv = dsv;
    } else {
        p.extra.com.bound_rtv = bound;
        p.extra.com.bound_dsv = dsv;
    }
    p.extra
        .gpu
        .emit(op::SET_RENDER_TARGETS, pk.u32(obj_id(p, dsv)));
    Ok(0)
}

fn create_buffer(p: &mut Process) -> R {
    // (this, const D3D11_BUFFER_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Buffer**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (size, usage, bind, cpu, misc, stride) = (
        p.rd32(desc)?,
        p.rd32(desc + 4)?,
        p.rd32(desc + 8)?,
        p.rd32(desc + 12)?,
        p.rd32(desc + 16)?,
        p.rd32(desc + 20)?,
    );
    let mut shadow = vec![0u8; size as usize];
    if init != 0 {
        let data = p.rd64(init)?;
        if data != 0 {
            shadow = p.read_bytes(data, size as usize)?;
        }
    }
    let id = new_res(
        p,
        ResInfo {
            is_buffer: true,
            size,
            usage,
            bind,
            cpu,
            misc,
            stride,
            shadow: shadow.clone(),
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_BUFFER,
        Pk::new()
            .u32(id)
            .u32(size)
            .u32(bind)
            .u32(usage)
            .u32(misc)
            .u32(stride),
    );
    if init != 0 {
        p.extra.gpu.emit(
            op::BUFFER_WRITE,
            Pk::new().u32(id).u32(0).u32(size).bytes(&shadow),
        );
    }
    let o = create(p, "ID3D11Buffer", Kind::Buffer { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn mip_count(w: u32, h: u32) -> u32 {
    32 - w.max(h).max(1).leading_zeros()
}

fn mip_count_3d(w: u32, h: u32, d: u32) -> u32 {
    32 - w.max(h).max(d).max(1).leading_zeros()
}

fn create_texture1d(p: &mut Process) -> R {
    // (this, const D3D11_TEXTURE1D_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Texture1D**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, mut mips, array, format) = (
        p.rd32(desc)?,
        p.rd32(desc + 4)?,
        p.rd32(desc + 8)?,
        p.rd32(desc + 12)?,
    );
    let (usage, bind, cpu, misc) = (
        p.rd32(desc + 16)?,
        p.rd32(desc + 20)?,
        p.rd32(desc + 24)?,
        p.rd32(desc + 28)?,
    );
    if w == 0 || array == 0 {
        return Ok(E_INVALIDARG);
    }
    if mips == 0 {
        mips = mip_count(w, 1);
    }
    if mips > mip_count(w, 1) {
        return Ok(E_INVALIDARG);
    }
    // The WebGPU 2D backing requires block-aligned height. D3D11 1D compressed textures are
    // rejected here until their one-row block layout has a verified representation.
    if format_info(format).1 != 1 {
        return Ok(E_NOTIMPL);
    }
    let keep_shadow = cpu != 0;
    let size = row_bytes(format, w);
    let id = new_res(
        p,
        ResInfo {
            width: w,
            height: 1,
            is_1d: true,
            mips,
            array,
            format,
            usage,
            bind,
            cpu,
            misc,
            size,
            shadow: if keep_shadow {
                vec![0u8; size as usize]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_TEXTURE1D,
        Pk::new()
            .u32(id)
            .u32(w)
            .u32(mips)
            .u32(array)
            .u32(format)
            .u32(bind)
            .u32(usage)
            .u32(misc),
    );
    if init != 0 {
        for slice in 0..array {
            for mip in 0..mips {
                let sub = slice * mips + mip;
                let sd = init + 16 * sub as u64;
                let (data, pitch) = (p.rd64(sd)?, p.rd32(sd + 8)?);
                if data == 0 {
                    continue;
                }
                let mw = mip_dim(w, mip);
                let pitch = if pitch == 0 {
                    row_bytes(format, mw)
                } else {
                    pitch
                };
                let len = pitch as usize;
                let range = p
                    .extra
                    .gpu
                    .emit_guest_payload(
                        op::TEXTURE_WRITE,
                    Pk::new()
                        .u32(id)
                        .u32(mip)
                        .u32(slice)
                        .u32(0)
                        .u32(0)
                        .u32(mw)
                        .u32(1)
                        .u32(pitch)
                            .u32(len as u32),
                        &p.mem,
                        data,
                        len,
                    )
                    .map_err(Stop::Crash)?;
                let bytes = &p.extra.gpu.buf[range];
                if keep_shadow && sub == 0 {
                    if let Some(r) = p.extra.com.res.get_mut(&id) {
                        let n = r.shadow.len().min(bytes.len());
                        r.shadow[..n].copy_from_slice(&bytes[..n]);
                    }
                }
            }
        }
    }
    let o = create(p, "ID3D11Texture1D", Kind::Texture1D { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn create_texture2d(p: &mut Process) -> R {
    // (this, const D3D11_TEXTURE2D_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Texture2D**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, h, mut mips, array, format) = (
        p.rd32(desc)?,
        p.rd32(desc + 4)?,
        p.rd32(desc + 8)?,
        p.rd32(desc + 12)?,
        p.rd32(desc + 16)?,
    );
    let (samples, sample_quality, usage, bind, cpu, misc) = (
        p.rd32(desc + 20)?,
        p.rd32(desc + 24)?,
        p.rd32(desc + 28)?,
        p.rd32(desc + 32)?,
        p.rd32(desc + 36)?,
        p.rd32(desc + 40)?,
    );
    if mips == 0 {
        mips = mip_count(w, h);
    }
    let array = array.max(1);
    let sample_count = samples.max(1);
    let supported_multisample = match bind {
        0x20 => supports_4x_multisample_render_target(format),
        0x40 => supports_4x_multisample_depth_resource(format),
        _ => false,
    };
    let invalid_multisample = sample_count > 1
        && (sample_count != 4
            || sample_quality != 0
            || !supported_multisample
            || mips != 1
            || array != 1
            || init != 0
            || usage != 0
            || cpu != 0
            || misc != 0);
    if w == 0 || h == 0 || invalid_multisample {
        return Ok(E_INVALIDARG);
    }
    let keep_shadow = cpu != 0;
    let size = row_bytes(format, w) * rows(format, h);
    let id = new_res(
        p,
        ResInfo {
            width: w,
            height: h,
            mips,
            array,
            format,
            samples: sample_count,
            usage,
            bind,
            cpu,
            misc,
            size,
            shadow: if keep_shadow {
                vec![0u8; size as usize]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_TEXTURE2D,
        Pk::new()
            .u32(id)
            .u32(w)
            .u32(h)
            .u32(mips)
            .u32(array)
            .u32(format)
            .u32(bind)
            .u32(usage)
            .u32(sample_count)
            .u32(misc),
    );
    if init != 0 {
        for slice in 0..array {
            for mip in 0..mips {
                let sub = slice * mips + mip;
                let sd = init + 16 * sub as u64;
                let (data, pitch) = (p.rd64(sd)?, p.rd32(sd + 8)?);
                if data == 0 {
                    continue;
                }
                let (mw, mh) = (mip_dim(w, mip), mip_dim(h, mip));
                let pitch = if pitch == 0 {
                    row_bytes(format, mw)
                } else {
                    pitch
                };
                let nrows = rows(format, mh);
                let len = (pitch * nrows) as usize;
                let range = p
                    .extra
                    .gpu
                    .emit_guest_payload(
                        op::TEXTURE_WRITE,
                    Pk::new()
                        .u32(id)
                        .u32(mip)
                        .u32(slice)
                        .u32(0)
                        .u32(0)
                        .u32(mw)
                        .u32(mh)
                        .u32(pitch)
                            .u32(len as u32),
                        &p.mem,
                        data,
                        len,
                    )
                    .map_err(Stop::Crash)?;
                let bytes = &p.extra.gpu.buf[range];
                if keep_shadow && sub == 0 {
                    if let Some(r) = p.extra.com.res.get_mut(&id) {
                        let n = r.shadow.len().min(bytes.len());
                        r.shadow[..n].copy_from_slice(&bytes[..n]);
                    }
                }
            }
        }
    }
    let o = create(p, "ID3D11Texture2D", Kind::Texture2D { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn create_texture3d(p: &mut Process) -> R {
    // (this, const D3D11_TEXTURE3D_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Texture3D**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, h, depth, mut mips, format) = (
        p.rd32(desc)?,
        p.rd32(desc + 4)?,
        p.rd32(desc + 8)?,
        p.rd32(desc + 12)?,
        p.rd32(desc + 16)?,
    );
    let (usage, bind, cpu, misc) = (
        p.rd32(desc + 20)?,
        p.rd32(desc + 24)?,
        p.rd32(desc + 28)?,
        p.rd32(desc + 32)?,
    );
    if w == 0 || h == 0 || depth == 0 {
        return Ok(E_INVALIDARG);
    }
    if mips == 0 {
        mips = mip_count_3d(w, h, depth);
    }
    let keep_shadow = cpu != 0;
    let base_pitch = row_bytes(format, w);
    let base_depth_pitch = base_pitch * rows(format, h);
    let size = base_depth_pitch.saturating_mul(depth);
    let id = new_res(
        p,
        ResInfo {
            width: w,
            height: h,
            depth,
            is_3d: true,
            mips,
            array: 1,
            format,
            usage,
            bind,
            cpu,
            misc,
            size,
            shadow: if keep_shadow {
                vec![0u8; size as usize]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_TEXTURE3D,
        Pk::new()
            .u32(id)
            .u32(w)
            .u32(h)
            .u32(depth)
            .u32(mips)
            .u32(format)
            .u32(bind)
            .u32(usage)
            .u32(misc),
    );
    if init != 0 {
        for mip in 0..mips {
            let sd = init + 16 * mip as u64;
            let (data, mut pitch, mut depth_pitch) =
                (p.rd64(sd)?, p.rd32(sd + 8)?, p.rd32(sd + 12)?);
            if data == 0 {
                continue;
            }
            let (mw, mh, md) = (mip_dim(w, mip), mip_dim(h, mip), mip_dim(depth, mip));
            if pitch == 0 {
                pitch = row_bytes(format, mw);
            }
            let nrows = rows(format, mh);
            if depth_pitch == 0 {
                depth_pitch = pitch * nrows;
            }
            for z in 0..md {
                let bytes = p.read_bytes(
                    data + z as u64 * depth_pitch as u64,
                    (pitch * nrows) as usize,
                )?;
                p.extra.gpu.emit(
                    op::TEXTURE_WRITE,
                    Pk::new()
                        .u32(id)
                        .u32(mip)
                        .u32(z)
                        .u32(0)
                        .u32(0)
                        .u32(mw)
                        .u32(mh)
                        .u32(pitch)
                        .u32(bytes.len() as u32)
                        .bytes(&bytes),
                );
                if keep_shadow && mip == 0 {
                    let dst = z as usize * base_depth_pitch as usize;
                    if dst < size as usize {
                        let n = (bytes.len())
                            .min(base_depth_pitch as usize)
                            .min(size as usize - dst);
                        p.extra.com.res.get_mut(&id).unwrap().shadow[dst..dst + n]
                            .copy_from_slice(&bytes[..n]);
                    }
                }
            }
        }
    }
    let o = create(p, "ID3D11Texture3D", Kind::Texture3D { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn create_view(p: &mut Process, vk: ViewKind) -> R {
    // (this, ID3D11Resource*, const D3D11_xxx_VIEW_DESC*, ID3D11xxxView**)
    let (res, desc, out) = (p.arg(1), p.arg(2), p.arg(3));
    let Some(rid) = kind_of(p, res).and_then(|k| k.host_id()) else {
        return Ok(E_INVALIDARG);
    };
    let info = p.extra.com.res.get(&rid).cloned().unwrap_or_default();
    let default_dim = if info.is_buffer {
        1
    } else if info.is_3d {
        8
    } else if info.is_1d {
        match (vk, info.array > 1) {
            (ViewKind::Dsv, false) => 1,
            (ViewKind::Dsv, true) => 2,
            (_, false) => 2,
            (_, true) => 3,
        }
    } else if vk == ViewKind::Uav && info.array > 1 {
        5
    } else if vk == ViewKind::Srv && info.misc & 0x4 != 0 {
        if info.array > 6 {
            10
        } else {
            9
        }
    } else {
        4
    };
    let default_buffer_stride = if info.misc & 0x20 != 0 {
        4
    } else {
        info.stride.max(4)
    };
    let default_count = if info.is_buffer {
        info.size / default_buffer_stride
    } else if vk == ViewKind::Uav {
        1
    } else {
        info.mips.max(1)
    };
    let default_slices = if info.is_buffer {
        0
    } else if info.is_3d && vk == ViewKind::Uav {
        mip_dim(info.depth, 0)
    } else {
        info.array.max(1)
    };
    let default_slices = if desc == 0 && vk == ViewKind::Srv && default_dim == 10 {
        default_slices / 6
    } else {
        default_slices
    };
    let (mut format, mut dim, mut a, mut b, mut c, mut d) = (
        info.format,
        default_dim,
        0u32,
        default_count,
        0u32,
        default_slices,
    );
    if desc != 0 {
        let f = p.rd32(desc)?;
        if f != 0 {
            format = f;
        }
        dim = p.rd32(desc + 4)?;
        // Union layout starts at +8: the first fields differ per view kind and dimension.
        match (vk, dim) {
            (ViewKind::Srv, 2) | (ViewKind::Srv, 4) => {
                a = p.rd32(desc + 8)?; // MostDetailedMip
                b = p.rd32(desc + 12)?; // MipLevels (-1 = all)
            }
            (ViewKind::Srv, 3) => {
                a = p.rd32(desc + 8)?; // MostDetailedMip
                b = p.rd32(desc + 12)?; // MipLevels (-1 = all)
                c = p.rd32(desc + 16)?; // FirstArraySlice
                d = p.rd32(desc + 20)?; // ArraySize
            }
            (ViewKind::Srv, 10) => {
                a = p.rd32(desc + 8)?; // MostDetailedMip
                b = p.rd32(desc + 12)?; // MipLevels (-1 = all)
                c = p.rd32(desc + 16)?; // First2DArrayFace
                d = p.rd32(desc + 20)?; // NumCubes
            }
            (ViewKind::Srv, 1) => {
                a = p.rd32(desc + 8)?; // FirstElement
                b = p.rd32(desc + 12)?; // NumElements
            }
            (ViewKind::Srv, 11) => {
                a = p.rd32(desc + 8)?; // FirstElement
                b = p.rd32(desc + 12)?; // NumElements
                c = p.rd32(desc + 16)?; // BUFFEREX flags
            }
            (ViewKind::Srv, 5) => {
                a = p.rd32(desc + 8)?;
                b = p.rd32(desc + 12)?;
                c = p.rd32(desc + 16)?;
                d = p.rd32(desc + 20)?;
            }
            (ViewKind::Srv, 8) => {
                a = p.rd32(desc + 8)?; // MostDetailedMip
                b = p.rd32(desc + 12)?; // MipLevels (-1 = all)
            }
            (ViewKind::Rtv, 2) | (ViewKind::Rtv, 4) | (ViewKind::Dsv, 1) | (ViewKind::Dsv, 3) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
            }
            (ViewKind::Uav, 2) | (ViewKind::Uav, 4) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
            }
            (ViewKind::Uav, 1) => {
                a = p.rd32(desc + 8)?; // FirstElement
                b = p.rd32(desc + 12)?; // NumElements
                c = p.rd32(desc + 16)?; // Flags
                d = 0;
            }
            (ViewKind::Rtv, 3) | (ViewKind::Rtv, 5) | (ViewKind::Dsv, 2) | (ViewKind::Dsv, 4) => {
                a = p.rd32(desc + 8)?;
                b = 1;
                c = p.rd32(desc + 12)?;
                d = p.rd32(desc + 16)?;
            }
            (ViewKind::Uav, 3) | (ViewKind::Uav, 5) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
                c = p.rd32(desc + 12)?; // FirstArraySlice
                d = p.rd32(desc + 16)?; // ArraySize
            }
            (ViewKind::Rtv, 8) | (ViewKind::Uav, 8) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
                c = p.rd32(desc + 12)?; // FirstWSlice
                d = p.rd32(desc + 16)?; // WSize
            }
            _ => {}
        }
    }
    if b == u32::MAX {
        if info.is_buffer {
            b = info
                .size
                .saturating_sub(a.saturating_mul(default_buffer_stride))
                / default_buffer_stride;
        } else {
            b = info.mips.max(1) - a.min(info.mips.max(1) - 1);
        }
    }
    let id = p.extra.gpu.new_id();
    let kind_code = match vk {
        ViewKind::Rtv => 0,
        ViewKind::Dsv => 1,
        ViewKind::Srv => 2,
        ViewKind::Uav => 3,
    };
    p.extra.gpu.emit(
        op::CREATE_VIEW,
        Pk::new()
            .u32(id)
            .u32(kind_code)
            .u32(rid)
            .u32(format)
            .u32(dim)
            .u32(a)
            .u32(b)
            .u32(c)
            .u32(d),
    );
    let iface = match vk {
        ViewKind::Rtv => "ID3D11RenderTargetView",
        ViewKind::Dsv => "ID3D11DepthStencilView",
        ViewKind::Srv => "ID3D11ShaderResourceView",
        ViewKind::Uav => "ID3D11UnorderedAccessView",
    };
    let o = create(
        p,
        iface,
        Kind::View {
            vk,
            id,
            resource: rid,
        },
    );
    p.wr64(out, o)?;
    Ok(S_OK)
}

/// Parse the DXBC input signature (`ISGN`/`ISG1`) -> (semantic name lowercase, index, register).
pub fn input_signature(dxbc: &[u8]) -> Vec<(String, u32, u32)> {
    let rd = |o: usize| {
        dxbc.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let mut out = Vec::new();
    if dxbc.get(..4) != Some(b"DXBC") {
        return out;
    }
    let n = rd(28).unwrap_or(0) as usize;
    for i in 0..n.min(64) {
        let Some(off) = rd(32 + 4 * i) else { break };
        let off = off as usize;
        let tag = dxbc.get(off..off + 4).unwrap_or(&[]);
        if tag != b"ISGN" && tag != b"ISG1" {
            continue;
        }
        let data = off + 8;
        let count = rd(data).unwrap_or(0) as usize;
        let stride = if tag == b"ISG1" { 32 } else { 24 };
        for e in 0..count.min(64) {
            let at = data + 8 + e * stride;
            let (name_off, idx, sv, reg) = if tag == b"ISG1" {
                (rd(at + 4), rd(at + 8), rd(at + 12), rd(at + 20))
            } else {
                (rd(at), rd(at + 4), rd(at + 8), rd(at + 16))
            };
            let (Some(name_off), Some(idx), Some(sv), Some(reg)) = (name_off, idx, sv, reg) else {
                continue;
            };
            if sv != 0 {
                continue; // system-value inputs (SV_VertexID, SV_InstanceID...) have no vertex buffer data
            }
            let s = data + name_off as usize;
            let end = dxbc[s..]
                .iter()
                .position(|&b| b == 0)
                .map(|k| s + k)
                .unwrap_or(s);
            out.push((
                String::from_utf8_lossy(&dxbc[s..end]).to_ascii_lowercase(),
                idx,
                reg,
            ));
        }
    }
    out
}

fn create_input_layout(p: &mut Process) -> R {
    // (this, const D3D11_INPUT_ELEMENT_DESC*, NumElements, pShaderBytecodeWithInputSignature, BytecodeLength, ID3D11InputLayout**)
    let (descs, n, code, len, out) = (
        p.arg(1),
        p.arg(2) as u64,
        p.arg(3),
        p.arg(4) as usize,
        p.arg(5),
    );
    if descs == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let sig = input_signature(&p.read_bytes(code, len)?);
    let mut offsets = [0u32; 16];
    let mut pk = Pk::new();
    let mut count = 0u32;
    for i in 0..n {
        let d = descs + 32 * i;
        let name = p.read_cstr(p.rd64(d)?)?.to_ascii_lowercase();
        let (idx, fmt, slot, off, class, step) = (
            p.rd32(d + 8)?,
            p.rd32(d + 12)?,
            p.rd32(d + 16)?,
            p.rd32(d + 20)?,
            p.rd32(d + 24)?,
            p.rd32(d + 28)?,
        );
        let (bw, _, bytes) = format_info(fmt);
        let _ = bw;
        let slot_i = (slot as usize).min(15);
        let offset = if off == 0xffff_ffff {
            offsets[slot_i]
        } else {
            off
        };
        offsets[slot_i] = offset + bytes;
        if let Some((_, _, reg)) = sig.iter().find(|(n2, i2, _)| *n2 == name && *i2 == idx) {
            pk = pk
                .u32(*reg)
                .u32(fmt)
                .u32(slot)
                .u32(offset)
                .u32(class)
                .u32(step);
            count += 1;
        }
    }
    let id = p.extra.gpu.new_id();
    // Layout records are prefixed by the element count (after the id).
    let body = pk;
    let mut head = Pk::new().u32(id).u32(count);
    head = head.bytes(&body_bytes(body));
    p.extra.gpu.emit(op::CREATE_INPUT_LAYOUT, head);
    let o = create(p, "ID3D11InputLayout", Kind::InputLayout { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn body_bytes(pk: Pk) -> Vec<u8> {
    pk.into_bytes()
}

fn create_shader(p: &mut Process, stage: u32, iface: &'static str) -> R {
    // (this, pShaderBytecode, BytecodeLength, ID3D11ClassLinkage*, ID3D11xxxShader**)
    let (code, len, out) = (p.arg(1), p.arg(2) as usize, p.arg(4));
    if code == 0 || out == 0 || len < 32 {
        return Ok(E_INVALIDARG);
    }
    let bytes = p.read_bytes(code, len)?;
    let id = p.extra.gpu.new_id();
    p.extra.gpu.emit(
        op::CREATE_SHADER,
        Pk::new().u32(id).u32(stage).u32(len as u32).bytes(&bytes),
    );
    let o = create(p, iface, Kind::Shader { id, stage });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn create_state(p: &mut Process, opcode: u32, size: usize, iface: &'static str) -> R {
    // (this, const D3D11_xxx_DESC*, ID3D11xxxState**)
    let (desc, out) = (p.arg(1), p.arg(2));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let raw = p.read_bytes(desc, size)?;
    let id = p.extra.gpu.new_id();
    p.extra.gpu.emit(opcode, Pk::new().u32(id).bytes(&raw));
    let o = create(p, iface, Kind::State { id });
    p.wr64(out, o)?;
    Ok(S_OK)
}

fn copy_resource(p: &mut Process, dst: u64, src: u64) -> Result<(), Stop> {
    let (d, s) = (obj_id(p, dst), obj_id(p, src));
    if d == 0 || s == 0 {
        return Ok(());
    }
    // Keep CPU shadows coherent for buffer -> buffer copies (staging readback of CPU-written data).
    if let (Some(sh), true) = (
        p.extra.com.res.get(&s).map(|r| r.shadow.clone()),
        p.extra.com.res.contains_key(&d),
    ) {
        if let Some(dr) = p.extra.com.res.get_mut(&d) {
            if !sh.is_empty() && dr.shadow.len() == sh.len() {
                dr.shadow = sh;
            }
        }
    }
    p.extra.gpu.emit(op::COPY_RESOURCE, Pk::new().u32(d).u32(s));
    Ok(())
}

fn copy_subresource_region(p: &mut Process) -> R {
    // (this, pDst, DstSubresource, DstX, DstY, DstZ, pSrc, SrcSubresource, pSrcBox)
    let (dst, dsub, dx, dy, dz, src, ssub, sbox) = (
        p.arg(1),
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4) as u32,
        p.arg(5) as u32,
        p.arg(6),
        p.arg(7) as u32,
        p.arg(8),
    );
    let (d, s) = (obj_id(p, dst), obj_id(p, src));
    let mut pk = Pk::new()
        .u32(d)
        .u32(dsub)
        .u32(dx)
        .u32(dy)
        .u32(dz)
        .u32(s)
        .u32(ssub);
    if sbox != 0 {
        for i in 0..6 {
            pk = pk.u32(p.rd32(sbox + 4 * i)?);
        }
        pk = pk.u32(1);
    } else {
        for _ in 0..6 {
            pk = pk.u32(0);
        }
        pk = pk.u32(0);
    }
    p.extra.gpu.emit(op::COPY_REGION, pk);
    Ok(0)
}

fn resolve_subresource(p: &mut Process) -> R {
    let (dst, dst_sub, src, src_sub, format) = (
        obj_id(p, p.arg(1)),
        p.arg(2) as u32,
        obj_id(p, p.arg(3)),
        p.arg(4) as u32,
        p.arg(5) as u32,
    );
    let dst_info = p.extra.com.res.get(&dst);
    let src_info = p.extra.com.res.get(&src);
    if let (Some(dst_info), Some(src_info)) = (dst_info, src_info) {
        if src_info.samples > 1
            && dst_info.samples == 1
            && src_info.format == format
            && dst_info.format == format
        {
            p.extra.gpu.emit(
                op::RESOLVE_TEXTURE,
                Pk::new()
                    .u32(dst)
                    .u32(dst_sub)
                    .u32(src)
                    .u32(src_sub)
                    .u32(format),
            );
        }
    }
    Ok(0)
}

fn update_subresource(p: &mut Process) -> R {
    // (this, pDstResource, DstSubresource, const D3D11_BOX* pDstBox, const void* pSrcData, SrcRowPitch, SrcDepthPitch)
    let (res, sub, bx, src, row_pitch, depth_pitch) = (
        p.arg(1),
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4),
        p.arg(5) as u32,
        p.arg(6) as u32,
    );
    let Some(kind) = kind_of(p, res) else {
        return Ok(0);
    };
    match kind {
        Kind::Buffer { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let (left, right) = if bx != 0 {
                (p.rd32(bx)?, p.rd32(bx + 12)?)
            } else {
                (0, info.size)
            };
            let n = right.saturating_sub(left) as usize;
            let bytes = p.read_bytes(src, n)?;
            if let Some(r) = p.extra.com.res.get_mut(&id) {
                if r.shadow.len() >= left as usize + n {
                    r.shadow[left as usize..left as usize + n].copy_from_slice(&bytes);
                }
            }
            p.extra.gpu.emit(
                op::BUFFER_WRITE,
                Pk::new().u32(id).u32(left).u32(n as u32).bytes(&bytes),
            );
        }
        Kind::Texture1D { id } | Kind::Texture2D { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let (mip, slice) = (sub % info.mips.max(1), sub / info.mips.max(1));
            let (mw, mh) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
            let (x, y, w, h) = if bx != 0 {
                (
                    p.rd32(bx)?,
                    p.rd32(bx + 4)?,
                    p.rd32(bx + 12)? - p.rd32(bx)?,
                    p.rd32(bx + 16)? - p.rd32(bx + 4)?,
                )
            } else {
                (0, 0, mw, mh)
            };
            let pitch = if row_pitch == 0 {
                row_bytes(info.format, w)
            } else {
                row_pitch
            };
            let nrows = rows(info.format, h);
            let len = (pitch * nrows) as usize;
            let range = p
                .extra
                .gpu
                .emit_guest_payload(
                    op::TEXTURE_WRITE,
                Pk::new()
                    .u32(id)
                    .u32(mip)
                    .u32(slice)
                    .u32(x)
                    .u32(y)
                    .u32(w)
                    .u32(h)
                    .u32(pitch)
                        .u32(len as u32),
                    &p.mem,
                    src,
                    len,
                )
                .map_err(Stop::Crash)?;
            let bytes = &p.extra.gpu.buf[range];
        }
        Kind::Texture3D { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let mip = sub % info.mips.max(1);
            let (mw, mh, md) = (
                mip_dim(info.width, mip),
                mip_dim(info.height, mip),
                mip_dim(info.depth, mip),
            );
            let (x, y, z, w, h, depth) = if bx != 0 {
                let (left, top, front) = (p.rd32(bx)?, p.rd32(bx + 4)?, p.rd32(bx + 8)?);
                let (right, bottom, back) = (p.rd32(bx + 12)?, p.rd32(bx + 16)?, p.rd32(bx + 20)?);
                (left, top, front, right - left, bottom - top, back - front)
            } else {
                (0, 0, 0, mw, mh, md)
            };
            let pitch = if row_pitch == 0 {
                row_bytes(info.format, w)
            } else {
                row_pitch
            };
            let nrows = rows(info.format, h);
            let depth_pitch = if depth_pitch == 0 {
                pitch * nrows
            } else {
                depth_pitch
            };
            for slice in 0..depth {
                let bytes = p.read_bytes(
                    src + slice as u64 * depth_pitch as u64,
                    (pitch * nrows) as usize,
                )?;
                p.extra.gpu.emit(
                    op::TEXTURE_WRITE,
                    Pk::new()
                        .u32(id)
                        .u32(mip)
                        .u32(z + slice)
                        .u32(x)
                        .u32(y)
                        .u32(w)
                        .u32(h)
                        .u32(pitch)
                        .u32(bytes.len() as u32)
                        .bytes(&bytes),
                );
            }
        }
        _ => {}
    }
    Ok(0)
}

fn map(p: &mut Process) -> R {
    map_with_readback(p, request_gpu_readback)
}

fn map_with_readback(
    p: &mut Process,
    request_readback: impl FnOnce(&[u8], u32, u32, &mut [u8]) -> i32,
) -> R {
    // (this, pResource, Subresource, MapType, MapFlags, D3D11_MAPPED_SUBRESOURCE*)
    let (res, sub, ty, out) = (p.arg(1), p.arg(2) as u32, p.arg(3) as u32, p.arg(5));
    let Some(id) = obj_id_opt(p, res) else {
        return Ok(E_INVALIDARG);
    };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    let (len, pitch, depth_pitch) = if info.is_buffer {
        (info.size as u64, info.size, info.size)
    } else {
        let mip = sub % info.mips.max(1);
        let (w, h) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
        let pitch = row_bytes(info.format, w);
        let depth_pitch = pitch * rows(info.format, h);
        let depth = if info.is_3d {
            mip_dim(info.depth, mip)
        } else {
            1
        };
        (depth_pitch as u64 * depth as u64, pitch, depth_pitch)
    };
    let addr = if info.map_addr != 0 {
        info.map_addr
    } else {
        kernel32::alloc_block(p, len.max(16), true)
    };
    // MAP_READ=1, WRITE=2, READ_WRITE=3, WRITE_DISCARD=4, WRITE_NO_OVERWRITE=5
    let reads = ty == 1 || ty == 3;
    let writes = ty != 1;
    let cpu_readable_staging = reads && info.usage == 3 && info.cpu & 0x0002_0000 != 0;
    if cpu_readable_staging {
        let commands = std::mem::take(&mut p.extra.gpu.buf);
        let mut bytes = vec![0; len as usize];
        match request_readback(&commands, id, sub, &mut bytes) {
            1 => p.write_bytes(addr, &bytes)?,
            0 => {
                p.extra.gpu.buf = commands;
                return Ok(E_NOTIMPL);
            }
            _ => return Ok(E_FAIL),
        }
    } else if reads || ty == 5 || ty == 2 {
        let sh = info.shadow.clone();
        let n = sh.len().min(len as usize);
        p.write_bytes(addr, &sh[..n])?;
    }
    if let Some(r) = p.extra.com.res.get_mut(&id) {
        r.map_addr = addr;
        r.map_write = writes;
        r.map_sub = sub;
    }
    p.wr64(out, addr)?;
    p.wr32(out + 8, pitch)?;
    p.wr32(out + 12, depth_pitch)?;
    Ok(S_OK)
}

fn obj_id_opt(p: &Process, ptr: u64) -> Option<u32> {
    kind_of(p, ptr).and_then(|k| k.host_id())
}

fn unmap(p: &mut Process) -> R {
    let (res, _sub) = (p.arg(1), p.arg(2));
    let Some(id) = obj_id_opt(p, res) else {
        return Ok(0);
    };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    if info.map_addr == 0 || !info.map_write {
        return Ok(0);
    }
    if info.is_buffer {
        let bytes = p.read_bytes(info.map_addr, info.size as usize)?;
        if let Some(r) = p.extra.com.res.get_mut(&id) {
            r.shadow = bytes.clone();
            r.map_write = false;
        }
        p.extra.gpu.emit(
            op::BUFFER_WRITE,
            Pk::new().u32(id).u32(0).u32(info.size).bytes(&bytes),
        );
    } else {
        let sub = info.map_sub;
        let mip = sub % info.mips.max(1);
        let (w, h) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
        let pitch = row_bytes(info.format, w);
        let nrows = rows(info.format, h);
        let depth_pitch = pitch * nrows;
        let depth = if info.is_3d {
            mip_dim(info.depth, mip)
        } else {
            1
        };
        let bytes = p.read_bytes(info.map_addr, (depth_pitch * depth) as usize)?;
        if let Some(r) = p.extra.com.res.get_mut(&id) {
            if sub == 0 && !r.shadow.is_empty() {
                let n = r.shadow.len().min(bytes.len());
                r.shadow[..n].copy_from_slice(&bytes[..n]);
            }
            r.map_write = false;
        }
        if info.is_3d {
            for z in 0..depth {
                let slice_bytes =
                    &bytes[(z * depth_pitch) as usize..((z + 1) * depth_pitch) as usize];
                p.extra.gpu.emit(
                    op::TEXTURE_WRITE,
                    Pk::new()
                        .u32(id)
                        .u32(mip)
                        .u32(z)
                        .u32(0)
                        .u32(0)
                        .u32(w)
                        .u32(h)
                        .u32(pitch)
                        .u32(slice_bytes.len() as u32)
                        .bytes(slice_bytes),
                );
            }
        } else {
            let slice = sub / info.mips.max(1);
            p.extra.gpu.emit(
                op::TEXTURE_WRITE,
                Pk::new()
                    .u32(id)
                    .u32(mip)
                    .u32(slice)
                    .u32(0)
                    .u32(0)
                    .u32(w)
                    .u32(h)
                    .u32(pitch)
                    .u32(bytes.len() as u32)
                    .bytes(&bytes),
            );
        }
    }
    Ok(0)
}

// ── GetDesc / GetParent (kind dependent) ─────────────────────────────────────

fn get_parent(p: &mut Process) -> R {
    // (this, REFIID riid, void** ppParent)
    let (this, out) = (p.arg(0), p.arg(2));
    dxgi::ensure_singletons(p);
    let parent = match kind_of(p, this) {
        Some(Kind::Adapter) | Some(Kind::SwapChain { .. }) => p.extra.com.factory,
        Some(Kind::Device) => p.extra.com.adapter,
        Some(Kind::Output) => p.extra.com.adapter,
        _ => 0,
    };
    if parent == 0 {
        p.wr64(out, 0)?;
        return Ok(E_NOINTERFACE);
    }
    p.extra.com.objs.get_mut(&parent).map(|o| o.refs += 1);
    p.wr64(out, parent)?;
    Ok(S_OK)
}

fn get_desc(p: &mut Process) -> R {
    let (this, out) = (p.arg(0), p.arg(1));
    let Some(kind) = kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    match kind {
        Kind::Adapter => {
            let mut b = vec![0u8; 304];
            for (i, c) in "AnyWASM WebGPU adapter".encode_utf16().enumerate() {
                b[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
            }
            b[256..260].copy_from_slice(&0x1414u32.to_le_bytes()); // vendor id
            b[260..264].copy_from_slice(&0x8c00u32.to_le_bytes());
            let vram: u64 = 4 << 30;
            b[272..280].copy_from_slice(&vram.to_le_bytes());
            b[288..296].copy_from_slice(&vram.to_le_bytes());
            p.write_bytes(out, &b)?;
        }
        Kind::Output => {
            let mut b = vec![0u8; 96];
            for (i, c) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
                b[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
            }
            b[64..68].copy_from_slice(&0i32.to_le_bytes());
            b[72..76].copy_from_slice(&1920i32.to_le_bytes());
            b[76..80].copy_from_slice(&1080i32.to_le_bytes());
            b[80..84].copy_from_slice(&1u32.to_le_bytes());
            p.write_bytes(out, &b)?;
        }
        Kind::SwapChain { backbuffer, .. } => {
            let Some(Kind::Texture2D { id }) = kind_of(p, backbuffer) else {
                return Ok(E_FAIL);
            };
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let mut b = vec![0u8; 72];
            for (off, v) in [
                (0usize, r.width),
                (4, r.height),
                (8, 60),
                (12, 1),
                (16, r.format),
                (28, 1),
                (36, 0x20),
                (40, 2),
                (56, 1),
            ] {
                b[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
            b[48..56].copy_from_slice(&p.extra.win.main_window.to_le_bytes());
            p.write_bytes(out, &b)?;
        }
        Kind::Buffer { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            for (i, v) in [r.size, r.usage, r.bind, r.cpu, r.misc, r.stride]
                .iter()
                .enumerate()
            {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        Kind::Texture1D { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let vals = [
                r.width, r.mips, r.array, r.format, r.usage, r.bind, r.cpu, r.misc,
            ];
            for (i, v) in vals.iter().enumerate() {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        Kind::Texture2D { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let vals = [
                r.width,
                r.height,
                r.mips,
                r.array,
                r.format,
                r.samples.max(1),
                0,
                r.usage,
                r.bind,
                r.cpu,
                r.misc,
            ];
            for (i, v) in vals.iter().enumerate() {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        Kind::Texture3D { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let vals = [
                r.width, r.height, r.depth, r.mips, r.format, r.usage, r.bind, r.cpu, r.misc,
            ];
            for (i, v) in vals.iter().enumerate() {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        _ => {}
    }
    Ok(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_create_device(levels: Option<&[u32]>) -> (Process, u64) {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const LEVELS: u64 = 0x10000;
        const OUT_LEVEL: u64 = 0x11000;
        const STACK: u64 = 0x20000;
        let mut p = Process::new(Vfs::new());
        for address in [LEVELS, OUT_LEVEL, STACK] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
        if let Some(levels) = levels {
            for (index, level) in levels.iter().enumerate() {
                p.wr32(LEVELS + 4 * index as u64, *level).unwrap();
            }
        }
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 0;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = 0;
        p.cpu.gpr[RSP] = STACK + PAGE_SIZE - 0x100;
        let feature_levels = if levels.is_some() { LEVELS } else { 0 };
        let feature_level_count = levels.map_or(0, |levels| levels.len() as u64);
        let stack = p.cpu.gpr[RSP];
        for (index, value) in [feature_levels, feature_level_count, 7, 0, OUT_LEVEL, 0]
            .into_iter()
            .enumerate()
        {
            p.wr64(stack + 40 + index as u64 * 8, value).unwrap();
        }
        let result = create_device(&mut p, false).unwrap();
        (p, result)
    }

    fn call_create_query(method_name: &str, query_type: u32) -> (Process, u64) {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.wr32(DESC, query_type).unwrap();
        p.wr32(DESC + 4, 0).unwrap();
        p.wr64(OUT, u64::MAX).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = OUT;
        let result = method("ID3D11Device", method_name).unwrap()(&mut p).unwrap();
        (p, result)
    }

    #[test]
    fn map_readback_for_3d_rgba8_preserves_packed_data_and_depth_pitches() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x14000;
        const STACK: u64 = 0x20000;
        let mut p = Process::new(Vfs::new());
        for address in [OUT, STACK] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
        let id = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            id,
            ResInfo {
                width: 2,
                height: 2,
                depth: 2,
                is_3d: true,
                mips: 1,
                array: 1,
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                usage: 3,
                cpu: 0x0002_0000, // D3D11_CPU_ACCESS_READ
                ..Default::default()
            },
        );
        let texture = create(&mut p, "ID3D11Texture3D", Kind::Texture3D { id });
        let stack = STACK + PAGE_SIZE - 0x100;
        p.cpu.gpr[RSP] = stack;
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = 1; // D3D11_MAP_READ
        p.wr64(stack + 48, OUT).unwrap();
        let expected = (0..32).collect::<Vec<u8>>();
        let result = map_with_readback(&mut p, |commands, resource, subresource, output| {
            assert!(commands.is_empty());
            assert_eq!(resource, id);
            assert_eq!(subresource, 0);
            assert_eq!(output.len(), expected.len());
            output.copy_from_slice(&expected);
            1
        })
        .unwrap();

        assert_eq!(result, S_OK);
        let mapped = p.rd64(OUT).unwrap();
        assert_eq!(p.rd32(OUT + 8).unwrap(), 8); // 2 RGBA8 texels per row
        assert_eq!(p.rd32(OUT + 12).unwrap(), 16); // two rows per depth slice
        assert_eq!(p.read_bytes(mapped, expected.len()).unwrap(), expected);
    }

    #[test]
    fn clear_state_unbinds_render_targets_and_emits_a_gpu_state_reset() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::RCX;

        let mut p = Process::new(Vfs::new());
        let context = create(&mut p, "ID3D11DeviceContext1", Kind::Context);
        p.extra.com.bound_rtv = vec![0x1234];
        p.extra.com.bound_dsv = 0x5678;
        p.extra.gpu.emit(op::DRAW, Pk::new().u32(3).u32(0));
        p.cpu.gpr[RCX] = context;

        let clear_state = method("ID3D11DeviceContext1", "ClearState").unwrap();
        assert_eq!(clear_state(&mut p).unwrap(), 0);

        assert!(p.extra.com.bound_rtv.is_empty());
        assert_eq!(p.extra.com.bound_dsv, 0);
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].0, op::DRAW);
        assert_eq!(commands[1], (op::CLEAR_STATE, Vec::new()));
    }

    #[test]
    fn flush1_signals_the_selected_target_idle_event() {
        use crate::process::Handle;
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX};

        let mut p = Process::new(Vfs::new());
        let event = p.new_handle(Handle::Event {
            signaled: false,
            manual: false,
        });
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[R8] = event;

        let flush1 = method("ID3D11DeviceContext3", "Flush1").unwrap();
        assert_eq!(flush1(&mut p).unwrap(), 0);
        assert!(matches!(
            p.handles.get(&event),
            Some(Handle::Event { signaled: true, .. })
        ));
    }

    #[test]
    fn event_query_completes_after_end_for_diligent_idle_wait() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        let query = create(
            &mut p,
            "ID3D11Query",
            Kind::Query {
                query_type: D3D11_QUERY_EVENT,
                complete: false,
            },
        );
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = query;
        p.cpu.gpr[R8] = OUT;
        p.cpu.gpr[R9] = 4;

        let get_data = method("ID3D11DeviceContext3", "GetData").unwrap();
        p.cpu.gpr[R8] = 0;
        assert_eq!(get_data(&mut p).unwrap(), E_INVALIDARG);
        p.cpu.gpr[R8] = OUT;
        assert_eq!(get_data(&mut p).unwrap(), S_FALSE);

        let end = method("ID3D11DeviceContext3", "End").unwrap();
        end(&mut p).unwrap();
        p.cpu.gpr[RDX] = query;
        p.cpu.gpr[R8] = OUT;
        p.cpu.gpr[R9] = 4;
        assert_eq!(get_data(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(OUT).unwrap(), 1);
    }

    #[test]
    fn begin_does_not_clear_a_completed_event_query() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DATA: u64 = 0x12000;
        let (mut p, result) = call_create_query("CreateQuery", D3D11_QUERY_EVENT);
        assert_eq!(result, S_OK);
        assert!(p.mem.map(DATA, PAGE_SIZE, PROT_RW));
        let query = p.rd64(0x11000).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = query;

        method("ID3D11DeviceContext3", "End").unwrap()(&mut p).unwrap();
        method("ID3D11DeviceContext3", "Begin").unwrap()(&mut p).unwrap();
        p.cpu.gpr[R8] = DATA;
        p.cpu.gpr[R9] = 4;
        let get_data = method("ID3D11DeviceContext3", "GetData").unwrap();
        assert_eq!(get_data(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(DATA).unwrap(), 1);
    }

    #[test]
    fn create_query_keeps_event_queries_supported() {
        const OUT: u64 = 0x11000;
        let (p, result) = call_create_query("CreateQuery", D3D11_QUERY_EVENT);
        assert_eq!(result, S_OK);
        let query = p.rd64(OUT).unwrap();
        assert_ne!(query, 0);
        assert!(matches!(
            kind_of(&p, query),
            Some(Kind::Query {
                query_type: D3D11_QUERY_EVENT,
                ..
            })
        ));
    }

    #[test]
    fn create_query_rejects_known_but_unsupported_query_types() {
        const OUT: u64 = 0x11000;
        let (p, result) = call_create_query("CreateQuery", 1); // D3D11_QUERY_OCCLUSION
        assert_eq!(result, E_NOTIMPL);
        assert_eq!(p.rd64(OUT).unwrap(), 0);
    }

    #[test]
    fn create_query_rejects_values_outside_the_d3d11_query_enum() {
        const OUT: u64 = 0x11000;
        let (p, result) = call_create_query("CreateQuery", 16);
        assert_eq!(result, E_INVALIDARG);
        assert_eq!(p.rd64(OUT).unwrap(), 0);
    }

    #[test]
    fn create_predicate_rejects_valid_but_unsupported_predicates() {
        const OUT: u64 = 0x11000;
        let (p, result) = call_create_query("CreatePredicate", 5); // D3D11_QUERY_OCCLUSION_PREDICATE
        assert_eq!(result, E_NOTIMPL);
        assert_eq!(p.rd64(OUT).unwrap(), 0);
    }

    #[test]
    fn create_predicate_rejects_non_predicate_query_types() {
        const OUT: u64 = 0x11000;
        let (p, result) = call_create_query("CreatePredicate", D3D11_QUERY_EVENT);
        assert_eq!(result, E_INVALIDARG);
        assert_eq!(p.rd64(OUT).unwrap(), 0);
    }

    #[test]
    fn create_device_rejects_a_feature_level_it_does_not_support() {
        let (_, result) = call_create_device(Some(&[0xa000])); // D3D_FEATURE_LEVEL_10_0
        assert_eq!(result, E_INVALIDARG);
    }

    #[test]
    fn create_device_selects_11_0_from_the_requested_feature_levels() {
        const OUT_LEVEL: u64 = 0x11000;
        let (p, result) = call_create_device(Some(&[0xa000, D3D_FEATURE_LEVEL_11_0]));
        assert_eq!(result, S_OK);
        assert_eq!(p.rd32(OUT_LEVEL).unwrap(), D3D_FEATURE_LEVEL_11_0);
    }

    #[test]
    fn create_device_rejects_11_1_in_the_requested_feature_levels() {
        let (_, result) =
            call_create_device(Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]));
        assert_eq!(result, E_INVALIDARG);
    }

    #[test]
    fn create_device_uses_11_0_when_no_feature_level_list_is_provided() {
        const OUT_LEVEL: u64 = 0x11000;
        let (p, result) = call_create_device(None);
        assert_eq!(result, S_OK);
        assert_eq!(p.rd32(OUT_LEVEL).unwrap(), D3D_FEATURE_LEVEL_11_0);
    }

    #[test]
    fn create_texture1d_emits_resource_and_initial_texels() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const INIT: u64 = 0x11000;
        const DATA: u64 = 0x12000;
        const OUT: u64 = 0x13000;
        const VIEW_DESC: u64 = 0x14000;
        let mut p = Process::new(Vfs::new());
        for addr in [DESC, INIT, DATA, OUT, VIEW_DESC] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        let desc = [4u32, 3, 2, 28, 0, 8, 0, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        p.write_bytes(DESC, &desc).unwrap();
        let mut expected_texels = Vec::new();
        for subresource in 0..6u64 {
            let mip = (subresource % 3) as u32;
            let width = mip_dim(4, mip);
            let data = DATA + subresource * 64;
            let pitch = width * 4;
            let address = data.to_le_bytes();
            let mut initial = address.to_vec();
            initial.extend(pitch.to_le_bytes()); // SysMemPitch
            initial.extend(pitch.to_le_bytes()); // SysMemSlicePitch
            p.write_bytes(INIT + subresource * 16, &initial).unwrap();
            let texels = (0..width)
                .flat_map(|_| [subresource as u8 + 1, 32, 64, 255])
                .collect::<Vec<_>>();
            p.write_bytes(data, &texels).unwrap();
            expected_texels.push((mip, (subresource / 3) as u32, width, pitch, texels));
        }
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = INIT;
        p.cpu.gpr[R9] = OUT;

        let Some(create_texture) = method("ID3D11Device", "CreateTexture1D") else {
            panic!("CreateTexture1D should be exposed by the D3D11 device");
        };
        assert_eq!(create_texture(&mut p).unwrap(), S_OK);

        let texture = p.rd64(OUT).unwrap();
        let texture_id = kind_of(&p, texture)
            .and_then(|kind| kind.host_id())
            .expect("CreateTexture1D should return a GPU texture resource");
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 7);
        assert_eq!(commands[0].0, 53); // CREATE_TEXTURE1D
        let fields = commands[0]
            .1
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields, vec![texture_id, 4, 3, 2, 28, 8, 0, 0]);
        for (index, (mip, slice, width, pitch, texels)) in expected_texels.iter().enumerate() {
            let (opcode, payload) = &commands[index + 1];
            assert_eq!(*opcode, op::TEXTURE_WRITE);
            let fields = payload[..36]
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(
                fields,
                vec![texture_id, *mip, *slice, 0, 0, *width, 1, *pitch, *pitch]
            );
            assert_eq!(&payload[36..], texels);
        }

        p.cpu.gpr[RCX] = texture;
        p.cpu.gpr[RDX] = OUT;
        assert_eq!(
            method("ID3D11Resource", "GetType").unwrap()(&mut p).unwrap(),
            S_OK
        );
        assert_eq!(p.rd32(OUT).unwrap(), 2); // D3D11_RESOURCE_DIMENSION_TEXTURE1D
        p.cpu.gpr[RCX] = texture;
        p.cpu.gpr[RDX] = DESC;
        assert_eq!(
            method("ID3D11Texture1D", "GetDesc").unwrap()(&mut p).unwrap(),
            S_OK
        );
        let actual_desc = (0..8)
            .map(|index| p.rd32(DESC + index * 4).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual_desc, vec![4, 3, 2, 28, 0, 8, 0, 0]);

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0; // default Texture1DArray SRV
        p.cpu.gpr[R9] = OUT;
        assert_eq!(
            method("ID3D11Device", "CreateShaderResourceView").unwrap()(&mut p).unwrap(),
            S_OK
        );
        let srv = p.rd64(OUT).unwrap();
        let view_id = kind_of(&p, srv).and_then(|kind| kind.host_id()).unwrap();
        let (opcode, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        assert_eq!(opcode, op::CREATE_VIEW);
        let view_fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(view_fields, vec![view_id, 2, texture_id, 28, 3, 0, 3, 0, 2]);

        let explicit_view = [28u32, 3, 1, 1, 1, 1]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        p.write_bytes(VIEW_DESC, &explicit_view).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = VIEW_DESC;
        p.cpu.gpr[R9] = OUT;
        assert_eq!(
            method("ID3D11Device", "CreateShaderResourceView").unwrap()(&mut p).unwrap(),
            S_OK
        );
        let explicit_view_id = kind_of(&p, p.rd64(OUT).unwrap())
            .and_then(|kind| kind.host_id())
            .unwrap();
        let (_, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        let fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            vec![explicit_view_id, 2, texture_id, 28, 3, 1, 1, 1, 1]
        );
    }

    #[test]
    fn create_texturecube_array_srv_tracks_cube_ranges() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const VIEW_DESC: u64 = 0x11000;
        const OUT: u64 = 0x12000;
        let mut p = Process::new(Vfs::new());
        for address in [DESC, VIEW_DESC, OUT] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
        let texture_desc = [4u32, 4, 1, 18, 28, 1, 0, 0, 0x8, 0, 0x4]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        p.write_bytes(DESC, &texture_desc).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = OUT;
        assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);

        let texture = p.rd64(OUT).unwrap();
        let texture_id = kind_of(&p, texture)
            .and_then(|kind| kind.host_id())
            .expect("CreateTexture2D should return a GPU texture resource");
        let create_srv = method("ID3D11Device", "CreateShaderResourceView").unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = OUT;
        assert_eq!(create_srv(&mut p).unwrap(), S_OK);

        let default_view_id = kind_of(&p, p.rd64(OUT).unwrap())
            .and_then(|kind| kind.host_id())
            .unwrap();
        let (_, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        let default_fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            default_fields,
            vec![default_view_id, 2, texture_id, 28, 10, 0, 1, 0, 3]
        );

        let explicit_desc = [28u32, 10, 0, 1, 6, 2]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        p.write_bytes(VIEW_DESC, &explicit_desc).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = VIEW_DESC;
        p.cpu.gpr[R9] = OUT;
        assert_eq!(create_srv(&mut p).unwrap(), S_OK);

        let explicit_view_id = kind_of(&p, p.rd64(OUT).unwrap())
            .and_then(|kind| kind.host_id())
            .unwrap();
        let (_, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        let explicit_fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            explicit_fields,
            vec![explicit_view_id, 2, texture_id, 28, 10, 0, 1, 6, 2]
        );
    }

    #[test]
    fn create_texture3d_emits_volume_creation_and_each_initial_z_slice() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const INIT: u64 = 0x11000;
        const DATA: u64 = 0x12000;
        const OUT: u64 = 0x13000;
        let mut p = Process::new(Vfs::new());
        for addr in [DESC, INIT, DATA, OUT] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        let mut desc = Vec::new();
        for value in [4u32, 4, 2, 1, 28, 0, 8, 0, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap();
        let mut initial = DATA.to_le_bytes().to_vec();
        initial.extend(16u32.to_le_bytes()); // row pitch
        initial.extend(64u32.to_le_bytes()); // depth pitch
        p.write_bytes(INIT, &initial).unwrap();
        let mut volume = vec![0u8; 128];
        for (z, rgba) in [[255, 0, 0, 255], [0, 255, 0, 255]].into_iter().enumerate() {
            for pixel in 0..16 {
                volume[z * 64 + pixel * 4..z * 64 + pixel * 4 + 4].copy_from_slice(&rgba);
            }
        }
        p.write_bytes(DATA, &volume).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = INIT;
        p.cpu.gpr[R9] = OUT;

        let create_texture = method("ID3D11Device", "CreateTexture3D").unwrap();
        assert_eq!(create_texture(&mut p).unwrap(), S_OK);

        let texture = p.rd64(OUT).unwrap();
        let texture_id = match kind_of(&p, texture) {
            Some(Kind::Texture3D { id }) => id,
            other => panic!("expected Texture3D resource, got {other:?}"),
        };
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].0, op::CREATE_TEXTURE3D);
        let fields = commands[0]
            .1
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields, vec![texture_id, 4, 4, 2, 1, 28, 8, 0, 0]);
        for (z, rgba) in [[255, 0, 0, 255], [0, 255, 0, 255]].into_iter().enumerate() {
            let (opcode, payload) = &commands[z + 1];
            assert_eq!(*opcode, op::TEXTURE_WRITE);
            let fields = payload[..36]
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(fields, vec![texture_id, 0, z as u32, 0, 0, 4, 4, 16, 64]);
            assert!(payload[36..].chunks_exact(4).all(|pixel| pixel == rgba));
        }

        let mut srv_desc = Vec::new();
        for value in [28u32, 8, 0, 1] {
            srv_desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &srv_desc).unwrap(); // Texture3D SRV: first mip 0, one mip
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;
        let create_srv = method("ID3D11Device", "CreateShaderResourceView").unwrap();
        assert_eq!(create_srv(&mut p).unwrap(), S_OK);
        let srv = p.rd64(OUT).unwrap();
        let view_id = match kind_of(&p, srv) {
            Some(Kind::View {
                vk: ViewKind::Srv,
                id,
                ..
            }) => id,
            other => panic!("expected Texture3D SRV, got {other:?}"),
        };
        let (opcode, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        assert_eq!(opcode, op::CREATE_VIEW);
        let fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields, vec![view_id, 2, texture_id, 28, 8, 0, 1, 0, 1]);
    }

    #[test]
    fn update_texture3d_emits_each_boxed_z_slice_with_depth_pitch() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const BOX: u64 = 0x10000;
        const DATA: u64 = 0x11000;
        const STACK: u64 = 0x12000;
        let mut p = Process::new(Vfs::new());
        let id = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            id,
            ResInfo {
                width: 4,
                height: 4,
                depth: 4,
                is_3d: true,
                mips: 1,
                array: 1,
                format: 28,
                ..Default::default()
            },
        );
        let texture = create(&mut p, "ID3D11Texture3D", Kind::Texture3D { id });
        for addr in [BOX, DATA, STACK] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        for (i, value) in [1u32, 1, 1, 3, 3, 3].into_iter().enumerate() {
            p.wr32(BOX + 4 * i as u64, value).unwrap();
        }
        let bytes = (0..32).map(|value| value as u8).collect::<Vec<_>>();
        p.write_bytes(DATA, &bytes).unwrap();
        p.cpu.gpr[RSP] = STACK;
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = BOX;
        p.wr64(STACK + 8 + 8 * 4, DATA).unwrap();
        p.wr64(STACK + 8 + 8 * 5, 8).unwrap();
        p.wr64(STACK + 8 + 8 * 6, 16).unwrap();

        let update = method("ID3D11DeviceContext", "UpdateSubresource").unwrap();
        assert_eq!(update(&mut p).unwrap(), 0);

        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 2);
        for (slice, command) in commands.iter().enumerate() {
            let (opcode, payload) = command;
            assert_eq!(*opcode, op::TEXTURE_WRITE);
            let fields = payload[..36]
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(fields, vec![id, 0, 1 + slice as u32, 1, 1, 2, 2, 8, 16]);
            assert_eq!(&payload[36..], &bytes[slice * 16..(slice + 1) * 16]);
        }
    }

    #[test]
    fn format_support_advertises_typed_uav_for_supported_formats() {
        const TYPED_UAV: u32 = 0x0200_0000;

        assert_eq!(format_support_flags(0), 0x0002_0001); // DXGI_FORMAT_UNKNOWN: buffers and CPU lockable only.

        assert_ne!(format_support_flags(41) & TYPED_UAV, 0); // DXGI_FORMAT_R32_FLOAT
        assert_ne!(format_support_flags(42) & TYPED_UAV, 0); // DXGI_FORMAT_R32_UINT
        assert_ne!(format_support_flags(43) & TYPED_UAV, 0); // DXGI_FORMAT_R32_SINT
        assert_ne!(format_support_flags(2) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32B32A32_FLOAT
        assert_ne!(format_support_flags(3) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32B32A32_UINT
        assert_ne!(format_support_flags(4) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32B32A32_SINT
        assert_ne!(format_support_flags(16) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32_FLOAT
        assert_ne!(format_support_flags(17) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32_UINT
        assert_ne!(format_support_flags(18) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32_SINT
        assert_ne!(format_support_flags(12) & TYPED_UAV, 0); // DXGI_FORMAT_R16G16B16A16_UINT
        assert_ne!(format_support_flags(14) & TYPED_UAV, 0); // DXGI_FORMAT_R16G16B16A16_SINT
        assert_ne!(format_support_flags(10) & TYPED_UAV, 0); // DXGI_FORMAT_R16G16B16A16_FLOAT
        assert_ne!(format_support_flags(28) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_UNORM
        assert_ne!(format_support_flags(31) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_SNORM
        assert_ne!(format_support_flags(30) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_UINT
        assert_ne!(format_support_flags(32) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_SINT
        assert_ne!(
            format_support_flags(28) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE,
            0
        );
        assert_ne!(
            format_support_flags(28) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_ne!(
            format_support_flags(87) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_ne!(
            format_support_flags(10) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE,
            0
        );
        assert_ne!(
            format_support_flags(10) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_eq!(
            format_support_flags(12) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_eq!(
            format_support_flags(14) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_eq!(
            format_support_flags(54) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET,
            0
        );
        assert_eq!(format_support_flags(54) & TYPED_UAV, 0); // DXGI_FORMAT_R16_FLOAT
        assert_eq!(format_support_flags(61) & TYPED_UAV, 0); // DXGI_FORMAT_R8_UNORM
    }

    #[test]
    fn check_feature_support_reports_typed_uav_format_queries() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DATA: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DATA, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 2; // D3D11_FEATURE_FORMAT_SUPPORT
        p.cpu.gpr[R8] = DATA;
        p.cpu.gpr[R9] = 8;
        p.wr32(DATA, DXGI_FORMAT_R8G8B8A8_UINT).unwrap();

        let check = method("ID3D11Device", "CheckFeatureSupport").unwrap();
        assert_eq!(check(&mut p).unwrap(), S_OK);
        assert_ne!(
            p.rd32(DATA + 4).unwrap() & D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW,
            0
        );

        p.cpu.gpr[RDX] = 3; // D3D11_FEATURE_FORMAT_SUPPORT2
        p.wr32(DATA, DXGI_FORMAT_R8G8B8A8_UINT).unwrap();
        assert_eq!(check(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(DATA + 4).unwrap(), 0x80); // D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE

        p.wr32(DATA, 54).unwrap(); // DXGI_FORMAT_R16_FLOAT is not a supported typed UAV.
        assert_eq!(check(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(DATA + 4).unwrap(), 0);

        p.cpu.gpr[R9] = 4;
        assert_eq!(check(&mut p).unwrap(), E_INVALIDARG);
        p.cpu.gpr[RDX] = 99;
        p.cpu.gpr[R9] = 8;
        assert_eq!(check(&mut p).unwrap(), E_INVALIDARG);
    }

    #[test]
    fn check_feature_support_reports_disabled_d3d11_options1_capabilities() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DATA: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DATA, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 10; // D3D11_FEATURE_D3D11_OPTIONS1
        p.cpu.gpr[R8] = DATA;
        p.cpu.gpr[R9] = 16; // Four 32-bit fields.
        p.write_bytes(DATA, &[0xff; 16]).unwrap();

        let check = method("ID3D11Device2", "CheckFeatureSupport").unwrap();
        assert_eq!(check(&mut p).unwrap(), S_OK);
        assert_eq!(p.read_bytes(DATA, 16).unwrap(), vec![0; 16]);

        p.cpu.gpr[R9] = 12;
        assert_eq!(check(&mut p).unwrap(), E_INVALIDARG);
    }

    #[test]
    fn create_deferred_context_returns_a_deferred_context_with_its_flags() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 0; // ContextFlags is reserved and must be zero.
        p.cpu.gpr[R8] = OUT;

        let create_context = method("ID3D11Device", "CreateDeferredContext")
            .expect("CreateDeferredContext should be implemented");
        assert_eq!(create_context(&mut p).unwrap(), S_OK);
        let context = p.rd64(OUT).unwrap();
        assert_ne!(context, 0);

        p.cpu.gpr[RCX] = context;
        let get_type = method("ID3D11DeviceContext", "GetType").unwrap();
        assert_eq!(get_type(&mut p).unwrap(), 1); // D3D11_DEVICE_CONTEXT_DEFERRED

        let get_flags = method("ID3D11DeviceContext", "GetContextFlags").unwrap();
        assert_eq!(get_flags(&mut p).unwrap(), 0);
    }

    #[test]
    fn create_deferred_context_rejects_nonzero_reserved_flags() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.wr64(OUT, 0xfeed).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 1;
        p.cpu.gpr[R8] = OUT;

        let create_context = method("ID3D11Device", "CreateDeferredContext").unwrap();
        assert_eq!(create_context(&mut p).unwrap(), E_INVALIDARG);
        assert_eq!(p.rd64(OUT).unwrap(), 0);
        assert!(p.extra.com.deferred_contexts.is_empty());
    }

    #[test]
    fn command_list_execution_brackets_recorded_commands_and_restores_state() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let immediate = create(&mut p, "ID3D11DeviceContext", Kind::Context);
        p.wr64(OUT, 0xfeed).unwrap();
        p.cpu.gpr[RCX] = immediate;
        p.cpu.gpr[RDX] = 0;
        p.cpu.gpr[R8] = OUT;
        assert_eq!(
            method("ID3D11DeviceContext", "FinishCommandList").unwrap()(&mut p).unwrap(),
            DXGI_ERROR_INVALID_CALL
        );
        assert_eq!(p.rd64(OUT).unwrap(), 0);

        let deferred = create(&mut p, "ID3D11DeviceContext", Kind::Context);
        p.extra
            .com
            .deferred_contexts
            .insert(deferred, DeferredContextState::default());
        let mut recorded = gpu::GpuStream::default();
        recorded.emit(op::SET_TOPOLOGY, Pk::new().u32(4));
        p.extra
            .com
            .deferred_contexts
            .get_mut(&deferred)
            .unwrap()
            .commands = recorded.buf;

        p.cpu.gpr[RCX] = deferred;
        p.cpu.gpr[RDX] = 1; // RestoreDeferredContextState
        p.cpu.gpr[R8] = OUT;
        assert_eq!(
            method("ID3D11DeviceContext", "FinishCommandList").unwrap()(&mut p).unwrap(),
            S_OK
        );
        let command_list = p.rd64(OUT).unwrap();
        assert!(matches!(
            kind_of(&p, command_list),
            Some(Kind::CommandList { .. })
        ));
        p.cpu.gpr[RCX] = command_list;
        assert_eq!(
            method("ID3D11CommandList", "GetContextFlags").unwrap()(&mut p).unwrap(),
            0
        );

        p.extra.gpu.buf.clear();
        p.cpu.gpr[RCX] = immediate;
        p.cpu.gpr[RDX] = command_list;
        p.cpu.gpr[R8] = 1; // Restore immediate-context state.
        assert_eq!(
            method("ID3D11DeviceContext", "ExecuteCommandList").unwrap()(&mut p).unwrap(),
            0
        );
        let opcodes = gpu::decode(&p.extra.gpu.buf)
            .into_iter()
            .map(|(opcode, _)| opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                op::PUSH_STATE,
                op::CLEAR_STATE,
                op::SET_TOPOLOGY,
                op::POP_STATE
            ]
        );

        p.extra.gpu.buf.clear();
        p.cpu.gpr[RCX] = immediate;
        p.cpu.gpr[RDX] = command_list;
        p.cpu.gpr[R8] = 0; // Leave the immediate context cleared.
        assert_eq!(
            method("ID3D11DeviceContext", "ExecuteCommandList").unwrap()(&mut p).unwrap(),
            0
        );
        let opcodes = gpu::decode(&p.extra.gpu.buf)
            .into_iter()
            .map(|(opcode, _)| opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![op::CLEAR_STATE, op::SET_TOPOLOGY, op::CLEAR_STATE]
        );
    }

    #[test]
    fn multisample_quality_levels_match_the_supported_resolve_path() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[R9] = OUT;

        let query = method("ID3D11Device", "CheckMultisampleQualityLevels").unwrap();
        let supported = [
            (DXGI_FORMAT_R8G8B8A8_UNORM, "RGBA8_UNORM"),
            (DXGI_FORMAT_B8G8R8A8_UNORM, "BGRA8_UNORM"),
            (DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, "RGBA8_SRGB"),
            (DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, "BGRA8_SRGB"),
            (DXGI_FORMAT_R16G16B16A16_FLOAT, "RGBA16_FLOAT"),
            (DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2_UNORM"),
            (DXGI_FORMAT_R11G11B10_FLOAT, "R11G11B10_FLOAT"),
            (DXGI_FORMAT_D32_FLOAT, "D32_FLOAT"),
            (DXGI_FORMAT_D24_UNORM_S8_UINT, "D24_UNORM_S8_UINT"),
            (DXGI_FORMAT_D16_UNORM, "D16_UNORM"),
        ];
        for (format, name) in supported {
            for samples in [2u32, 4, 8] {
                p.cpu.gpr[RDX] = format as u64;
                p.cpu.gpr[R8] = samples as u64;
                assert_eq!(query(&mut p).unwrap(), S_OK);
                assert_eq!(
                    p.rd32(OUT).unwrap(),
                    u32::from(samples == 4),
                    "{name} at {samples}x should report support only for the 4x path"
                );
            }
        }

        for (format, name) in [
            (54u32, "R16_FLOAT"),
            (DXGI_FORMAT_R8G8B8A8_UINT, "RGBA8_UINT"),
        ] {
            for samples in [2u32, 4, 8] {
                p.cpu.gpr[RDX] = format as u64;
                p.cpu.gpr[R8] = samples as u64;
                assert_eq!(query(&mut p).unwrap(), S_OK);
                assert_eq!(p.rd32(OUT).unwrap(), 0, "{name} at {samples}x is unsupported");
            }
        }
    }

    #[test]
    fn create_texture2d_accepts_multisampled_depth_stencil_resources() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [8u32, 8, 1, 1, 44, 4, 0, 0, 0x40, 0, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = OUT;

        assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
        assert_ne!(p.rd64(OUT).unwrap(), 0);
        let stream = &p.extra.gpu.buf;
        assert_eq!(
            u32::from_le_bytes(stream[0..4].try_into().unwrap()),
            op::CREATE_TEXTURE2D
        );
        assert_eq!(u32::from_le_bytes(stream[28..32].try_into().unwrap()), 44); // typeless D24S8 backing
        assert_eq!(u32::from_le_bytes(stream[32..36].try_into().unwrap()), 0x40); // depth-stencil bind
        assert_eq!(u32::from_le_bytes(stream[40..44].try_into().unwrap()), 4); // sample count
    }

    #[test]
    fn create_texture2d_forwards_supported_sample_count() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [8u32, 8, 1, 1, 28, 4, 0, 0, 0x20, 0, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DESC;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = OUT;

        assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
        assert_ne!(p.rd64(OUT).unwrap(), 0);
        let mut bgra_desc = desc;
        bgra_desc[16..20].copy_from_slice(&DXGI_FORMAT_B8G8R8A8_UNORM.to_le_bytes());
        p.write_bytes(DESC, &bgra_desc).unwrap();
        p.cpu.gpr[R9] = OUT + 8;
        assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
        assert_ne!(p.rd64(OUT + 8).unwrap(), 0);

        let mut float16_desc = bgra_desc.clone();
        float16_desc[16..20].copy_from_slice(&DXGI_FORMAT_R16G16B16A16_FLOAT.to_le_bytes());
        p.write_bytes(DESC, &float16_desc).unwrap();
        p.cpu.gpr[R9] = OUT + 16;
        assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
        assert_ne!(p.rd64(OUT + 16).unwrap(), 0);

        for (index, format) in [
            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
            DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        ]
        .into_iter()
        .enumerate()
        {
            let mut srgb_desc = float16_desc.clone();
            srgb_desc[16..20].copy_from_slice(&format.to_le_bytes());
            p.write_bytes(DESC, &srgb_desc).unwrap();
            let out = OUT + 24 + index as u64 * 8;
            p.cpu.gpr[R9] = out;
            assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
            assert_ne!(p.rd64(out).unwrap(), 0);
        }

        for (index, format) in [
            DXGI_FORMAT_R10G10B10A2_UNORM,
            DXGI_FORMAT_R11G11B10_FLOAT,
        ]
        .into_iter()
        .enumerate()
        {
            let mut packed_desc = float16_desc.clone();
            packed_desc[16..20].copy_from_slice(&format.to_le_bytes());
            p.write_bytes(DESC, &packed_desc).unwrap();
            let out = OUT + 40 + index as u64 * 8;
            p.cpu.gpr[R9] = out;
            assert_eq!(create_texture2d(&mut p).unwrap(), S_OK);
            assert_ne!(p.rd64(out).unwrap(), 0);
        }

        let stream = &p.extra.gpu.buf;
        assert_eq!(
            u32::from_le_bytes(stream[0..4].try_into().unwrap()),
            op::CREATE_TEXTURE2D
        );
        assert_eq!(u32::from_le_bytes(stream[40..44].try_into().unwrap()), 4);
        assert_eq!(
            u32::from_le_bytes(stream[76..80].try_into().unwrap()),
            DXGI_FORMAT_B8G8R8A8_UNORM
        );
        assert_eq!(u32::from_le_bytes(stream[88..92].try_into().unwrap()), 4);
        assert_eq!(
            u32::from_le_bytes(stream[124..128].try_into().unwrap()),
            DXGI_FORMAT_R16G16B16A16_FLOAT
        );
        assert_eq!(u32::from_le_bytes(stream[136..140].try_into().unwrap()), 4);
        assert_eq!(
            u32::from_le_bytes(stream[268..272].try_into().unwrap()),
            DXGI_FORMAT_R10G10B10A2_UNORM
        );
        assert_eq!(u32::from_le_bytes(stream[280..284].try_into().unwrap()), 4);
        assert_eq!(
            u32::from_le_bytes(stream[316..320].try_into().unwrap()),
            DXGI_FORMAT_R11G11B10_FLOAT
        );
        assert_eq!(u32::from_le_bytes(stream[328..332].try_into().unwrap()), 4);
    }

    #[test]
    fn resolve_subresource_emits_the_source_and_destination_resources() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const STACK: u64 = 0x20000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        let dst_id = p.extra.gpu.new_id();
        let src_id = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            dst_id,
            ResInfo {
                width: 8,
                height: 8,
                format: 28,
                samples: 1,
                ..Default::default()
            },
        );
        p.extra.com.res.insert(
            src_id,
            ResInfo {
                width: 8,
                height: 8,
                format: 28,
                samples: 4,
                ..Default::default()
            },
        );
        let dst = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: dst_id });
        let src = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: src_id });

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = dst;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = src;
        p.cpu.gpr[RSP] = STACK;
        p.wr64(STACK + 40, 0).unwrap(); // SrcSubresource
        p.wr64(STACK + 48, 28).unwrap(); // DXGI_FORMAT_R8G8B8A8_UNORM

        let resolve = method("ID3D11DeviceContext", "ResolveSubresource").unwrap();
        assert_eq!(resolve(&mut p).unwrap(), 0);
        let stream = &p.extra.gpu.buf;
        assert_eq!(
            u32::from_le_bytes(stream[0..4].try_into().unwrap()),
            op::RESOLVE_TEXTURE
        );
        let payload = &stream[8..28];
        let fields = payload
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields, vec![dst_id, 0, src_id, 0, 28]);

        let dst16_id = p.extra.gpu.new_id();
        let src16_id = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            dst16_id,
            ResInfo {
                width: 8,
                height: 8,
                format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                samples: 1,
                ..Default::default()
            },
        );
        p.extra.com.res.insert(
            src16_id,
            ResInfo {
                width: 8,
                height: 8,
                format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                samples: 4,
                ..Default::default()
            },
        );
        let dst16 = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: dst16_id });
        let src16 = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: src16_id });
        p.cpu.gpr[RDX] = dst16;
        p.cpu.gpr[R9] = src16;
        p.wr64(STACK + 48, DXGI_FORMAT_R16G16B16A16_FLOAT as u64)
            .unwrap();
        assert_eq!(resolve(&mut p).unwrap(), 0);
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 2);
        let (_, payload) = &commands[1];
        let fields = payload
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            vec![dst16_id, 0, src16_id, 0, DXGI_FORMAT_R16G16B16A16_FLOAT]
        );

        let dst_srgb_id = p.extra.gpu.new_id();
        let src_srgb_id = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            dst_srgb_id,
            ResInfo {
                width: 8,
                height: 8,
                format: DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                samples: 1,
                ..Default::default()
            },
        );
        p.extra.com.res.insert(
            src_srgb_id,
            ResInfo {
                width: 8,
                height: 8,
                format: DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                samples: 4,
                ..Default::default()
            },
        );
        let dst_srgb = create(
            &mut p,
            "ID3D11Texture2D",
            Kind::Texture2D { id: dst_srgb_id },
        );
        let src_srgb = create(
            &mut p,
            "ID3D11Texture2D",
            Kind::Texture2D { id: src_srgb_id },
        );
        p.cpu.gpr[RDX] = dst_srgb;
        p.cpu.gpr[R9] = src_srgb;
        p.wr64(STACK + 48, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB as u64)
            .unwrap();
        assert_eq!(resolve(&mut p).unwrap(), 0);
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 3);
        let (_, payload) = &commands[2];
        let fields = payload
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            vec![
                dst_srgb_id,
                0,
                src_srgb_id,
                0,
                DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            ]
        );
    }

    #[test]
    fn create_unordered_access_view_tracks_texture2d_mip_slice() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        let resource = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            resource,
            ResInfo {
                width: 32,
                height: 32,
                mips: 4,
                array: 1,
                format: DXGI_FORMAT_R32_FLOAT,
                ..Default::default()
            },
        );
        let texture = create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id: resource });
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [DXGI_FORMAT_R32_FLOAT, 4, 2, 0, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap(); // D3D11_UAV_DIMENSION_TEXTURE2D, MipSlice=2
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;

        let create_uav = method("ID3D11Device1", "CreateUnorderedAccessView").unwrap();
        assert_eq!(create_uav(&mut p).unwrap(), S_OK);
        let view = p.rd64(OUT).unwrap();
        assert!(matches!(
            kind_of(&p, view),
            Some(Kind::View {
                vk: ViewKind::Uav,
                resource: id,
                ..
            }) if id == resource
        ));

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::CREATE_VIEW);
        assert_eq!(u32::from_le_bytes(payload[4..8].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(payload[20..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(payload[24..28].try_into().unwrap()), 1);
    }

    #[test]
    fn create_unordered_access_view_tracks_buffer_range_and_flags() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        let resource = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            resource,
            ResInfo {
                is_buffer: true,
                size: 64,
                stride: 4,
                misc: 0x40, // D3D11_RESOURCE_MISC_BUFFER_STRUCTURED
                ..Default::default()
            },
        );
        let buffer = create(&mut p, "ID3D11Buffer", Kind::Buffer { id: resource });
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [0u32, 1, 2, 3, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap(); // BUFFER, FirstElement=2, NumElements=3, Flags=0
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = buffer;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;

        let create_uav = method("ID3D11Device1", "CreateUnorderedAccessView").unwrap();
        assert_eq!(create_uav(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::CREATE_VIEW);
        let fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields[1..], [3, resource, 0, 1, 2, 3, 0, 0]);
    }

    #[test]
    fn create_raw_buffer_uav_preserves_raw_flag_and_element_range() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        let resource = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            resource,
            ResInfo {
                is_buffer: true,
                size: 64,
                misc: 0x20, // D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS
                ..Default::default()
            },
        );
        let buffer = create(&mut p, "ID3D11Buffer", Kind::Buffer { id: resource });
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [39u32, 1, 2, 3, 1] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap(); // R32_TYPELESS raw UAV, elements [2, 5)
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = buffer;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;

        let create_uav = method("ID3D11Device1", "CreateUnorderedAccessView").unwrap();
        assert_eq!(create_uav(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::CREATE_VIEW);
        let fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields[1..], [3, resource, 39, 1, 2, 3, 1, 0]);
    }

    #[test]
    fn create_structured_buffer_srv_preserves_element_range() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const DESC: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        let resource = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            resource,
            ResInfo {
                is_buffer: true,
                size: 64,
                stride: 4,
                misc: 0x40, // D3D11_RESOURCE_MISC_BUFFER_STRUCTURED
                ..Default::default()
            },
        );
        let buffer = create(&mut p, "ID3D11Buffer", Kind::Buffer { id: resource });
        assert!(p.mem.map(DESC, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        let mut desc = Vec::new();
        for value in [0u32, 1, 1, 3, 0, 0] {
            desc.extend(value.to_le_bytes());
        }
        p.write_bytes(DESC, &desc).unwrap(); // BUFFER SRV, elements [1, 4)
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = buffer;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;

        let create_srv = method("ID3D11Device", "CreateShaderResourceView").unwrap();
        assert_eq!(create_srv(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::CREATE_VIEW);
        let fields = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields[1..], [2, resource, 0, 1, 1, 3, 0, 0]);
    }

    #[test]
    fn cs_set_unordered_access_views_emits_view_ids_and_initial_counts() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const VIEWS: u64 = 0x10000;
        const COUNTS: u64 = 0x11000;
        const STACK: u64 = 0x12000;
        let mut p = Process::new(Vfs::new());
        let mut view_ids = Vec::new();
        let mut view_ptrs = Vec::new();
        for _ in 0..2 {
            let id = p.extra.gpu.new_id();
            let ptr = create(
                &mut p,
                "ID3D11UnorderedAccessView",
                Kind::View {
                    vk: ViewKind::Uav,
                    id,
                    resource: 1,
                },
            );
            view_ids.push(id);
            view_ptrs.push(ptr);
        }
        for addr in [VIEWS, COUNTS, STACK] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        for (i, ptr) in view_ptrs.iter().enumerate() {
            p.wr64(VIEWS + 8 * i as u64, *ptr).unwrap();
            p.wr32(COUNTS + 4 * i as u64, [u32::MAX, 0][i]).unwrap();
        }
        p.cpu.gpr[RSP] = STACK;
        p.wr64(STACK + 8 + 8 * 4, COUNTS).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 3; // StartSlot
        p.cpu.gpr[R8] = 2; // NumUAVs
        p.cpu.gpr[R9] = VIEWS;

        let bind = method("ID3D11DeviceContext3", "CSSetUnorderedAccessViews").unwrap();
        assert_eq!(bind(&mut p).unwrap(), 0);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::SET_UAVS);
        assert_eq!(
            payload
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![3, 2, view_ids[0], u32::MAX, view_ids[1], 0]
        );
    }

    #[test]
    fn vs_set_constant_buffers1_emits_buffer_ranges_in_shader_constants() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const BUFFERS: u64 = 0x10000;
        const FIRST: u64 = 0x11000;
        const COUNT: u64 = 0x12000;
        const STACK: u64 = 0x13000;
        let mut p = Process::new(Vfs::new());
        let resource = p.extra.gpu.new_id();
        p.extra.com.res.insert(
            resource,
            ResInfo {
                is_buffer: true,
                size: 4096,
                ..Default::default()
            },
        );
        let buffer = create(&mut p, "ID3D11Buffer", Kind::Buffer { id: resource });
        for addr in [BUFFERS, FIRST, COUNT, STACK] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        p.wr64(BUFFERS, buffer).unwrap();
        p.wr32(FIRST, 32).unwrap();
        p.wr32(COUNT, 16).unwrap();
        p.cpu.gpr[RSP] = STACK;
        p.wr64(STACK + 8 + 8 * 4, FIRST).unwrap();
        p.wr64(STACK + 8 + 8 * 5, COUNT).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 2; // StartSlot
        p.cpu.gpr[R8] = 1; // NumBuffers
        p.cpu.gpr[R9] = BUFFERS;

        let bind = method("ID3D11DeviceContext1", "VSSetConstantBuffers1").unwrap();
        assert_eq!(bind(&mut p).unwrap(), 0);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().unwrap();
        assert_eq!(*opcode, op::SET_CBUFFERS1);
        assert_eq!(
            payload
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![0, 2, 1, resource, 32, 16]
        );
    }

    #[test]
    fn texture_update_and_bounded_copy_emit_the_requested_subresources() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const UPDATE_BOX: u64 = 0x10000;
        const COPY_BOX: u64 = 0x11000;
        const UPDATE_DATA: u64 = 0x12000;
        const STACK: u64 = 0x13000;
        let mut p = Process::new(Vfs::new());
        let mut texture = || {
            let id = p.extra.gpu.new_id();
            p.extra.com.res.insert(
                id,
                ResInfo {
                    width: 4,
                    height: 4,
                    mips: 1,
                    array: 1,
                    format: 28, // DXGI_FORMAT_R8G8B8A8_UNORM
                    ..Default::default()
                },
            );
            (
                create(&mut p, "ID3D11Texture2D", Kind::Texture2D { id }),
                id,
            )
        };
        let (dst, dst_id) = texture();
        let (src, src_id) = texture();
        drop(texture);
        for addr in [UPDATE_BOX, COPY_BOX, UPDATE_DATA, STACK] {
            assert!(p.mem.map(addr, PAGE_SIZE, PROT_RW));
        }
        for (i, value) in [1, 0, 0, 2, 2, 1].into_iter().enumerate() {
            p.wr32(UPDATE_BOX + 4 * i as u64, value).unwrap();
        }
        for (i, value) in [1, 1, 0, 3, 3, 1].into_iter().enumerate() {
            p.wr32(COPY_BOX + 4 * i as u64, value).unwrap();
        }
        let updated_pixels = [0xff, 0xff, 0x00, 0xff, 0x00, 0xff, 0xff, 0xff];
        p.write_bytes(UPDATE_DATA, &updated_pixels).unwrap();
        p.cpu.gpr[RSP] = STACK;

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = dst;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = UPDATE_BOX;
        p.wr64(STACK + 8 + 8 * 4, UPDATE_DATA).unwrap();
        p.wr64(STACK + 8 + 8 * 5, 4).unwrap();
        let update = method("ID3D11DeviceContext", "UpdateSubresource").unwrap();
        assert_eq!(update(&mut p).unwrap(), 0);

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = dst;
        p.cpu.gpr[R8] = 0;
        p.cpu.gpr[R9] = 0; // DstX
        p.wr64(STACK + 8 + 8 * 4, 1).unwrap(); // DstY
        p.wr64(STACK + 8 + 8 * 5, 0).unwrap(); // DstZ
        p.wr64(STACK + 8 + 8 * 6, src).unwrap();
        p.wr64(STACK + 8 + 8 * 7, 0).unwrap(); // SrcSubresource
        p.wr64(STACK + 8 + 8 * 8, COPY_BOX).unwrap();
        let copy = method("ID3D11DeviceContext", "CopySubresourceRegion").unwrap();
        assert_eq!(copy(&mut p).unwrap(), 0);

        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 2);
        let (update_op, update_payload) = &commands[0];
        assert_eq!(*update_op, op::TEXTURE_WRITE);
        let update_words = update_payload[..36]
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(update_words, vec![dst_id, 0, 0, 1, 0, 1, 2, 4, 8]);
        assert_eq!(&update_payload[36..], &updated_pixels);
        let (copy_op, copy_payload) = &commands[1];
        assert_eq!(*copy_op, op::COPY_REGION);
        let copy_words = copy_payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            copy_words,
            vec![dst_id, 0, 0, 1, 0, src_id, 0, 1, 1, 0, 3, 3, 1, 1]
        );
    }

    #[test]
    fn create_default_3d_uav_covers_one_mip_and_the_full_depth() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        const RESOURCE_ID: u32 = 41;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.extra.com.res.insert(
            RESOURCE_ID,
            ResInfo {
                width: 4,
                height: 4,
                depth: 3,
                is_3d: true,
                mips: 2,
                format: 2,
                ..Default::default()
            },
        );
        let texture = create(
            &mut p,
            "ID3D11Texture3D",
            Kind::Texture3D { id: RESOURCE_ID },
        );
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0; // default view descriptor
        p.cpu.gpr[R9] = OUT;
        let create_uav = method("ID3D11Device", "CreateUnorderedAccessView").unwrap();
        assert_eq!(create_uav(&mut p).unwrap(), S_OK);

        let (opcode, payload) = gpu::decode(&p.extra.gpu.buf).pop().unwrap();
        assert_eq!(opcode, op::CREATE_VIEW);
        let words = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(words, vec![1, 3, RESOURCE_ID, 2, 8, 0, 1, 0, 3]);
    }

    #[test]
    fn create_default_array_uav_covers_one_mip_and_every_array_slice() {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const OUT: u64 = 0x10000;
        const RESOURCE_ID: u32 = 42;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.extra.com.res.insert(
            RESOURCE_ID,
            ResInfo {
                width: 4,
                height: 4,
                mips: 2,
                array: 3,
                format: 28,
                ..Default::default()
            },
        );
        let texture = create(
            &mut p,
            "ID3D11Texture2D",
            Kind::Texture2D { id: RESOURCE_ID },
        );
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = 0; // default view descriptor
        p.cpu.gpr[R9] = OUT;
        let create_uav = method("ID3D11Device", "CreateUnorderedAccessView").unwrap();
        assert_eq!(create_uav(&mut p).unwrap(), S_OK);

        let (opcode, payload) = gpu::decode(&p.extra.gpu.buf).pop().unwrap();
        assert_eq!(opcode, op::CREATE_VIEW);
        let words = payload
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(words, vec![1, 3, RESOURCE_ID, 28, 5, 0, 1, 0, 3]);
    }

    fn call_clear_uav(method_name: &str, value_words: [u32; 4]) -> Process {
        use crate::vfs::Vfs;
        use anywasm_cpu::cpu::{R8, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

        const VALUES: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(VALUES, PAGE_SIZE, PROT_RW));
        let view = create(
            &mut p,
            "ID3D11UnorderedAccessView",
            Kind::View {
                vk: ViewKind::Uav,
                id: 73,
                resource: 41,
            },
        );
        for (index, value) in value_words.into_iter().enumerate() {
            p.wr32(VALUES + 4 * index as u64, value).unwrap();
        }
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = view;
        p.cpu.gpr[R8] = VALUES;
        method("ID3D11DeviceContext", method_name).unwrap()(&mut p).unwrap();
        p
    }

    #[test]
    fn clear_uav_float_emits_view_and_preserves_float_bits() {
        let p = call_clear_uav(
            "ClearUnorderedAccessViewFloat",
            [
                1.5f32.to_bits(),
                (-2.0f32).to_bits(),
                0.25f32.to_bits(),
                1.0f32.to_bits(),
            ],
        );
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, op::CLEAR_UAV);
        let words = commands[0]
            .1
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            vec![
                73,
                1,
                1.5f32.to_bits(),
                (-2.0f32).to_bits(),
                0.25f32.to_bits(),
                1.0f32.to_bits()
            ]
        );
    }

    #[test]
    fn clear_uav_uint_emits_view_and_preserves_integer_bits() {
        let p = call_clear_uav("ClearUnorderedAccessViewUint", [0xdead_beef, 2, 3, 4]);
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, op::CLEAR_UAV);
        let words = commands[0]
            .1
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(words, vec![73, 0, 0xdead_beef, 2, 3, 4]);
    }
}
