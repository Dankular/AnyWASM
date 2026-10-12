//! Direct3D 9 compatibility surface. Device work is translated to the shared WebGPU stream.

use super::{com, dxgi, gpu::{op, Pk}, *};
use crate::process::{Process, Stop};

const S_OK: u64 = 0;
const E_NOTIMPL: u64 = 0x8000_4001;
const E_POINTER: u64 = 0x8000_4003;
const E_INVALIDARG: u64 = 0x8007_0057;
const E_OUTOFMEMORY: u64 = 0x8007_000e;
const D3DERR_INVALIDCALL: u64 = 0x8876_086c;
const D3DERR_NOTAVAILABLE: u64 = 0x8876_086a;

const IID_D3D9: &str = "81bdcbca-64d4-426d-ae8d-ad0147f4275c";
const IID_D3D9_DEVICE: &str = "d0223b96-bf7a-43fd-92bd-a43b0d82b9eb";
const IID_D3D9_SURFACE: &str = "0cfbaf3a-9ff6-429a-99b3-a2796af8b89b";

const D3D9_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "RegisterSoftwareDevice", "GetAdapterCount",
    "GetAdapterIdentifier", "GetAdapterModeCount", "EnumAdapterModes", "GetAdapterDisplayMode",
    "CheckDeviceType", "CheckDeviceFormat", "CheckDeviceMultiSampleType", "CheckDepthStencilMatch",
    "CheckDeviceFormatConversion", "GetDeviceCaps", "GetAdapterMonitor", "CreateDevice",
];

const D3D9_DEVICE_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "TestCooperativeLevel", "GetAvailableTextureMem",
    "EvictManagedResources", "GetDirect3D", "GetDeviceCaps", "GetDisplayMode", "GetCreationParameters",
    "SetCursorProperties", "SetCursorPosition", "ShowCursor", "CreateAdditionalSwapChain", "GetSwapChain",
    "GetNumberOfSwapChains", "Reset", "Present", "GetBackBuffer", "GetRasterStatus", "SetDialogBoxMode",
    "SetGammaRamp", "GetGammaRamp", "CreateTexture", "CreateVolumeTexture", "CreateCubeTexture",
    "CreateVertexBuffer", "CreateIndexBuffer", "CreateRenderTarget", "CreateDepthStencilSurface",
    "UpdateSurface", "UpdateTexture", "GetRenderTargetData", "GetFrontBufferData", "StretchRect", "ColorFill",
    "CreateOffscreenPlainSurface", "SetRenderTarget", "GetRenderTarget", "SetDepthStencilSurface",
    "GetDepthStencilSurface", "BeginScene", "EndScene", "Clear", "SetTransform", "GetTransform",
    "MultiplyTransform", "SetViewport", "GetViewport", "SetMaterial", "GetMaterial", "SetLight", "GetLight",
    "LightEnable", "GetLightEnable", "SetClipPlane", "GetClipPlane", "SetRenderState", "CreateStateBlock",
    "BeginStateBlock", "EndStateBlock", "SetClipStatus", "GetClipStatus", "GetTexture", "SetTexture",
    "GetTextureStageState", "SetTextureStageState", "GetSamplerState", "SetSamplerState", "ValidateDevice",
    "SetPaletteEntries", "GetPaletteEntries", "SetCurrentTexturePalette", "GetCurrentTexturePalette",
    "SetScissorRect", "GetScissorRect", "SetSoftwareVertexProcessing", "GetSoftwareVertexProcessing",
    "SetNPatchMode", "GetNPatchMode", "DrawPrimitive", "DrawIndexedPrimitive", "DrawPrimitiveUP",
    "DrawIndexedPrimitiveUP", "ProcessVertices", "CreateVertexDeclaration", "SetVertexDeclaration",
    "GetVertexDeclaration", "SetFVF", "GetFVF", "CreateVertexShader", "SetVertexShader", "GetVertexShader",
    "SetVertexShaderConstantF", "GetVertexShaderConstantF", "SetVertexShaderConstantI",
    "GetVertexShaderConstantI", "SetVertexShaderConstantB", "GetVertexShaderConstantB", "SetStreamSource",
    "GetStreamSource", "SetStreamSourceFreq", "GetStreamSourceFreq", "SetIndices", "GetIndices",
    "CreatePixelShader", "SetPixelShader", "GetPixelShader", "SetPixelShaderConstantF", "GetPixelShaderConstantF",
    "SetPixelShaderConstantI", "GetPixelShaderConstantI", "SetPixelShaderConstantB", "GetPixelShaderConstantB",
    "DrawRectPatch", "DrawTriPatch", "DeletePatch", "CreateQuery",
];

const D3D9_SURFACE_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData", "FreePrivateData",
    "SetPriority", "GetPriority", "PreLoad", "GetType", "GetContainer", "GetDesc", "LockRect", "UnlockRect",
    "GetDC", "ReleaseDC",
];

pub fn lookup(name: &str) -> Option<HleFn> {
    (name == "Direct3DCreate9").then_some(direct3d_create9)
}
pub fn methods(iface: &str) -> Option<&'static [&'static str]> {
    match iface {
        "IDirect3D9" => Some(D3D9_METHODS),
        "IDirect3DDevice9" => Some(D3D9_DEVICE_METHODS),
        "IDirect3DSurface9" => Some(D3D9_SURFACE_METHODS),
        _ => None,
    }
}

pub fn iid_name(iid: &str) -> Option<&'static str> {
    match iid {
        IID_D3D9 => Some("IDirect3D9"),
        IID_D3D9_DEVICE => Some("IDirect3DDevice9"),
        IID_D3D9_SURFACE => Some("IDirect3DSurface9"),
        _ => None,
    }
}

pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    let (iface, method) = name.split_once("::")?;
    let count = match (iface, method) {
        ("IDirect3D9", "QueryInterface") => 3,
        ("IDirect3D9", "AddRef" | "Release" | "GetAdapterCount") => 1,
        ("IDirect3D9", "RegisterSoftwareDevice" | "GetAdapterMonitor") => 2,
        ("IDirect3D9", "GetAdapterIdentifier") => 4,
        ("IDirect3D9", "GetAdapterModeCount") => 3,
        // Include `this` in each Win32 stdcall count.
        ("IDirect3D9", "EnumAdapterModes") => 5,
        ("IDirect3D9", "GetAdapterDisplayMode") => 3,
        ("IDirect3D9", "CheckDeviceType" | "CheckDepthStencilMatch") => 6,
        ("IDirect3D9", "CheckDeviceFormat" | "CheckDeviceMultiSampleType") => 7,
        ("IDirect3D9", "CheckDeviceFormatConversion") => 5,
        ("IDirect3D9", "GetDeviceCaps") => 4,
        ("IDirect3D9", "CreateDevice") => 7,
        ("IDirect3DDevice9", "QueryInterface") => 3,
        ("IDirect3DDevice9", "AddRef" | "Release" | "TestCooperativeLevel" | "GetAvailableTextureMem" | "EvictManagedResources" | "GetNumberOfSwapChains" | "BeginScene" | "EndScene") => 1,
        ("IDirect3DDevice9", "GetDirect3D" | "GetDeviceCaps" | "GetFVF" | "GetSoftwareVertexProcessing") => 2,
        ("IDirect3DDevice9", "GetDisplayMode" | "GetCreationParameters" | "GetRenderTarget" | "GetScissorRect" | "GetViewport" | "GetSwapChain" | "GetStreamSource" | "GetIndices" | "GetVertexShader" | "GetPixelShader") => 3,
        ("IDirect3DDevice9", "GetBackBuffer") => 5,
        ("IDirect3DDevice9", "Present") => 5,
        ("IDirect3DDevice9", "Clear") => 7,
        ("IDirect3DDevice9", "SetRenderTarget" | "SetTexture" | "SetFVF" | "SetVertexShader" | "SetPixelShader" | "SetIndices" | "SetScissorRect" | "SetViewport" | "SetRenderState" | "SetTransform" | "SetSoftwareVertexProcessing") => 3,
        ("IDirect3DDevice9", "SetTextureStageState" | "SetSamplerState") => 4,
        ("IDirect3DDevice9", "DrawPrimitive") => 4,
        ("IDirect3DDevice9", "DrawPrimitiveUP") => 5,
        ("IDirect3DDevice9", "DrawIndexedPrimitive") => 7,
        ("IDirect3DDevice9", "Reset") => 2,
        ("IDirect3DSurface9", "QueryInterface") => 3,
        ("IDirect3DSurface9", "AddRef" | "Release" | "GetPriority" | "PreLoad" | "GetType") => 1,
        ("IDirect3DSurface9", "GetDevice" | "GetDesc" | "UnlockRect" | "GetDC") => 2,
        ("IDirect3DSurface9", "GetContainer") => 3,
        ("IDirect3DSurface9", "LockRect") => 4,
        _ => return None,
    };
    Some(count * 4)
}

pub fn method(iface: &str, method: &str) -> Option<HleFn> {
    Some(match (iface, method) {
        ("IDirect3D9", "RegisterSoftwareDevice") => |_| Ok(S_OK),
        ("IDirect3D9", "GetAdapterCount") => |_| Ok(1),
        ("IDirect3D9", "GetAdapterIdentifier") => get_adapter_identifier,
        ("IDirect3D9", "GetAdapterModeCount") => get_adapter_mode_count,
        ("IDirect3D9", "EnumAdapterModes") => enum_adapter_modes,
        ("IDirect3D9", "GetAdapterDisplayMode") => get_adapter_display_mode,
        ("IDirect3D9", "CheckDeviceType" | "CheckDeviceFormat" | "CheckDeviceMultiSampleType" | "CheckDepthStencilMatch" | "CheckDeviceFormatConversion") => check_device,
        ("IDirect3D9", "GetDeviceCaps") => get_device_caps,
        ("IDirect3D9", "GetAdapterMonitor") => |_| Ok(0),
        ("IDirect3D9", "CreateDevice") => create_device,
        ("IDirect3DDevice9", "TestCooperativeLevel") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "GetAvailableTextureMem") => |_| Ok(512 * 1024 * 1024),
        ("IDirect3DDevice9", "EvictManagedResources") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "GetDirect3D") => get_direct3d,
        ("IDirect3DDevice9", "GetDeviceCaps") => get_device_caps_device,
        ("IDirect3DDevice9", "GetDisplayMode") => get_display_mode,
        ("IDirect3DDevice9", "GetCreationParameters") => get_creation_parameters,
        ("IDirect3DDevice9", "GetNumberOfSwapChains") => |_| Ok(1),
        ("IDirect3DDevice9", "Reset") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "Present") => present,
        ("IDirect3DDevice9", "GetBackBuffer") => get_backbuffer,
        ("IDirect3DDevice9", "SetRenderTarget") => set_render_target,
        ("IDirect3DDevice9", "GetRenderTarget") => get_render_target,
        ("IDirect3DDevice9", "BeginScene" | "EndScene") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "Clear") => clear,
        ("IDirect3DDevice9", "SetViewport") => set_viewport,
        ("IDirect3DDevice9", "SetScissorRect") => set_scissor_rect,
        ("IDirect3DDevice9", "SetFVF") => set_fvf,
        ("IDirect3DDevice9", "GetFVF") => get_fvf,
        ("IDirect3DDevice9", "SetTexture" | "SetRenderState" | "SetTransform" | "SetTextureStageState" | "SetSamplerState" | "SetVertexShader" | "SetPixelShader" | "SetVertexShaderConstantF" | "SetPixelShaderConstantF") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "DrawPrimitive") => draw_primitive,
        ("IDirect3DDevice9", "DrawPrimitiveUP") => draw_primitive_up,
        ("IDirect3DDevice9", "DrawIndexedPrimitive") => draw_indexed_primitive,
        ("IDirect3DSurface9", "GetDevice") => surface_get_device,
        ("IDirect3DSurface9", "GetContainer") => |_| Ok(E_NOTIMPL),
        ("IDirect3DSurface9", "GetDesc") => surface_get_desc,
        ("IDirect3DSurface9", "LockRect") => |_| Ok(E_NOTIMPL),
        ("IDirect3DSurface9", "UnlockRect") => |_| Ok(S_OK),
        ("IDirect3DSurface9", "GetDC") => |_| Ok(E_NOTIMPL),
        ("IDirect3DSurface9", "ReleaseDC") => |_| Ok(S_OK),
        _ => return None,
    })
}

fn direct3d_create9(_p: &mut Process) -> R {
    Ok(com::create(_p, "IDirect3D9", com::Kind::Direct3D9))
}

fn check_device(_p: &mut Process) -> R {
    Ok(S_OK)
}

fn adapter_modes(format: u32) -> &'static [(u32, u32)] {
    const RGB32: &[(u32, u32)] = &[
        (640, 480),
        (800, 600),
        (1024, 768),
        (1280, 720),
        (1920, 1080),
    ];
    const RGB16: &[(u32, u32)] = &[(640, 480), (800, 600), (1024, 768)];
    match format {
        21 | 22 => RGB32, // A8R8G8B8 / X8R8G8B8
        23 => RGB16,      // R5G6B5
        _ => &[],
    }
}

fn write_display_mode(
    p: &mut Process,
    out: u64,
    width: u32,
    height: u32,
    format: u32,
) -> Result<(), Stop> {
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(&width.to_le_bytes());
    data[4..8].copy_from_slice(&height.to_le_bytes());
    data[8..12].copy_from_slice(&60u32.to_le_bytes());
    data[12..16].copy_from_slice(&format.to_le_bytes());
    p.write_bytes(out, &data)
}

fn get_adapter_mode_count(p: &mut Process) -> R {
    let format = p.arg(2) as u32;
    Ok(adapter_modes(format).len() as u64)
}

fn get_adapter_display_mode(p: &mut Process) -> R {
    let out = p.arg(2);
    if out == 0 { return Ok(E_POINTER); }
    write_display_mode(p, out, 1920, 1080, 22)?;
    Ok(S_OK)
}

fn enum_adapter_modes(p: &mut Process) -> R {
    let (format, index, out) = (p.arg(2) as u32, p.arg(3) as usize, p.arg(4));
    if out == 0 { return Ok(E_POINTER); }
    let Some(&(width, height)) = adapter_modes(format).get(index) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    write_display_mode(p, out, width, height, format)?;
    Ok(S_OK)
}

fn get_adapter_identifier(p: &mut Process) -> R {
    let out = p.arg(3);
    if out == 0 { return Ok(E_POINTER); }
    let mut data = vec![0u8; 1100];
    put_cstr(&mut data[0..512], "anywasm_webgpu.dll");
    put_cstr(&mut data[512..1024], "AnyWASM WebGPU Adapter");
    put_cstr(&mut data[1024..1056], "\\\\.\\DISPLAY1");
    data[1064..1068].copy_from_slice(&0x1414u32.to_le_bytes()); // Microsoft
    data[1068..1072].copy_from_slice(&0x008cu32.to_le_bytes()); // Basic Render Driver
    p.write_bytes(out, &data)?;
    Ok(S_OK)
}

fn put_cstr(out: &mut [u8], value: &str) {
    let n = value.len().min(out.len().saturating_sub(1));
    out[..n].copy_from_slice(&value.as_bytes()[..n]);
}

fn write_caps(p: &mut Process, out: u64) -> Result<(), Stop> {
    let mut caps = [0u8; 304];
    let words = [
        1u32, 0, 0x0002_0000, 0xa000_0010, 0, 0x8000_0000, 0x0000_0001, 0x0001_0000,
        0x0000_01ff, 0x0003_ffff, 0x0000_00ff, 0x0000_1fff, 0x0000_1fff, 0x0000_00ff,
        0x0000_000f, 0x0000_ffff, 0x0000_003f, 0x0000_003f, 0x0000_003f, 0x0000_001f,
        0x0000_001f, 0x0000_000f, 8192, 8192, 2048, 8192, 8192, 16,
    ];
    for (index, word) in words.into_iter().enumerate() {
        let offset = index * 4;
        if offset + 4 <= caps.len() { caps[offset..offset + 4].copy_from_slice(&word.to_le_bytes()); }
    }
    caps[112..116].copy_from_slice(&1.0e10f32.to_le_bytes());
    caps[116..132].copy_from_slice(&[0u8; 16]);
    caps[132..136].copy_from_slice(&1.0f32.to_le_bytes());
    caps[140..144].copy_from_slice(&0x0000_000fu32.to_le_bytes());
    caps[144..148].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
    caps[148..152].copy_from_slice(&8u32.to_le_bytes());
    caps[152..156].copy_from_slice(&8u32.to_le_bytes());
    caps[156..160].copy_from_slice(&0x0000_003fu32.to_le_bytes());
    caps[160..164].copy_from_slice(&8u32.to_le_bytes());
    caps[164..168].copy_from_slice(&6u32.to_le_bytes());
    caps[168..172].copy_from_slice(&4u32.to_le_bytes());
    caps[176..180].copy_from_slice(&64.0f32.to_le_bytes());
    caps[180..184].copy_from_slice(&0x000f_ffffu32.to_le_bytes());
    caps[184..188].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
    caps[188..192].copy_from_slice(&16u32.to_le_bytes());
    caps[192..196].copy_from_slice(&2048u32.to_le_bytes());
    caps[196..200].copy_from_slice(&0x0300u32.to_le_bytes());
    caps[200..204].copy_from_slice(&256u32.to_le_bytes());
    caps[204..208].copy_from_slice(&0x0300u32.to_le_bytes());
    caps[208..212].copy_from_slice(&8.0f32.to_le_bytes());
    caps[212..216].copy_from_slice(&0x0000_0003u32.to_le_bytes());
    caps[236..240].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
    caps[240..244].copy_from_slice(&1u32.to_le_bytes());
    p.write_bytes(out, &caps)
}

fn get_device_caps(p: &mut Process) -> R {
    let out = p.arg(3);
    if out == 0 { return Ok(E_POINTER); }
    write_caps(p, out)?;
    Ok(S_OK)
}

fn get_device_caps_device(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 { return Ok(E_POINTER); }
    write_caps(p, out)?;
    Ok(S_OK)
}

fn dxgi_format(d3d: u32) -> u32 {
    match d3d {
        21 => 87, // A8R8G8B8 -> B8G8R8A8_UNORM
        22 => 88, // X8R8G8B8 -> B8G8R8X8_UNORM
        23 => 85, // R5G6B5 -> B5G6R5_UNORM
        32 => 28, // A8B8G8R8 -> R8G8B8A8_UNORM
        _ => 87,
    }
}

fn create_backbuffer_view(p: &mut Process, backbuffer: u64) -> Result<u64, Stop> {
    let Some(com::Kind::Texture2D { id: texture_id }) = com::kind_of(p, backbuffer) else {
        return Ok(0);
    };
    let info = p.extra.com.res.get(&texture_id).cloned().unwrap_or_default();
    let id = p.extra.gpu.new_id();
    p.extra.gpu.emit(
        op::CREATE_VIEW,
        Pk::new().u32(id).u32(0).u32(texture_id).u32(info.format).u32(4).u32(0).u32(1).u32(0).u32(1),
    );
    Ok(com::create(p, "ID3D11RenderTargetView", com::Kind::View {
        vk: com::ViewKind::Rtv,
        id,
        resource: texture_id,
    }))
}

fn create_device(p: &mut Process) -> R {
    let (focus, params, out) = (p.arg(3), p.arg(5), p.arg(6));
    if params == 0 || out == 0 { return Ok(E_INVALIDARG); }
    p.wrptr(out, 0)?;
    let width = p.rd32(params)?;
    let height = p.rd32(params + 4)?;
    let format = p.rd32(params + 8)?;
    let buffers = p.rd32(params + 12)?.max(1);
    let hwnd = if p.rd32(params + 28)? != 0 { p.rd32(params + 28)? as u64 } else { focus };
    let fallback = if p.extra.win.main_window != 0 { p.extra.win.main_window } else {
        p.extra.win.windows.iter().find(|(_, w)| w.visible).map(|(h, _)| *h).unwrap_or(0)
    };
    let hwnd = if hwnd == 0 { fallback } else { hwnd };
    let (window_width, window_height) = p.extra.win.windows.get(&hwnd)
        .map(|w| (w.w.max(1) as u32, w.h.max(1) as u32))
        .unwrap_or((640, 480));
    let width = if width == 0 { window_width } else { width };
    let height = if height == 0 { window_height } else { height };
    let format = dxgi_format(format);
    let swapchain = dxgi::make_swap_chain(p, hwnd, width, height, format, buffers);
    let (backbuffer, sc_id) = match com::kind_of(p, swapchain) {
        Some(com::Kind::SwapChain { id, backbuffer }) => (backbuffer, id),
        _ => return Ok(E_OUTOFMEMORY),
    };
    let rtv = create_backbuffer_view(p, backbuffer)?;
    let rtv_id = match com::kind_of(p, rtv) { Some(com::Kind::View { id, .. }) => id as u64, _ => 0 };
    p.extra.gpu.emit(op::SET_RENDER_TARGETS, Pk::new().u32(1).u32(rtv_id as u32).u32(0));
    p.extra.gpu.emit(
        op::SET_VIEWPORTS,
        Pk::new().u32(1).f32(0.0).f32(0.0).f32(width as f32).f32(height as f32).f32(0.0).f32(1.0),
    );
    if params != 0 {
        p.wr32(params, width)?;
        p.wr32(params + 4, height)?;
        p.wr32(params + 8, format_to_d3d(format))?;
        p.wr32(params + 12, buffers)?;
        p.wr32(params + 28, hwnd as u32)?;
    }
    let parent = p.arg(0);
    let device = com::create(p, "IDirect3DDevice9", com::Kind::Direct3DDevice9 {
        d3d9: parent,
        swapchain,
        backbuffer,
        rtv,
        current_rtv: rtv,
        window: hwnd,
        width,
        height,
        fvf: 0,
    });
    p.wrptr(out, device)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(format!("[d3d9] CreateDevice hwnd={hwnd:#x} size={width}x{height} swapchain={sc_id} device={device:#x}\n").as_bytes());
    }
    Ok(S_OK)
}

fn format_to_d3d(format: u32) -> u32 {
    match format { 87 => 21, 88 => 22, 85 => 23, 28 => 32, _ => 21 }
}

fn device_state(p: &Process, device: u64) -> Option<(u64, u64, u64, u64, u32, u32, u32)> {
    let o = p.extra.com.objs.get(&device)?;
    match o.kind {
        com::Kind::Direct3DDevice9 { d3d9, swapchain, backbuffer, rtv, current_rtv, width, height, .. } => {
            Some((d3d9, swapchain, backbuffer, rtv, width, height, current_rtv as u32))
        }
        _ => None,
    }
}

fn get_direct3d(p: &mut Process) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    let Some((d3d9, ..)) = device_state(p, device) else { return Ok(D3DERR_INVALIDCALL); };
    if let Some(o) = p.extra.com.objs.get_mut(&d3d9) { o.refs += 1; }
    p.wrptr(out, d3d9)?;
    Ok(S_OK)
}

fn get_display_mode(p: &mut Process) -> R {
    let out = p.arg(2);
    if out == 0 { return Ok(E_POINTER); }
    let (width, height, format) = device_state(p, p.arg(0))
        .map(|(_, _, backbuffer, _, width, height, _)| {
            let format = match com::kind_of(p, backbuffer) {
                Some(com::Kind::Texture2D { id }) => p
                    .extra
                    .com
                    .res
                    .get(&id)
                    .map(|resource| format_to_d3d(resource.format))
                    .unwrap_or(22),
                _ => 22,
            };
            (width, height, format)
        })
        .unwrap_or((640, 480, 22));
    write_display_mode(p, out, width, height, format)?;
    Ok(S_OK)
}

fn get_creation_parameters(p: &mut Process) -> R {
    let this = p.arg(0);
    let out = p.arg(1);
    let Some((_, _, _, _, _, _, _)) = device_state(p, this) else { return Ok(D3DERR_INVALIDCALL); };
    if out == 0 { return Ok(E_POINTER); }
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(&1u32.to_le_bytes());
    let hwnd = device_state(p, this).map(|(_, _, _, _, _, _, _)| p.extra.com.objs.get(&this).and_then(|o| if let com::Kind::Direct3DDevice9 {window,..}=o.kind { Some(window as u32) } else {None}).unwrap_or(0)).unwrap_or(0);
    data[4..8].copy_from_slice(&hwnd.to_le_bytes());
    p.write_bytes(out, &data)?;
    Ok(S_OK)
}

fn create_surface_for_device(p: &mut Process, device: u64) -> Result<u64, Stop> {
    let Some((_, _, backbuffer, rtv, width, height, _)) = device_state(p, device) else { return Ok(0); };
    let texture_id = match com::kind_of(p, backbuffer) { Some(com::Kind::Texture2D { id }) => id, _ => 0 };
    let format = p.extra.com.res.get(&texture_id).map(|r| format_to_d3d(r.format)).unwrap_or(21);
    Ok(com::create(p, "IDirect3DSurface9", com::Kind::Direct3DSurface9 {
        device,
        texture_id,
        rtv,
        width,
        height,
        format,
    }))
}

fn get_backbuffer(p: &mut Process) -> R {
    let (device, index, out) = (p.arg(0), p.arg(2), p.arg(4));
    if out == 0 { return Ok(E_POINTER); }
    if index != 0 { p.wrptr(out, 0)?; return Ok(D3DERR_INVALIDCALL); }
    let surface = create_surface_for_device(p, device)?;
    if surface == 0 { return Ok(D3DERR_INVALIDCALL); }
    p.wrptr(out, surface)?;
    Ok(S_OK)
}

fn current_render_target(p: &Process, device: u64) -> Option<u64> {
    let o = p.extra.com.objs.get(&device)?;
    match o.kind { com::Kind::Direct3DDevice9 { current_rtv, .. } => Some(current_rtv), _ => None }
}

fn set_render_target(p: &mut Process) -> R {
    let (device, index, surface) = (p.arg(0), p.arg(1), p.arg(2));
    if index != 0 { return Ok(D3DERR_INVALIDCALL); }
    let rtv = match com::kind_of(p, surface) {
        Some(com::Kind::Direct3DSurface9 { rtv, .. }) => rtv,
        _ => return Ok(D3DERR_INVALIDCALL),
    };
    if let Some(o) = p.extra.com.objs.get_mut(&device) {
        if let com::Kind::Direct3DDevice9 { current_rtv, .. } = &mut o.kind { *current_rtv = rtv; }
    }
    let id = match com::kind_of(p, rtv) { Some(com::Kind::View { id, .. }) => id, _ => 0 };
    p.extra.gpu.emit(op::SET_RENDER_TARGETS, Pk::new().u32(1).u32(id).u32(0));
    Ok(S_OK)
}

fn get_render_target(p: &mut Process) -> R {
    let (device, index, out) = (p.arg(0), p.arg(1), p.arg(2));
    if out == 0 { return Ok(E_POINTER); }
    if index != 0 { p.wrptr(out, 0)?; return Ok(D3DERR_INVALIDCALL); }
    let surface = create_surface_for_device(p, device)?;
    if surface == 0 { return Ok(D3DERR_INVALIDCALL); }
    p.wrptr(out, surface)?;
    Ok(S_OK)
}

fn present(p: &mut Process) -> R {
    let Some((_, swapchain, backbuffer, ..)) = device_state(p, p.arg(0)) else { return Ok(D3DERR_INVALIDCALL); };
    let (sc_id, tex_id) = match (com::kind_of(p, swapchain), com::kind_of(p, backbuffer)) {
        (Some(com::Kind::SwapChain { id, .. }), Some(com::Kind::Texture2D { id: tex })) => (id, tex),
        _ => return Ok(D3DERR_INVALIDCALL),
    };
    p.extra.gpu.emit(op::PRESENT, Pk::new().u32(sc_id).u32(tex_id));
    p.yield_request = Some(Stop::Frame);
    Ok(S_OK)
}

fn clear(p: &mut Process) -> R {
    let (device, flags, color, depth, stencil) = (p.arg(0), p.arg(3) as u32, p.arg(4) as u32, p.arg(5), p.arg(6) as u32);
    let Some(rtv) = current_render_target(p, device) else { return Ok(D3DERR_INVALIDCALL); };
    if flags & 1 != 0 {
        let id = match com::kind_of(p, rtv) { Some(com::Kind::View { id, .. }) => id, _ => 0 };
        let [a, r, g, b] = color.to_be_bytes();
        p.extra.gpu.emit(op::CLEAR_RTV, Pk::new().u32(id).f32(r as f32 / 255.0).f32(g as f32 / 255.0).f32(b as f32 / 255.0).f32(a as f32 / 255.0));
    }
    if flags & (2 | 4) != 0 && depth != 0 {
        let id = match com::kind_of(p, depth) { Some(com::Kind::View { id, .. }) => id, _ => 0 };
        p.extra.gpu.emit(op::CLEAR_DSV, Pk::new().u32(id).u32(((flags & 2 != 0) as u32) | (((flags & 4 != 0) as u32) << 1)).f32(f32::from_bits(p.arg(5) as u32)).u32(stencil));
    }
    Ok(S_OK)
}

fn set_viewport(p: &mut Process) -> R {
    let desc = p.arg(1);
    if desc == 0 { return Ok(E_POINTER); }
    let (x,y,w,h) = (p.rd32(desc)?, p.rd32(desc+4)?, p.rd32(desc+8)?, p.rd32(desc+12)?);
    let (minz,maxz) = (f32::from_bits(p.rd32(desc+16)?), f32::from_bits(p.rd32(desc+20)?));
    p.extra.gpu.emit(op::SET_VIEWPORTS, Pk::new().u32(1).f32(x as f32).f32(y as f32).f32(w as f32).f32(h as f32).f32(minz).f32(maxz));
    Ok(S_OK)
}

fn set_scissor_rect(p: &mut Process) -> R {
    let rect = p.arg(1);
    if rect == 0 { return Ok(E_POINTER); }
    p.extra.gpu.emit(op::SET_SCISSORS, Pk::new().u32(1).i32(p.rd32(rect)? as i32).i32(p.rd32(rect+4)? as i32).i32(p.rd32(rect+8)? as i32).i32(p.rd32(rect+12)? as i32));
    Ok(S_OK)
}

fn set_fvf(p: &mut Process) -> R {
    let (device, fvf) = (p.arg(0), p.arg(1) as u32);
    if !matches!(fvf, 0x42 | 0x44) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let Some((_, _, _, _, width, height, _)) = device_state(p, device) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if let Some(o) = p.extra.com.objs.get_mut(&device) {
        if let com::Kind::Direct3DDevice9 { fvf: current, .. } = &mut o.kind { *current = fvf; }
    }
    p.extra.gpu.emit(op::SET_FIXED_FUNCTION, Pk::new().u32(fvf).u32(width).u32(height));
    Ok(S_OK)
}

fn get_fvf(p: &mut Process) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    if out == 0 { return Ok(E_POINTER); }
    let fvf = p.extra.com.objs.get(&device).and_then(|o| if let com::Kind::Direct3DDevice9 { fvf, .. } = o.kind {Some(fvf)} else {None}).unwrap_or(0);
    p.wr32(out, fvf)?;
    Ok(S_OK)
}

fn primitive_vertices(kind: u32, primitives: u32) -> u32 {
    match kind { 1 => primitives, 2 => primitives.saturating_mul(2), 3 => primitives.saturating_add(1), 4 => primitives.saturating_mul(3), 5 | 6 => primitives.saturating_add(2), _ => 0 }
}

fn draw_primitive(p: &mut Process) -> R {
    let (kind, start, primitives) = (p.arg(1) as u32, p.arg(2) as u32, p.arg(3) as u32);
    p.extra.gpu.emit(op::SET_TOPOLOGY, Pk::new().u32(kind));
    p.extra.gpu.emit(op::DRAW, Pk::new().u32(primitive_vertices(kind, primitives)).u32(start));
    Ok(S_OK)
}

fn draw_indexed_primitive(p: &mut Process) -> R {
    let (kind, base, start, primitives) = (p.arg(1) as u32, p.arg(2) as i32, p.arg(5) as u32, p.arg(6) as u32);
    p.extra.gpu.emit(op::SET_TOPOLOGY, Pk::new().u32(kind));
    p.extra.gpu.emit(op::DRAW_INDEXED, Pk::new().u32(primitive_vertices(kind, primitives)).u32(start).i32(base));
    Ok(S_OK)
}

fn draw_primitive_up(p: &mut Process) -> R {
    let (device, kind, primitive_count, data, stride) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4) as u32,
    );
    let fvf = p.extra.com.objs.get(&device).and_then(|o| match o.kind {
        com::Kind::Direct3DDevice9 { fvf, .. } => Some(fvf),
        _ => None,
    });
    let Some(fvf) = fvf else { return Ok(D3DERR_INVALIDCALL); };
    let Some(min_stride) = fvf_stride(fvf) else { return Ok(D3DERR_NOTAVAILABLE); };
    if kind == 0 || kind > 5 || stride < min_stride || data == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    if primitive_count == 0 {
        return Ok(S_OK);
    }
    let vertex_count = primitive_vertices(kind, primitive_count);
    let Some(byte_count) = vertex_count.checked_mul(stride) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if byte_count > 16 * 1024 * 1024 {
        return Ok(D3DERR_INVALIDCALL);
    }
    let vertices = p.read_bytes(data, byte_count as usize)?;
    let id = p.extra.gpu.new_id();
    p.extra.gpu.emit(
        op::CREATE_BUFFER,
        Pk::new().u32(id).u32(byte_count).u32(1).u32(0).u32(0).u32(stride),
    );
    p.extra.gpu.emit(
        op::BUFFER_WRITE,
        Pk::new().u32(id).u32(0).u32(byte_count).bytes(&vertices),
    );
    p.extra.gpu.emit(
        op::SET_VERTEX_BUFFERS,
        Pk::new().u32(0).u32(1).u32(id).u32(stride).u32(0),
    );
    p.extra.gpu.emit(op::SET_TOPOLOGY, Pk::new().u32(kind));
    p.extra.gpu.emit(op::DRAW, Pk::new().u32(vertex_count).u32(0));
    p.extra.gpu.emit(
        op::SET_VERTEX_BUFFERS,
        Pk::new().u32(0).u32(1).u32(0).u32(0).u32(0),
    );
    p.extra.gpu.emit(op::RELEASE, Pk::new().u32(id));
    Ok(S_OK)
}

fn fvf_stride(fvf: u32) -> Option<u32> {
    match fvf {
        0x42 => Some(16), // XYZ + D3DCOLOR
        0x44 => Some(20), // XYZRHW + D3DCOLOR
        _ => None,
    }
}

fn surface_get_device(p: &mut Process) -> R {
    let (surface, out) = (p.arg(0), p.arg(1));
    let Some(com::Kind::Direct3DSurface9 { device, .. }) = com::kind_of(p, surface) else { return Ok(D3DERR_INVALIDCALL); };
    if let Some(o) = p.extra.com.objs.get_mut(&device) { o.refs += 1; }
    p.wrptr(out, device)?;
    Ok(S_OK)
}

fn surface_get_desc(p: &mut Process) -> R {
    let (surface, out) = (p.arg(0), p.arg(1));
    if out == 0 { return Ok(E_POINTER); }
    let Some(com::Kind::Direct3DSurface9 { width,height,format,.. }) = com::kind_of(p, surface) else { return Ok(D3DERR_INVALIDCALL); };
    let mut data = [0u8; 32];
    data[0..4].copy_from_slice(&format.to_le_bytes());
    data[4..8].copy_from_slice(&1u32.to_le_bytes());
    data[24..28].copy_from_slice(&width.to_le_bytes());
    data[28..32].copy_from_slice(&height.to_le_bytes());
    p.write_bytes(out, &data)?;
    Ok(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
    use anywasm_cpu::CpuMode;
    use crate::vfs::Vfs;

    #[test]
    fn draw_primitive_up_emits_a_fvf_draw_and_releases_its_temporary_buffer() {
        const STACK: u64 = 0x10000;
        const VERTICES: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(VERTICES, PAGE_SIZE, PROT_RW));
        let device = com::create(
            &mut p,
            "IDirect3DDevice9",
            com::Kind::Direct3DDevice9 {
                d3d9: 0,
                swapchain: 0,
                backbuffer: 0,
                rtv: 0,
                current_rtv: 0,
                window: 0,
                width: 640,
                height: 480,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;

        p.wr32(STACK + 4, device as u32).unwrap();
        p.wr32(STACK + 8, 0x44).unwrap(); // XYZRHW | DIFFUSE
        let set_fvf = method("IDirect3DDevice9", "SetFVF").unwrap();
        assert_eq!(set_fvf(&mut p).unwrap(), S_OK);

        let vertex_bytes = (0..60).map(|byte| byte as u8).collect::<Vec<_>>();
        p.write_bytes(VERTICES, &vertex_bytes).unwrap();
        for (offset, value) in [
            (4, device as u32),
            (8, 4), // triangle list
            (12, 1), // one primitive
            (16, VERTICES as u32),
            (20, 20), // XYZRHW + D3DCOLOR stride
        ] {
            p.wr32(STACK + offset, value).unwrap();
        }
        let draw = method("IDirect3DDevice9", "DrawPrimitiveUP").unwrap();
        assert_eq!(draw(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(
            commands.iter().map(|command| command.0).collect::<Vec<_>>(),
            vec![
                op::SET_FIXED_FUNCTION,
                op::CREATE_BUFFER,
                op::BUFFER_WRITE,
                op::SET_VERTEX_BUFFERS,
                op::SET_TOPOLOGY,
                op::DRAW,
                op::SET_VERTEX_BUFFERS,
                op::RELEASE,
            ]
        );
        assert_eq!(
            commands[0]
                .1
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![0x44, 640, 480]
        );
        let buffer_id = u32::from_le_bytes(commands[1].1[0..4].try_into().unwrap());
        assert_eq!(
            commands[1]
                .1
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![buffer_id, 60, 1, 0, 0, 20]
        );
        assert_eq!(&commands[2].1[12..], &vertex_bytes);
        assert_eq!(
            commands[5]
                .1
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![3, 0]
        );
        assert_eq!(
            commands[7].1,
            buffer_id.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn device_get_device_caps_writes_its_single_pointer_argument() {
        const STACK: u64 = 0x10000;
        const CAPS: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(CAPS, PAGE_SIZE, PROT_RW));
        let device = com::create(
            &mut p,
            "IDirect3DDevice9",
            com::Kind::Direct3DDevice9 {
                d3d9: 0,
                swapchain: 0,
                backbuffer: 0,
                rtv: 0,
                current_rtv: 0,
                window: 0,
                width: 640,
                height: 480,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK + 4, device as u32).unwrap();
        p.wr32(STACK + 8, CAPS as u32).unwrap();

        let get_caps = method("IDirect3DDevice9", "GetDeviceCaps").unwrap();
        assert_eq!(get_caps(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(CAPS).unwrap(), 1);
        assert_eq!(p.rd32(CAPS + 148).unwrap(), 8);
    }
}
