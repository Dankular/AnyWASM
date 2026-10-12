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
const D3DERR_NOTFOUND: u64 = 0x8876_0866;
const D3DERR_MOREDATA: u64 = 0x8876_0867;

const IID_D3D9: &str = "81bdcbca-64d4-426d-ae8d-ad0147f4275c";
const IID_D3D9_DEVICE: &str = "d0223b96-bf7a-43fd-92bd-a43b0d82b9eb";
const IID_D3D9_SURFACE: &str = "0cfbaf3a-9ff6-429a-99b3-a2796af8b89b";
const IID_D3D9_RESOURCE: &str = "05eec05d-8f7d-4362-b999-d1baf357c704";
const IID_D3D9_BASE_TEXTURE: &str = "580ca87e-1d3c-4d54-991d-b7d3e3c298ce";
const IID_D3D9_TEXTURE: &str = "85c31227-3de5-4f00-9b3a-f11ac38c18b5";
const IID_D3D9_VERTEX_BUFFER: &str = "b64bb1b5-fd70-4df6-bf91-19d0a12455e3";
const IID_D3D9_INDEX_BUFFER: &str = "7c9dd65e-d3f7-4529-acee-785830acde35";
const IID_D3D9_VERTEX_DECLARATION: &str = "dd13c59c-36fa-4098-a8fb-c7ed39dc8546";
const IID_D3D9_VERTEX_SHADER: &str = "efc5557e-6265-4613-8a94-43857889eb36";
const IID_D3D9_PIXEL_SHADER: &str = "6d3bdbdc-5b02-4415-b852-ce5e8bccb289";

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
    "LightEnable", "GetLightEnable", "SetClipPlane", "GetClipPlane", "SetRenderState", "GetRenderState", "CreateStateBlock",
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

const D3D9_RESOURCE_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData",
    "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType",
];

const D3D9_BASE_TEXTURE_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData",
    "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType", "SetLOD", "GetLOD",
    "GetLevelCount", "SetAutoGenFilterType", "GetAutoGenFilterType", "GenerateMipSubLevels",
];

const D3D9_TEXTURE_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData",
    "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType", "SetLOD", "GetLOD",
    "GetLevelCount", "SetAutoGenFilterType", "GetAutoGenFilterType", "GenerateMipSubLevels",
    "GetLevelDesc", "GetSurfaceLevel", "LockRect", "UnlockRect", "AddDirtyRect",
];

const D3D9_VERTEX_BUFFER_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData",
    "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType", "Lock", "Unlock",
    "GetDesc",
];

const D3D9_INDEX_BUFFER_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "SetPrivateData", "GetPrivateData",
    "FreePrivateData", "SetPriority", "GetPriority", "PreLoad", "GetType", "Lock", "Unlock",
    "GetDesc",
];

const D3D9_VERTEX_DECLARATION_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "GetDeclaration",
];
const D3D9_SHADER_METHODS: &[&str] = &[
    "QueryInterface", "AddRef", "Release", "GetDevice", "GetFunction",
];

pub fn lookup(name: &str) -> Option<HleFn> {
    (name == "Direct3DCreate9").then_some(direct3d_create9)
}
pub fn methods(iface: &str) -> Option<&'static [&'static str]> {
    match iface {
        "IDirect3D9" => Some(D3D9_METHODS),
        "IDirect3DDevice9" => Some(D3D9_DEVICE_METHODS),
        "IDirect3DSurface9" => Some(D3D9_SURFACE_METHODS),
        "IDirect3DResource9" => Some(D3D9_RESOURCE_METHODS),
        "IDirect3DBaseTexture9" => Some(D3D9_BASE_TEXTURE_METHODS),
        "IDirect3DTexture9" => Some(D3D9_TEXTURE_METHODS),
        "IDirect3DVertexBuffer9" => Some(D3D9_VERTEX_BUFFER_METHODS),
        "IDirect3DIndexBuffer9" => Some(D3D9_INDEX_BUFFER_METHODS),
        "IDirect3DVertexDeclaration9" => Some(D3D9_VERTEX_DECLARATION_METHODS),
        "IDirect3DVertexShader9" | "IDirect3DPixelShader9" => Some(D3D9_SHADER_METHODS),
        _ => None,
    }
}

pub fn iid_name(iid: &str) -> Option<&'static str> {
    match iid {
        IID_D3D9 => Some("IDirect3D9"),
        IID_D3D9_DEVICE => Some("IDirect3DDevice9"),
        IID_D3D9_SURFACE => Some("IDirect3DSurface9"),
        IID_D3D9_RESOURCE => Some("IDirect3DResource9"),
        IID_D3D9_BASE_TEXTURE => Some("IDirect3DBaseTexture9"),
        IID_D3D9_TEXTURE => Some("IDirect3DTexture9"),
        IID_D3D9_VERTEX_BUFFER => Some("IDirect3DVertexBuffer9"),
        IID_D3D9_INDEX_BUFFER => Some("IDirect3DIndexBuffer9"),
        IID_D3D9_VERTEX_DECLARATION => Some("IDirect3DVertexDeclaration9"),
        IID_D3D9_VERTEX_SHADER => Some("IDirect3DVertexShader9"),
        IID_D3D9_PIXEL_SHADER => Some("IDirect3DPixelShader9"),
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
        ("IDirect3DDevice9", "GetDisplayMode" | "GetSwapChain" | "GetRenderTarget") => 3,
        ("IDirect3DDevice9", "GetCreationParameters" | "GetScissorRect" | "GetViewport" | "GetIndices" | "GetVertexShader" | "GetPixelShader") => 2,
        ("IDirect3DDevice9", "GetStreamSource") => 5,
        ("IDirect3DDevice9", "GetBackBuffer") => 5,
        ("IDirect3DDevice9", "CreateDepthStencilSurface") => 9,
        ("IDirect3DDevice9", "CreateTexture") => 9,
        ("IDirect3DDevice9", "GetDepthStencilSurface" | "SetDepthStencilSurface") => 2,
        ("IDirect3DDevice9", "CreateVertexBuffer" | "CreateIndexBuffer") => 7,
        ("IDirect3DDevice9", "CreateVertexDeclaration") => 3,
        ("IDirect3DDevice9", "CreateVertexShader" | "CreatePixelShader") => 3,
        ("IDirect3DDevice9", "SetVertexDeclaration" | "GetVertexDeclaration") => 2,
        ("IDirect3DDevice9", "Present") => 5,
        ("IDirect3DDevice9", "Clear") => 7,
        ("IDirect3DDevice9", "SetRenderTarget" | "SetTexture" | "SetRenderState" | "SetTransform") => 3,
        ("IDirect3DDevice9", "GetRenderState") => 3,
        ("IDirect3DDevice9", "SetFVF" | "SetVertexShader" | "SetPixelShader" | "SetIndices" | "SetScissorRect" | "SetViewport" | "SetSoftwareVertexProcessing") => 2,
        ("IDirect3DDevice9", "SetStreamSource") => 5,
        ("IDirect3DDevice9", "SetTextureStageState" | "SetSamplerState") => 4,
        ("IDirect3DDevice9", "DrawPrimitive") => 4,
        ("IDirect3DDevice9", "DrawPrimitiveUP") => 5,
        ("IDirect3DDevice9", "DrawIndexedPrimitive") => 7,
        ("IDirect3DDevice9", "Reset") => 2,
        ("IDirect3DSurface9", "QueryInterface") => 3,
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "QueryInterface") => 3,
        ("IDirect3DSurface9", "AddRef" | "Release" | "GetPriority" | "PreLoad" | "GetType") => 1,
        ("IDirect3DSurface9", "GetDevice" | "GetDesc" | "GetDC") => 2,
        ("IDirect3DSurface9", "UnlockRect") => 1,
        ("IDirect3DSurface9", "GetContainer") => 3,
        ("IDirect3DSurface9", "LockRect") => 4,
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "AddRef" | "Release" | "GetPriority" | "PreLoad" | "GetType" | "GetLOD" | "GetLevelCount" | "GetAutoGenFilterType" | "GenerateMipSubLevels") => 1,
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "GetDevice" | "SetPriority" | "SetLOD" | "SetAutoGenFilterType") => 2,
        ("IDirect3DTexture9", "GetLevelDesc" | "GetSurfaceLevel" | "AddDirtyRect") => 3,
        ("IDirect3DTexture9", "LockRect") => 5,
        ("IDirect3DTexture9", "UnlockRect") => 2,
        ("IDirect3DResource9", "GetDevice") => 2,
        ("IDirect3DResource9", "SetPrivateData") => 5,
        ("IDirect3DResource9", "GetPrivateData") => 4,
        ("IDirect3DResource9", "FreePrivateData") => 2,
        ("IDirect3DResource9", "SetPriority") => 2,
        ("IDirect3DResource9", "GetPriority" | "PreLoad" | "GetType") => 1,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetDevice") => 2,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "SetPrivateData") => 5,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetPrivateData") => 4,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "FreePrivateData") => 2,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "SetPriority") => 2,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetPriority" | "PreLoad" | "GetType" | "Unlock") => 1,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "Lock") => 5,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetDesc") => 2,
        ("IDirect3DVertexDeclaration9", "GetDevice") => 2,
        ("IDirect3DVertexDeclaration9", "GetDeclaration") => 3,
        ("IDirect3DVertexShader9" | "IDirect3DPixelShader9", "GetDevice") => 2,
        ("IDirect3DVertexShader9" | "IDirect3DPixelShader9", "GetFunction") => 3,
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
        ("IDirect3D9", "CheckDeviceType" | "CheckDeviceFormat" | "CheckDepthStencilMatch" | "CheckDeviceFormatConversion") => check_device,
        ("IDirect3D9", "CheckDeviceMultiSampleType") => check_multisample,
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
        ("IDirect3DDevice9", "CreateDepthStencilSurface") => create_depth_stencil_surface,
        ("IDirect3DDevice9", "CreateTexture") => create_texture9,
        ("IDirect3DDevice9", "GetDepthStencilSurface") => get_depth_stencil_surface,
        ("IDirect3DDevice9", "GetStreamSource") => get_stream_source,
        ("IDirect3DDevice9", "GetIndices") => get_indices,
        ("IDirect3DDevice9", "SetRenderTarget") => set_render_target,
        ("IDirect3DDevice9", "GetRenderTarget") => get_render_target,
        ("IDirect3DDevice9", "SetDepthStencilSurface") => set_depth_stencil_surface,
        ("IDirect3DDevice9", "CreateVertexBuffer") => |p| create_buffer9(p, true),
        ("IDirect3DDevice9", "CreateIndexBuffer") => |p| create_buffer9(p, false),
        ("IDirect3DDevice9", "CreateVertexDeclaration") => create_vertex_declaration,
        ("IDirect3DDevice9", "CreateVertexShader") => |p| create_shader9(p, true),
        ("IDirect3DDevice9", "CreatePixelShader") => |p| create_shader9(p, false),
        ("IDirect3DDevice9", "SetVertexDeclaration") => set_vertex_declaration,
        ("IDirect3DDevice9", "GetVertexDeclaration") => get_vertex_declaration,
        ("IDirect3DDevice9", "BeginScene" | "EndScene") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "Clear") => clear,
        ("IDirect3DDevice9", "SetViewport") => set_viewport,
        ("IDirect3DDevice9", "SetScissorRect") => set_scissor_rect,
        ("IDirect3DDevice9", "SetFVF") => set_fvf,
        ("IDirect3DDevice9", "GetFVF") => get_fvf,
        ("IDirect3DDevice9", "SetStreamSource") => set_stream_source,
        ("IDirect3DDevice9", "SetIndices") => set_indices,
        ("IDirect3DDevice9", "SetRenderState") => set_render_state,
        ("IDirect3DDevice9", "GetRenderState") => get_render_state,
        ("IDirect3DDevice9", "SetVertexShader") => |p| set_shader9(p, true),
        ("IDirect3DDevice9", "SetPixelShader") => |p| set_shader9(p, false),
        ("IDirect3DDevice9", "GetVertexShader") => |p| get_shader9(p, true),
        ("IDirect3DDevice9", "GetPixelShader") => |p| get_shader9(p, false),
        ("IDirect3DDevice9", "SetTexture") => set_texture,
        ("IDirect3DDevice9", "SetSamplerState") => set_sampler_state,
        ("IDirect3DDevice9", "SetTextureStageState") => set_texture_stage_state,
        ("IDirect3DDevice9", "SetTransform" | "SetVertexShaderConstantF" | "SetPixelShaderConstantF") => |_| Ok(S_OK),
        ("IDirect3DDevice9", "DrawPrimitive") => draw_primitive,
        ("IDirect3DDevice9", "DrawPrimitiveUP") => draw_primitive_up,
        ("IDirect3DDevice9", "DrawIndexedPrimitive") => draw_indexed_primitive,
        ("IDirect3DSurface9", "GetDevice") => surface_get_device,
        ("IDirect3DSurface9", "GetContainer") => |_| Ok(E_NOTIMPL),
        ("IDirect3DSurface9", "GetDesc") => surface_get_desc,
        ("IDirect3DSurface9", "LockRect") => surface_lock_rect,
        ("IDirect3DSurface9", "UnlockRect") => surface_unlock_rect,
        ("IDirect3DSurface9", "GetDC") => |_| Ok(E_NOTIMPL),
        ("IDirect3DSurface9", "ReleaseDC") => |_| Ok(S_OK),
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "GetDevice") => resource_get_device,
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "SetPriority" | "PreLoad" | "SetAutoGenFilterType" | "GenerateMipSubLevels") => |_| Ok(S_OK),
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "GetPriority" | "GetLOD" | "GetAutoGenFilterType") => |_| Ok(0),
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "SetLOD") => |_| Ok(0),
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "GetLevelCount") => texture_level_count,
        ("IDirect3DBaseTexture9" | "IDirect3DTexture9", "GetType") => get_resource_type,
        ("IDirect3DTexture9", "GetLevelDesc") => texture_get_level_desc,
        ("IDirect3DTexture9", "GetSurfaceLevel") => texture_get_surface_level,
        ("IDirect3DTexture9", "LockRect") => texture_lock_rect,
        ("IDirect3DTexture9", "UnlockRect") => texture_unlock_rect,
        ("IDirect3DTexture9", "AddDirtyRect") => |_| Ok(S_OK),
        ("IDirect3DResource9" | "IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetDevice") => resource_get_device,
        ("IDirect3DVertexDeclaration9", "GetDevice") => declaration_get_device,
        ("IDirect3DVertexDeclaration9", "GetDeclaration") => declaration_get_elements,
        ("IDirect3DVertexShader9" | "IDirect3DPixelShader9", "GetDevice") => shader9_get_device,
        ("IDirect3DVertexShader9" | "IDirect3DPixelShader9", "GetFunction") => shader9_get_function,
        ("IDirect3DResource9" | "IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "SetPriority" | "PreLoad") => |_| Ok(S_OK),
        ("IDirect3DResource9" | "IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetPriority") => |_| Ok(0),
        ("IDirect3DResource9" | "IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetType") => get_resource_type,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "Lock") => buffer9_lock,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "Unlock") => buffer9_unlock,
        ("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", "GetDesc") => buffer9_get_desc,
        _ => return None,
    })
}

fn direct3d_create9(_p: &mut Process) -> R {
    Ok(com::create(_p, "IDirect3D9", com::Kind::Direct3D9))
}

fn check_device(_p: &mut Process) -> R {
    Ok(S_OK)
}

fn check_multisample(p: &mut Process) -> R {
    if p.arg(5) != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let quality_levels = p.arg(6);
    if quality_levels != 0 {
        p.wr32(quality_levels, 1)?;
    }
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
        20 => 87, // 24-bit D3D9 RGB is expanded to BGRA8.
        21 => 87, // A8R8G8B8 -> B8G8R8A8_UNORM
        22 => 88, // X8R8G8B8 -> B8G8R8X8_UNORM
        23 => 85, // R5G6B5 -> B5G6R5_UNORM
        28 => 65, // A8 -> A8_UNORM
        32 => 28, // A8B8G8R8 -> R8G8B8A8_UNORM
        0x3154_5844 => 71, // DXT1 -> BC1
        0x3254_5844 | 0x3354_5844 => 74, // DXT2/3 -> BC2
        0x3454_5844 | 0x3554_5844 => 77, // DXT4/5 -> BC3
        50 => 61, // L8 -> R8_UNORM
        51 => 49, // A8L8 -> R8G8_UNORM
        71 => 40, // D3DFMT_D32 -> DXGI_FORMAT_D32_FLOAT
        75 => 45, // D3DFMT_D24S8 -> DXGI_FORMAT_D24_UNORM_S8_UINT
        77 => 46, // D3DFMT_D24X8 -> DXGI_FORMAT_R24_UNORM_X8_TYPELESS
        80 => 55, // D3DFMT_D16 -> DXGI_FORMAT_D16_UNORM
        _ => 87,
    }
}

fn texture_mip_dim(base: u32, level: u32) -> u32 {
    (base >> level).max(1)
}

fn texture_level_layout(width: u32, height: u32, format: u32) -> (u32, u32, u32, u32, u32) {
    let (block_width, block_height, bytes_per_block) = super::d3d11::format_info(format);
    let row_pitch = width.div_ceil(block_width) * bytes_per_block;
    let rows = height.div_ceil(block_height);
    (width, height, row_pitch, rows, row_pitch.saturating_mul(rows))
}

fn texture_level_offset(info: &com::ResInfo, level: u32) -> u32 {
    (0..level.min(info.mips)).fold(0u32, |offset, mip| {
        let (_, _, _, _, size) = texture_level_layout(
            texture_mip_dim(info.width, mip),
            texture_mip_dim(info.height, mip),
            info.format,
        );
        offset.saturating_add(size)
    })
}

fn create_texture9(p: &mut Process) -> R {
    let (device, width, height, requested_levels, usage, d3d_format, pool, out) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4) as u32,
        p.arg(5) as u32,
        p.arg(6) as u32,
        p.arg(7),
    );
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if width == 0 || height == 0 || !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let format = dxgi_format(d3d_format);
    let levels = if requested_levels == 0 {
        32 - width.max(height).leading_zeros()
    } else {
        requested_levels
    };
    let (block_width, block_height, bytes_per_block) = super::d3d11::format_info(format);
    let mut size = 0u32;
    for level in 0..levels {
        let w = texture_mip_dim(width, level);
        let h = texture_mip_dim(height, level);
        size = size.saturating_add(w.div_ceil(block_width) * h.div_ceil(block_height) * bytes_per_block);
    }
    if size == 0 || size == u32::MAX {
        return Ok(E_OUTOFMEMORY);
    }
    let mut bind = 0x8; // shader resource
    if usage & 0x1 != 0 { bind |= 0x20; } // render target
    if usage & 0x2 != 0 { bind |= 0x40; } // depth stencil
    let id = com::new_res(p, com::ResInfo {
        width,
        height,
        mips: levels,
        array: 1,
        format,
        usage,
        bind,
        size,
        shadow: vec![0; size as usize],
        ..Default::default()
    });
    p.extra.gpu.emit(
        op::CREATE_TEXTURE2D,
        Pk::new().u32(id).u32(width).u32(height).u32(levels).u32(1).u32(format).u32(bind).u32(0).u32(1).u32(0),
    );
    let view_id = p.extra.gpu.new_id();
    p.extra.gpu.emit(
        op::CREATE_VIEW,
        Pk::new().u32(view_id).u32(2).u32(id).u32(format).u32(4).u32(0).u32(levels).u32(0).u32(1),
    );
    p.extra.com.d3d9_texture_views.insert(id, view_id);
    let texture = com::create(p, "IDirect3DTexture9", com::Kind::Direct3DTexture9 {
        id,
        device,
        width,
        height,
        levels,
        format: d3d_format,
        pool,
    });
    p.wrptr(out, texture)?;
    Ok(S_OK)
}

fn d3d9_texture(p: &Process, texture: u64) -> Option<(u32, u64, u32, u32, u32, u32, u32)> {
    match com::kind_of(p, texture)? {
        com::Kind::Direct3DTexture9 { id, device, width, height, levels, format, pool } => {
            Some((id, device, width, height, levels, format, pool))
        }
        _ => None,
    }
}

fn set_texture(p: &mut Process) -> R {
    let (device, stage, texture) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if stage != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let view = if texture == 0 {
        0
    } else {
        let Some((resource, ..)) = d3d9_texture(p, texture) else {
            return Ok(D3DERR_INVALIDCALL);
        };
        let Some(&view) = p.extra.com.d3d9_texture_views.get(&resource) else {
            return Ok(D3DERR_INVALIDCALL);
        };
        view
    };

    let key = (device, stage);
    let previous = p.extra.com.d3d9_bound_textures.get(&key).copied().unwrap_or(0);
    if previous != texture {
        if texture != 0 && com::add_ref_obj(p, texture) == 0 {
            return Ok(D3DERR_INVALIDCALL);
        }
        if texture == 0 {
            p.extra.com.d3d9_bound_textures.remove(&key);
        } else {
            p.extra.com.d3d9_bound_textures.insert(key, texture);
        }
        if previous != 0 {
            com::release_obj(p, previous);
        }
    }
    p.extra.gpu.emit(
        op::SET_SRVS,
        Pk::new().u32(1).u32(stage).u32(1).u32(view),
    );
    Ok(S_OK)
}

fn d3d9_sampler_state(p: &Process, device: u64, stage: u32, state: u32, default: u32) -> u32 {
    p.extra.com.d3d9_sampler_states
        .get(&(device, stage, state))
        .copied()
        .unwrap_or(default)
}

fn set_sampler_state(p: &mut Process) -> R {
    let (device, stage, state, value) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
    );
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if stage != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let valid = match state {
        1..=3 => matches!(value, 1..=3), // wrap, mirror, clamp-to-edge
        5 | 6 => matches!(value, 1..=3), // point, linear, anisotropic
        7 => matches!(value, 0..=2), // none, point, linear
        8 => value == 0, // WebGPU has no sampler LOD bias
        9 => value <= 31, // most detailed mip level
        10 => (1..=16).contains(&value),
        11 => value == 0, // sRGB sampling needs an sRGB texture view
        _ => false,
    };
    if !valid {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let state_key = (device, stage, state);
    if p.extra.com.d3d9_sampler_states.get(&state_key).copied() == Some(value) {
        return Ok(S_OK);
    }
    p.extra.com.d3d9_sampler_states.insert(state_key, value);

    let min_filter = d3d9_sampler_state(p, device, stage, 6, 1);
    let mag_filter = d3d9_sampler_state(p, device, stage, 5, 1);
    let mip_filter = d3d9_sampler_state(p, device, stage, 7, 0);
    let anisotropy = d3d9_sampler_state(p, device, stage, 10, 1);
    let filter = if min_filter == 3 || mag_filter == 3 {
        0x55 // D3D11_FILTER_ANISOTROPIC
    } else {
        u32::from(min_filter == 2) << 4
            | u32::from(mag_filter == 2) << 2
            | u32::from(mip_filter == 2)
    };
    let address_u = d3d9_sampler_state(p, device, stage, 1, 1);
    let address_v = d3d9_sampler_state(p, device, stage, 2, 1);
    let address_w = d3d9_sampler_state(p, device, stage, 3, 1);
    let max_mip = d3d9_sampler_state(p, device, stage, 9, 0);
    let max_lod = if mip_filter == 0 { 0.0f32 } else { 32.0f32 };
    let mut desc = [0u8; 52];
    for (offset, field) in [
        (0, filter),
        (4, address_u),
        (8, address_v),
        (12, address_w),
        (20, anisotropy),
        (24, 8), // D3D11_COMPARISON_ALWAYS
    ] {
        desc[offset..offset + 4].copy_from_slice(&field.to_le_bytes());
    }
    desc[44..48].copy_from_slice(&(max_mip as f32).to_le_bytes());
    desc[48..52].copy_from_slice(&max_lod.to_le_bytes());

    let sampler_id = p.extra.gpu.new_id();
    p.extra.gpu.emit(op::CREATE_SAMPLER, Pk::new().u32(sampler_id).bytes(&desc));
    p.extra.gpu.emit(
        op::SET_SAMPLERS,
        Pk::new().u32(1).u32(stage).u32(1).u32(sampler_id),
    );
    if let Some(previous) = p.extra.com.d3d9_sampler_objects.insert((device, stage), sampler_id) {
        p.extra.gpu.emit(op::RELEASE, Pk::new().u32(previous));
    }
    Ok(S_OK)
}

fn texture_level_count(p: &mut Process) -> R {
    Ok(d3d9_texture(p, p.arg(0)).map(|(_, _, _, _, levels, _, _)| levels as u64).unwrap_or(0))
}

fn texture_get_level_desc(p: &mut Process) -> R {
    let (texture, level, out) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    let Some((id, _, width, height, levels, format, pool)) = d3d9_texture(p, texture) else { return Ok(D3DERR_INVALIDCALL); };
    if out == 0 { return Ok(E_POINTER); }
    if level >= levels { return Ok(D3DERR_INVALIDCALL); }
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    let w = texture_mip_dim(width, level);
    let h = texture_mip_dim(height, level);
    let mut desc = [0u8; 32];
    desc[0..4].copy_from_slice(&format.to_le_bytes());
    desc[4..8].copy_from_slice(&3u32.to_le_bytes()); // D3DRTYPE_TEXTURE
    desc[8..12].copy_from_slice(&info.usage.to_le_bytes());
    desc[12..16].copy_from_slice(&pool.to_le_bytes());
    desc[24..28].copy_from_slice(&w.to_le_bytes());
    desc[28..32].copy_from_slice(&h.to_le_bytes());
    p.write_bytes(out, &desc)?;
    Ok(S_OK)
}

fn texture_get_surface_level(p: &mut Process) -> R {
    let (texture, level, out) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if out == 0 { return Ok(E_POINTER); }
    p.wrptr(out, 0)?;
    let Some((id, device, width, height, levels, format, _)) = d3d9_texture(p, texture) else { return Ok(D3DERR_INVALIDCALL); };
    if level >= levels { return Ok(D3DERR_INVALIDCALL); }
    let surface = com::create(p, "IDirect3DSurface9", com::Kind::Direct3DSurface9 {
        device,
        texture_id: id,
        level,
        rtv: 0,
        width: texture_mip_dim(width, level),
        height: texture_mip_dim(height, level),
        format,
    });
    p.wrptr(out, surface)?;
    Ok(S_OK)
}

fn texture_lock_rect(p: &mut Process) -> R {
    let texture = p.arg(0);
    let Some((id, _, width, height, levels, _, _)) = d3d9_texture(p, texture) else { return Ok(D3DERR_INVALIDCALL); };
    let mip = p.arg(1) as u32;
    let rect_ptr = p.arg(2);
    let out = p.arg(3);
    let flags = p.arg(4) as u32;
    lock_texture_level(p, id, width, height, levels, mip, rect_ptr, out, flags)
}

fn lock_texture_level(
    p: &mut Process,
    id: u32,
    width: u32,
    height: u32,
    levels: u32,
    mip: u32,
    rect_ptr: u64,
    out: u64,
    flags: u32,
) -> R {
    if out == 0 { return Ok(E_POINTER); }
    if mip >= levels { return Ok(D3DERR_INVALIDCALL); }
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    if info.map_addr != 0 { return Ok(D3DERR_INVALIDCALL); }
    let w = texture_mip_dim(width, mip);
    let h = texture_mip_dim(height, mip);
    let (bw, bh, bb) = super::d3d11::format_info(info.format);
    let (x, y) = if rect_ptr == 0 {
        (0, 0)
    } else {
        let left = p.rd32(rect_ptr)? as i32;
        let top = p.rd32(rect_ptr + 4)? as i32;
        let right = p.rd32(rect_ptr + 8)? as i32;
        let bottom = p.rd32(rect_ptr + 12)? as i32;
        if left < 0 || top < 0 || right <= left || bottom <= top || right as u32 > w || bottom as u32 > h
            || left as u32 % bw != 0 || top as u32 % bh != 0
            || (right as u32 != w && right as u32 % bw != 0)
            || (bottom as u32 != h && bottom as u32 % bh != 0)
        {
            return Ok(D3DERR_INVALIDCALL);
        }
        (left as u32, top as u32)
    };
    let (_, _, pitch, _, size) = texture_level_layout(w, h, info.format);
    if size == 0 { return Ok(D3DERR_INVALIDCALL); }
    let base = super::kernel32::alloc_block(p, size as u64, true);
    if base == 0 { return Ok(E_OUTOFMEMORY); }
    let offset = texture_level_offset(&info, mip);
    if flags & 0x2000 == 0 && !info.shadow.is_empty() {
        let start = offset as usize;
        let end = start.saturating_add(size as usize).min(info.shadow.len());
        if end > start { p.write_bytes(base, &info.shadow[start..end])?; }
    }
    if let Some(resource) = p.extra.com.res.get_mut(&id) {
        resource.map_addr = base;
        resource.map_write = flags & 0x10 == 0;
        resource.map_sub = mip;
    }
    let byte_offset = (y / bh) * pitch + (x / bw) * bb;
    p.wr32(out, pitch)?;
    p.wr32(out + 4, (base + byte_offset as u64) as u32)?;
    Ok(S_OK)
}

fn texture_unlock_rect(p: &mut Process) -> R {
    let texture = p.arg(0);
    let Some((id, _, width, height, levels, _, _)) = d3d9_texture(p, texture) else { return Ok(D3DERR_INVALIDCALL); };
    unlock_texture_level(p, id, width, height, levels)
}

fn unlock_texture_level(p: &mut Process, id: u32, width: u32, height: u32, levels: u32) -> R {
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    if info.map_addr == 0 || info.map_sub >= levels { return Ok(D3DERR_INVALIDCALL); }
    let mip = info.map_sub;
    let w = texture_mip_dim(width, mip);
    let h = texture_mip_dim(height, mip);
    let (_, _, pitch, _, size) = texture_level_layout(w, h, info.format);
    if info.map_write {
        let bytes = p.read_bytes(info.map_addr, size as usize)?;
        let offset = texture_level_offset(&info, mip) as usize;
        if let Some(resource) = p.extra.com.res.get_mut(&id) {
            if resource.shadow.len() >= offset.saturating_add(bytes.len()) {
                resource.shadow[offset..offset + bytes.len()].copy_from_slice(&bytes);
            }
        }
        p.extra.gpu.emit(
            op::TEXTURE_WRITE,
            Pk::new().u32(id).u32(mip).u32(0).u32(0).u32(0).u32(w).u32(h).u32(pitch).u32(size).bytes(&bytes),
        );
    }
    super::kernel32::free_block(p, info.map_addr);
    if let Some(resource) = p.extra.com.res.get_mut(&id) {
        resource.map_addr = 0;
        resource.map_write = false;
    }
    Ok(S_OK)
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
    let multisample = p.rd32(params + 16)?;
    let windowed = p.rd32(params + 32)? != 0;
    let auto_depth = p.rd32(params + 36)? != 0;
    let auto_depth_format = p.rd32(params + 40)?;
    let hwnd = if p.rd32(params + 28)? != 0 { p.rd32(params + 28)? as u64 } else { focus };
    let fallback = if p.extra.win.main_window != 0 { p.extra.win.main_window } else {
        p.extra.win.windows.iter().find(|(_, w)| w.visible).map(|(h, _)| *h).unwrap_or(0)
    };
    let hwnd = if hwnd == 0 { fallback } else { hwnd };
    let (window_width, window_height) = p.extra.win.windows.get(&hwnd)
        .map(|w| (w.client_w.max(1) as u32, w.client_h.max(1) as u32))
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
    if auto_depth {
        if multisample != 0 {
            return Ok(D3DERR_NOTAVAILABLE);
        }
        let surface = create_depth_surface(p, device, width, height, auto_depth_format)?;
        if surface == 0 {
            return Ok(D3DERR_NOTAVAILABLE);
        }
        p.extra.com.d3d9_depth_surfaces.insert(device, surface);
        bind_depth_surface(p, device, surface)?;
    }
    if !windowed {
        super::user32::set_fullscreen_window(p, hwnd, width, height)?;
    }
    if p.trace_imports {
        p.stderr.extend_from_slice(format!("[d3d9] CreateDevice hwnd={hwnd:#x} size={width}x{height} auto_depth={auto_depth} depth_format={auto_depth_format} swapchain={sc_id} device={device:#x}\n").as_bytes());
    }
    Ok(S_OK)
}

fn format_to_d3d(format: u32) -> u32 {
    match format {
        87 => 21,
        88 => 22,
        85 => 23,
        28 => 32,
        40 => 71,
        45 => 75,
        46 => 77,
        55 => 80,
        _ => 21,
    }
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
        level: 0,
        rtv,
        width,
        height,
        format,
    }))
}

fn create_depth_surface(
    p: &mut Process,
    device: u64,
    width: u32,
    height: u32,
    format: u32,
) -> Result<u64, Stop> {
    let dxgi = match format {
        71 => 40, // D3DFMT_D32
        75 => 45, // D3DFMT_D24S8
        77 => 46, // D3DFMT_D24X8
        80 => 55, // D3DFMT_D16
        _ => return Ok(0),
    };
    let texture_id = com::new_res(
        p,
        com::ResInfo {
            width,
            height,
            mips: 1,
            array: 1,
            format: dxgi,
            samples: 1,
            bind: 0x40, // D3D11_BIND_DEPTH_STENCIL
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_TEXTURE2D,
        Pk::new()
            .u32(texture_id)
            .u32(width)
            .u32(height)
            .u32(1)
            .u32(1)
            .u32(dxgi)
            .u32(0x40)
            .u32(0)
            .u32(1)
            .u32(0),
    );
    let view_id = p.extra.gpu.new_id();
    p.extra.gpu.emit(
        op::CREATE_VIEW,
        Pk::new()
            .u32(view_id)
            .u32(1) // DSV
            .u32(texture_id)
            .u32(dxgi)
            .u32(4) // TEXTURE2D
            .u32(0)
            .u32(1)
            .u32(0)
            .u32(1),
    );
    let view = com::create(
        p,
        "ID3D11DepthStencilView",
        com::Kind::View {
            vk: com::ViewKind::Dsv,
            id: view_id,
            resource: texture_id,
        },
    );
    Ok(com::create(
        p,
        "IDirect3DSurface9",
        com::Kind::Direct3DSurface9 {
            device,
            texture_id,
            level: 0,
            rtv: view,
            width,
            height,
            format,
        },
    ))
}

fn create_depth_stencil_surface(p: &mut Process) -> R {
    let (device, width, height, format, multisample, out) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4),
        p.arg(7),
    );
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if width == 0 || height == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    if multisample != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let surface = create_depth_surface(p, device, width, height, format)?;
    if surface == 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    p.wrptr(out, surface)?;
    Ok(S_OK)
}

fn create_buffer9(p: &mut Process, vertex: bool) -> R {
    let (device, length, usage, format_or_fvf, pool, out) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4) as u32,
        p.arg(5),
    );
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if length == 0 || length > 64 * 1024 * 1024 {
        return Ok(D3DERR_INVALIDCALL);
    }
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if !vertex && !matches!(format_or_fvf, 101 | 102) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let bind = if vertex { 1 } else { 2 };
    let stride = if vertex { fvf_stride(format_or_fvf).unwrap_or(0) } else { 0 };
    let id = com::new_res(
        p,
        com::ResInfo {
            is_buffer: true,
            size: length,
            format: if vertex { 0 } else { format_or_fvf },
            usage,
            bind,
            cpu: pool,
            misc: if vertex { format_or_fvf } else { 0 },
            stride,
            shadow: vec![0; length as usize],
            ..Default::default()
        },
    );
    p.extra.gpu.emit(
        op::CREATE_BUFFER,
        Pk::new().u32(id).u32(length).u32(bind).u32(0).u32(0).u32(stride),
    );
    let (iface, kind) = if vertex {
        (
            "IDirect3DVertexBuffer9",
            com::Kind::Direct3DVertexBuffer9 { id, device },
        )
    } else {
        (
            "IDirect3DIndexBuffer9",
            com::Kind::Direct3DIndexBuffer9 { id, device },
        )
    };
    let buffer = com::create(p, iface, kind);
    p.wrptr(out, buffer)?;
    Ok(S_OK)
}

fn create_shader9(p: &mut Process, vertex: bool) -> R {
    let (device, function, out) = (p.arg(0), p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let Some(function) = read_shader9_function(p, function, vertex) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let iface = if vertex {
        "IDirect3DVertexShader9"
    } else {
        "IDirect3DPixelShader9"
    };
    let shader = com::create(
        p,
        iface,
        com::Kind::Direct3DShader9 {
            device,
            vertex,
            function,
        },
    );
    p.wrptr(out, shader)?;
    Ok(S_OK)
}

fn read_shader9_function(p: &Process, function: u64, vertex: bool) -> Option<Vec<u8>> {
    if function == 0 {
        return None;
    }
    const MAX_SHADER_WORDS: usize = 65_536;
    let version = p.rd32(function).ok()?;
    let version_type = if vertex { 0xfffe } else { 0xffff };
    if version >> 16 != version_type {
        return None;
    }
    let major = (version >> 8) & 0xff;
    let mut bytes = version.to_le_bytes().to_vec();
    let mut cursor = function.checked_add(4)?;
    for _ in 1..MAX_SHADER_WORDS {
        let token = p.rd32(cursor).ok()?;
        bytes.extend_from_slice(&token.to_le_bytes());
        cursor = cursor.checked_add(4)?;
        let opcode = token & 0xffff;
        if opcode == 0xffff {
            return Some(bytes);
        }
        if opcode == 0xfffe {
            let comment_words = ((token >> 16) & 0x7fff) as usize;
            for _ in 0..comment_words {
                let word = p.rd32(cursor).ok()?;
                bytes.extend_from_slice(&word.to_le_bytes());
                cursor = cursor.checked_add(4)?;
            }
        } else if major >= 2 {
            let operand_words = ((token >> 24) & 0xf) as usize;
            for _ in 0..operand_words {
                let word = p.rd32(cursor).ok()?;
                bytes.extend_from_slice(&word.to_le_bytes());
                cursor = cursor.checked_add(4)?;
            }
        }
    }
    None
}

fn set_shader9(p: &mut Process, vertex: bool) -> R {
    let (device, shader) = (p.arg(0), p.arg(1));
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if shader != 0
        && !matches!(
            com::kind_of(p, shader),
            Some(com::Kind::Direct3DShader9 { vertex: is_vertex, .. }) if is_vertex == vertex
        )
    {
        return Ok(D3DERR_INVALIDCALL);
    }
    if vertex {
        p.extra.com.d3d9_vertex_shaders.insert(device, shader);
    } else {
        p.extra.com.d3d9_pixel_shaders.insert(device, shader);
    }
    Ok(S_OK)
}

fn get_shader9(p: &mut Process, vertex: bool) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let shader = if vertex {
        p.extra.com.d3d9_vertex_shaders.get(&device).copied().unwrap_or(0)
    } else {
        p.extra.com.d3d9_pixel_shaders.get(&device).copied().unwrap_or(0)
    };
    if shader != 0 {
        if let Some(object) = p.extra.com.objs.get_mut(&shader) {
            object.refs = object.refs.saturating_add(1);
        }
    }
    p.wrptr(out, shader)?;
    Ok(S_OK)
}

fn shader9_get_device(p: &mut Process) -> R {
    let (shader, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let Some(com::Kind::Direct3DShader9 { device, .. }) = com::kind_of(p, shader) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if let Some(object) = p.extra.com.objs.get_mut(&device) {
        object.refs = object.refs.saturating_add(1);
    }
    p.wrptr(out, device)?;
    Ok(S_OK)
}

fn shader9_get_function(p: &mut Process) -> R {
    let (shader, out_data, out_size) = (p.arg(0), p.arg(1), p.arg(2));
    if out_size == 0 {
        return Ok(E_POINTER);
    }
    let Some(com::Kind::Direct3DShader9 { function, .. }) = com::kind_of(p, shader) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let required = function.len() as u32;
    let available = p.rd32(out_size)?;
    if out_data == 0 {
        p.wr32(out_size, required)?;
        return Ok(S_OK);
    }
    if available < required {
        p.wr32(out_size, required)?;
        return Ok(D3DERR_MOREDATA);
    }
    p.write_bytes(out_data, &function)?;
    p.wr32(out_size, required)?;
    Ok(S_OK)
}

fn d3d9_buffer_id(p: &Process, buffer: u64) -> Option<u32> {
    match com::kind_of(p, buffer)? {
        com::Kind::Direct3DVertexBuffer9 { id, .. }
        | com::Kind::Direct3DIndexBuffer9 { id, .. } => Some(id),
        _ => None,
    }
}

fn buffer9_lock(p: &mut Process) -> R {
    let (buffer, offset, requested, out, flags) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4) as u32,
    );
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    let Some(id) = d3d9_buffer_id(p, buffer) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    let length = if requested == 0 {
        info.size.saturating_sub(offset)
    } else {
        requested
    };
    if info.map_addr != 0
        || offset > info.size
        || length == 0
        || offset.checked_add(length).is_none_or(|end| end > info.size)
    {
        return Ok(D3DERR_INVALIDCALL);
    }
    let addr = super::kernel32::alloc_block(p, info.size.max(16) as u64, true);
    if flags & 0x2000 == 0 && !info.shadow.is_empty() {
        p.write_bytes(addr, &info.shadow)?;
    }
    if let Some(resource) = p.extra.com.res.get_mut(&id) {
        resource.map_addr = addr;
        resource.map_write = flags & 0x10 == 0; // D3DLOCK_READONLY
        resource.map_sub = offset;
    }
    p.wrptr(out, addr + offset as u64)?;
    Ok(S_OK)
}

fn buffer9_unlock(p: &mut Process) -> R {
    let buffer = p.arg(0);
    let Some(id) = d3d9_buffer_id(p, buffer) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    if info.map_addr == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    if info.map_write {
        let bytes = p.read_bytes(info.map_addr, info.size as usize)?;
        if let Some(resource) = p.extra.com.res.get_mut(&id) {
            resource.shadow = bytes.clone();
        }
        p.extra.gpu.emit(
            op::BUFFER_WRITE,
            Pk::new().u32(id).u32(0).u32(info.size).bytes(&bytes),
        );
    }
    super::kernel32::free_block(p, info.map_addr);
    if let Some(resource) = p.extra.com.res.get_mut(&id) {
        resource.map_addr = 0;
        resource.map_write = false;
    }
    Ok(S_OK)
}

fn resource_get_device(p: &mut Process) -> R {
    let (resource, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let device = match com::kind_of(p, resource) {
        Some(com::Kind::Direct3DVertexBuffer9 { device, .. })
        | Some(com::Kind::Direct3DIndexBuffer9 { device, .. })
        | Some(com::Kind::Direct3DSurface9 { device, .. }) => device,
        _ => return Ok(D3DERR_INVALIDCALL),
    };
    if let Some(object) = p.extra.com.objs.get_mut(&device) {
        object.refs += 1;
    }
    p.wrptr(out, device)?;
    Ok(S_OK)
}

fn get_resource_type(p: &mut Process) -> R {
    Ok(match com::kind_of(p, p.arg(0)) {
        Some(com::Kind::Direct3DVertexBuffer9 { .. }) => 6,
        Some(com::Kind::Direct3DIndexBuffer9 { .. }) => 7,
        Some(com::Kind::Direct3DTexture9 { .. }) => 3,
        _ => return Ok(D3DERR_INVALIDCALL),
    })
}

fn buffer9_get_desc(p: &mut Process) -> R {
    let (buffer, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let Some(id) = d3d9_buffer_id(p, buffer) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let info = p.extra.com.res.get(&id).cloned().unwrap_or_default();
    let is_vertex = matches!(
        com::kind_of(p, buffer),
        Some(com::Kind::Direct3DVertexBuffer9 { .. })
    );
    let mut desc = [0u8; 28];
    let format = if is_vertex { 100 } else { info.format }; // VERTEXDATA for vertex buffers
    desc[0..4].copy_from_slice(&format.to_le_bytes());
    desc[4..8].copy_from_slice(&(if is_vertex { 6u32 } else { 7u32 }).to_le_bytes());
    desc[8..12].copy_from_slice(&info.usage.to_le_bytes());
    desc[12..16].copy_from_slice(&info.cpu.to_le_bytes());
    desc[16..20].copy_from_slice(&info.size.to_le_bytes());
    if is_vertex {
        desc[20..24].copy_from_slice(&info.misc.to_le_bytes());
        desc[24..28].copy_from_slice(&info.stride.to_le_bytes());
    }
    p.write_bytes(out, &desc)?;
    Ok(S_OK)
}

fn set_stream_source(p: &mut Process) -> R {
    let (device, stream, buffer, offset, stride) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as u32,
        p.arg(4) as u32,
    );
    if stream != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let id = if buffer == 0 {
        0
    } else {
        let Some(com::Kind::Direct3DVertexBuffer9 { id, .. }) = com::kind_of(p, buffer) else {
            return Ok(D3DERR_INVALIDCALL);
        };
        id
    };
    p.extra.com.d3d9_stream0.insert(device, (buffer, stride, offset));
    p.extra.gpu.emit(
        op::SET_VERTEX_BUFFERS,
        Pk::new().u32(0).u32(1).u32(id).u32(stride).u32(offset),
    );
    Ok(S_OK)
}

fn get_stream_source(p: &mut Process) -> R {
    let (device, stream, out_buffer, out_offset, out_stride) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3),
        p.arg(4),
    );
    if stream != 0 {
        if out_buffer != 0 { p.wrptr(out_buffer, 0)?; }
        return Ok(D3DERR_INVALIDCALL);
    }
    let (buffer, stride, offset) = p.extra.com.d3d9_stream0.get(&device).copied().unwrap_or((0, 0, 0));
    if out_buffer != 0 {
        if buffer != 0 {
            if let Some(object) = p.extra.com.objs.get_mut(&buffer) { object.refs += 1; }
        }
        p.wrptr(out_buffer, buffer)?;
    }
    if out_offset != 0 { p.wr32(out_offset, offset)?; }
    if out_stride != 0 { p.wr32(out_stride, stride)?; }
    Ok(S_OK)
}

fn set_indices(p: &mut Process) -> R {
    let (device, buffer) = (p.arg(0), p.arg(1));
    let (id, format) = if buffer == 0 {
        (0, 57)
    } else {
        let Some(com::Kind::Direct3DIndexBuffer9 { id, .. }) = com::kind_of(p, buffer) else {
            return Ok(D3DERR_INVALIDCALL);
        };
        let d3d_format = p.extra.com.res.get(&id).map(|info| info.format).unwrap_or(101);
        let format = match d3d_format {
            101 => 57, // R16_UINT
            102 => 42, // R32_UINT
            _ => return Ok(D3DERR_INVALIDCALL),
        };
        (id, format)
    };
    p.extra.com.d3d9_indices.insert(device, buffer);
    p.extra.gpu.emit(op::SET_INDEX_BUFFER, Pk::new().u32(id).u32(format).u32(0));
    Ok(S_OK)
}

fn get_indices(p: &mut Process) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let buffer = p.extra.com.d3d9_indices.get(&device).copied().unwrap_or(0);
    if buffer != 0 {
        if let Some(object) = p.extra.com.objs.get_mut(&buffer) { object.refs += 1; }
    }
    p.wrptr(out, buffer)?;
    Ok(S_OK)
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

fn depth_view_id(p: &Process, surface: u64) -> Option<u32> {
    let com::Kind::Direct3DSurface9 { rtv, .. } = com::kind_of(p, surface)? else {
        return None;
    };
    match com::kind_of(p, rtv)? {
        com::Kind::View {
            vk: com::ViewKind::Dsv,
            id,
            ..
        } => Some(id),
        _ => None,
    }
}

fn bind_depth_surface(p: &mut Process, device: u64, surface: u64) -> Result<(), Stop> {
    if surface == 0 {
        p.extra.com.d3d9_depth_surfaces.remove(&device);
    } else {
        if depth_view_id(p, surface).is_none() {
            return Ok(());
        }
        p.extra.com.d3d9_depth_surfaces.insert(device, surface);
    }
    let rtv_id = current_render_target(p, device)
        .and_then(|rtv| match com::kind_of(p, rtv) {
            Some(com::Kind::View { id, .. }) => Some(id),
            _ => None,
        })
        .unwrap_or(0);
    let dsv_id = p
        .extra
        .com
        .d3d9_depth_surfaces
        .get(&device)
        .and_then(|surface| depth_view_id(p, *surface))
        .unwrap_or(0);
    p.extra
        .gpu
        .emit(op::SET_RENDER_TARGETS, Pk::new().u32(1).u32(rtv_id).u32(dsv_id));
    Ok(())
}

fn get_depth_stencil_surface(p: &mut Process) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let surface = p
        .extra
        .com
        .d3d9_depth_surfaces
        .get(&device)
        .copied()
        .unwrap_or(0);
    if surface == 0 {
        p.wrptr(out, 0)?;
        return Ok(D3DERR_NOTFOUND);
    }
    if let Some(object) = p.extra.com.objs.get_mut(&surface) {
        object.refs += 1;
    }
    p.wrptr(out, surface)?;
    Ok(S_OK)
}

fn set_depth_stencil_surface(p: &mut Process) -> R {
    let (device, surface) = (p.arg(0), p.arg(1));
    if !matches!(
        com::kind_of(p, device),
        Some(com::Kind::Direct3DDevice9 { .. })
    ) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if surface != 0 && depth_view_id(p, surface).is_none() {
        return Ok(D3DERR_INVALIDCALL);
    }
    bind_depth_surface(p, device, surface)?;
    Ok(S_OK)
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
    let dsv_id = p
        .extra
        .com
        .d3d9_depth_surfaces
        .get(&device)
        .and_then(|surface| depth_view_id(p, *surface))
        .unwrap_or(0);
    p.extra.gpu.emit(op::SET_RENDER_TARGETS, Pk::new().u32(1).u32(id).u32(dsv_id));
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
    // D3D9's default presentation interval waits for one vertical blank.
    p.extra
        .gpu
        .emit(op::PRESENT, Pk::new().u32(sc_id).u32(tex_id).u32(1));
    p.yield_request = Some(Stop::Frame);
    Ok(S_OK)
}

fn clear(p: &mut Process) -> R {
    let (device, flags, color, depth, stencil) = (
        p.arg(0),
        p.arg(3) as u32,
        p.arg(4) as u32,
        f32::from_bits(p.arg(5) as u32),
        p.arg(6) as u32,
    );
    let Some(rtv) = current_render_target(p, device) else { return Ok(D3DERR_INVALIDCALL); };
    if flags & 1 != 0 {
        let id = match com::kind_of(p, rtv) { Some(com::Kind::View { id, .. }) => id, _ => 0 };
        let [a, r, g, b] = color.to_be_bytes();
        p.extra.gpu.emit(op::CLEAR_RTV, Pk::new().u32(id).f32(r as f32 / 255.0).f32(g as f32 / 255.0).f32(b as f32 / 255.0).f32(a as f32 / 255.0));
    }
    if flags & (2 | 4) != 0 {
        let dsv = p
            .extra
            .com
            .d3d9_depth_surfaces
            .get(&device)
            .copied()
            .and_then(|surface| depth_view_id(p, surface));
        if let Some(id) = dsv {
            let clear_flags = ((flags & 2 != 0) as u32) | (((flags & 4 != 0) as u32) << 1);
            p.extra.gpu.emit(op::CLEAR_DSV, Pk::new().u32(id).u32(clear_flags).f32(depth).u32(stencil));
        }
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

fn set_render_state(p: &mut Process) -> R {
    let (device, state, value) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u32);
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    p.extra.com.d3d9_render_states.insert((device, state), value);
    p.extra.gpu.emit(op::D3D9_RENDER_STATE, Pk::new().u32(state).u32(value));
    if state == 60 {
        let fvf = p.extra.com.objs.get(&device).and_then(|object| match object.kind {
            com::Kind::Direct3DDevice9 { fvf, .. } => Some(fvf),
            _ => None,
        }).unwrap_or(0);
        if fvf_stride(fvf).is_some() {
            apply_fvf(p, device, fvf)?;
        }
    }
    Ok(S_OK)
}

fn get_render_state(p: &mut Process) -> R {
    let (device, state, out) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let default = match state {
        7 | 14 | 137 | 141 => 1, // ZENABLE, ZWRITEENABLE, LIGHTING, COLORVERTEX
        22 => 3,                 // CULL_CCW
        _ => 0,
    };
    let value = p
        .extra
        .com
        .d3d9_render_states
        .get(&(device, state))
        .copied()
        .unwrap_or(default);
    p.wr32(out, value)?;
    Ok(S_OK)
}

fn set_fvf(p: &mut Process) -> R {
    let (device, fvf) = (p.arg(0), p.arg(1) as u32);
    if fvf_stride(fvf).is_none() {
        return Ok(D3DERR_INVALIDCALL);
    }
    apply_fvf(p, device, fvf)
}

fn apply_fvf(p: &mut Process, device: u64, fvf: u32) -> R {
    let Some((_, _, _, _, width, height, _)) = device_state(p, device) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if let Some(o) = p.extra.com.objs.get_mut(&device) {
        if let com::Kind::Direct3DDevice9 { fvf: current, .. } = &mut o.kind { *current = fvf; }
    }
    let stage_state = |state, default| {
        p.extra.com.d3d9_texture_stage_states
            .get(&(device, 0, state))
            .copied()
            .unwrap_or(default)
    };
    p.extra.gpu.emit(
        op::SET_FIXED_FUNCTION,
        Pk::new()
            .u32(fvf)
            .u32(width)
            .u32(height)
            .u32(stage_state(1, 4)) // D3DTSS_COLOROP = D3DTOP_MODULATE
            .u32(stage_state(2, 2)) // D3DTSS_COLORARG1 = D3DTA_TEXTURE
            .u32(stage_state(3, 1)) // D3DTSS_COLORARG2 = D3DTA_CURRENT
            .u32(stage_state(4, 2)) // D3DTSS_ALPHAOP = D3DTOP_SELECTARG1
            .u32(stage_state(5, 2)) // D3DTSS_ALPHAARG1 = D3DTA_TEXTURE
            .u32(stage_state(6, 1)) // D3DTSS_ALPHAARG2 = D3DTA_CURRENT
            .u32(p.extra.com.d3d9_render_states.get(&(device, 60)).copied().unwrap_or(u32::MAX)),
    );
    Ok(S_OK)
}

fn set_texture_stage_state(p: &mut Process) -> R {
    let (device, stage, state, value) = (
        p.arg(0),
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
    );
    if !matches!(com::kind_of(p, device), Some(com::Kind::Direct3DDevice9 { .. })) {
        return Ok(D3DERR_INVALIDCALL);
    }
    if stage != 0 {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let valid = match state {
        1 => matches!(value, 1..=16 | 18..=21 | 24), // selected add, subtract, blend, modulate-add, and dot-product ops
        2 | 3 | 5 | 6 => value <= 2, // diffuse/current or texture
        4 => matches!(value, 2..=16), // selected, arithmetic, and blend alpha ops
        _ => false,
    };
    if !valid {
        return Ok(D3DERR_NOTAVAILABLE);
    }
    let key = (device, stage, state);
    if p.extra.com.d3d9_texture_stage_states.get(&key).copied() == Some(value) {
        return Ok(S_OK);
    }
    p.extra.com.d3d9_texture_stage_states.insert(key, value);
    let fvf = p.extra.com.objs.get(&device).and_then(|object| match object.kind {
        com::Kind::Direct3DDevice9 { fvf, .. } => Some(fvf),
        _ => None,
    }).unwrap_or(0);
    if fvf_stride(fvf).is_some() {
        apply_fvf(p, device, fvf)?;
    }
    Ok(S_OK)
}

fn declaration_fvf(p: &Process, address: u64) -> Result<Option<(u32, Vec<u8>)>, Stop> {
    if address == 0 {
        return Ok(None);
    }
    let mut elements = Vec::new();
    let (mut position, mut normal, mut color, mut texcoord) = (0u32, false, false, false);
    let mut terminated = false;
    for index in 0..64u64 {
        let bytes = p.read_bytes(address + index * 8, 8)?;
        let stream = u16::from_le_bytes([bytes[0], bytes[1]]);
        let offset = u16::from_le_bytes([bytes[2], bytes[3]]) as u32;
        let kind = bytes[4];
        let method = bytes[5];
        let usage = bytes[6];
        let usage_index = bytes[7];
        elements.extend_from_slice(&bytes);
        if stream == 0x00ff && kind == 17 {
            terminated = true;
            break;
        }
        if stream != 0 || method != 0 {
            return Ok(None);
        }
        let element_size = match kind {
            0 | 5 | 6 | 8 | 9 | 11 | 13 | 14 | 15 => 4,
            1 | 7 | 10 | 12 | 16 => 8,
            2 => 12,
            3 => 16,
            4 => 4,
            _ => return Ok(None),
        };
        match usage {
            0 if kind == 2 => position = 0x02, // POSITION / FLOAT3 => XYZ
            9 if kind == 3 => position = 0x04, // POSITIONT / FLOAT4 => XYZRHW
            3 if kind == 2 && usage_index == 0 => normal = true, // NORMAL / FLOAT3
            10 if kind == 4 && usage_index == 0 => color = true,
            5 if kind == 1 && usage_index == 0 => texcoord = true,
            _ => return Ok(None),
        }
        let _end = offset.saturating_add(element_size);
    }
    if !terminated || position == 0 || !color {
        return Ok(None);
    }
    let fvf = position
        | if normal { 0x10 } else { 0 }
        | 0x40
        | if texcoord { 0x100 } else { 0 };
    if fvf_stride(fvf).is_none() {
        return Ok(None);
    }
    Ok(Some((fvf, elements)))
}

fn create_vertex_declaration(p: &mut Process) -> R {
    let (device, elements, out) = (p.arg(0), p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    let Some((fvf, elements)) = declaration_fvf(p, elements)? else {
        if p.trace_modules_only {
            let bytes = p.read_bytes(elements, 64).unwrap_or_default();
            let bytes = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ");
            p.stderr.extend_from_slice(
                format!("[d3d9-declaration-rejected] elements={elements:#x} bytes=[{bytes}]\n").as_bytes(),
            );
        }
        return Ok(D3DERR_INVALIDCALL);
    };
    let declaration = com::create(
        p,
        "IDirect3DVertexDeclaration9",
        com::Kind::Direct3DVertexDeclaration9 {
            device,
            fvf,
            elements,
        },
    );
    p.wrptr(out, declaration)?;
    Ok(S_OK)
}

fn set_vertex_declaration(p: &mut Process) -> R {
    let (device, declaration) = (p.arg(0), p.arg(1));
    let Some(com::Kind::Direct3DDevice9 { .. }) = com::kind_of(p, device) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if declaration == 0 {
        p.extra.com.d3d9_vertex_declarations.remove(&device);
        return apply_fvf(p, device, 0);
    }
    let (declaration, fvf) = match com::kind_of(p, declaration) {
        Some(com::Kind::Direct3DVertexDeclaration9 { fvf, .. }) => (declaration, fvf),
        _ => {
            let Some((fvf, elements)) = declaration_fvf(p, declaration)? else {
                return Ok(D3DERR_INVALIDCALL);
            };
            let declaration = com::create(
                p,
                "IDirect3DVertexDeclaration9",
                com::Kind::Direct3DVertexDeclaration9 {
                    device,
                    fvf,
                    elements,
                },
            );
            (declaration, fvf)
        }
    };
    p.extra.com.d3d9_vertex_declarations.insert(device, declaration);
    apply_fvf(p, device, fvf)
}

fn get_vertex_declaration(p: &mut Process) -> R {
    let (device, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let declaration = p
        .extra
        .com
        .d3d9_vertex_declarations
        .get(&device)
        .copied()
        .unwrap_or(0);
    if declaration != 0 {
        if let Some(object) = p.extra.com.objs.get_mut(&declaration) {
            object.refs += 1;
        }
    }
    p.wrptr(out, declaration)?;
    Ok(S_OK)
}

fn declaration_get_device(p: &mut Process) -> R {
    let (declaration, out) = (p.arg(0), p.arg(1));
    if out == 0 {
        return Ok(E_POINTER);
    }
    let Some(com::Kind::Direct3DVertexDeclaration9 { device, .. }) =
        com::kind_of(p, declaration)
    else {
        return Ok(D3DERR_INVALIDCALL);
    };
    if let Some(object) = p.extra.com.objs.get_mut(&device) {
        object.refs += 1;
    }
    p.wrptr(out, device)?;
    Ok(S_OK)
}

fn declaration_get_elements(p: &mut Process) -> R {
    let (declaration, elements_out, count_out) = (p.arg(0), p.arg(1), p.arg(2));
    if count_out == 0 {
        return Ok(E_POINTER);
    }
    let Some(com::Kind::Direct3DVertexDeclaration9 { elements, .. }) =
        com::kind_of(p, declaration)
    else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let count = (elements.len() / 8) as u32;
    if elements_out != 0 {
        let capacity = p.rd32(count_out)? as usize;
        let bytes = (capacity.min(count as usize)).saturating_mul(8);
        p.write_bytes(elements_out, &elements[..bytes])?;
    }
    p.wr32(count_out, count)?;
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
        0x142 => Some(24), // XYZ + D3DCOLOR + TEX1
        0x144 => Some(28), // XYZRHW + D3DCOLOR + TEX1
        0x152 => Some(36), // XYZ + NORMAL + D3DCOLOR + TEX1
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

fn surface_lock_rect(p: &mut Process) -> R {
    let (surface, out, rect, flags) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as u32);
    let Some(com::Kind::Direct3DSurface9 { texture_id, level, .. }) = com::kind_of(p, surface) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let info = p.extra.com.res.get(&texture_id).cloned().unwrap_or_default();
    if texture_id == 0 || info.width == 0 || info.height == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    lock_texture_level(p, texture_id, info.width, info.height, info.mips.max(1), level, rect, out, flags)
}

fn surface_unlock_rect(p: &mut Process) -> R {
    let surface = p.arg(0);
    let Some(com::Kind::Direct3DSurface9 { texture_id, .. }) = com::kind_of(p, surface) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let info = p.extra.com.res.get(&texture_id).cloned().unwrap_or_default();
    if texture_id == 0 || info.width == 0 || info.height == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    unlock_texture_level(p, texture_id, info.width, info.height, info.mips.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
    use anywasm_cpu::CpuMode;
    use crate::vfs::Vfs;

    #[test]
    fn create_texture_returns_a_guest_texture_and_emits_a_webgpu_resource() {
        const STACK: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        for address in [STACK, OUT] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
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
        for (offset, value) in [
            (4, device as u32),
            (8, 512),
            (12, 512),
            (16, 1),
            (20, 0),
            (24, 0x3154_5844), // D3DFMT_DXT1
            (28, 1), // D3DPOOL_MANAGED
            (32, OUT as u32),
            (36, 0), // shared handle
        ] {
            p.wr32(STACK + offset, value).unwrap();
        }

        let create_texture = method("IDirect3DDevice9", "CreateTexture").unwrap();
        assert_eq!(create_texture(&mut p).unwrap(), S_OK);
        assert_ne!(p.rd32(OUT).unwrap(), 0);
        let commands = gpu::decode(&p.extra.gpu.buf);
        assert!(commands.iter().any(|(opcode, _)| *opcode == op::CREATE_TEXTURE2D));
    }

    #[test]
    fn set_texture_binds_the_d3d9_texture_srv_to_the_pixel_shader_stage() {
        const STACK: u64 = 0x10000;
        const OUT: u64 = 0x11000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        for address in [STACK, OUT] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
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
                width: 64,
                height: 64,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;
        for (offset, value) in [
            (4, device as u32),
            (8, 1),
            (12, 1),
            (16, 1),
            (20, 0),
            (24, 21), // D3DFMT_A8R8G8B8
            (28, 1), // D3DPOOL_MANAGED
            (32, OUT as u32),
            (36, 0), // shared handle
        ] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "CreateTexture").unwrap()(&mut p).unwrap(), S_OK);
        let texture = p.rd32(OUT).unwrap() as u64;
        let resource = match com::kind_of(&p, texture).unwrap() {
            com::Kind::Direct3DTexture9 { id, .. } => id,
            other => panic!("expected Direct3DTexture9, got {other:?}"),
        };
        let view = p.extra.com.d3d9_texture_views[&resource];

        for (offset, value) in [(4, device as u32), (8, 0), (12, texture as u32)] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "SetTexture").unwrap()(&mut p).unwrap(), S_OK);
        let commands = gpu::decode(&p.extra.gpu.buf);
        let (_, payload) = commands.iter().find(|(opcode, _)| *opcode == op::SET_SRVS)
            .expect("SetTexture should bind a pixel-shader SRV");
        assert_eq!(
            payload,
            &[
                1u32.to_le_bytes(),
                0u32.to_le_bytes(),
                1u32.to_le_bytes(),
                view.to_le_bytes(),
            ]
            .concat(),
        );
    }

    #[test]
    fn set_sampler_state_rebuilds_the_d3d9_sampler_with_linear_minification() {
        const STACK: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
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
                width: 64,
                height: 64,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;
        for (offset, value) in [(4, device as u32), (8, 0), (12, 6), (16, 2)] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "SetSamplerState").unwrap()(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (_, sampler) = commands.iter().find(|(opcode, _)| *opcode == op::CREATE_SAMPLER)
            .expect("SetSamplerState should create the translated sampler");
        assert_eq!(u32::from_le_bytes(sampler[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(sampler[4..8].try_into().unwrap()), 0x10); // linear min, point mag/mip
        assert_eq!(u32::from_le_bytes(sampler[8..12].try_into().unwrap()), 1); // wrap U
        assert_eq!(u32::from_le_bytes(sampler[12..16].try_into().unwrap()), 1); // wrap V
        assert_eq!(u32::from_le_bytes(sampler[24..28].try_into().unwrap()), 1); // max anisotropy
        let (_, binding) = commands.iter().find(|(opcode, _)| *opcode == op::SET_SAMPLERS)
            .expect("SetSamplerState should bind the translated sampler");
        assert_eq!(
            binding,
            &[
                1u32.to_le_bytes(),
                0u32.to_le_bytes(),
                1u32.to_le_bytes(),
                1u32.to_le_bytes(),
            ]
            .concat(),
        );
    }

    #[test]
    fn set_texture_stage_state_reemits_stage_zero_fixed_function_state() {
        const STACK: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
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
                width: 64,
                height: 64,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;
        for (offset, value) in [(4, device as u32), (8, 0x144)] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "SetFVF").unwrap()(&mut p).unwrap(), S_OK);
        let set_texture_stage_state = method("IDirect3DDevice9", "SetTextureStageState").unwrap();
        for color_op in [8, 9, 11, 12, 13, 14, 15, 16, 18, 19, 20, 21, 24] {
            for (offset, value) in [(4, device as u32), (8, 0), (12, 1), (16, color_op)] {
                p.wr32(STACK + offset, value).unwrap();
            }
            assert_eq!(set_texture_stage_state(&mut p).unwrap(), S_OK, "color op {color_op}");

            let commands = gpu::decode(&p.extra.gpu.buf);
            let (_, state) = commands.iter().rev().find(|(opcode, _)| *opcode == op::SET_FIXED_FUNCTION)
                .expect("SetTextureStageState should update the fixed-function shader state");
            let words = state.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect::<Vec<_>>();
            assert_eq!(words, [0x144, 64, 64, color_op, 2, 1, 2, 2, 1, u32::MAX]);
        }
        for alpha_op in [5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16] {
            for (offset, value) in [(4, device as u32), (8, 0), (12, 4), (16, alpha_op)] {
                p.wr32(STACK + offset, value).unwrap();
            }
            assert_eq!(set_texture_stage_state(&mut p).unwrap(), S_OK, "alpha op {alpha_op}");

            let commands = gpu::decode(&p.extra.gpu.buf);
            let (_, state) = commands.iter().rev().find(|(opcode, _)| *opcode == op::SET_FIXED_FUNCTION)
                .expect("SetTextureStageState should update the fixed-function shader state");
            let words = state.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect::<Vec<_>>();
            assert_eq!(words, [0x144, 64, 64, 24, 2, 1, alpha_op, 2, 1, u32::MAX]);
        }
    }

    #[test]
    fn set_render_state_emits_the_state_for_the_webgpu_executor() {
        const STACK: u64 = 0x10000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
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
                width: 64,
                height: 64,
                fvf: 0,
            },
        );
        p.cpu.gpr[RSP] = STACK;
        for (offset, value) in [(4, device as u32), (8, 7), (12, 0)] {
            p.wr32(STACK + offset, value).unwrap();
        }

        assert_eq!(method("IDirect3DDevice9", "SetRenderState").unwrap()(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let (opcode, payload) = commands.last().expect("SetRenderState should reach the renderer");
        assert_eq!(*opcode, 48);
        assert_eq!(payload, &[7u32.to_le_bytes(), 0u32.to_le_bytes()].concat());

        for (offset, value) in [(4, device as u32), (8, 0x144)] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "SetFVF").unwrap()(&mut p).unwrap(), S_OK);
        let texture_factor = 0x40ab_cdef;
        for (offset, value) in [(4, device as u32), (8, 60), (12, texture_factor)] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "SetRenderState").unwrap()(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        assert_eq!(commands[commands.len() - 2].0, op::D3D9_RENDER_STATE);
        assert_eq!(commands[commands.len() - 2].1, [60u32.to_le_bytes(), texture_factor.to_le_bytes()].concat());
        assert_eq!(commands.last().unwrap().0, op::SET_FIXED_FUNCTION);
        let state = &commands.last().unwrap().1;
        let words = state.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect::<Vec<_>>();
        assert_eq!(words, [0x144, 64, 64, 4, 2, 1, 2, 2, 1, texture_factor]);
    }

    #[test]
    fn surface_lock_rect_uploads_a_dxt1_texture_level() {
        const STACK: u64 = 0x10000;
        const TEX_OUT: u64 = 0x11000;
        const SURFACE_OUT: u64 = 0x12000;
        const LOCKED_RECT: u64 = 0x13000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        for address in [STACK, TEX_OUT, SURFACE_OUT, LOCKED_RECT] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
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
        for (offset, value) in [
            (4, device as u32), (8, 512), (12, 512), (16, 1), (20, 0),
            (24, 0x3154_5844), (28, 1), (32, TEX_OUT as u32), (36, 0),
        ] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DDevice9", "CreateTexture").unwrap()(&mut p).unwrap(), S_OK);
        let texture = p.rd32(TEX_OUT).unwrap();
        let Some(com::Kind::Direct3DTexture9 { id, .. }) = com::kind_of(&p, texture as u64) else { panic!("expected D3D9 texture"); };
        assert_eq!(p.extra.com.res[&id].format, 71);
        p.wr32(STACK + 4, texture).unwrap();
        p.wr32(STACK + 8, 0).unwrap();
        p.wr32(STACK + 12, SURFACE_OUT as u32).unwrap();
        assert_eq!(method("IDirect3DTexture9", "GetSurfaceLevel").unwrap()(&mut p).unwrap(), S_OK);
        let surface = p.rd32(SURFACE_OUT).unwrap();

        for (offset, value) in [
            (4, surface), (8, LOCKED_RECT as u32), (12, 0), (16, 0x800),
        ] {
            p.wr32(STACK + offset, value).unwrap();
        }
        assert_eq!(method("IDirect3DSurface9", "LockRect").unwrap()(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd32(LOCKED_RECT).unwrap(), 1024);
        let pixels = p.rd32(LOCKED_RECT + 4).unwrap() as u64;
        let block = [1, 2, 3, 4, 5, 6, 7, 8];
        p.write_bytes(pixels, &block).unwrap();
        p.wr32(STACK + 4, surface).unwrap();
        assert_eq!(method("IDirect3DSurface9", "UnlockRect").unwrap()(&mut p).unwrap(), S_OK);

        let commands = gpu::decode(&p.extra.gpu.buf);
        let upload = commands.iter().find(|(opcode, _)| *opcode == op::TEXTURE_WRITE).unwrap();
        assert_eq!(u32::from_le_bytes(upload.1[28..32].try_into().unwrap()), 1024);
        assert_eq!(&upload.1[36..44], &block);
    }

    #[test]
    fn create_vertex_declaration_accepts_xyz_normal_diffuse_texcoord_layout() {
        const STACK: u64 = 0x10000;
        const ELEMENTS: u64 = 0x11000;
        const OUT: u64 = 0x12000;
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        for address in [STACK, ELEMENTS, OUT] {
            assert!(p.mem.map(address, PAGE_SIZE, PROT_RW));
        }
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
                width: 800,
                height: 600,
                fvf: 0,
            },
        );
        let declaration = [
            0, 0, 0, 0, 2, 0, 0, 0,       // POSITION: FLOAT3 at byte 0
            0, 0, 12, 0, 2, 0, 3, 0,      // NORMAL: FLOAT3 at byte 12
            0, 0, 24, 0, 4, 0, 10, 0,     // COLOR0: D3DCOLOR at byte 24
            0, 0, 28, 0, 1, 0, 5, 0,      // TEXCOORD0: FLOAT2 at byte 28
            0xff, 0, 0, 0, 17, 0, 0, 0,   // D3DDECL_END
        ];
        p.write_bytes(ELEMENTS, &declaration).unwrap();
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK + 4, device as u32).unwrap();
        p.wr32(STACK + 8, ELEMENTS as u32).unwrap();
        p.wr32(STACK + 12, OUT as u32).unwrap();

        assert_eq!(create_vertex_declaration(&mut p).unwrap(), S_OK);
        let declaration = p.rd32(OUT).unwrap() as u64;
        let Some(com::Kind::Direct3DVertexDeclaration9 { fvf, .. }) = com::kind_of(&p, declaration) else {
            panic!("CreateVertexDeclaration did not return a D3D9 declaration");
        };
        assert_eq!(fvf, 0x152); // XYZ | NORMAL | DIFFUSE | TEX1
        assert_eq!(fvf_stride(fvf), Some(36));
    }

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
            vec![0x44, 640, 480, 4, 2, 1, 2, 2, 1, u32::MAX]
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
