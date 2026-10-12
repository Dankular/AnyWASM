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

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "D3D11CreateDevice" => |p| create_device(p, false),
        "D3D11CreateDeviceAndSwapChain" => |p| create_device(p, true),
        _ => return None,
    })
}

const D3D_FEATURE_LEVEL_11_0: u32 = 0xb000;
const D3D11_QUERY_EVENT: u32 = 0;
const DXGI_FORMAT_R32_FLOAT: u32 = 41;
const DXGI_FORMAT_R32_UINT: u32 = 42;
const DXGI_FORMAT_R32_SINT: u32 = 43;
const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
const DXGI_FORMAT_R8G8B8A8_UINT: u32 = 30;
const DXGI_FORMAT_R8G8B8A8_SINT: u32 = 32;
const DXGI_FORMAT_R16G16B16A16_FLOAT: u32 = 10;
const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
const D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW: u32 = 0x0200_0000;
const D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE: u32 = 0x80;
const D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE: u32 = 0x0004_0000;
const D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET: u32 = 0x0020_0000;

fn format_support_flags(format: u32) -> u32 {
    let mut flags = 0x0003_ffff; // common sampling, render-target and texture capabilities
    if matches!(format, DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM) {
        flags |= D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE | D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET;
    }
    if matches!(
        format,
        DXGI_FORMAT_R8G8B8A8_UNORM
            | DXGI_FORMAT_R8G8B8A8_UINT
            | DXGI_FORMAT_R8G8B8A8_SINT
            | DXGI_FORMAT_R16G16B16A16_FLOAT
            | DXGI_FORMAT_R32G32B32A32_FLOAT
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
    let (pdev, pfl, pctx, pdesc, psc) = if with_swap_chain { (p.arg(9), p.arg(10), p.arg(11), p.arg(7), p.arg(8)) } else { (p.arg(7), p.arg(8), p.arg(9), 0, 0) };
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
        p.wr32(pfl, D3D_FEATURE_LEVEL_11_0)?;
    }
    if with_swap_chain && psc != 0 && pdesc != 0 {
        let (w, h, fmt) = (p.rd32(pdesc)?, p.rd32(pdesc + 4)?, p.rd32(pdesc + 16)?);
        let (buffers, hwnd) = (p.rd32(pdesc + 40)?, p.rd64(pdesc + 48)?);
        let sc = dxgi::make_swap_chain(p, hwnd, w, h, fmt, buffers.max(1));
        p.wr64(psc, sc)?;
    }
    Ok(S_OK)
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
                let obj = p.extra.com.objs.iter().find(|(_, o)| o.kind.host_id() == Some(resource) && matches!(o.kind, Kind::Buffer { .. } | Kind::Texture2D { .. } | Kind::Texture3D { .. })).map(|(k, _)| *k);
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
            let dimension = match kind_of(p, p.arg(0)) {
                Some(Kind::Buffer { .. }) => 1,
                Some(Kind::Texture2D { .. }) => 3,
                Some(Kind::Texture3D { .. }) => 4,
                _ => 0,
            };
            p.wr32(o, dimension)?;
            Ok(0)
        },
        // ── ID3D11Device ──
        "CreateBuffer" => create_buffer,
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
        "CreateBlendState" | "CreateBlendState1" => |p| create_state(p, op::CREATE_BLEND, 264, "ID3D11BlendState"),
        "CreateRasterizerState" | "CreateRasterizerState1" => |p| create_state(p, op::CREATE_RASTER, 40, "ID3D11RasterizerState"),
        "CreateDepthStencilState" => |p| create_state(p, op::CREATE_DEPTH, 52, "ID3D11DepthStencilState"),
        "CreateQuery" | "CreatePredicate" => |p| {
            let out = p.arg(2);
            let query_type = p.rd32(p.arg(1))?;
            let q = create(
                p,
                "ID3D11Query",
                Kind::Query {
                    query_type,
                    complete: false,
                },
            );
            p.wr64(out, q)?;
            Ok(S_OK)
        },
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
            p.wr32(out, if matches!(format, DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM) && matches!(samples, 1 | 4) { 1 } else { 0 })?;
            Ok(S_OK)
        },
        "CheckFeatureSupport" => check_feature_support,
        "GetFeatureLevel" => |_| Ok(D3D_FEATURE_LEVEL_11_0 as u64),
        "GetCreationFlags" => |_| Ok(0),
        "GetDeviceRemovedReason" => |_| Ok(S_OK),
        "SetExceptionMode" => |_| Ok(S_OK),
        "GetExceptionMode" => |_| Ok(0),
        // ── ID3D11DeviceContext: pipeline state ──
        "IASetInputLayout" => |p| {
            let id = obj_id(p, p.arg(1));
            p.extra.gpu.emit(op::SET_INPUT_LAYOUT, Pk::new().u32(id));
            Ok(0)
        },
        "IASetVertexBuffers" => ia_set_vertex_buffers,
        "IASetIndexBuffer" => |p| {
            let (b, f, o) = (obj_id(p, p.arg(1)), p.arg(2) as u32, p.arg(3) as u32);
            p.extra.gpu.emit(op::SET_INDEX_BUFFER, Pk::new().u32(b).u32(f).u32(o));
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
                pk = pk.f32(if f != 0 { f32::from_bits(p.rd32(f + 4 * i)?) } else { 1.0 });
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
            p.extra.gpu.emit(op::DRAW_INDEXED, Pk::new().u32(a).u32(b).i32(c));
            Ok(0)
        },
        "DrawInstanced" => |p| {
            let (a, b, c, d) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32, p.arg(4) as u32);
            p.extra.gpu.emit(op::DRAW_INSTANCED, Pk::new().u32(a).u32(b).u32(c).u32(d));
            Ok(0)
        },
        "DrawIndexedInstanced" => |p| {
            let (a, b, c, d, e) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32, p.arg(4) as i32, p.arg(5) as u32);
            p.extra.gpu.emit(op::DRAW_INDEXED_INSTANCED, Pk::new().u32(a).u32(b).u32(c).i32(d).u32(e));
            Ok(0)
        },
        "Dispatch" => |p| {
            let (a, b, c) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32);
            p.extra.gpu.emit(op::DISPATCH, Pk::new().u32(a).u32(b).u32(c));
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
        "ClearDepthStencilView" => |p| {
            let (v, flags) = (obj_id(p, p.arg(1)), p.arg(2) as u32);
            let depth = f32::from_bits(p.cpu.xmm[3] as u32);
            let stencil = p.arg(4) as u32 & 0xff;
            p.extra.gpu.emit(op::CLEAR_DSV, Pk::new().u32(v).u32(flags).f32(depth).u32(stencil));
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
        "Begin" => |p| set_query_complete(p, false),
        "End" => |p| set_query_complete(p, true),
        "GetData" => get_query_data,
        "ResolveSubresource" => resolve_subresource,
        "Flush" | "ClearState" | "SetPredication" | "FinishCommandList" | "ExecuteCommandList" | "DiscardResource" | "DiscardView" => |_| Ok(0),
        "OMGetRenderTargets" => |p| {
            let (n, rtvs, dsv) = (p.arg(1) as usize, p.arg(2), p.arg(3));
            let bound = p.extra.com.bound_rtv.clone();
            for i in 0..n {
                if rtvs != 0 {
                    p.wr64(rtvs + 8 * i as u64, bound.get(i).copied().unwrap_or(0))?;
                }
            }
            if dsv != 0 {
                p.wr64(dsv, p.extra.com.bound_dsv)?;
            }
            Ok(0)
        },
        _ => return None,
    })
}

fn set_query_complete(p: &mut Process, complete: bool) -> R {
    let query = p.arg(1);
    if let Some(Kind::Query {
        query_type: D3D11_QUERY_EVENT,
        complete: state,
    }) = p
        .extra
        .com
        .objs
        .get_mut(&query)
        .map(|obj| &mut obj.kind)
    {
        *state = complete;
    }
    Ok(0)
}

fn get_query_data(p: &mut Process) -> R {
    let query = p.arg(1);
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
    let (data, size) = (p.arg(2), p.arg(3) as u32);
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

    let (feature, data, size) = (p.arg(1), p.arg(2), p.arg(3) as u32);
    let expected_size = match feature {
        FEATURE_THREADING | FEATURE_FORMAT_SUPPORT | FEATURE_FORMAT_SUPPORT2 => 8,
        FEATURE_DOUBLES | FEATURE_D3D10_X_HARDWARE_OPTIONS => 4,
        _ => return Ok(E_INVALIDARG),
    };
    if data == 0 || size != expected_size {
        return Ok(E_INVALIDARG);
    }

    match feature {
        FEATURE_THREADING | FEATURE_DOUBLES => p.write_bytes(data, &vec![0; size as usize])?,
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
        9..=22 => (1, 1, 8),   // R16G16B16A16, R32G32, R32G8X24
        23..=47 => (1, 1, 4),  // R10G10B10A2, R11G11B10, R8G8B8A8, R16G16, R32, R24G8
        48..=59 => (1, 1, 2),  // R8G8, R16
        60..=65 => (1, 1, 1),  // R8, A8
        67..=69 => (1, 1, 4),
        70..=72 | 79..=81 => (4, 4, 8),                  // BC1, BC4
        73..=78 | 82..=84 | 94..=99 => (4, 4, 16),       // BC2, BC3, BC5, BC6H, BC7
        85 | 86 | 115 => (1, 1, 2),                      // B5G6R5, B5G5R5A1, B4G4R4A4
        87..=93 => (1, 1, 4),                            // B8G8R8A8 / X8
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
    p.extra.gpu.emit(op::SET_SHADER, Pk::new().u32(stage).u32(id));
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
        let buffer = if buffers != 0 { p.rd64(buffers + 8 * i)? } else { 0 };
        let first_constant = if first != 0 { p.rd32(first + 4 * i)? } else { 0 };
        let num_constants = if count != 0 { p.rd32(count + 4 * i)? } else { 0 };
        pk = pk.u32(obj_id(p, buffer)).u32(first_constant).u32(num_constants);
    }
    p.extra.gpu.emit(op::SET_CBUFFERS1, pk);
    Ok(0)
}

fn set_uavs(p: &mut Process) -> R {
    // (this, StartSlot, NumUAVs, ppUAVs, pUAVInitialCounts)
    let (start, n, views, counts) = (
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4),
    );
    let mut pk = Pk::new().u32(start).u32(n);
    for i in 0..n as u64 {
        let view = if views != 0 { p.rd64(views + 8 * i)? } else { 0 };
        let initial_count = if counts != 0 { p.rd32(counts + 4 * i)? } else { u32::MAX };
        pk = pk.u32(obj_id(p, view)).u32(initial_count);
    }
    p.extra.gpu.emit(op::SET_UAVS, pk);
    Ok(0)
}

fn ia_set_vertex_buffers(p: &mut Process) -> R {
    // (this, StartSlot, NumBuffers, ppVertexBuffers, pStrides, pOffsets)
    let (start, n, bufs, strides, offsets) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3), p.arg(4), p.arg(5));
    let mut pk = Pk::new().u32(start).u32(n);
    for i in 0..n as u64 {
        let ptr = if bufs != 0 { p.rd64(bufs + 8 * i)? } else { 0 };
        let (s, o) = (if strides != 0 { p.rd32(strides + 4 * i)? } else { 0 }, if offsets != 0 { p.rd32(offsets + 4 * i)? } else { 0 });
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
    p.extra.com.bound_rtv = bound;
    p.extra.com.bound_dsv = dsv;
    p.extra.gpu.emit(op::SET_RENDER_TARGETS, pk.u32(obj_id(p, dsv)));
    Ok(0)
}

fn create_buffer(p: &mut Process) -> R {
    // (this, const D3D11_BUFFER_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Buffer**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (size, usage, bind, cpu, misc, stride) = (p.rd32(desc)?, p.rd32(desc + 4)?, p.rd32(desc + 8)?, p.rd32(desc + 12)?, p.rd32(desc + 16)?, p.rd32(desc + 20)?);
    let mut shadow = vec![0u8; size as usize];
    if init != 0 {
        let data = p.rd64(init)?;
        if data != 0 {
            shadow = p.read_bytes(data, size as usize)?;
        }
    }
    let id = new_res(p, ResInfo { is_buffer: true, size, usage, bind, cpu, misc, stride, shadow: shadow.clone(), ..Default::default() });
    p.extra.gpu.emit(op::CREATE_BUFFER, Pk::new().u32(id).u32(size).u32(bind).u32(usage).u32(misc).u32(stride));
    if init != 0 {
        p.extra.gpu.emit(op::BUFFER_WRITE, Pk::new().u32(id).u32(0).u32(size).bytes(&shadow));
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

fn create_texture2d(p: &mut Process) -> R {
    // (this, const D3D11_TEXTURE2D_DESC*, const D3D11_SUBRESOURCE_DATA*, ID3D11Texture2D**)
    let (desc, init, out) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 {
        return Ok(E_INVALIDARG);
    }
    let (w, h, mut mips, array, format) = (p.rd32(desc)?, p.rd32(desc + 4)?, p.rd32(desc + 8)?, p.rd32(desc + 12)?, p.rd32(desc + 16)?);
    let (samples, sample_quality, usage, bind, cpu, misc) = (
        p.rd32(desc + 20)?, p.rd32(desc + 24)?, p.rd32(desc + 28)?, p.rd32(desc + 32)?, p.rd32(desc + 36)?, p.rd32(desc + 40)?,
    );
    if mips == 0 {
        mips = mip_count(w, h);
    }
    let array = array.max(1);
    let sample_count = samples.max(1);
    if w == 0 || h == 0
        || sample_count > 1
            && (sample_count != 4 || sample_quality != 0 || !matches!(format, DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM) || mips != 1 || array != 1 || init != 0 || usage != 0 || bind != 0x20 || cpu != 0 || misc != 0)
    {
        return Ok(E_INVALIDARG);
    }
    let keep_shadow = cpu != 0;
    let size = row_bytes(format, w) * rows(format, h);
    let id = new_res(p, ResInfo { width: w, height: h, mips, array, format, samples: sample_count, usage, bind, cpu, misc, size, shadow: if keep_shadow { vec![0u8; size as usize] } else { Vec::new() }, ..Default::default() });
    p.extra.gpu.emit(op::CREATE_TEXTURE2D, Pk::new().u32(id).u32(w).u32(h).u32(mips).u32(array).u32(format).u32(bind).u32(usage).u32(sample_count).u32(misc));
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
                let pitch = if pitch == 0 { row_bytes(format, mw) } else { pitch };
                let nrows = rows(format, mh);
                let bytes = p.read_bytes(data, (pitch * nrows) as usize)?;
                p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(slice).u32(0).u32(0).u32(mw).u32(mh).u32(pitch).u32(bytes.len() as u32).bytes(&bytes));
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
        p.rd32(desc)?, p.rd32(desc + 4)?, p.rd32(desc + 8)?, p.rd32(desc + 12)?, p.rd32(desc + 16)?,
    );
    let (usage, bind, cpu, misc) = (p.rd32(desc + 20)?, p.rd32(desc + 24)?, p.rd32(desc + 28)?, p.rd32(desc + 32)?);
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
    let id = new_res(p, ResInfo {
        width: w, height: h, depth, is_3d: true, mips, array: 1, format, usage, bind, cpu, misc, size,
        shadow: if keep_shadow { vec![0u8; size as usize] } else { Vec::new() },
        ..Default::default()
    });
    p.extra.gpu.emit(op::CREATE_TEXTURE3D, Pk::new().u32(id).u32(w).u32(h).u32(depth).u32(mips).u32(format).u32(bind).u32(usage).u32(misc));
    if init != 0 {
        for mip in 0..mips {
            let sd = init + 16 * mip as u64;
            let (data, mut pitch, mut depth_pitch) = (p.rd64(sd)?, p.rd32(sd + 8)?, p.rd32(sd + 12)?);
            if data == 0 {
                continue;
            }
            let (mw, mh, md) = (mip_dim(w, mip), mip_dim(h, mip), mip_dim(depth, mip));
            if pitch == 0 { pitch = row_bytes(format, mw); }
            let nrows = rows(format, mh);
            if depth_pitch == 0 { depth_pitch = pitch * nrows; }
            for z in 0..md {
                let bytes = p.read_bytes(data + z as u64 * depth_pitch as u64, (pitch * nrows) as usize)?;
                p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(z).u32(0).u32(0).u32(mw).u32(mh).u32(pitch).u32(bytes.len() as u32).bytes(&bytes));
                if keep_shadow && mip == 0 {
                    let dst = z as usize * base_depth_pitch as usize;
                    if dst < size as usize {
                        let n = (bytes.len()).min(base_depth_pitch as usize).min(size as usize - dst);
                        p.extra.com.res.get_mut(&id).unwrap().shadow[dst..dst + n].copy_from_slice(&bytes[..n]);
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
    let Some(rid) = kind_of(p, res).and_then(|k| k.host_id()) else { return Ok(E_INVALIDARG) };
    let info = p.extra.com.res.get(&rid).cloned().unwrap_or_default();
    let default_dim = if info.is_buffer { 1 } else if info.is_3d { 8 } else { 4 };
    let default_buffer_stride = if info.misc & 0x20 != 0 { 4 } else { info.stride.max(4) };
    let default_count = if info.is_buffer {
        info.size / default_buffer_stride
    } else {
        info.mips.max(1)
    };
    let default_slices = if info.is_buffer { 0 } else { info.array.max(1) };
    let (mut format, mut dim, mut a, mut b, mut c, mut d) = (info.format, default_dim, 0u32, default_count, 0u32, default_slices);
    if desc != 0 {
        let f = p.rd32(desc)?;
        if f != 0 {
            format = f;
        }
        dim = p.rd32(desc + 4)?;
        // Union layout starts at +8: the first fields differ per view kind and dimension.
        match (vk, dim) {
            (ViewKind::Srv, 4) => {
                a = p.rd32(desc + 8)?; // MostDetailedMip
                b = p.rd32(desc + 12)?; // MipLevels (-1 = all)
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
            (ViewKind::Rtv, 4) | (ViewKind::Dsv, 3) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
            }
            (ViewKind::Uav, 4) => {
                a = p.rd32(desc + 8)?; // MipSlice
                b = 1;
            }
            (ViewKind::Uav, 1) => {
                a = p.rd32(desc + 8)?; // FirstElement
                b = p.rd32(desc + 12)?; // NumElements
                c = p.rd32(desc + 16)?; // Flags
                d = 0;
            }
            (ViewKind::Rtv, 5) | (ViewKind::Dsv, 4) => {
                a = p.rd32(desc + 8)?;
                b = 1;
                c = p.rd32(desc + 12)?;
                d = p.rd32(desc + 16)?;
            }
            (ViewKind::Uav, 5) => {
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
            b = info.size.saturating_sub(a.saturating_mul(default_buffer_stride)) / default_buffer_stride;
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
    p.extra.gpu.emit(op::CREATE_VIEW, Pk::new().u32(id).u32(kind_code).u32(rid).u32(format).u32(dim).u32(a).u32(b).u32(c).u32(d));
    let iface = match vk {
        ViewKind::Rtv => "ID3D11RenderTargetView",
        ViewKind::Dsv => "ID3D11DepthStencilView",
        ViewKind::Srv => "ID3D11ShaderResourceView",
        ViewKind::Uav => "ID3D11UnorderedAccessView",
    };
    let o = create(p, iface, Kind::View { vk, id, resource: rid });
    p.wr64(out, o)?;
    Ok(S_OK)
}

/// Parse the DXBC input signature (`ISGN`/`ISG1`) -> (semantic name lowercase, index, register).
pub fn input_signature(dxbc: &[u8]) -> Vec<(String, u32, u32)> {
    let rd = |o: usize| dxbc.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
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
            let (name_off, idx, sv, reg) = if tag == b"ISG1" { (rd(at + 4), rd(at + 8), rd(at + 12), rd(at + 20)) } else { (rd(at), rd(at + 4), rd(at + 8), rd(at + 16)) };
            let (Some(name_off), Some(idx), Some(sv), Some(reg)) = (name_off, idx, sv, reg) else { continue };
            if sv != 0 {
                continue; // system-value inputs (SV_VertexID, SV_InstanceID...) have no vertex buffer data
            }
            let s = data + name_off as usize;
            let end = dxbc[s..].iter().position(|&b| b == 0).map(|k| s + k).unwrap_or(s);
            out.push((String::from_utf8_lossy(&dxbc[s..end]).to_ascii_lowercase(), idx, reg));
        }
    }
    out
}

fn create_input_layout(p: &mut Process) -> R {
    // (this, const D3D11_INPUT_ELEMENT_DESC*, NumElements, pShaderBytecodeWithInputSignature, BytecodeLength, ID3D11InputLayout**)
    let (descs, n, code, len, out) = (p.arg(1), p.arg(2) as u64, p.arg(3), p.arg(4) as usize, p.arg(5));
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
        let (idx, fmt, slot, off, class, step) = (p.rd32(d + 8)?, p.rd32(d + 12)?, p.rd32(d + 16)?, p.rd32(d + 20)?, p.rd32(d + 24)?, p.rd32(d + 28)?);
        let (bw, _, bytes) = format_info(fmt);
        let _ = bw;
        let slot_i = (slot as usize).min(15);
        let offset = if off == 0xffff_ffff { offsets[slot_i] } else { off };
        offsets[slot_i] = offset + bytes;
        if let Some((_, _, reg)) = sig.iter().find(|(n2, i2, _)| *n2 == name && *i2 == idx) {
            pk = pk.u32(*reg).u32(fmt).u32(slot).u32(offset).u32(class).u32(step);
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
    p.extra.gpu.emit(op::CREATE_SHADER, Pk::new().u32(id).u32(stage).u32(len as u32).bytes(&bytes));
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
    if let (Some(sh), true) = (p.extra.com.res.get(&s).map(|r| r.shadow.clone()), p.extra.com.res.contains_key(&d)) {
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
    let (dst, dsub, dx, dy, dz, src, ssub, sbox) = (p.arg(1), p.arg(2) as u32, p.arg(3) as u32, p.arg(4) as u32, p.arg(5) as u32, p.arg(6), p.arg(7) as u32, p.arg(8));
    let (d, s) = (obj_id(p, dst), obj_id(p, src));
    let mut pk = Pk::new().u32(d).u32(dsub).u32(dx).u32(dy).u32(dz).u32(s).u32(ssub);
    if sbox != 0 {
        pk = pk.u32(1);
        for i in 0..6 {
            pk = pk.u32(p.rd32(sbox + 4 * i)?);
        }
    } else {
        pk = pk.u32(0);
        for _ in 0..6 {
            pk = pk.u32(0);
        }
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
        if src_info.samples > 1 && dst_info.samples == 1 && src_info.format == format && dst_info.format == format {
            p.extra.gpu.emit(op::RESOLVE_TEXTURE, Pk::new().u32(dst).u32(dst_sub).u32(src).u32(src_sub).u32(format));
        }
    }
    Ok(0)
}

fn update_subresource(p: &mut Process) -> R {
    // (this, pDstResource, DstSubresource, const D3D11_BOX* pDstBox, const void* pSrcData, SrcRowPitch, SrcDepthPitch)
    let (res, sub, bx, src, row_pitch, depth_pitch) = (p.arg(1), p.arg(2) as u32, p.arg(3), p.arg(4), p.arg(5) as u32, p.arg(6) as u32);
    let Some(kind) = kind_of(p, res) else { return Ok(0) };
    match kind {
        Kind::Buffer { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let (left, right) = if bx != 0 { (p.rd32(bx)?, p.rd32(bx + 12)?) } else { (0, info.size) };
            let n = right.saturating_sub(left) as usize;
            let bytes = p.read_bytes(src, n)?;
            if let Some(r) = p.extra.com.res.get_mut(&id) {
                if r.shadow.len() >= left as usize + n {
                    r.shadow[left as usize..left as usize + n].copy_from_slice(&bytes);
                }
            }
            p.extra.gpu.emit(op::BUFFER_WRITE, Pk::new().u32(id).u32(left).u32(n as u32).bytes(&bytes));
        }
        Kind::Texture2D { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let (mip, slice) = (sub % info.mips.max(1), sub / info.mips.max(1));
            let (mw, mh) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
            let (x, y, w, h) = if bx != 0 { (p.rd32(bx)?, p.rd32(bx + 4)?, p.rd32(bx + 12)? - p.rd32(bx)?, p.rd32(bx + 16)? - p.rd32(bx + 4)?) } else { (0, 0, mw, mh) };
            let pitch = if row_pitch == 0 { row_bytes(info.format, w) } else { row_pitch };
            let nrows = rows(info.format, h);
            let bytes = p.read_bytes(src, (pitch * nrows) as usize)?;
            p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(slice).u32(x).u32(y).u32(w).u32(h).u32(pitch).u32(bytes.len() as u32).bytes(&bytes));
        }
        Kind::Texture3D { id } => {
            let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let mip = sub % info.mips.max(1);
            let (mw, mh, md) = (mip_dim(info.width, mip), mip_dim(info.height, mip), mip_dim(info.depth, mip));
            let (x, y, z, w, h, depth) = if bx != 0 {
                let (left, top, front) = (p.rd32(bx)?, p.rd32(bx + 4)?, p.rd32(bx + 8)?);
                let (right, bottom, back) = (p.rd32(bx + 12)?, p.rd32(bx + 16)?, p.rd32(bx + 20)?);
                (left, top, front, right - left, bottom - top, back - front)
            } else {
                (0, 0, 0, mw, mh, md)
            };
            let pitch = if row_pitch == 0 { row_bytes(info.format, w) } else { row_pitch };
            let nrows = rows(info.format, h);
            let depth_pitch = if depth_pitch == 0 { pitch * nrows } else { depth_pitch };
            for slice in 0..depth {
                let bytes = p.read_bytes(src + slice as u64 * depth_pitch as u64, (pitch * nrows) as usize)?;
                p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(z + slice).u32(x).u32(y).u32(w).u32(h).u32(pitch).u32(bytes.len() as u32).bytes(&bytes));
            }
        }
        _ => {}
    }
    Ok(0)
}

fn map(p: &mut Process) -> R {
    // (this, pResource, Subresource, MapType, MapFlags, D3D11_MAPPED_SUBRESOURCE*)
    let (res, sub, ty, out) = (p.arg(1), p.arg(2) as u32, p.arg(3) as u32, p.arg(5));
    let Some(id) = obj_id_opt(p, res) else { return Ok(E_INVALIDARG) };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    let (len, pitch, depth_pitch) = if info.is_buffer {
        (info.size as u64, info.size, info.size)
    } else {
        let mip = sub % info.mips.max(1);
        let (w, h) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
        let pitch = row_bytes(info.format, w);
        let depth_pitch = pitch * rows(info.format, h);
        let depth = if info.is_3d { mip_dim(info.depth, mip) } else { 1 };
        (depth_pitch as u64 * depth as u64, pitch, depth_pitch)
    };
    let addr = if info.map_addr != 0 { info.map_addr } else { kernel32::alloc_block(p, len.max(16), true) };
    // MAP_READ=1, WRITE=2, READ_WRITE=3, WRITE_DISCARD=4, WRITE_NO_OVERWRITE=5
    let reads = ty == 1 || ty == 3;
    let writes = ty != 1;
    if reads || ty == 5 || (ty == 2) {
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
    let Some(id) = obj_id_opt(p, res) else { return Ok(0) };
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
        p.extra.gpu.emit(op::BUFFER_WRITE, Pk::new().u32(id).u32(0).u32(info.size).bytes(&bytes));
    } else {
        let sub = info.map_sub;
        let mip = sub % info.mips.max(1);
        let (w, h) = (mip_dim(info.width, mip), mip_dim(info.height, mip));
        let pitch = row_bytes(info.format, w);
        let nrows = rows(info.format, h);
        let depth_pitch = pitch * nrows;
        let depth = if info.is_3d { mip_dim(info.depth, mip) } else { 1 };
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
                let slice_bytes = &bytes[(z * depth_pitch) as usize..((z + 1) * depth_pitch) as usize];
                p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(z).u32(0).u32(0).u32(w).u32(h).u32(pitch).u32(slice_bytes.len() as u32).bytes(slice_bytes));
            }
        } else {
            let slice = sub / info.mips.max(1);
            p.extra.gpu.emit(op::TEXTURE_WRITE, Pk::new().u32(id).u32(mip).u32(slice).u32(0).u32(0).u32(w).u32(h).u32(pitch).u32(bytes.len() as u32).bytes(&bytes));
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
    let Some(kind) = kind_of(p, this) else { return Ok(E_INVALIDARG) };
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
            let Some(Kind::Texture2D { id }) = kind_of(p, backbuffer) else { return Ok(E_FAIL) };
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let mut b = vec![0u8; 72];
            for (off, v) in [(0usize, r.width), (4, r.height), (8, 60), (12, 1), (16, r.format), (28, 1), (36, 0x20), (40, 2), (56, 1)] {
                b[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
            b[48..56].copy_from_slice(&p.extra.win.main_window.to_le_bytes());
            p.write_bytes(out, &b)?;
        }
        Kind::Buffer { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            for (i, v) in [r.size, r.usage, r.bind, r.cpu, r.misc, r.stride].iter().enumerate() {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        Kind::Texture2D { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let vals = [r.width, r.height, r.mips, r.array, r.format, r.samples.max(1), 0, r.usage, r.bind, r.cpu, r.misc];
            for (i, v) in vals.iter().enumerate() {
                p.wr32(out + 4 * i as u64, *v)?;
            }
        }
        Kind::Texture3D { id } => {
            let r = p.extra.com.res.get(&id).cloned().unwrap_or_default();
            let vals = [r.width, r.height, r.depth, r.mips, r.format, r.usage, r.bind, r.cpu, r.misc];
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

    #[test]
    fn flush1_signals_the_selected_target_idle_event() {
        use anywasm_cpu::cpu::{R8, RCX};
        use crate::process::Handle;
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
    fn create_texture3d_emits_volume_creation_and_each_initial_z_slice() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        let fields = commands[0].1.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>();
        assert_eq!(fields, vec![texture_id, 4, 4, 2, 1, 28, 8, 0, 0]);
        for (z, rgba) in [[255, 0, 0, 255], [0, 255, 0, 255]].into_iter().enumerate() {
            let (opcode, payload) = &commands[z + 1];
            assert_eq!(*opcode, op::TEXTURE_WRITE);
            let fields = payload[..36].chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>();
            assert_eq!(fields, vec![texture_id, 0, z as u32, 0, 0, 4, 4, 16, 64]);
            assert!(payload[36..].chunks_exact(4).all(|pixel| pixel == rgba));
        }

        let mut srv_desc = Vec::new();
        for value in [28u32, 8, 0, 1] { srv_desc.extend(value.to_le_bytes()); }
        p.write_bytes(DESC, &srv_desc).unwrap(); // Texture3D SRV: first mip 0, one mip
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = texture;
        p.cpu.gpr[R8] = DESC;
        p.cpu.gpr[R9] = OUT;
        let create_srv = method("ID3D11Device", "CreateShaderResourceView").unwrap();
        assert_eq!(create_srv(&mut p).unwrap(), S_OK);
        let srv = p.rd64(OUT).unwrap();
        let view_id = match kind_of(&p, srv) {
            Some(Kind::View { vk: ViewKind::Srv, id, .. }) => id,
            other => panic!("expected Texture3D SRV, got {other:?}"),
        };
        let (opcode, payload) = gpu::decode(&p.extra.gpu.buf).last().unwrap().clone();
        assert_eq!(opcode, op::CREATE_VIEW);
        let fields = payload.chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(fields, vec![view_id, 2, texture_id, 28, 8, 0, 1, 0, 1]);
    }

    #[test]
    fn update_texture3d_emits_each_boxed_z_slice_with_depth_pitch() {
        use anywasm_cpu::cpu::{RSP, RCX, RDX, R8, R9};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

        const BOX: u64 = 0x10000;
        const DATA: u64 = 0x11000;
        const STACK: u64 = 0x12000;
        let mut p = Process::new(Vfs::new());
        let id = p.extra.gpu.new_id();
        p.extra.com.res.insert(id, ResInfo {
            width: 4, height: 4, depth: 4, is_3d: true, mips: 1, array: 1, format: 28,
            ..Default::default()
        });
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
            let fields = payload[..36].chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(fields, vec![id, 0, 1 + slice as u32, 1, 1, 2, 2, 8, 16]);
            assert_eq!(&payload[36..], &bytes[slice * 16..(slice + 1) * 16]);
        }
    }

    #[test]
    fn format_support_advertises_typed_uav_for_supported_formats() {
        const TYPED_UAV: u32 = 0x0200_0000;

        assert_ne!(format_support_flags(41) & TYPED_UAV, 0); // DXGI_FORMAT_R32_FLOAT
        assert_ne!(format_support_flags(42) & TYPED_UAV, 0); // DXGI_FORMAT_R32_UINT
        assert_ne!(format_support_flags(43) & TYPED_UAV, 0); // DXGI_FORMAT_R32_SINT
        assert_ne!(format_support_flags(2) & TYPED_UAV, 0); // DXGI_FORMAT_R32G32B32A32_FLOAT
        assert_ne!(format_support_flags(10) & TYPED_UAV, 0); // DXGI_FORMAT_R16G16B16A16_FLOAT
        assert_ne!(format_support_flags(28) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_UNORM
        assert_ne!(format_support_flags(30) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_UINT
        assert_ne!(format_support_flags(32) & TYPED_UAV, 0); // DXGI_FORMAT_R8G8B8A8_SINT
        assert_ne!(format_support_flags(28) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE, 0);
        assert_ne!(format_support_flags(28) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET, 0);
        assert_ne!(format_support_flags(87) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET, 0);
        assert_eq!(format_support_flags(54) & D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET, 0);
        assert_eq!(format_support_flags(54) & TYPED_UAV, 0); // DXGI_FORMAT_R16_FLOAT
        assert_eq!(format_support_flags(61) & TYPED_UAV, 0); // DXGI_FORMAT_R8_UNORM
    }

    #[test]
    fn check_feature_support_reports_typed_uav_format_queries() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        assert_ne!(p.rd32(DATA + 4).unwrap() & D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW, 0);

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
    fn multisample_quality_levels_match_the_supported_resolve_path() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

        const OUT: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(OUT, PAGE_SIZE, PROT_RW));
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = DXGI_FORMAT_R8G8B8A8_UNORM as u64;
        p.cpu.gpr[R8] = 4;
        p.cpu.gpr[R9] = OUT;

        let query = method("ID3D11Device", "CheckMultisampleQualityLevels").unwrap();
        assert_eq!(query(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(OUT).unwrap(), 1);

        p.cpu.gpr[RDX] = DXGI_FORMAT_B8G8R8A8_UNORM as u64;
        assert_eq!(query(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(OUT).unwrap(), 1);

        p.cpu.gpr[RDX] = 54; // DXGI_FORMAT_R16_FLOAT has no typed 4x path.
        assert_eq!(query(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(OUT).unwrap(), 0);

        p.cpu.gpr[RDX] = DXGI_FORMAT_R8G8B8A8_UNORM as u64;
        p.cpu.gpr[R8] = 2;
        assert_eq!(query(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(OUT).unwrap(), 0);
    }

    #[test]
    fn create_texture2d_forwards_supported_sample_count() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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

        let stream = &p.extra.gpu.buf;
        assert_eq!(u32::from_le_bytes(stream[0..4].try_into().unwrap()), op::CREATE_TEXTURE2D);
        assert_eq!(u32::from_le_bytes(stream[40..44].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(stream[76..80].try_into().unwrap()), DXGI_FORMAT_B8G8R8A8_UNORM);
        assert_eq!(u32::from_le_bytes(stream[88..92].try_into().unwrap()), 4);
    }

    #[test]
    fn resolve_subresource_emits_the_source_and_destination_resources() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

        const STACK: u64 = 0x20000;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        let dst_id = p.extra.gpu.new_id();
        let src_id = p.extra.gpu.new_id();
        p.extra.com.res.insert(dst_id, ResInfo { width: 8, height: 8, format: 28, samples: 1, ..Default::default() });
        p.extra.com.res.insert(src_id, ResInfo { width: 8, height: 8, format: 28, samples: 4, ..Default::default() });
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
        assert_eq!(u32::from_le_bytes(stream[0..4].try_into().unwrap()), op::RESOLVE_TEXTURE);
        let payload = &stream[8..28];
        let fields = payload.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>();
        assert_eq!(fields, vec![dst_id, 0, src_id, 0, 28]);
    }

    #[test]
    fn create_unordered_access_view_tracks_texture2d_mip_slice() {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
        use crate::vfs::Vfs;

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
            vec![dst_id, 0, 0, 1, 0, src_id, 0, 1, 1, 1, 0, 3, 3, 1]
        );
    }
}
