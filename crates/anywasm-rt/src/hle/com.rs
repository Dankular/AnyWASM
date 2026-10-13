//! COM object model for host-implemented interfaces (DXGI / Direct3D 11).
//!
//! A guest COM object is 16 bytes of guest heap whose first qword points at a vtable. Each vtable
//! slot holds the address of a host thunk named `"<Interface>::<Method>"` (slot order generated from
//! the mingw-w64 headers, see `vtables.rs`). Host state lives in [`ComState`], keyed by the object's
//! guest address.

use super::gpu::{op, Pk};
use super::*;
use crate::process::*;
use anywasm_cpu::CpuMode;
use std::collections::HashMap;

pub const S_OK: u64 = 0;
pub const S_FALSE: u64 = 1;
pub const E_NOTIMPL: u64 = 0x8000_4001;
pub const E_NOINTERFACE: u64 = 0x8000_4002;
pub const E_FAIL: u64 = 0x8000_4005;
pub const E_INVALIDARG: u64 = 0x8007_0057;
pub const E_OUTOFMEMORY: u64 = 0x8007_000e;
pub const DXGI_ERROR_INVALID_CALL: u64 = 0x887a_0001;
pub const DXGI_ERROR_NOT_FOUND: u64 = 0x887a_0002;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Rtv,
    Dsv,
    Srv,
    Uav,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Factory,
    Adapter,
    Output,
    Device,
    Context,
    CommandList {
        commands: Vec<u8>,
        flags: u32,
    },
    SwapChain {
        id: u32,
        backbuffer: u64,
    },
    Buffer {
        id: u32,
    },
    Texture1D {
        id: u32,
    },
    Texture2D {
        id: u32,
    },
    Texture3D {
        id: u32,
    },
    View {
        vk: ViewKind,
        id: u32,
        resource: u32,
    },
    Shader {
        id: u32,
        stage: u32,
    },
    InputLayout {
        id: u32,
    },
    State {
        id: u32,
    },
    Query {
        query_type: u32,
        complete: bool,
    },
    DirectInput8,
    DirectInputDevice {
        device_type: u32,
        acquired: bool,
        data_size: u32,
    },
    Direct3D9,
    Direct3DDevice9 {
        d3d9: u64,
        swapchain: u64,
        backbuffer: u64,
        rtv: u64,
        current_rtv: u64,
        window: u64,
        width: u32,
        height: u32,
        fvf: u32,
    },
    Direct3DSurface9 {
        device: u64,
        texture_id: u32,
        level: u32,
        rtv: u64,
        width: u32,
        height: u32,
        format: u32,
    },
    Direct3DVertexBuffer9 {
        id: u32,
        device: u64,
    },
    Direct3DIndexBuffer9 {
        id: u32,
        device: u64,
    },
    Direct3DTexture9 {
        id: u32,
        device: u64,
        width: u32,
        height: u32,
        levels: u32,
        format: u32,
        pool: u32,
    },
    Direct3DVertexDeclaration9 {
        device: u64,
        fvf: u32,
        elements: Vec<u8>,
    },
    Direct3DShader9 {
        device: u64,
        vertex: bool,
        function: Vec<u8>,
    },
    DirectDraw,
    DirectSound,
    SoundBuffer {
        memory: u64,
        size: u32,
        caps: u32,
    },
}

impl Kind {
    /// Host id carried by the object, if it owns GPU-side state.
    pub fn host_id(&self) -> Option<u32> {
        match self {
            Kind::SwapChain { id, .. }
            | Kind::Buffer { id }
            | Kind::Direct3DVertexBuffer9 { id, .. }
            | Kind::Direct3DIndexBuffer9 { id, .. }
            | Kind::Direct3DTexture9 { id, .. }
            | Kind::Texture1D { id }
            | Kind::Texture2D { id }
            | Kind::Texture3D { id }
            | Kind::View { id, .. }
            | Kind::Shader { id, .. }
            | Kind::InputLayout { id }
            | Kind::State { id } => Some(*id),
            _ => None,
        }
    }
}

pub struct ComObj {
    pub iface: &'static str,
    pub kind: Kind,
    pub refs: u32,
    pub block: u64,
}

/// CPU-visible description of a buffer or texture.
#[derive(Clone, Default)]
pub struct ResInfo {
    pub is_buffer: bool,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub is_1d: bool,
    pub is_3d: bool,
    pub mips: u32,
    pub array: u32,
    pub format: u32,
    pub samples: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
    pub size: u32,
    pub stride: u32,
    /// Last known CPU-side contents (buffers; textures with CPU access).
    pub shadow: Vec<u8>,
    /// Guest scratch memory handed out by Map, and the write intent to flush on Unmap.
    pub map_addr: u64,
    pub map_write: bool,
    pub map_sub: u32,
}

#[derive(Default)]
pub struct ComState {
    pub objs: HashMap<u64, ComObj>,
    vtables: HashMap<&'static str, u64>,
    pub res: HashMap<u32, ResInfo>,
    id_refs: HashMap<u32, u32>,
    pub device: u64,
    pub dxgi_device: u64,
    pub context: u64,
    /// Recorded state and commands owned by each deferred D3D11 context.
    pub deferred_contexts: HashMap<u64, DeferredContextState>,
    pub factory: u64,
    pub adapter: u64,
    pub output: u64,
    /// Always-ready wait objects exposed by swap chains for frame-latency pacing.
    pub frame_latency_handles: HashMap<u32, u64>,
    /// Currently bound render targets (guest COM pointers) for `OMGetRenderTargets`.
    pub bound_rtv: Vec<u64>,
    pub bound_dsv: u64,
    /// Current Direct3D 9 depth-stencil surface per device object.
    pub d3d9_depth_surfaces: HashMap<u64, u64>,
    /// Current Direct3D 9 viewport per device, stored as the six D3DVIEWPORT9 words.
    pub d3d9_viewports: HashMap<u64, [u32; 6]>,
    /// Current Direct3D 9 scissor rectangle per device, stored as four RECT words.
    pub d3d9_scissor_rects: HashMap<u64, [u32; 4]>,
    /// Direct3D 9 stream zero binding `(buffer, stride, offset)` per device.
    pub d3d9_stream0: HashMap<u64, (u64, u32, u32)>,
    /// Direct3D 9 index buffer binding per device.
    pub d3d9_indices: HashMap<u64, u64>,
    /// Current Direct3D 9 vertex declaration per device.
    pub d3d9_vertex_declarations: HashMap<u64, u64>,
    /// Current Direct3D 9 vertex and pixel shader objects per device.
    pub d3d9_vertex_shaders: HashMap<u64, u64>,
    pub d3d9_pixel_shaders: HashMap<u64, u64>,
    /// Shader-resource views lazily associated with D3D9 textures.
    pub d3d9_texture_views: HashMap<u32, u32>,
    /// Textures retained by D3D9 device stage bindings.
    pub d3d9_bound_textures: HashMap<(u64, u32), u64>,
    /// D3D9 sampler-state values keyed by `(device, sampler, state)`.
    pub d3d9_sampler_states: HashMap<(u64, u32, u32), u32>,
    /// Current translated sampler object keyed by `(device, sampler)`.
    pub d3d9_sampler_objects: HashMap<(u64, u32), u32>,
    /// D3D9 stage-0 texture-combine state keyed by `(device, stage, state)`.
    pub d3d9_texture_stage_states: HashMap<(u64, u32, u32), u32>,
    /// Direct3D 9 render-state values set by the guest, keyed by `(device, state)`.
    pub d3d9_render_states: HashMap<(u64, u32), u32>,
}

#[derive(Default)]
pub struct DeferredContextState {
    pub flags: u32,
    pub commands: Vec<u8>,
    /// State-setting commands retained across FinishCommandList(TRUE).
    pub state_commands: Vec<u8>,
    pub bound_rtv: Vec<u64>,
    pub bound_dsv: u64,
}

/// Canonical interface family used to share method implementations across versions
/// (`ID3D11Device3` -> `ID3D11Device`, `IDXGIFactory2` -> `IDXGIFactory`, ...).
pub fn canonical(iface: &str) -> &str {
    for fam in [
        "ID3D11DeviceContext",
        "ID3D11Device",
        "IDXGIFactory",
        "IDXGIAdapter",
        "IDXGISwapChain",
        "IDXGIDevice",
        "IDXGIOutput",
    ] {
        if let Some(rest) = iface.strip_prefix(fam) {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                return fam;
            }
        }
    }
    iface
}

/// Create a guest COM object of interface `iface` backed by host state `kind`.
pub fn create(p: &mut Process, iface: &'static str, kind: Kind) -> u64 {
    let vt = vtable_for(p, iface);
    let block = kernel32::alloc_block(p, 32, true);
    let _ = p.wrptr(block, vt);
    if let Some(id) = kind.host_id() {
        *p.extra.com.id_refs.entry(id).or_insert(0) += 1;
    }
    p.extra.com.objs.insert(
        block,
        ComObj {
            iface,
            kind,
            refs: 1,
            block,
        },
    );
    block
}

fn vtable_for(p: &mut Process, iface: &'static str) -> u64 {
    if let Some(&v) = p.extra.com.vtables.get(iface) {
        return v;
    }
    let methods = vtables::methods(iface)
        .or_else(|| dsound::methods(iface))
        .or_else(|| directinput8::methods(iface))
        .or_else(|| d3d9::methods(iface))
        .unwrap_or(&["QueryInterface", "AddRef", "Release"]);
    let pointer_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let vt = kernel32::alloc_block(p, methods.len() as u64 * pointer_size + pointer_size, true);
    for (i, m) in methods.iter().enumerate() {
        let a = p.thunk_for("com", &format!("{iface}::{m}"), true).unwrap();
        let _ = p.wrptr(vt + pointer_size * i as u64, a);
    }
    p.extra.com.vtables.insert(iface, vt);
    vt
}

pub fn kind_of(p: &Process, this: u64) -> Option<Kind> {
    p.extra.com.objs.get(&this).map(|o| o.kind.clone())
}

/// Write an interface pointer to `*out` (if non-null).
pub fn out_ptr(p: &mut Process, out: u64, obj: u64) -> Result<(), Stop> {
    if out != 0 {
        p.wrptr(out, obj)?;
    }
    Ok(())
}

fn guid_string(p: &Process, a: u64) -> Result<String, Stop> {
    let b = p.read_bytes(a, 16)?;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes(b[0..4].try_into().unwrap()),
        u16::from_le_bytes(b[4..6].try_into().unwrap()),
        u16::from_le_bytes(b[6..8].try_into().unwrap()),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    ))
}

/// Whether an object of `kind` may be queried as interface `want`.
fn supports(kind: &Kind, want: &str) -> bool {
    let w = canonical(want);
    if matches!(w, "IUnknown") {
        return true;
    }
    let fam: &[&str] = match kind {
        Kind::DirectInput8 => &["IDirectInput8A", "IDirectInput8W"],
        Kind::DirectInputDevice { .. } => &["IDirectInputDevice8A", "IDirectInputDevice8W"],
        Kind::Direct3D9 => &["IDirect3D9"],
        Kind::Direct3DDevice9 { .. } => &["IDirect3DDevice9"],
        Kind::Direct3DSurface9 { .. } => &["IDirect3DSurface9"],
        Kind::Direct3DVertexBuffer9 { .. } => &["IDirect3DVertexBuffer9", "IDirect3DResource9"],
        Kind::Direct3DIndexBuffer9 { .. } => &["IDirect3DIndexBuffer9", "IDirect3DResource9"],
        Kind::Direct3DTexture9 { .. } => &[
            "IDirect3DTexture9",
            "IDirect3DBaseTexture9",
            "IDirect3DResource9",
        ],
        Kind::Direct3DVertexDeclaration9 { .. } => &["IDirect3DVertexDeclaration9"],
        Kind::Direct3DShader9 { vertex: true, .. } => &["IDirect3DVertexShader9"],
        Kind::Direct3DShader9 { vertex: false, .. } => &["IDirect3DPixelShader9"],
        Kind::Device => &["ID3D11Device", "IDXGIDevice", "IDXGIObject"],
        Kind::Context => &["ID3D11DeviceContext", "ID3D11DeviceChild"],
        Kind::CommandList { .. } => &["ID3D11CommandList", "ID3D11DeviceChild"],
        Kind::SwapChain { .. } => &["IDXGISwapChain", "IDXGIObject", "IDXGIDeviceSubObject"],
        Kind::Factory => &["IDXGIFactory", "IDXGIObject"],
        Kind::Adapter => &["IDXGIAdapter", "IDXGIObject"],
        Kind::Output => &["IDXGIOutput", "IDXGIObject"],
        Kind::Buffer { .. } => &["ID3D11Buffer", "ID3D11Resource", "ID3D11DeviceChild"],
        Kind::Texture1D { .. } => &[
            "ID3D11Texture1D",
            "ID3D11Resource",
            "ID3D11DeviceChild",
            "IDXGIObject",
            "IDXGIResource",
        ],
        Kind::Texture2D { .. } => &[
            "ID3D11Texture2D",
            "ID3D11Resource",
            "ID3D11DeviceChild",
            "IDXGISurface",
            "IDXGIObject",
            "IDXGIResource",
        ],
        Kind::Texture3D { .. } => &[
            "ID3D11Texture3D",
            "ID3D11Texture3D1",
            "ID3D11Resource",
            "ID3D11DeviceChild",
            "IDXGIObject",
            "IDXGIResource",
        ],
        Kind::View { vk, .. } => match vk {
            ViewKind::Rtv => &["ID3D11RenderTargetView", "ID3D11View", "ID3D11DeviceChild"],
            ViewKind::Dsv => &["ID3D11DepthStencilView", "ID3D11View", "ID3D11DeviceChild"],
            ViewKind::Srv => &[
                "ID3D11ShaderResourceView",
                "ID3D11View",
                "ID3D11DeviceChild",
            ],
            ViewKind::Uav => &[
                "ID3D11UnorderedAccessView",
                "ID3D11View",
                "ID3D11DeviceChild",
            ],
        },
        Kind::Shader { stage, .. } => match stage {
            0 => &["ID3D11VertexShader", "ID3D11DeviceChild"],
            1 => &["ID3D11PixelShader", "ID3D11DeviceChild"],
            _ => &["ID3D11ComputeShader", "ID3D11DeviceChild"],
        },
        Kind::InputLayout { .. } => &["ID3D11InputLayout", "ID3D11DeviceChild"],
        Kind::State { .. } => &[
            "ID3D11SamplerState",
            "ID3D11BlendState",
            "ID3D11RasterizerState",
            "ID3D11DepthStencilState",
            "ID3D11DeviceChild",
        ],
        Kind::Query { .. } => &["ID3D11Query", "ID3D11DeviceChild"],
        Kind::DirectDraw => &["IDirectDraw7"],
        Kind::DirectSound => &["IDirectSound8"],
        Kind::SoundBuffer { caps, .. } => {
            if caps & (dsound::DSBCAPS_PRIMARYBUFFER | dsound::DSBCAPS_CTRL3D)
                == (dsound::DSBCAPS_PRIMARYBUFFER | dsound::DSBCAPS_CTRL3D)
            {
                &["IDirectSoundBuffer8", "IDirectSound3DListener"]
            } else {
                &["IDirectSoundBuffer8"]
            }
        }
    };
    fam.iter().any(|f| *f == w)
}

fn static_iface(name: &str) -> Option<&'static str> {
    // Interned lookup: only names we have vtables for are valid interfaces.
    if vtables::methods(name).is_none()
        && directinput8::methods(name).is_none()
        && d3d9::methods(name).is_none()
        && dsound::methods(name).is_none()
    {
        return None;
    }
    for (k, _) in ALL_NAMES.iter().map(|n| (*n, ())) {
        if k == name {
            return Some(k);
        }
    }
    None
}

/// Every interface name present in the generated tables (for interning &'static str).
const ALL_NAMES: &[&str] = &[
    "IUnknown",
    "IDXGIObject",
    "IDXGIDeviceSubObject",
    "IDXGIFactory",
    "IDXGIFactory1",
    "IDXGIFactory2",
    "IDXGIFactory3",
    "IDXGIFactory4",
    "IDXGIAdapter",
    "IDXGIAdapter1",
    "IDXGIAdapter2",
    "IDXGIOutput",
    "IDXGISwapChain",
    "IDXGISwapChain1",
    "IDXGISwapChain2",
    "IDXGISwapChain3",
    "IDXGIDevice",
    "IDXGIDevice1",
    "IDXGIDevice2",
    "IDXGISurface",
    "IDXGIResource",
    "ID3D11Device",
    "ID3D11Device1",
    "ID3D11Device2",
    "ID3D11Device3",
    "ID3D11DeviceContext",
    "ID3D11DeviceContext1",
    "ID3D11DeviceContext2",
    "ID3D11DeviceContext3",
    "ID3D11CommandList",
    "IDirectInput8A",
    "IDirectInput8W",
    "IDirectInputDevice8A",
    "IDirectInputDevice8W",
    "IDirect3D9",
    "IDirect3DDevice9",
    "IDirect3DSurface9",
    "IDirect3DResource9",
    "IDirect3DVertexBuffer9",
    "IDirect3DIndexBuffer9",
    "IDirect3DBaseTexture9",
    "IDirect3DTexture9",
    "IDirect3DVertexDeclaration9",
    "IDirect3DVertexShader9",
    "IDirect3DPixelShader9",
    "ID3D11DeviceChild",
    "ID3D11Resource",
    "ID3D11Buffer",
    "ID3D11Texture2D",
    "ID3D11View",
    "ID3D11RenderTargetView",
    "ID3D11DepthStencilView",
    "ID3D11ShaderResourceView",
    "ID3D11UnorderedAccessView",
    "ID3D11VertexShader",
    "ID3D11PixelShader",
    "ID3D11ComputeShader",
    "ID3D11InputLayout",
    "ID3D11SamplerState",
    "ID3D11BlendState",
    "ID3D11RasterizerState",
    "ID3D11DepthStencilState",
    "ID3D11Query",
    "ID3D10Blob",
    "ID3DBlob",
    "IDirectDraw7",
    "IDirectSound8",
    "IDirectSoundBuffer8",
    "IDirectSound3DListener",
];

pub fn query_interface(p: &mut Process) -> R {
    let (this, riid, out) = (p.arg(0), p.arg(1), p.arg(2));
    let Some(obj) = p.extra.com.objs.get(&this) else {
        return Ok(E_NOINTERFACE);
    };
    let (kind, own) = (obj.kind.clone(), obj.iface);
    let iid = guid_string(p, riid)?;
    let Some(name) = vtables::iid_name(&iid)
        .or_else(|| directinput8::iid_name(&iid))
        .or_else(|| d3d9::iid_name(&iid))
        .or_else(|| dsound::iid_name(&iid))
    else {
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[com] QueryInterface this={this:#x} iface={} iid={iid} -> unknown\n",
                    obj.iface
                )
                .as_bytes(),
            );
        }
        p.wrptr(out, 0)?;
        return Ok(E_NOINTERFACE);
    };
    if !supports(&kind, name) {
        if p.trace_imports {
            p.stderr.extend_from_slice(format!("[com] QueryInterface this={this:#x} iface={} iid={iid} want={name} -> unsupported\n", obj.iface).as_bytes());
        }
        p.wrptr(out, 0)?;
        return Ok(E_NOINTERFACE);
    }
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[com] QueryInterface this={this:#x} iface={} iid={iid} want={name} -> supported\n",
                obj.iface
            )
            .as_bytes(),
        );
    }
    if name == "IUnknown" || name == own {
        p.wrptr(out, this)?;
        add_ref_obj(p, this);
        return Ok(S_OK);
    }
    if canonical(name) == canonical(own) {
        let Some(iface) = static_iface(name) else {
            p.wrptr(out, 0)?;
            return Ok(E_NOINTERFACE);
        };
        // Interface versions share a prefix-compatible vtable. If the guest asks for a newer
        // version, upgrade the wrapper's vtable before returning the same COM identity so its
        // newly added methods do not dispatch through zero-filled slots.
        if interface_version(iface) > interface_version(own) {
            let vt = vtable_for(p, iface);
            p.wrptr(this, vt)?;
            if let Some(obj) = p.extra.com.objs.get_mut(&this) {
                obj.iface = iface;
            }
        }
        p.wrptr(out, this)?;
        add_ref_obj(p, this);
        return Ok(S_OK);
    }
    // A different interface of the same object: make a sibling wrapper sharing the host state.
    let Some(iface) = static_iface(name) else {
        p.wrptr(out, 0)?;
        return Ok(E_NOINTERFACE);
    };
    // Keep versioned D3D11 interfaces on their shared vtable, but use a separate vtable for
    // IDXGIDevice: its method slots do not match ID3D11Device.
    let singleton = match kind {
        Kind::Device if canonical(name) == "IDXGIDevice" => Some(p.extra.com.dxgi_device),
        Kind::Device => Some(p.extra.com.device),
        Kind::Context => Some(p.extra.com.context),
        _ => None,
    };
    if let Some(s) = singleton.filter(|&s| s != 0) {
        // Same pointer works for any version of the interface because vtables are prefix-compatible.
        p.wrptr(out, s)?;
        add_ref_obj(p, s);
        return Ok(S_OK);
    }
    let o = create(p, iface, kind);
    p.wrptr(out, o)?;
    Ok(S_OK)
}

fn interface_version(iface: &str) -> u32 {
    let family = canonical(iface);
    iface[family.len()..].parse().unwrap_or(0)
}

pub fn add_ref_obj(p: &mut Process, this: u64) -> u64 {
    match p.extra.com.objs.get_mut(&this) {
        Some(o) => {
            o.refs += 1;
            o.refs as u64
        }
        None => 0,
    }
}

pub fn add_ref(p: &mut Process) -> R {
    let this = p.arg(0);
    Ok(add_ref_obj(p, this))
}

pub fn release(p: &mut Process) -> R {
    let this = p.arg(0);
    Ok(release_obj(p, this))
}

pub fn release_obj(p: &mut Process, this: u64) -> u64 {
    let Some(o) = p.extra.com.objs.get_mut(&this) else {
        return 0;
    };
    if o.refs > 0 {
        o.refs -= 1;
    }
    let refs = o.refs;
    if refs == 0 {
        let o = p.extra.com.objs.remove(&this).unwrap();
        p.extra.com.deferred_contexts.remove(&this);
        if matches!(&o.kind, Kind::Direct3DDevice9 { .. }) {
            p.extra.com.d3d9_viewports.remove(&this);
            p.extra.com.d3d9_scissor_rects.remove(&this);
            let bound = p
                .extra
                .com
                .d3d9_bound_textures
                .iter()
                .filter_map(|(&(device, stage), &texture)| {
                    (device == this).then_some((stage, texture))
                })
                .collect::<Vec<_>>();
            for (stage, texture) in bound {
                p.extra.com.d3d9_bound_textures.remove(&(this, stage));
                p.extra
                    .gpu
                    .emit(op::SET_SRVS, Pk::new().u32(1).u32(stage).u32(1).u32(0));
                release_obj(p, texture);
            }
            let samplers = p
                .extra
                .com
                .d3d9_sampler_objects
                .iter()
                .filter_map(|(&(device, stage), &sampler)| {
                    (device == this).then_some((stage, sampler))
                })
                .collect::<Vec<_>>();
            for (stage, sampler) in samplers {
                p.extra.com.d3d9_sampler_objects.remove(&(this, stage));
                p.extra.gpu.emit(op::RELEASE, Pk::new().u32(sampler));
            }
            p.extra
                .com
                .d3d9_sampler_states
                .retain(|(device, _, _), _| *device != this);
            p.extra
                .com
                .d3d9_texture_stage_states
                .retain(|(device, _, _), _| *device != this);
        }
        if let Some(id) = o.kind.host_id() {
            let left = p.extra.com.id_refs.get_mut(&id).map(|c| {
                *c = c.saturating_sub(1);
                *c
            });
            if left == Some(0) {
                p.extra.com.id_refs.remove(&id);
                if let Some(handle) = p.extra.com.frame_latency_handles.remove(&id) {
                    p.handles.remove(&handle);
                }
                p.extra.com.d3d9_texture_views.remove(&id);
                // The backbuffer texture belongs to its swap chain; the host releases it with it.
                p.extra.com.res.remove(&id);
                p.extra.gpu.emit(op::RELEASE, Pk::new().u32(id));
            }
        }
        if let Kind::SoundBuffer { memory, .. } = o.kind {
            let shared = p.extra.com.objs.values().any(|other| {
                matches!(other.kind, Kind::SoundBuffer { memory: other_memory, .. } if other_memory == memory)
            });
            if !shared {
                kernel32::free_block(p, memory);
            }
        }
        kernel32::free_block(p, o.block);
    }
    refs as u64
}

pub fn get_device(p: &mut Process) -> R {
    let (_this, out) = (p.arg(0), p.arg(1));
    let d = p.extra.com.device;
    if d != 0 {
        add_ref_obj(p, d);
    }
    out_ptr(p, out, d)?;
    Ok(0)
}

pub fn lookup(name: &str) -> Option<HleFn> {
    let (iface, m) = name.split_once("::")?;
    Some(match m {
        "QueryInterface" => query_interface,
        "AddRef" => add_ref,
        "Release" => release,
        "GetDevice"
            if matches!(
                iface,
                "IDirect3DSurface9"
                    | "IDirect3DResource9"
                    | "IDirect3DVertexBuffer9"
                    | "IDirect3DIndexBuffer9"
                    | "IDirect3DBaseTexture9"
                    | "IDirect3DTexture9"
                    | "IDirect3DVertexDeclaration9"
                    | "IDirect3DVertexShader9"
                    | "IDirect3DPixelShader9"
            ) =>
        {
            return d3d9::method(iface, m)
        }
        "GetDevice" => get_device,
        "GetPrivateData" => |_| Ok(DXGI_ERROR_NOT_FOUND),
        "SetPrivateData" | "SetPrivateDataInterface" | "SetEvictionPriority" => |_| Ok(S_OK),
        "GetEvictionPriority" => |_| Ok(0x8000_0000),
        _ => {
            return d3d9::method(iface, m)
                .or_else(|| d3d11::method(iface, m))
                .or_else(|| dxgi::method(iface, m))
                .or_else(|| ddraw::method(iface, m))
                .or_else(|| dsound::method(iface, m))
                .or_else(|| directinput8::method(iface, m))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vfs;
    use anywasm_cpu::cpu::{R8, RCX, RDX};
    use anywasm_cpu::mem::PROT_RW;

    #[test]
    fn query_interface_upgrades_versioned_vtable_for_new_methods() {
        const GUID: u64 = 0x10000;
        const OUT: u64 = GUID + 0x20;
        let mut p = Process::new(Vfs::new());
        let object = create(
            &mut p,
            "IDXGISwapChain1",
            Kind::SwapChain {
                id: 1,
                backbuffer: 0,
            },
        );
        let old_vtable = p.rd64(object).unwrap();
        assert!(p.mem.map(GUID, 0x1000, PROT_RW));
        p.write_bytes(
            GUID,
            &[
                0xc4, 0x2a, 0xbe, 0xa8, 0x9f, 0x19, 0x46, 0x49, 0xb3, 0x31, 0x79, 0x59, 0x9f, 0xb9,
                0x8d, 0xe7,
            ],
        )
        .unwrap(); // IID_IDXGISwapChain2
        p.cpu.gpr[RCX] = object;
        p.cpu.gpr[RDX] = GUID;
        p.cpu.gpr[R8] = OUT;

        assert_eq!(query_interface(&mut p).unwrap(), S_OK);
        assert_eq!(p.rd64(OUT).unwrap(), object);
        assert_eq!(p.extra.com.objs[&object].iface, "IDXGISwapChain2");
        let new_vtable = p.rd64(object).unwrap();
        assert_ne!(new_vtable, old_vtable);
        for slot in 0..vtables::methods("IDXGISwapChain2").unwrap().len() {
            assert_ne!(
                p.rd64(new_vtable + 8 * slot as u64).unwrap(),
                0,
                "vtable slot {slot}"
            );
        }
    }
}

/// Register a new resource in the CPU-side table and return its host id.
pub fn new_res(p: &mut Process, info: ResInfo) -> u32 {
    let id = p.extra.gpu.new_id();
    p.extra.com.res.insert(id, info);
    id
}
