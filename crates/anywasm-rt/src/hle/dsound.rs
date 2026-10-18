//! DirectSound 8 compatibility used to keep legacy Win32 startup moving.

use super::com::{self, Kind, S_OK};
use super::*;
use crate::process::Process;
use anywasm_cpu::{
    mem::{PAGE_SIZE, PROT_W},
    CpuMode,
};

const E_INVALIDARG: u64 = 0x8007_0057;
const E_NOINTERFACE: u64 = 0x8000_4002;
const CLASS_E_NOAGGREGATION: u64 = 0x8004_0110;
// dsound.h defines DSERR_INVALIDPARAM as E_INVALIDARG. 0x8878000A is
// DSERR_ALLOCATED, so don't use that code for invalid handles or arguments.
const DSERR_INVALIDPARAM: u64 = E_INVALIDARG;
const DSERR_ALLOCATED: u64 = 0x8878_000a;
const DSERR_CONTROLUNAVAIL: u64 = 0x8878_001e;
const DSERR_INVALIDCALL: u64 = 0x8878_0032;
const DSERR_PRIOLEVELNEEDED: u64 = 0x8878_0046;
const DSERR_BADFORMAT: u64 = 0x8878_0064;
const DSBUFFERDESC_SIZE: u32 = 36;
const DSBCAPS_SIZE: u32 = 20;
const DSBSIZE_MIN: u32 = 4;
const DSBSIZE_MAX: u32 = 0x0fff_ffff;
const MAX_SOUND_BUFFER: u32 = 16 * 1024 * 1024;
const DSBVOLUME_MIN: i32 = -10_000;
const DSBVOLUME_MAX: i32 = 0;
const DSBFREQUENCY_ORIGINAL: u32 = 0;
const DSBFREQUENCY_MIN: u32 = 100;
const DSBFREQUENCY_MAX: u32 = 100_000;
const WAVEFORMATEX_PCM_SIZE: usize = 18;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_UNKNOWN: u16 = 0;
const DSSCL_NORMAL: u32 = 1;
const DSSCL_PRIORITY: u32 = 2;
const DSSCL_EXCLUSIVE: u32 = 3;
const DSSCL_WRITEPRIMARY: u32 = 4;
const DSBPLAY_LOOPING: u32 = 0x1;
const DSBPLAY_LOCHARDWARE: u32 = 0x2;
const DSBPLAY_LOCSOFTWARE: u32 = 0x4;
const DSBPLAY_TERMINATEBY_TIME: u32 = 0x8;
const DSBPLAY_TERMINATEBY_DISTANCE: u32 = 0x10;
const DSBPLAY_TERMINATEBY_PRIORITY: u32 = 0x20;
const DSBSTATUS_PLAYING: u32 = 0x1;
const DSBSTATUS_LOOPING: u32 = 0x4;
const DSFX_LOCHARDWARE: u32 = 0x1;
const DSFX_LOCSOFTWARE: u32 = 0x2;
const DSFXR_UNALLOCATED: u32 = 3;
const MAX_EFFECTS: u32 = 64;
const HWND_DESKTOP: u64 = 0x0001_0010;
const WS_CHILD: u32 = 0x4000_0000;
pub const DSBCAPS_PRIMARYBUFFER: u32 = 0x1;
const DSBCAPS_STATIC: u32 = 0x2;
const DSBCAPS_LOCHARDWARE: u32 = 0x4;
pub const DSBCAPS_LOCSOFTWARE: u32 = 0x8;
const DSBCAPS_CTRLPAN: u32 = 0x40;
const DSBCAPS_CTRLVOLUME: u32 = 0x80;
const DSBCAPS_CTRLFREQUENCY: u32 = 0x20;
pub const DSBCAPS_CTRL3D: u32 = 0x10;
const DSBCAPS_CTRLPOSITIONNOTIFY: u32 = 0x100;
const DSBCAPS_CTRLFX: u32 = 0x200;
const DSBCAPS_STICKYFOCUS: u32 = 0x4000;
const DSBCAPS_GLOBALFOCUS: u32 = 0x8000;
const DSBCAPS_GETCURRENTPOSITION2: u32 = 0x1_0000;
const DSBCAPS_MUTE3DATMAXDISTANCE: u32 = 0x2_0000;
const DSBCAPS_LOCDEFER: u32 = 0x4_0000;
const DSBCAPS_TRUEPLAYPOSITION: u32 = 0x8_0000;
const DSBCAPS_KNOWN_MASK: u32 = DSBCAPS_PRIMARYBUFFER
    | DSBCAPS_STATIC
    | DSBCAPS_LOCHARDWARE
    | DSBCAPS_LOCSOFTWARE
    | DSBCAPS_CTRL3D
    | DSBCAPS_CTRLFREQUENCY
    | DSBCAPS_CTRLPAN
    | DSBCAPS_CTRLVOLUME
    | DSBCAPS_CTRLPOSITIONNOTIFY
    | DSBCAPS_CTRLFX
    | DSBCAPS_STICKYFOCUS
    | DSBCAPS_GLOBALFOCUS
    | DSBCAPS_GETCURRENTPOSITION2
    | DSBCAPS_MUTE3DATMAXDISTANCE
    | DSBCAPS_LOCDEFER
    | DSBCAPS_TRUEPLAYPOSITION;
const DSBCAPS_SUPPORTED_SECONDARY: u32 = DSBCAPS_LOCSOFTWARE
    | DSBCAPS_LOCDEFER
    | DSBCAPS_CTRL3D
    | DSBCAPS_CTRLFREQUENCY
    | DSBCAPS_CTRLVOLUME
    | DSBCAPS_CTRLFX
    | DSBCAPS_GLOBALFOCUS
    | DSBCAPS_GETCURRENTPOSITION2;
const DSBLOCK_FROMWRITECURSOR: u32 = 0x1;
const DSBLOCK_ENTIREBUFFER: u32 = 0x2;
const DSBLOCK_KNOWN_MASK: u32 = DSBLOCK_FROMWRITECURSOR | DSBLOCK_ENTIREBUFFER;
const DS3D_IMMEDIATE: u32 = 0;
const DS3D_DEFERRED: u32 = 1;
const DS3D_MAXROLLOFFFACTOR: f32 = 10.0;
const DS3D_MAXDOPPLERFACTOR: f32 = 10.0;
const DS3DLISTENER_SIZE: u32 = 64;
const IID_IDIRECTSOUND3DLISTENER: &str = "279afa84-4981-11ce-a521-0020af0be560";
const IID_IDIRECTSOUND3DBUFFER: &str = "279afa86-4981-11ce-a521-0020af0be560";
const IID_IDIRECTSOUND: &str = "279afa83-4981-11ce-a521-0020af0be560";
const IID_IDIRECTSOUNDBUFFER: &str = "279afa85-4981-11ce-a521-0020af0be560";

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    fn audio_clock_us() -> u64;
}

#[cfg(target_arch = "wasm32")]
fn monotonic_audio_clock_us(_p: &Process) -> u64 {
    // SAFETY: the browser runtime supplies a monotonic performance.now-based clock.
    unsafe { audio_clock_us() }
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_audio_clock_us(p: &Process) -> u64 {
    // Native HLE tests use the process's controllable virtual clock.
    p.now_ticks() / 100
}

fn effect_desc_size(p: &Process) -> u32 {
    if p.cpu.mode == CpuMode::X86_32 {
        32
    } else {
        40
    }
}

// The translation runtime exposes one virtual playback device. Keeping its ID stable lets
// callers round-trip the result through GetDeviceID and DirectSoundCreate without pretending
// that a host audio endpoint or capture device exists.
const DSDEVID_DEFAULT_PLAYBACK: [u8; 16] = [
    0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_CAPTURE: [u8; 16] = [
    0x01, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_VOICE_PLAYBACK: [u8; 16] = [
    0x02, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_VOICE_CAPTURE: [u8; 16] = [
    0x03, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const VIRTUAL_PLAYBACK_DEVICE: [u8; 16] = [
    0xa4, 0x7d, 0x39, 0x2c, 0x8a, 0x13, 0x4f, 0x40, 0x9d, 0x2a, 0x41, 0x57, 0x53, 0x4d, 0x00, 0x01,
];

macro_rules! unimplemented_method {
    ($function:ident, $name:literal) => {
        fn $function(_p: &mut Process) -> R {
            Err(Stop::Unimplemented {
                dll: "dsound".into(),
                name: format!("{} (DirectSound operation is not modeled)", $name),
            })
        }
    };
}

unimplemented_method!(unimplemented_compact, "IDirectSound8::Compact");
unimplemented_method!(
    unimplemented_set_speaker_config,
    "IDirectSound8::SetSpeakerConfig"
);
unimplemented_method!(unimplemented_sound_initialize, "IDirectSound8::Initialize");
unimplemented_method!(
    unimplemented_buffer_initialize,
    "IDirectSoundBuffer8::Initialize"
);
unimplemented_method!(
    unimplemented_buffer3d_get_all_parameters,
    "IDirectSound3DBuffer::GetAllParameters"
);
unimplemented_method!(
    unimplemented_buffer3d_get_cone_angles,
    "IDirectSound3DBuffer::GetConeAngles"
);
unimplemented_method!(
    unimplemented_buffer3d_get_cone_orientation,
    "IDirectSound3DBuffer::GetConeOrientation"
);
unimplemented_method!(
    unimplemented_buffer3d_get_cone_outside_volume,
    "IDirectSound3DBuffer::GetConeOutsideVolume"
);
unimplemented_method!(
    unimplemented_buffer3d_get_max_distance,
    "IDirectSound3DBuffer::GetMaxDistance"
);
unimplemented_method!(
    unimplemented_buffer3d_get_min_distance,
    "IDirectSound3DBuffer::GetMinDistance"
);
unimplemented_method!(
    unimplemented_buffer3d_get_mode,
    "IDirectSound3DBuffer::GetMode"
);
unimplemented_method!(
    unimplemented_buffer3d_get_position,
    "IDirectSound3DBuffer::GetPosition"
);
unimplemented_method!(
    unimplemented_buffer3d_get_velocity,
    "IDirectSound3DBuffer::GetVelocity"
);
unimplemented_method!(
    unimplemented_buffer3d_set_all_parameters,
    "IDirectSound3DBuffer::SetAllParameters"
);
unimplemented_method!(
    unimplemented_buffer3d_set_cone_angles,
    "IDirectSound3DBuffer::SetConeAngles"
);
unimplemented_method!(
    unimplemented_buffer3d_set_cone_orientation,
    "IDirectSound3DBuffer::SetConeOrientation"
);
unimplemented_method!(
    unimplemented_buffer3d_set_cone_outside_volume,
    "IDirectSound3DBuffer::SetConeOutsideVolume"
);
unimplemented_method!(
    unimplemented_buffer3d_set_max_distance,
    "IDirectSound3DBuffer::SetMaxDistance"
);
unimplemented_method!(
    unimplemented_buffer3d_set_min_distance,
    "IDirectSound3DBuffer::SetMinDistance"
);
unimplemented_method!(
    unimplemented_buffer3d_set_mode,
    "IDirectSound3DBuffer::SetMode"
);
unimplemented_method!(
    unimplemented_buffer3d_set_position,
    "IDirectSound3DBuffer::SetPosition"
);
unimplemented_method!(
    unimplemented_buffer3d_set_velocity,
    "IDirectSound3DBuffer::SetVelocity"
);
const LISTENER_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "GetAllParameters",
    "GetDistanceFactor",
    "GetDopplerFactor",
    "GetOrientation",
    "GetPosition",
    "GetRolloffFactor",
    "GetVelocity",
    "SetAllParameters",
    "SetDistanceFactor",
    "SetDopplerFactor",
    "SetOrientation",
    "SetPosition",
    "SetRolloffFactor",
    "SetVelocity",
    "CommitDeferredSettings",
];

const BUFFER_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "GetCaps",
    "GetCurrentPosition",
    "GetFormat",
    "GetVolume",
    "GetPan",
    "GetFrequency",
    "GetStatus",
    "Initialize",
    "Lock",
    "Play",
    "SetCurrentPosition",
    "SetFormat",
    "SetVolume",
    "SetPan",
    "SetFrequency",
    "Stop",
    "Unlock",
    "Restore",
];

const BUFFER3D_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "GetAllParameters",
    "GetConeAngles",
    "GetConeOrientation",
    "GetConeOutsideVolume",
    "GetMaxDistance",
    "GetMinDistance",
    "GetMode",
    "GetPosition",
    "GetVelocity",
    "SetAllParameters",
    "SetConeAngles",
    "SetConeOrientation",
    "SetConeOutsideVolume",
    "SetMaxDistance",
    "SetMinDistance",
    "SetMode",
    "SetPosition",
    "SetVelocity",
];

pub fn methods(interface: &str) -> Option<&'static [&'static str]> {
    match interface {
        "IDirectSound3DListener" => Some(LISTENER_METHODS),
        "IDirectSound3DBuffer" => Some(BUFFER3D_METHODS),
        "IDirectSoundBuffer" => Some(BUFFER_METHODS),
        _ => None,
    }
}

pub fn iid_name(iid: &str) -> Option<&'static str> {
    match iid {
        IID_IDIRECTSOUND3DLISTENER => Some("IDirectSound3DListener"),
        IID_IDIRECTSOUND3DBUFFER => Some("IDirectSound3DBuffer"),
        IID_IDIRECTSOUNDBUFFER => Some("IDirectSoundBuffer"),
        // DirectSoundCreate returns the base interface. Its vtable is prefix-compatible with
        // IDirectSound8, which is the modeled object used for the virtual device.
        IID_IDIRECTSOUND => Some("IDirectSound8"),
        _ => None,
    }
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "#1" => direct_sound_create,
        "#9" => get_device_id,
        "#11" => direct_sound_create8,
        _ => return None,
    })
}

/// Number of guest stack bytes consumed by a DSOUND export on x86 stdcall.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    match name {
        "#1" | "#11" => Some(12), // (device GUID, output pointer, outer unknown)
        "#9" => Some(8),          // (source GUID, destination GUID)
        _ => None,
    }
}

pub fn method(iface: &str, name: &str) -> Option<HleFn> {
    Some(match (iface, name) {
        ("IDirectSound8", "CreateSoundBuffer") => create_sound_buffer,
        ("IDirectSound8", "GetCaps") => get_sound_caps,
        ("IDirectSound8", "DuplicateSoundBuffer") => duplicate_sound_buffer,
        ("IDirectSound8", "GetSpeakerConfig") => get_speaker_config,
        ("IDirectSound8", "VerifyCertification") => verify_certification,
        ("IDirectSound8", "SetCooperativeLevel") => set_cooperative_level,
        ("IDirectSound8", "Compact") => unimplemented_compact,
        ("IDirectSound8", "SetSpeakerConfig") => unimplemented_set_speaker_config,
        ("IDirectSound8", "Initialize") => unimplemented_sound_initialize,
        ("IDirectSoundBuffer8", "GetCaps") => get_buffer_caps,
        ("IDirectSoundBuffer8", "GetCurrentPosition") => get_current_position,
        ("IDirectSoundBuffer8", "GetFormat") => get_format,
        ("IDirectSoundBuffer8", "GetVolume") => get_volume,
        ("IDirectSoundBuffer8", "GetPan") => get_pan,
        ("IDirectSoundBuffer8", "GetFrequency") => get_frequency,
        ("IDirectSoundBuffer8", "GetStatus") => get_status,
        ("IDirectSoundBuffer8", "Lock") => lock_buffer,
        ("IDirectSoundBuffer8", "GetObjectInPath") => get_object_in_path,
        ("IDirectSoundBuffer8", "SetFX") => set_fx,
        ("IDirectSoundBuffer8", "AcquireResources") => acquire_resources,
        ("IDirectSound3DListener", "GetAllParameters") => get_listener_parameters,
        ("IDirectSound3DListener", "GetDistanceFactor") => get_listener_distance_factor,
        ("IDirectSound3DListener", "GetDopplerFactor") => get_listener_doppler_factor,
        ("IDirectSound3DListener", "GetOrientation") => get_listener_orientation,
        ("IDirectSound3DListener", "GetPosition") => get_listener_position,
        ("IDirectSound3DListener", "GetRolloffFactor") => get_listener_rolloff_factor,
        ("IDirectSound3DListener", "GetVelocity") => get_listener_velocity,
        ("IDirectSound3DListener", "SetAllParameters") => set_listener_parameters,
        ("IDirectSound3DListener", "SetDistanceFactor") => set_listener_distance_factor,
        ("IDirectSound3DListener", "SetDopplerFactor") => set_listener_doppler_factor,
        ("IDirectSound3DListener", "SetOrientation") => set_listener_orientation,
        ("IDirectSound3DListener", "SetPosition") => set_listener_position,
        ("IDirectSound3DListener", "SetRolloffFactor") => set_listener_rolloff_factor,
        ("IDirectSound3DListener", "SetVelocity") => set_listener_velocity,
        ("IDirectSound3DListener", "CommitDeferredSettings") => commit_listener_settings,
        ("IDirectSound3DBuffer", "GetAllParameters") => unimplemented_buffer3d_get_all_parameters,
        ("IDirectSound3DBuffer", "GetConeAngles") => unimplemented_buffer3d_get_cone_angles,
        ("IDirectSound3DBuffer", "GetConeOrientation") => {
            unimplemented_buffer3d_get_cone_orientation
        }
        ("IDirectSound3DBuffer", "GetConeOutsideVolume") => {
            unimplemented_buffer3d_get_cone_outside_volume
        }
        ("IDirectSound3DBuffer", "GetMaxDistance") => unimplemented_buffer3d_get_max_distance,
        ("IDirectSound3DBuffer", "GetMinDistance") => unimplemented_buffer3d_get_min_distance,
        ("IDirectSound3DBuffer", "GetMode") => unimplemented_buffer3d_get_mode,
        ("IDirectSound3DBuffer", "GetPosition") => unimplemented_buffer3d_get_position,
        ("IDirectSound3DBuffer", "GetVelocity") => unimplemented_buffer3d_get_velocity,
        ("IDirectSound3DBuffer", "SetAllParameters") => unimplemented_buffer3d_set_all_parameters,
        ("IDirectSound3DBuffer", "SetConeAngles") => unimplemented_buffer3d_set_cone_angles,
        ("IDirectSound3DBuffer", "SetConeOrientation") => {
            unimplemented_buffer3d_set_cone_orientation
        }
        ("IDirectSound3DBuffer", "SetConeOutsideVolume") => {
            unimplemented_buffer3d_set_cone_outside_volume
        }
        ("IDirectSound3DBuffer", "SetMaxDistance") => unimplemented_buffer3d_set_max_distance,
        ("IDirectSound3DBuffer", "SetMinDistance") => unimplemented_buffer3d_set_min_distance,
        ("IDirectSound3DBuffer", "SetMode") => unimplemented_buffer3d_set_mode,
        ("IDirectSound3DBuffer", "SetPosition") => unimplemented_buffer3d_set_position,
        ("IDirectSound3DBuffer", "SetVelocity") => unimplemented_buffer3d_set_velocity,
        ("IDirectSoundBuffer8", "Initialize") => unimplemented_buffer_initialize,
        ("IDirectSoundBuffer8", "SetCurrentPosition") => set_buffer_current_position,
        ("IDirectSoundBuffer8", "SetFormat") => set_buffer_format,
        ("IDirectSoundBuffer8", "SetVolume") => set_volume,
        ("IDirectSoundBuffer8", "SetPan") => set_pan,
        ("IDirectSoundBuffer8", "SetFrequency") => set_frequency,
        ("IDirectSoundBuffer8", "Stop") => stop_buffer,
        ("IDirectSoundBuffer8", "Unlock") => unlock_buffer,
        ("IDirectSoundBuffer8", "Restore") => |_| Ok(S_OK),
        ("IDirectSoundBuffer8", "Play") => play_buffer,
        _ => return None,
    })
}

fn direct_sound_create8(p: &mut Process) -> R {
    let (out, outer) = (p.arg(1), p.arg(2));
    if out == 0 || outer != 0 {
        return Ok(E_INVALIDARG);
    }
    let sound = com::create(p, "IDirectSound8", Kind::DirectSound);
    p.wrptr(out, sound)?;
    Ok(S_OK)
}

fn direct_sound_create(p: &mut Process) -> R {
    let (device_guid, out, outer) = (p.arg(0), p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    p.wrptr(out, 0)?;
    if outer != 0 {
        return Ok(CLASS_E_NOAGGREGATION);
    }
    if !supports_playback_guid(p, device_guid)? {
        return Ok(DSERR_INVALIDPARAM);
    }

    let sound = com::create(p, "IDirectSound8", Kind::DirectSound);
    p.wrptr(out, sound)?;
    Ok(S_OK)
}

fn set_cooperative_level(p: &mut Process) -> R {
    let (this, hwnd, level) = (p.arg(0), p.arg(1), p.arg(2) as u32);
    if !matches!(com::kind_of(p, this), Some(Kind::DirectSound)) {
        return Ok(DSERR_INVALIDPARAM);
    }

    let valid_window = if hwnd == HWND_DESKTOP {
        true
    } else {
        p.extra
            .win
            .windows
            .get(&hwnd)
            .is_some_and(|window| window.style & WS_CHILD == 0)
    };
    if !valid_window
        || !matches!(
            level,
            DSSCL_NORMAL | DSSCL_PRIORITY | DSSCL_EXCLUSIVE | DSSCL_WRITEPRIMARY
        )
    {
        return Ok(DSERR_INVALIDPARAM);
    }

    // DSSCL_EXCLUSIVE has the same effect as DSSCL_PRIORITY for DirectSound 8,
    // but retain the caller's value in the object state for accurate queries
    // by future buffer operations.
    p.extra.com.dsound_cooperative.insert(this, (hwnd, level));
    Ok(S_OK)
}

fn supports_playback_guid(p: &Process, guid: u64) -> Result<bool, Stop> {
    if guid == 0 {
        return Ok(true);
    }
    let bytes = p.read_bytes(guid, 16)?;
    Ok(bytes == DSDEVID_DEFAULT_PLAYBACK
        || bytes == DSDEVID_DEFAULT_VOICE_PLAYBACK
        || bytes == VIRTUAL_PLAYBACK_DEVICE)
}

fn get_device_id(p: &mut Process) -> R {
    let (source, destination) = (p.arg(0), p.arg(1));
    if source == 0 || destination == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    let source = p.read_bytes(source, 16)?;
    if source == DSDEVID_DEFAULT_PLAYBACK || source == DSDEVID_DEFAULT_VOICE_PLAYBACK {
        p.write_bytes(destination, &VIRTUAL_PLAYBACK_DEVICE)?;
        return Ok(S_OK);
    }
    if source == VIRTUAL_PLAYBACK_DEVICE {
        p.write_bytes(destination, &source)?;
        return Ok(S_OK);
    }
    // Capture and voice-capture defaults cannot be represented by this playback-only model.
    // Other GUIDs have no corresponding virtual device, so report the documented invalid-ID
    // result instead of echoing arbitrary values as if they identified a device.
    if source == DSDEVID_DEFAULT_CAPTURE || source == DSDEVID_DEFAULT_VOICE_CAPTURE {
        return Ok(DSERR_INVALIDPARAM);
    }
    Ok(DSERR_INVALIDPARAM)
}

fn create_sound_buffer(p: &mut Process) -> R {
    let (desc, out, outer) = (p.arg(1), p.arg(2), p.arg(3));
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wrptr(out, 0)?;
    if desc == 0 || outer != 0 {
        return Ok(if outer != 0 {
            CLASS_E_NOAGGREGATION
        } else {
            E_INVALIDARG
        });
    }
    let desc_size = p.rd32(desc)?;
    if desc_size < DSBUFFERDESC_SIZE {
        return Ok(E_INVALIDARG);
    }
    let this = p.arg(0);
    if !matches!(com::kind_of(p, this), Some(Kind::DirectSound)) {
        return Ok(DSERR_INVALIDPARAM);
    }
    let caps = p.rd32(desc + 4)?;
    let requested_size = p.rd32(desc + 8)?;
    let reserved = p.rd32(desc + 12)?;
    let format = p.rd32(desc + 16)?;
    if reserved != 0 {
        return Ok(E_INVALIDARG);
    }
    if caps & DSBCAPS_PRIMARYBUFFER != 0 {
        if requested_size != 0 || format != 0 {
            return Ok(DSERR_INVALIDPARAM);
        }
        if caps & !(DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D) != 0 {
            return Err(Stop::Unimplemented {
                dll: "dsound".into(),
                name: format!(
                    "IDirectSound8::CreateSoundBuffer primary flags {caps:#x} are not modeled"
                ),
            });
        }
        let algorithm = p.read_bytes(desc + 20, 16)?;
        if algorithm.iter().any(|byte| *byte != 0) {
            return Err(Stop::Unimplemented {
                dll: "dsound".into(),
                name: "IDirectSound8::CreateSoundBuffer non-default 3D algorithm".into(),
            });
        }
        if p.extra.com.objs.values().any(|object| {
            matches!(object.kind, Kind::SoundBuffer { device, caps, .. }
                if device == this && caps & DSBCAPS_PRIMARYBUFFER != 0)
        }) {
            return Ok(DSERR_ALLOCATED);
        }
        p.extra
            .com
            .dsound_listeners
            .insert(this, com::DSoundListenerState::default());
        p.extra.com.dsound_listener_pending.remove(&this);
        p.extra.com.dsound_primary_formats.remove(&this);
        let buffer = com::create(
            p,
            "IDirectSoundBuffer",
            Kind::SoundBuffer {
                device: this,
                memory: 0,
                // A primary buffer is the device mix format, not guest-addressable sample
                // storage. It therefore has no lockable guest byte range.
                size: 0,
                caps,
            },
        );
        p.wrptr(out, buffer)?;
        return Ok(S_OK);
    }
    if caps & !DSBCAPS_KNOWN_MASK != 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    if caps & DSBCAPS_LOCHARDWARE != 0 && caps & DSBCAPS_LOCSOFTWARE != 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    if caps & DSBCAPS_CTRL3D != 0 && caps & DSBCAPS_CTRLPAN != 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    if caps & DSBCAPS_LOCHARDWARE != 0 {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: "IDirectSound8::CreateSoundBuffer hardware-resident secondary buffer".into(),
        });
    }
    let unsupported_caps = caps & !(DSBCAPS_SUPPORTED_SECONDARY | DSBCAPS_PRIMARYBUFFER);
    if unsupported_caps != 0 {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: format!(
                "IDirectSound8::CreateSoundBuffer unsupported secondary flags {unsupported_caps:#x}"
            ),
        });
    }
    if !(DSBSIZE_MIN..=DSBSIZE_MAX).contains(&requested_size) {
        return Ok(DSERR_INVALIDPARAM);
    }
    if requested_size > MAX_SOUND_BUFFER {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: format!(
                "IDirectSound8::CreateSoundBuffer {requested_size:#x}-byte buffer exceeds the guest-backed buffer limit {MAX_SOUND_BUFFER:#x}"
            ),
        });
    }
    if format == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    let format = p.read_bytes(format as u64, WAVEFORMATEX_PCM_SIZE)?;
    let tag = u16::from_le_bytes(format[0..2].try_into().unwrap());
    if tag != WAVE_FORMAT_PCM {
        if tag == WAVE_FORMAT_UNKNOWN {
            return Ok(DSERR_BADFORMAT);
        }
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: format!(
                "IDirectSound8::CreateSoundBuffer WAVEFORMATEX tag {tag:#x} is not modeled"
            ),
        });
    }
    let channels = u16::from_le_bytes(format[2..4].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
    let average_bytes = u32::from_le_bytes(format[8..12].try_into().unwrap());
    let block_align = u16::from_le_bytes(format[12..14].try_into().unwrap());
    let bits_per_sample = u16::from_le_bytes(format[14..16].try_into().unwrap());
    if !matches!(channels, 1 | 2) || sample_rate == 0 || !matches!(bits_per_sample, 8 | 16) {
        return Ok(DSERR_BADFORMAT);
    }
    let expected_align = channels * (bits_per_sample / 8);
    let Some(expected_average) = sample_rate.checked_mul(expected_align as u32) else {
        return Ok(DSERR_BADFORMAT);
    };
    if block_align != expected_align || average_bytes != expected_average {
        return Ok(DSERR_BADFORMAT);
    }
    if requested_size % block_align as u32 != 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    if caps & DSBCAPS_CTRL3D != 0 && channels != 1 {
        return Ok(DSERR_BADFORMAT);
    }
    if caps & DSBCAPS_CTRL3D != 0 {
        let algorithm = p.read_bytes(desc + 20, 16)?;
        if algorithm.iter().any(|byte| *byte != 0) {
            return Err(Stop::Unimplemented {
                dll: "dsound".into(),
                name: "IDirectSound8::CreateSoundBuffer non-default 3D algorithm".into(),
            });
        }
    }

    // The guest VFS-backed implementation stores all samples in software memory. Report that
    // actual location through GetCaps even when the caller leaves both location flags unset.
    let caps = caps | DSBCAPS_LOCSOFTWARE;
    let memory = super::kernel32::alloc_block(p, requested_size as u64, true);
    let buffer = com::create(
        p,
        "IDirectSoundBuffer",
        Kind::SoundBuffer {
            device: this,
            memory,
            size: requested_size,
            caps,
        },
    );
    p.extra.com.dsound_buffer_formats.insert(
        buffer,
        format.try_into().expect("WAVEFORMATEX size checked"),
    );
    p.extra.com.dsound_buffer_controls.insert(
        buffer,
        com::DSoundBufferControlState {
            volume: DSBVOLUME_MAX,
            frequency: sample_rate,
        },
    );
    p.wrptr(out, buffer)?;
    Ok(S_OK)
}

fn duplicate_sound_buffer(p: &mut Process) -> R {
    let (source, out) = (p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let Some(Kind::SoundBuffer {
        device,
        memory,
        size,
        caps,
    }) = com::kind_of(p, source)
    else {
        p.wrptr(out, 0)?;
        return Ok(E_INVALIDARG);
    };
    if caps & DSBCAPS_PRIMARYBUFFER != 0 {
        p.wrptr(out, 0)?;
        return Ok(DSERR_INVALIDCALL);
    }
    let Some(format) = p.extra.com.dsound_buffer_formats.get(&source).copied() else {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: "IDirectSound8::DuplicateSoundBuffer without retained source format".into(),
        });
    };
    let controls = p
        .extra
        .com
        .dsound_buffer_controls
        .get(&source)
        .copied()
        .unwrap_or_else(|| com::DSoundBufferControlState {
            volume: DSBVOLUME_MAX,
            frequency: u32::from_le_bytes(format[4..8].try_into().unwrap()),
        });
    let duplicate_memory = super::kernel32::alloc_block(p, size as u64, true);
    let bytes = p.read_bytes(memory, size as usize)?;
    p.write_bytes(duplicate_memory, &bytes)?;
    let duplicate = com::create(
        p,
        "IDirectSoundBuffer",
        Kind::SoundBuffer {
            device,
            memory: duplicate_memory,
            size,
            caps,
        },
    );
    p.extra.com.dsound_buffer_formats.insert(duplicate, format);
    p.extra
        .com
        .dsound_buffer_controls
        .insert(duplicate, controls);
    p.wrptr(out, duplicate)?;
    Ok(S_OK)
}

fn get_sound_caps(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let size = p.rd32(out)?;
    if size < 20 || size > 4096 {
        return Ok(E_INVALIDARG);
    }
    p.write_bytes(out, &vec![0; size as usize])?;
    p.wr32(out, size)?;
    Ok(S_OK)
}

fn get_buffer_caps(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let size = p.rd32(out)?;
    if size < DSBCAPS_SIZE || size > 4096 {
        return Ok(E_INVALIDARG);
    }
    p.write_bytes(out, &vec![0; size as usize])?;
    p.wr32(out, size)?;
    let (caps, buffer_size) = match com::kind_of(p, p.arg(0)) {
        Some(Kind::SoundBuffer { caps, size, .. }) => (caps, size),
        _ => return Ok(E_INVALIDARG),
    };
    p.wr32(out + 4, caps)?;
    p.wr32(out + 8, buffer_size)?;
    Ok(S_OK)
}

fn get_speaker_config(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, 0x0000_0004)?; // stereo
    Ok(S_OK)
}

fn verify_certification(p: &mut Process) -> R {
    let out = p.arg(1);
    if out != 0 {
        p.wr32(out, 0)?;
    }
    Ok(S_OK)
}

fn write_float(p: &mut Process, out: u64, value: f32) -> R {
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, value.to_bits())?;
    Ok(S_OK)
}

fn write_vector(p: &mut Process, out: u64, value: [f32; 3]) -> R {
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    for (index, component) in value.into_iter().enumerate() {
        p.wr32(out + index as u64 * 4, component.to_bits())?;
    }
    Ok(S_OK)
}

fn get_listener_parameters(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let out = p.arg(1);
    if out == 0 || p.rd32(out)? != DS3DLISTENER_SIZE {
        return Ok(E_INVALIDARG);
    }
    let state = listener_state(p, device);
    write_listener_state(p, out, state)?;
    Ok(S_OK)
}

fn get_listener_distance_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    write_float(p, p.arg(1), listener_state(p, device).distance_factor)
}

fn get_listener_doppler_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    write_float(p, p.arg(1), listener_state(p, device).doppler_factor)
}

fn get_listener_orientation(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let state = listener_state(p, device);
    write_vector(p, p.arg(1), state.orient_front)?;
    write_vector(p, p.arg(2), state.orient_top)
}

fn get_listener_position(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    write_vector(p, p.arg(1), listener_state(p, device).position)
}

fn get_listener_rolloff_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    write_float(p, p.arg(1), listener_state(p, device).rolloff_factor)
}

fn get_listener_velocity(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    write_vector(p, p.arg(1), listener_state(p, device).velocity)
}

fn listener_device(p: &Process, this: u64) -> Option<u64> {
    match com::kind_of(p, this)? {
        Kind::SoundBuffer { device, caps, .. }
            if caps & (DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D)
                == (DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D) =>
        {
            Some(device)
        }
        _ => None,
    }
}

fn listener_state(p: &Process, device: u64) -> com::DSoundListenerState {
    p.extra
        .com
        .dsound_listeners
        .get(&device)
        .copied()
        .unwrap_or_default()
}

fn valid_listener_state(state: com::DSoundListenerState) -> bool {
    let all_finite = state
        .position
        .into_iter()
        .chain(state.velocity)
        .chain(state.orient_front)
        .chain(state.orient_top)
        .all(f32::is_finite);
    let cross = [
        state.orient_front[1] * state.orient_top[2] - state.orient_front[2] * state.orient_top[1],
        state.orient_front[2] * state.orient_top[0] - state.orient_front[0] * state.orient_top[2],
        state.orient_front[0] * state.orient_top[1] - state.orient_front[1] * state.orient_top[0],
    ];
    all_finite
        && cross.iter().any(|component| *component != 0.0)
        && state.distance_factor.is_finite()
        && state.distance_factor >= f32::MIN_POSITIVE
        && state.rolloff_factor.is_finite()
        && (0.0..=DS3D_MAXROLLOFFFACTOR).contains(&state.rolloff_factor)
        && state.doppler_factor.is_finite()
        && (0.0..=DS3D_MAXDOPPLERFACTOR).contains(&state.doppler_factor)
}

fn write_listener_state(p: &mut Process, out: u64, state: com::DSoundListenerState) -> R {
    p.wr32(out, DS3DLISTENER_SIZE)?;
    write_vector(p, out + 4, state.position)?;
    write_vector(p, out + 16, state.velocity)?;
    write_vector(p, out + 28, state.orient_front)?;
    write_vector(p, out + 40, state.orient_top)?;
    p.wr32(out + 52, state.distance_factor.to_bits())?;
    p.wr32(out + 56, state.rolloff_factor.to_bits())?;
    p.wr32(out + 60, state.doppler_factor.to_bits())?;
    Ok(S_OK)
}

fn apply_listener_state(
    p: &mut Process,
    device: u64,
    state: com::DSoundListenerState,
    apply: u32,
) -> u64 {
    if !valid_listener_state(state) {
        return DSERR_INVALIDPARAM;
    }
    match apply {
        DS3D_IMMEDIATE => {
            p.extra.com.dsound_listeners.insert(device, state);
            S_OK
        }
        DS3D_DEFERRED => {
            p.extra.com.dsound_listener_pending.insert(device, state);
            S_OK
        }
        _ => DSERR_INVALIDPARAM,
    }
}

fn set_listener_parameters(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let (input, apply) = (p.arg(1), p.arg(2) as u32);
    if input == 0 || p.rd32(input)? != DS3DLISTENER_SIZE {
        return Ok(DSERR_INVALIDPARAM);
    }
    let read_vec = |p: &Process, offset| -> Result<[f32; 3], Stop> {
        Ok([
            f32::from_bits(p.rd32(input + offset)?),
            f32::from_bits(p.rd32(input + offset + 4)?),
            f32::from_bits(p.rd32(input + offset + 8)?),
        ])
    };
    let state = com::DSoundListenerState {
        position: read_vec(p, 4)?,
        velocity: read_vec(p, 16)?,
        orient_front: read_vec(p, 28)?,
        orient_top: read_vec(p, 40)?,
        distance_factor: f32::from_bits(p.rd32(input + 52)?),
        rolloff_factor: f32::from_bits(p.rd32(input + 56)?),
        doppler_factor: f32::from_bits(p.rd32(input + 60)?),
    };
    Ok(apply_listener_state(p, device, state, apply))
}

fn listener_update_base(p: &Process, device: u64, apply: u32) -> com::DSoundListenerState {
    if apply == DS3D_DEFERRED {
        p.extra
            .com
            .dsound_listener_pending
            .get(&device)
            .copied()
            .unwrap_or_else(|| listener_state(p, device))
    } else {
        listener_state(p, device)
    }
}

fn set_listener_position(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(4) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.position = [1, 2, 3].map(|index| f32::from_bits(p.arg(index) as u32));
    Ok(apply_listener_state(p, device, state, apply))
}

fn set_listener_velocity(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(4) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.velocity = [1, 2, 3].map(|index| f32::from_bits(p.arg(index) as u32));
    Ok(apply_listener_state(p, device, state, apply))
}

fn set_listener_orientation(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(7) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.orient_front = [1, 2, 3].map(|index| f32::from_bits(p.arg(index) as u32));
    state.orient_top = [4, 5, 6].map(|index| f32::from_bits(p.arg(index) as u32));
    Ok(apply_listener_state(p, device, state, apply))
}

fn set_listener_distance_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(2) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.distance_factor = f32::from_bits(p.arg(1) as u32);
    Ok(apply_listener_state(p, device, state, apply))
}

fn set_listener_rolloff_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(2) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.rolloff_factor = f32::from_bits(p.arg(1) as u32);
    Ok(apply_listener_state(p, device, state, apply))
}

fn set_listener_doppler_factor(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let apply = p.arg(2) as u32;
    let mut state = listener_update_base(p, device, apply);
    state.doppler_factor = f32::from_bits(p.arg(1) as u32);
    Ok(apply_listener_state(p, device, state, apply))
}

fn commit_listener_settings(p: &mut Process) -> R {
    let Some(device) = listener_device(p, p.arg(0)) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    if let Some(state) = p.extra.com.dsound_listener_pending.remove(&device) {
        p.extra.com.dsound_listeners.insert(device, state);
    }
    Ok(S_OK)
}

fn get_current_position(p: &mut Process) -> R {
    let this = p.arg(0);
    let Some(Kind::SoundBuffer { size, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    let now = monotonic_audio_clock_us(p);
    let cursor = update_playback_cursor(p, this, now);
    let play_out = p.arg(1);
    let write_out = p.arg(2);
    if play_out != 0 {
        p.wr32(play_out, cursor)?;
    }
    if write_out != 0 {
        let write_cursor = if size == 0 {
            0
        } else if p
            .extra
            .com
            .dsound_buffer_playback
            .get(&this)
            .is_some_and(|playback| playback.playing)
        {
            let format = p
                .extra
                .com
                .dsound_buffer_formats
                .get(&this)
                .copied()
                .unwrap_or_else(default_primary_format);
            let average_bytes = u32::from_le_bytes(format[8..12].try_into().unwrap());
            let block_align = u16::from_le_bytes(format[12..14].try_into().unwrap()) as u32;
            let block_align = block_align.max(1);
            // WebAudio exposes no DirectSound write cursor. Report a stable software write
            // horizon 10 ms ahead of the play cursor, rounded to a complete sample frame.
            let lead = ((average_bytes / 100) / block_align) * block_align;
            cursor.wrapping_add(lead) % size
        } else {
            cursor
        };
        p.wr32(write_out, write_cursor)?;
    }
    Ok(S_OK)
}

fn set_buffer_current_position(p: &mut Process) -> R {
    let (this, position) = (p.arg(0), p.arg(1) as u32);
    let Some(Kind::SoundBuffer { caps, size, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & DSBCAPS_PRIMARYBUFFER != 0 {
        return Ok(DSERR_INVALIDCALL);
    }
    let Some(format) = p.extra.com.dsound_buffer_formats.get(&this).copied() else {
        return Ok(E_INVALIDARG);
    };
    let block_align = u16::from_le_bytes([format[12], format[13]]) as u32;
    if position >= size || block_align == 0 || position % block_align != 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    let now = monotonic_audio_clock_us(p);
    update_playback_cursor(p, this, now);
    let playback =
        p.extra
            .com
            .dsound_buffer_playback
            .entry(this)
            .or_insert(com::DSoundBufferPlaybackState {
                started_at_us: now,
                start_cursor: 0,
                cursor: 0,
                playing: false,
                looping: false,
            });
    let was_playing = playback.playing;
    playback.started_at_us = now;
    playback.start_cursor = position;
    playback.cursor = position;
    if was_playing {
        p.extra
            .com
            .dsound_audio_events
            .push(com::DSoundAudioEvent::Seek {
                buffer: this,
                cursor: position,
            });
    }
    Ok(S_OK)
}

fn write_u32(p: &mut Process, value: u32) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wr32(out, value)?;
    Ok(S_OK)
}

fn get_volume(p: &mut Process) -> R {
    get_control(p, DSBCAPS_CTRLVOLUME, 0)
}

fn get_pan(p: &mut Process) -> R {
    get_control(p, DSBCAPS_CTRLPAN, 0)
}

fn get_frequency(p: &mut Process) -> R {
    get_control(p, DSBCAPS_CTRLFREQUENCY, 44_100)
}

fn get_control(p: &mut Process, control: u32, value: u32) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let this = p.arg(0);
    let Some(Kind::SoundBuffer { caps, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & control == 0 {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    let controls = p
        .extra
        .com
        .dsound_buffer_controls
        .get(&this)
        .copied()
        .unwrap_or_default();
    let value = match control {
        DSBCAPS_CTRLVOLUME => controls.volume as u32,
        DSBCAPS_CTRLFREQUENCY => controls.frequency,
        _ => value,
    };
    p.wr32(out, value)?;
    Ok(S_OK)
}

fn set_control(p: &mut Process, control: u32, name: &'static str) -> R {
    let this = p.arg(0);
    let Some(Kind::SoundBuffer { caps, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & control == 0 {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    if caps & DSBCAPS_PRIMARYBUFFER != 0 {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: format!(
                "IDirectSoundBuffer8::{name} on the primary mixer (audio output is not modeled)"
            ),
        });
    }

    let now = monotonic_audio_clock_us(p);
    let was_playing = p
        .extra
        .com
        .dsound_buffer_playback
        .get(&this)
        .is_some_and(|playback| playback.playing);
    let cursor = if was_playing {
        update_playback_cursor(p, this, now)
    } else {
        0
    };
    let requested = p.arg(1) as u32;
    let mut controls = p
        .extra
        .com
        .dsound_buffer_controls
        .get(&this)
        .copied()
        .unwrap_or_default();
    match control {
        DSBCAPS_CTRLVOLUME => {
            let volume = requested as i32;
            if !(DSBVOLUME_MIN..=DSBVOLUME_MAX).contains(&volume) {
                return Ok(DSERR_INVALIDPARAM);
            }
            controls.volume = volume;
        }
        DSBCAPS_CTRLFREQUENCY => {
            let original_frequency = p
                .extra
                .com
                .dsound_buffer_formats
                .get(&this)
                .map(|format| u32::from_le_bytes(format[4..8].try_into().unwrap()))
                .unwrap_or(controls.frequency);
            let frequency = if requested == DSBFREQUENCY_ORIGINAL {
                original_frequency
            } else {
                requested
            };
            if !(DSBFREQUENCY_MIN..=DSBFREQUENCY_MAX).contains(&frequency) {
                return Ok(DSERR_INVALIDPARAM);
            }
            controls.frequency = frequency;
        }
        _ => {
            return Err(Stop::Unimplemented {
                dll: "dsound".into(),
                name: format!("IDirectSoundBuffer8::{name} (control state is not modeled)"),
            });
        }
    }
    p.extra.com.dsound_buffer_controls.insert(this, controls);
    if let Some(playback) = p.extra.com.dsound_buffer_playback.get_mut(&this) {
        if was_playing {
            playback.started_at_us = now;
            playback.start_cursor = cursor;
            playback.cursor = cursor;
        }
    }
    if was_playing {
        p.extra
            .com
            .dsound_audio_events
            .push(com::DSoundAudioEvent::Update {
                buffer: this,
                frequency: controls.frequency,
                volume: controls.volume,
            });
    }
    Ok(S_OK)
}

fn set_volume(p: &mut Process) -> R {
    set_control(p, DSBCAPS_CTRLVOLUME, "SetVolume")
}

fn set_pan(p: &mut Process) -> R {
    set_control(p, DSBCAPS_CTRLPAN, "SetPan")
}

fn set_frequency(p: &mut Process) -> R {
    set_control(p, DSBCAPS_CTRLFREQUENCY, "SetFrequency")
}

fn get_status(p: &mut Process) -> R {
    let this = p.arg(0);
    if !matches!(com::kind_of(p, this), Some(Kind::SoundBuffer { .. })) {
        return Ok(E_INVALIDARG);
    }
    let now = monotonic_audio_clock_us(p);
    update_playback_cursor(p, this, now);
    let status = p
        .extra
        .com
        .dsound_buffer_playback
        .get(&this)
        .filter(|playback| playback.playing)
        .map_or(0, |playback| {
            DSBSTATUS_PLAYING
                | if playback.looping {
                    DSBSTATUS_LOOPING
                } else {
                    0
                }
        });
    write_u32(p, status)
}

fn update_playback_cursor(p: &mut Process, this: u64, now_us: u64) -> u32 {
    let Some(Kind::SoundBuffer { size, .. }) = com::kind_of(p, this) else {
        return 0;
    };
    let Some(playback) = p.extra.com.dsound_buffer_playback.get(&this).copied() else {
        return 0;
    };
    if !playback.playing || size == 0 {
        return playback.cursor;
    }
    let Some(format) = p.extra.com.dsound_buffer_formats.get(&this).copied() else {
        return playback.cursor;
    };
    let block_align = u16::from_le_bytes([format[12], format[13]]) as u64;
    let controls = p
        .extra
        .com
        .dsound_buffer_controls
        .get(&this)
        .copied()
        .unwrap_or_default();
    let elapsed_us = now_us.saturating_sub(playback.started_at_us);
    let advanced_frames = (elapsed_us as u128 * controls.frequency as u128) / 1_000_000;
    let advanced_bytes = advanced_frames.saturating_mul(block_align as u128);
    let (cursor, playing) = if playback.looping {
        (
            ((playback.start_cursor as u128 + advanced_bytes) % size as u128) as u32,
            true,
        )
    } else if advanced_bytes >= size.saturating_sub(playback.start_cursor) as u128 {
        (0, false)
    } else {
        (
            (playback.start_cursor as u128 + advanced_bytes) as u32,
            true,
        )
    };
    if let Some(state) = p.extra.com.dsound_buffer_playback.get_mut(&this) {
        state.cursor = cursor;
        state.playing = playing;
    }
    cursor
}

fn play_buffer(p: &mut Process) -> R {
    let (this, reserved, priority, flags) = (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(3) as u32);
    let known_flags = DSBPLAY_LOOPING
        | DSBPLAY_LOCHARDWARE
        | DSBPLAY_LOCSOFTWARE
        | DSBPLAY_TERMINATEBY_TIME
        | DSBPLAY_TERMINATEBY_DISTANCE
        | DSBPLAY_TERMINATEBY_PRIORITY;
    if reserved != 0
        || flags & !known_flags != 0
        || flags & DSBPLAY_LOCHARDWARE != 0 && flags & DSBPLAY_LOCSOFTWARE != 0
    {
        return Ok(DSERR_INVALIDPARAM);
    }
    let Some(Kind::SoundBuffer {
        memory, size, caps, ..
    }) = com::kind_of(p, this)
    else {
        return Ok(E_INVALIDARG);
    };
    let deferred = caps & DSBCAPS_LOCDEFER != 0;
    if !deferred && priority != 0
        || !deferred
            && flags
                & (DSBPLAY_LOCHARDWARE
                    | DSBPLAY_LOCSOFTWARE
                    | DSBPLAY_TERMINATEBY_TIME
                    | DSBPLAY_TERMINATEBY_DISTANCE
                    | DSBPLAY_TERMINATEBY_PRIORITY)
                != 0
        || caps & DSBCAPS_PRIMARYBUFFER != 0 && flags & DSBPLAY_LOOPING == 0
    {
        return Ok(DSERR_INVALIDPARAM);
    }
    if flags
        & (DSBPLAY_LOCHARDWARE
            | DSBPLAY_TERMINATEBY_TIME
            | DSBPLAY_TERMINATEBY_DISTANCE
            | DSBPLAY_TERMINATEBY_PRIORITY)
        != 0
    {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    if !p.extra.com.dsound_audio_enabled {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: "IDirectSoundBuffer8::Play (browser WebAudio output is unavailable)".into(),
        });
    }
    if caps & DSBCAPS_PRIMARYBUFFER == 0 {
        if memory == 0 || size == 0 {
            return Ok(DSERR_INVALIDCALL);
        }
        if p.extra
            .com
            .dsound_buffer_locks
            .get(&this)
            .is_some_and(|locks| !locks.is_empty())
        {
            return Ok(DSERR_INVALIDCALL);
        }
    }
    let now = monotonic_audio_clock_us(p);
    let cursor = update_playback_cursor(p, this, now);
    if p.extra
        .com
        .dsound_buffer_playback
        .get(&this)
        .is_some_and(|playback| playback.playing)
    {
        // DirectSound Play on an already-playing buffer is an idempotent success.
        return Ok(S_OK);
    }
    let controls = p
        .extra
        .com
        .dsound_buffer_controls
        .get(&this)
        .copied()
        .unwrap_or_default();
    let looping = flags & DSBPLAY_LOOPING != 0;
    p.extra.com.dsound_buffer_playback.insert(
        this,
        com::DSoundBufferPlaybackState {
            started_at_us: now,
            start_cursor: cursor,
            cursor,
            playing: true,
            looping,
        },
    );
    if caps & DSBCAPS_PRIMARYBUFFER == 0 {
        let Some(format) = p.extra.com.dsound_buffer_formats.get(&this).copied() else {
            p.extra.com.dsound_buffer_playback.remove(&this);
            return Ok(E_INVALIDARG);
        };
        let samples = p.read_bytes(memory, size as usize)?;
        p.extra
            .com
            .dsound_audio_events
            .push(com::DSoundAudioEvent::Play {
                buffer: this,
                format,
                frequency: controls.frequency,
                volume: controls.volume,
                started_at_us: now,
                cursor,
                looping,
                samples,
            });
    }
    Ok(S_OK)
}

fn stop_buffer(p: &mut Process) -> R {
    let this = p.arg(0);
    if !matches!(com::kind_of(p, this), Some(Kind::SoundBuffer { .. })) {
        return Ok(E_INVALIDARG);
    }
    let now = monotonic_audio_clock_us(p);
    update_playback_cursor(p, this, now);
    if let Some(playback) = p.extra.com.dsound_buffer_playback.get_mut(&this) {
        if playback.playing {
            playback.playing = false;
            p.extra
                .com
                .dsound_audio_events
                .push(com::DSoundAudioEvent::Stop { buffer: this });
        }
    }
    Ok(S_OK)
}

fn set_fx(p: &mut Process) -> R {
    let (this, count, descriptions, results) = (p.arg(0), p.arg(1) as u32, p.arg(2), p.arg(3));
    let Some(Kind::SoundBuffer { caps, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & DSBCAPS_CTRLFX == 0 {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    if p.extra
        .com
        .dsound_buffer_playback
        .get(&this)
        .is_some_and(|playback| playback.playing)
        || p.extra
            .com
            .dsound_buffer_locks
            .get(&this)
            .is_some_and(|locks| !locks.is_empty())
    {
        return Ok(DSERR_INVALIDCALL);
    }
    if count > MAX_EFFECTS || (count != 0 && descriptions == 0) {
        return Ok(DSERR_INVALIDPARAM);
    }
    if count == 0 {
        return Ok(S_OK);
    }
    if results == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    let mut descriptors = Vec::with_capacity(count as usize);
    let descriptor_size = effect_desc_size(p);
    for index in 0..count {
        let Some(address) = descriptions.checked_add(index as u64 * descriptor_size as u64) else {
            return Ok(DSERR_INVALIDPARAM);
        };
        let size = p.rd32(address)?;
        let flags = p.rd32(address + 4)?;
        let (reserved1, reserved2) = if descriptor_size == 40 {
            (p.rd64(address + 24)?, p.rd64(address + 32)?)
        } else {
            (
                u64::from(p.rd32(address + 24)?),
                u64::from(p.rd32(address + 28)?),
            )
        };
        if size != descriptor_size
            || flags & !(DSFX_LOCHARDWARE | DSFX_LOCSOFTWARE) != 0
            || flags == (DSFX_LOCHARDWARE | DSFX_LOCSOFTWARE)
            || reserved1 != 0
            || reserved2 != 0
        {
            return Ok(DSERR_INVALIDPARAM);
        }
        descriptors.push((address, flags));
    }
    // The WebAudio bridge currently exposes PCM playback only. Do not claim a DirectSound FX
    // graph exists: report each recognized effect as unallocated and return the real failure.
    for (index, _) in descriptors.iter().enumerate() {
        p.wr32(results + index as u64 * 4, DSFXR_UNALLOCATED)?;
    }
    Ok(DSERR_CONTROLUNAVAIL)
}

fn acquire_resources(p: &mut Process) -> R {
    let (this, flags, count, results) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u32, p.arg(3));
    let Some(Kind::SoundBuffer { caps, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if flags
        & !(DSBPLAY_LOCHARDWARE
            | DSBPLAY_LOCSOFTWARE
            | DSBPLAY_TERMINATEBY_TIME
            | DSBPLAY_TERMINATEBY_DISTANCE
            | DSBPLAY_TERMINATEBY_PRIORITY)
        != 0
        || flags & DSBPLAY_LOCHARDWARE != 0 && flags & DSBPLAY_LOCSOFTWARE != 0
        || count > MAX_EFFECTS
    {
        return Ok(DSERR_INVALIDPARAM);
    }
    if caps & DSBCAPS_LOCDEFER == 0 {
        return Ok(DSERR_INVALIDCALL);
    }
    if flags
        & (DSBPLAY_LOCHARDWARE
            | DSBPLAY_TERMINATEBY_TIME
            | DSBPLAY_TERMINATEBY_DISTANCE
            | DSBPLAY_TERMINATEBY_PRIORITY)
        != 0
    {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    if count == 0 {
        return Ok(S_OK);
    }
    if results == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    for index in 0..count {
        p.wr32(results + index as u64 * 4, DSFXR_UNALLOCATED)?;
    }
    Ok(DSERR_CONTROLUNAVAIL)
}

fn get_format(p: &mut Process) -> R {
    let (out, capacity, written) = (p.arg(1), p.arg(2) as usize, p.arg(3));
    let this = p.arg(0);
    let Some(Kind::SoundBuffer { caps, device, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & DSBCAPS_PRIMARYBUFFER != 0 {
        if written != 0 {
            p.wr32(written, WAVEFORMATEX_PCM_SIZE as u32)?;
        }
        if out == 0 {
            return Ok(S_OK);
        }
        if capacity < WAVEFORMATEX_PCM_SIZE {
            return Ok(DSERR_INVALIDPARAM);
        }
        let format = p
            .extra
            .com
            .dsound_primary_formats
            .get(&device)
            .copied()
            .unwrap_or_else(default_primary_format);
        p.write_bytes(out, &format)?;
        return Ok(S_OK);
    }
    if written != 0 {
        p.wr32(written, WAVEFORMATEX_PCM_SIZE as u32)?;
    }
    if out == 0 {
        return Ok(S_OK);
    }
    if capacity < WAVEFORMATEX_PCM_SIZE {
        return Ok(DSERR_INVALIDPARAM);
    }
    let Some(format) = p.extra.com.dsound_buffer_formats.get(&this).copied() else {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: "IDirectSoundBuffer8::GetFormat without retained secondary format".into(),
        });
    };
    p.write_bytes(out, &format)?;
    Ok(S_OK)
}

fn default_primary_format() -> [u8; WAVEFORMATEX_PCM_SIZE] {
    let mut format = [0u8; WAVEFORMATEX_PCM_SIZE];
    format[0..2].copy_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    format[2..4].copy_from_slice(&2u16.to_le_bytes());
    format[4..8].copy_from_slice(&44_100u32.to_le_bytes());
    format[8..12].copy_from_slice(&176_400u32.to_le_bytes());
    format[12..14].copy_from_slice(&4u16.to_le_bytes());
    format[14..16].copy_from_slice(&16u16.to_le_bytes());
    format
}

fn set_buffer_format(p: &mut Process) -> R {
    let this = p.arg(0);
    let Some(Kind::SoundBuffer { device, caps, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if caps & DSBCAPS_PRIMARYBUFFER == 0 {
        return Err(Stop::Unimplemented {
            dll: "dsound".into(),
            name: "IDirectSoundBuffer8::SetFormat on a secondary buffer".into(),
        });
    }
    let cooperative_level = p
        .extra
        .com
        .dsound_cooperative
        .get(&device)
        .map_or(DSSCL_NORMAL, |(_, level)| *level);
    if cooperative_level < DSSCL_PRIORITY {
        return Ok(DSERR_PRIOLEVELNEEDED);
    }
    let input = p.arg(1);
    if input == 0 {
        return Ok(E_INVALIDARG);
    }
    let format = p.read_bytes(input, WAVEFORMATEX_PCM_SIZE)?;
    let tag = u16::from_le_bytes(format[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(format[2..4].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
    let average_bytes = u32::from_le_bytes(format[8..12].try_into().unwrap());
    let block_align = u16::from_le_bytes(format[12..14].try_into().unwrap());
    let bits_per_sample = u16::from_le_bytes(format[14..16].try_into().unwrap());
    let extra_size = u16::from_le_bytes(format[16..18].try_into().unwrap());
    if tag != 1
        || !matches!(channels, 1 | 2)
        || sample_rate == 0
        || !matches!(bits_per_sample, 8 | 16 | 24 | 32)
        || extra_size != 0
    {
        return Ok(DSERR_BADFORMAT);
    }
    let expected_align = channels * (bits_per_sample / 8);
    let Some(expected_average) = sample_rate.checked_mul(expected_align as u32) else {
        return Ok(DSERR_BADFORMAT);
    };
    if block_align != expected_align || average_bytes != expected_average {
        return Ok(DSERR_BADFORMAT);
    }
    p.extra.com.dsound_primary_formats.insert(
        device,
        format.try_into().expect("WAVEFORMATEX size checked"),
    );
    Ok(S_OK)
}

fn lock_buffer(p: &mut Process) -> R {
    let (this, offset, bytes) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u32);
    let (first_ptr_out, first_len_out, second_ptr_out, second_len_out) =
        (p.arg(3), p.arg(4), p.arg(5), p.arg(6));
    let flags = p.arg(7) as u32;
    if first_ptr_out == 0
        || first_len_out == 0
        || flags & !DSBLOCK_KNOWN_MASK != 0
        || flags & DSBLOCK_KNOWN_MASK == DSBLOCK_KNOWN_MASK
    {
        return Ok(E_INVALIDARG);
    }
    let Some(Kind::SoundBuffer { memory, size, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    if memory == 0 || size == 0 {
        return Ok(DSERR_CONTROLUNAVAIL);
    }
    if flags & DSBLOCK_ENTIREBUFFER != 0 {
        // Per DirectSound, ENTIREBUFFER ignores both offset and requested length.
        return lock_buffer_range(
            p,
            this,
            memory,
            size,
            0,
            size,
            first_ptr_out,
            first_len_out,
            second_ptr_out,
            second_len_out,
        );
    }
    // Playback is not modeled, so a stopped buffer's current write cursor is zero.
    let offset = if flags & DSBLOCK_FROMWRITECURSOR != 0 {
        0
    } else {
        offset
    };
    if offset >= size || bytes > size {
        return Ok(DSERR_INVALIDPARAM);
    }
    lock_buffer_range(
        p,
        this,
        memory,
        size,
        offset,
        bytes,
        first_ptr_out,
        first_len_out,
        second_ptr_out,
        second_len_out,
    )
}

#[allow(clippy::too_many_arguments)]
fn lock_buffer_range(
    p: &mut Process,
    this: u64,
    memory: u64,
    size: u32,
    offset: u32,
    bytes: u32,
    first_ptr_out: u64,
    first_len_out: u64,
    second_ptr_out: u64,
    second_len_out: u64,
) -> R {
    let Some(first_ptr) = memory.checked_add(offset as u64) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    let pointer_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    if !guest_range_has_protection(p, memory, size as u64, PROT_W)
        || !guest_range_has_protection(p, first_ptr_out, pointer_size, PROT_W)
        || !guest_range_has_protection(p, first_len_out, 4, PROT_W)
        || (second_ptr_out != 0
            && !guest_range_has_protection(p, second_ptr_out, pointer_size, PROT_W))
        || (second_len_out != 0 && !guest_range_has_protection(p, second_len_out, 4, PROT_W))
    {
        return Ok(DSERR_INVALIDPARAM);
    }
    let first_bytes = bytes.min(size - offset);
    let mut second_bytes = bytes.saturating_sub(first_bytes);
    let second_ptr = if second_bytes == 0 {
        0
    } else if second_ptr_out == 0 {
        // Without a second pointer output, DirectSound locks only the contiguous first part.
        second_bytes = 0;
        0
    } else {
        memory
    };
    p.wrptr(first_ptr_out, first_ptr)?;
    p.wr32(first_len_out, first_bytes)?;
    if second_ptr_out != 0 {
        p.wrptr(second_ptr_out, second_ptr)?;
    }
    if second_len_out != 0 {
        p.wr32(second_len_out, second_bytes)?;
    }
    p.extra
        .com
        .dsound_buffer_locks
        .entry(this)
        .or_default()
        .push(com::DSoundBufferLock {
            first_ptr,
            first_len: first_bytes,
            second_ptr,
            second_len: second_bytes,
        });
    Ok(S_OK)
}

fn guest_range_has_protection(p: &Process, address: u64, len: u64, required: u8) -> bool {
    if len == 0 {
        return true;
    }
    let Some(last) = address.checked_add(len - 1) else {
        return false;
    };
    if last >= (1u64 << 48) {
        return false;
    }
    let mut page = address & !(PAGE_SIZE - 1);
    let last_page = last & !(PAGE_SIZE - 1);
    loop {
        if p.mem
            .prot_of(page)
            .is_none_or(|prot| prot & required != required)
        {
            return false;
        }
        if page == last_page {
            return true;
        }
        let Some(next) = page.checked_add(PAGE_SIZE) else {
            return false;
        };
        page = next;
    }
}

fn unlock_buffer(p: &mut Process) -> R {
    let (this, first_ptr, first_bytes, second_ptr, second_bytes) = (
        p.arg(0),
        p.arg(1),
        p.arg(2) as u32,
        p.arg(3),
        p.arg(4) as u32,
    );
    if first_ptr == 0 {
        return Ok(DSERR_INVALIDPARAM);
    }
    if !matches!(com::kind_of(p, this), Some(Kind::SoundBuffer { .. })) {
        return Ok(DSERR_INVALIDPARAM);
    }
    let Some(locks) = p.extra.com.dsound_buffer_locks.get_mut(&this) else {
        return Ok(DSERR_INVALIDCALL);
    };
    // Unlock must use the exact returned pointers, including the NULL second pointer. The byte
    // counts may be smaller than the locked spans because they describe bytes actually written.
    let Some(index) = locks.iter().rposition(|lock| {
        lock.first_ptr == first_ptr
            && lock.second_ptr == second_ptr
            && first_bytes <= lock.first_len
            && second_bytes <= lock.second_len
    }) else {
        return Ok(DSERR_INVALIDPARAM);
    };
    locks.remove(index);
    if locks.is_empty() {
        p.extra.com.dsound_buffer_locks.remove(&this);
    }
    Ok(S_OK)
}

fn get_object_in_path(p: &mut Process) -> R {
    let out = p.arg(4);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(E_NOINTERFACE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::{
        cpu::RSP,
        mem::{PAGE_SIZE, PROT_RW},
        CpuMode,
    };

    const STACK: u64 = 0x10000;
    const INPUT_GUID: u64 = 0x20000;
    const OUTPUT: u64 = 0x21000;
    const DESC: u64 = 0x22000;
    const LISTENER_DATA: u64 = 0x23000;
    const FORMAT: u64 = 0x24000;
    const FORMAT_OUT: u64 = 0x25000;
    const CAPS_OUT: u64 = 0x26000;

    fn x86_process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        for base in [
            STACK,
            INPUT_GUID,
            OUTPUT,
            DESC,
            LISTENER_DATA,
            FORMAT,
            FORMAT_OUT,
            CAPS_OUT,
        ] {
            assert!(process.mem.map(base, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process
    }

    fn set_args(process: &mut Process, args: &[u32]) {
        for (index, value) in args.iter().enumerate() {
            process.wr32(STACK + 4 + 4 * index as u64, *value).unwrap();
        }
    }

    fn create_primary(process: &mut Process, sound: u64, caps: u32) -> u64 {
        process.wr32(DESC, DSBUFFERDESC_SIZE).unwrap();
        process.wr32(DESC + 4, caps).unwrap();
        process.wr32(DESC + 8, 0).unwrap();
        process.wr32(DESC + 12, 0).unwrap();
        process.wr32(DESC + 16, 0).unwrap();
        process.write_bytes(DESC + 20, &[0; 16]).unwrap();
        set_args(process, &[sound as u32, DESC as u32, OUTPUT as u32, 0]);
        assert_eq!(
            method("IDirectSound8", "CreateSoundBuffer").unwrap()(process).unwrap(),
            S_OK
        );
        process.rdptr(OUTPUT).unwrap()
    }

    fn pcm_format(channels: u16, sample_rate: u32, bits_per_sample: u16) -> [u8; 18] {
        let block_align = channels * (bits_per_sample / 8);
        let mut format = [0u8; WAVEFORMATEX_PCM_SIZE];
        format[0..2].copy_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
        format[2..4].copy_from_slice(&channels.to_le_bytes());
        format[4..8].copy_from_slice(&sample_rate.to_le_bytes());
        format[8..12].copy_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        format[12..14].copy_from_slice(&block_align.to_le_bytes());
        format[14..16].copy_from_slice(&bits_per_sample.to_le_bytes());
        format
    }

    fn set_secondary_desc(
        process: &mut Process,
        sound: u64,
        caps: u32,
        size: u32,
        reserved: u32,
        format: u64,
    ) {
        process.wr32(DESC, DSBUFFERDESC_SIZE).unwrap();
        process.wr32(DESC + 4, caps).unwrap();
        process.wr32(DESC + 8, size).unwrap();
        process.wr32(DESC + 12, reserved).unwrap();
        process.wr32(DESC + 16, format as u32).unwrap();
        process.write_bytes(DESC + 20, &[0; 16]).unwrap();
        set_args(process, &[sound as u32, DESC as u32, OUTPUT as u32, 0]);
    }

    fn create_secondary(
        process: &mut Process,
        sound: u64,
        caps: u32,
        size: u32,
        format: &[u8; WAVEFORMATEX_PCM_SIZE],
    ) -> u64 {
        process.write_bytes(FORMAT, format).unwrap();
        set_secondary_desc(process, sound, caps, size, 0, FORMAT);
        assert_eq!(
            method("IDirectSound8", "CreateSoundBuffer").unwrap()(process).unwrap(),
            S_OK
        );
        process.rdptr(OUTPUT).unwrap()
    }

    fn query_interface(process: &mut Process, object: u64, guid: [u8; 16]) -> u64 {
        process.write_bytes(INPUT_GUID, &guid).unwrap();
        set_args(process, &[object as u32, INPUT_GUID as u32, OUTPUT as u32]);
        assert_eq!(com::query_interface(process).unwrap(), S_OK);
        process.rdptr(OUTPUT).unwrap()
    }

    #[test]
    fn secondary_buffer_retains_pcm_format_and_guest_storage() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(1, 44_100, 16);
        let buffer = create_secondary(&mut process, sound, DSBCAPS_CTRL3D, 0x400, &format);

        assert!(matches!(
            com::kind_of(&process, buffer),
            Some(Kind::SoundBuffer {
                device,
                memory,
                size: 0x400,
                caps,
            }) if device == sound && memory != 0 && caps == DSBCAPS_CTRL3D | DSBCAPS_LOCSOFTWARE
        ));
        assert_eq!(
            process.extra.com.dsound_buffer_formats.get(&buffer),
            Some(&format)
        );

        process.wr32(CAPS_OUT, DSBCAPS_SIZE).unwrap();
        set_args(&mut process, &[buffer as u32, CAPS_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetCaps").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(
            process.rd32(CAPS_OUT + 4).unwrap(),
            DSBCAPS_CTRL3D | DSBCAPS_LOCSOFTWARE
        );
        assert_eq!(process.rd32(CAPS_OUT + 8).unwrap(), 0x400);

        set_args(&mut process, &[buffer as u32, 0, 0, OUTPUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFormat").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), WAVEFORMATEX_PCM_SIZE as u32);

        set_args(
            &mut process,
            &[
                buffer as u32,
                FORMAT_OUT as u32,
                WAVEFORMATEX_PCM_SIZE as u32,
                OUTPUT as u32,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFormat").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(
            process
                .read_bytes(FORMAT_OUT, WAVEFORMATEX_PCM_SIZE)
                .unwrap(),
            format
        );
        set_args(
            &mut process,
            &[buffer as u32, FORMAT_OUT as u32, 17, OUTPUT as u32],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFormat").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), WAVEFORMATEX_PCM_SIZE as u32);

        // Lock returns the actual guest-backed sample memory so guest writes persist in the
        // buffer instead of being acknowledged against a temporary or fabricated range.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0,
                0x400,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process).unwrap(),
            S_OK
        );
        let sample_memory = process.rdptr(OUTPUT).unwrap();
        assert_ne!(sample_memory, 0);
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), 0x400);
        assert_eq!(process.rd32(OUTPUT + 12).unwrap(), 0);
        process.write_bytes(sample_memory, &[0xa5, 0x5a]).unwrap();
        assert_eq!(process.read_bytes(sample_memory, 2).unwrap(), [0xa5, 0x5a]);
        set_args(
            &mut process,
            &[buffer as u32, sample_memory as u32, 2, 0, 0],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(!process.extra.com.dsound_buffer_locks.contains_key(&buffer));

        let buffer3d_iid = [
            0x86, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0x00, 0x20, 0xaf, 0x0b,
            0xe5, 0x60,
        ];
        let buffer3d = query_interface(&mut process, buffer, buffer3d_iid);
        assert_ne!(buffer3d, 0);
        set_args(&mut process, &[buffer3d as u32, LISTENER_DATA as u32, 0, 0]);
        assert!(matches!(
            method("IDirectSound3DBuffer", "SetPosition").unwrap()(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "dsound" && name.contains("SetPosition")
        ));

        assert_eq!(com::release_obj(&mut process, buffer3d), 0);
        assert!(process
            .extra
            .com
            .dsound_buffer_formats
            .contains_key(&buffer));
        assert_eq!(com::release_obj(&mut process, buffer), 0);
        assert!(!process
            .extra
            .com
            .dsound_buffer_formats
            .contains_key(&buffer));
    }

    #[test]
    fn lock_unlock_tracks_wrapped_guest_ranges_and_requires_matching_pointers() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(2, 44_100, 16);
        let buffer = create_secondary(&mut process, sound, 0, 0x400, &format);
        let memory = match com::kind_of(&process, buffer).unwrap() {
            Kind::SoundBuffer { memory, .. } => memory,
            _ => unreachable!(),
        };

        // A non-null but unmapped required output is rejected before partial output writes or
        // lock-state changes.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0,
                8,
                OUTPUT as u32,
                0xdead_beef,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(DSERR_INVALIDPARAM)
        );
        assert!(!process.extra.com.dsound_buffer_locks.contains_key(&buffer));

        assert!(process
            .mem
            .map(0x27000, PAGE_SIZE, anywasm_cpu::mem::PROT_R));
        set_args(
            &mut process,
            &[
                buffer as u32,
                0,
                8,
                OUTPUT as u32,
                0x27000,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(DSERR_INVALIDPARAM)
        );
        assert!(!process.extra.com.dsound_buffer_locks.contains_key(&buffer));

        // Eight bytes at the end plus eight bytes from the beginning gives two real guest spans.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0x3f8,
                16,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), memory + 0x3f8);
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), 8);
        assert_eq!(process.rdptr(OUTPUT + 8).unwrap(), memory);
        assert_eq!(process.rd32(OUTPUT + 12).unwrap(), 8);

        // DirectSound permits committing fewer bytes than were locked.
        set_args(
            &mut process,
            &[buffer as u32, (memory + 0x3f8) as u32, 4, memory as u32, 6],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(S_OK)
        );

        // Microsoft documents NULL for both second outputs as a request not to lock the wrap.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0x3f8,
                16,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                0,
                0,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), 8);
        set_args(
            &mut process,
            &[buffer as u32, (memory + 0x3f8) as u32, 8, 0, 0],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(!process.extra.com.dsound_buffer_locks.contains_key(&buffer));

        // A zero-byte Lock is still paired by the returned pointer, as in the GTA runtime trace.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0x10,
                0,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), memory + 0x10);
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), 0);
        assert_eq!(process.rdptr(OUTPUT + 8).unwrap(), 0);
        assert_eq!(process.rd32(OUTPUT + 12).unwrap(), 0);
        set_args(
            &mut process,
            &[buffer as u32, (memory + 0x10) as u32, 0, 0, 0],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(S_OK)
        );

        // A mismatched pointer pair fails without consuming the outstanding lock.
        set_args(
            &mut process,
            &[
                buffer as u32,
                0x20,
                8,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(S_OK)
        );
        let first_ptr = process.rdptr(OUTPUT).unwrap();
        set_args(
            &mut process,
            &[buffer as u32, first_ptr as u32, 0, 0xdead_beef, 0],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(DSERR_INVALIDPARAM)
        );
        set_args(&mut process, &[buffer as u32, first_ptr as u32, 0, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(S_OK)
        );
        set_args(&mut process, &[buffer as u32, first_ptr as u32, 0, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Unlock").unwrap()(&mut process),
            Ok(DSERR_INVALIDCALL)
        );

        set_args(
            &mut process,
            &[
                buffer as u32,
                0x10,
                4,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(process.extra.com.dsound_buffer_locks.contains_key(&buffer));
        assert_eq!(com::release_obj(&mut process, buffer), 0);
        assert!(!process.extra.com.dsound_buffer_locks.contains_key(&buffer));
    }

    #[test]
    fn gta_secondary_caps_0x182a0_create_stateful_controls_without_claiming_audio_effects() {
        const GTA_FLAGS: u32 = DSBCAPS_CTRLFREQUENCY
            | DSBCAPS_CTRLVOLUME
            | DSBCAPS_CTRLFX
            | DSBCAPS_GLOBALFOCUS
            | DSBCAPS_GETCURRENTPOSITION2;
        const CAPS_OUT: u64 = 0x2_6000;
        const VALUE_OUT: u64 = 0x2_5000;

        assert_eq!(GTA_FLAGS, 0x0001_82a0);
        let mut process = x86_process();
        assert!(process.mem.map(CAPS_OUT, 0x1000, PROT_RW));
        assert!(process.mem.map(VALUE_OUT, 0x1000, PROT_RW));
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(2, 44_100, 16);
        let buffer = create_secondary(&mut process, sound, GTA_FLAGS, 0x400, &format);

        assert!(matches!(
            com::kind_of(&process, buffer),
            Some(Kind::SoundBuffer { caps, size: 0x400, .. })
                if caps == GTA_FLAGS | DSBCAPS_LOCSOFTWARE
        ));
        process.wr32(CAPS_OUT, DSBCAPS_SIZE).unwrap();
        set_args(&mut process, &[buffer as u32, CAPS_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetCaps").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(
            process.rd32(CAPS_OUT + 4).unwrap(),
            GTA_FLAGS | DSBCAPS_LOCSOFTWARE
        );
        assert_eq!(process.rd32(CAPS_OUT + 8).unwrap(), 0x400);

        set_args(&mut process, &[buffer as u32, (-750i32) as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetVolume").unwrap()(&mut process),
            Ok(S_OK)
        );
        set_args(&mut process, &[buffer as u32, VALUE_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetVolume").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(VALUE_OUT).unwrap() as i32, -750);
        set_args(&mut process, &[buffer as u32, 1]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetVolume").unwrap()(&mut process),
            Ok(DSERR_INVALIDPARAM)
        );

        set_args(&mut process, &[buffer as u32, 22_050]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFrequency").unwrap()(&mut process),
            Ok(S_OK)
        );
        set_args(&mut process, &[buffer as u32, VALUE_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFrequency").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(VALUE_OUT).unwrap(), 22_050);
        set_args(&mut process, &[buffer as u32, DSBFREQUENCY_ORIGINAL]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFrequency").unwrap()(&mut process),
            Ok(S_OK)
        );
        set_args(&mut process, &[buffer as u32, VALUE_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFrequency").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(VALUE_OUT).unwrap(), 44_100);
        set_args(&mut process, &[buffer as u32, 99]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFrequency").unwrap()(&mut process),
            Ok(DSERR_INVALIDPARAM)
        );

        set_args(&mut process, &[buffer as u32, 0, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFX").unwrap()(&mut process),
            Ok(S_OK)
        );
        let effect_size = effect_desc_size(&process);
        process.wr32(DESC, effect_size).unwrap();
        process.wr32(DESC + 4, DSFX_LOCSOFTWARE).unwrap();
        process.wr32(DESC + 24, 0).unwrap();
        process.wr32(DESC + 28, 0).unwrap();
        set_args(
            &mut process,
            &[buffer as u32, 1, DESC as u32, OUTPUT as u32],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFX").unwrap()(&mut process),
            Ok(DSERR_CONTROLUNAVAIL)
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), DSFXR_UNALLOCATED);
        set_args(&mut process, &[buffer as u32, 0, 0, 0]);
        assert!(matches!(
            method("IDirectSoundBuffer8", "Play").unwrap()(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "dsound" && name.contains("WebAudio output is unavailable")
        ));
    }

    #[test]
    fn secondary_buffer_rejects_malformed_descriptors_and_formats() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let pcm = pcm_format(1, 44_100, 16);
        let call_create = |process: &mut Process,
                           caps: u32,
                           size: u32,
                           reserved: u32,
                           format: u64,
                           desc_size: u32|
         -> Result<u64, Stop> {
            process.wr32(DESC, desc_size).unwrap();
            process.wr32(DESC + 4, caps).unwrap();
            process.wr32(DESC + 8, size).unwrap();
            process.wr32(DESC + 12, reserved).unwrap();
            process.wr32(DESC + 16, format as u32).unwrap();
            process.write_bytes(DESC + 20, &[0; 16]).unwrap();
            process.wrptr(OUTPUT, 0xdead_beef).unwrap();
            set_args(process, &[sound as u32, DESC as u32, OUTPUT as u32, 0]);
            method("IDirectSound8", "CreateSoundBuffer").unwrap()(process)
        };

        process.write_bytes(FORMAT, &pcm).unwrap();
        assert_eq!(
            call_create(&mut process, 0, 0x400, 0, FORMAT, 20).unwrap(),
            E_INVALIDARG
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), 0);
        assert_eq!(
            call_create(&mut process, 0, 0x400, 1, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            E_INVALIDARG
        );
        assert_eq!(
            call_create(&mut process, 0, 0x400, 0, 0, DSBUFFERDESC_SIZE).unwrap(),
            E_INVALIDARG
        );
        assert_eq!(
            call_create(&mut process, 0, 3, 0, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(
            call_create(&mut process, 0, 0x401, 0, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(
            call_create(
                &mut process,
                DSBCAPS_CTRL3D | DSBCAPS_CTRLPAN,
                0x400,
                0,
                FORMAT,
                DSBUFFERDESC_SIZE,
            )
            .unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(
            call_create(
                &mut process,
                0x8000_0000,
                0x400,
                0,
                FORMAT,
                DSBUFFERDESC_SIZE
            )
            .unwrap(),
            DSERR_INVALIDPARAM
        );

        let mut malformed = pcm;
        malformed[2..4].copy_from_slice(&3u16.to_le_bytes());
        process.write_bytes(FORMAT, &malformed).unwrap();
        assert_eq!(
            call_create(&mut process, 0, 0x400, 0, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            DSERR_BADFORMAT
        );
        let mut malformed = pcm;
        malformed[12..14].copy_from_slice(&1u16.to_le_bytes());
        process.write_bytes(FORMAT, &malformed).unwrap();
        assert_eq!(
            call_create(&mut process, 0, 0x400, 0, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            DSERR_BADFORMAT
        );
        let mut malformed = pcm;
        malformed[8..12].copy_from_slice(&1u32.to_le_bytes());
        process.write_bytes(FORMAT, &malformed).unwrap();
        assert_eq!(
            call_create(&mut process, 0, 0x400, 0, FORMAT, DSBUFFERDESC_SIZE).unwrap(),
            DSERR_BADFORMAT
        );
        let mut malformed = pcm_format(2, 44_100, 16);
        process.write_bytes(FORMAT, &malformed).unwrap();
        assert_eq!(
            call_create(
                &mut process,
                DSBCAPS_CTRL3D,
                0x400,
                0,
                FORMAT,
                DSBUFFERDESC_SIZE,
            )
            .unwrap(),
            DSERR_BADFORMAT
        );
        malformed[0..2].copy_from_slice(&0x55u16.to_le_bytes());
        process.write_bytes(FORMAT, &malformed).unwrap();
        assert!(matches!(
            call_create(&mut process, 0, 0x400, 0, FORMAT, DSBUFFERDESC_SIZE),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "dsound" && name.contains("WAVEFORMATEX tag")
        ));
        process.write_bytes(FORMAT, &pcm).unwrap();
        assert_eq!(
            call_create(
                &mut process,
                DSBCAPS_CTRLVOLUME,
                0x400,
                0,
                FORMAT,
                DSBUFFERDESC_SIZE,
            )
            .unwrap(),
            S_OK
        );
        let volume_buffer = process.rdptr(OUTPUT).unwrap();
        assert_ne!(volume_buffer, 0);
        assert_eq!(com::release_obj(&mut process, volume_buffer), 0);
    }

    fn float_bits(value: f32) -> u32 {
        value.to_bits()
    }

    fn listener_iid() -> [u8; 16] {
        [
            0x84, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0x00, 0x20, 0xaf, 0x0b,
            0xe5, 0x60,
        ]
    }

    #[test]
    fn playback_reports_a_coverage_gap_without_an_audio_backend() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(2, 44_100, 16);
        let buffer = create_secondary(&mut process, sound, 0, 0x2000, &format);
        set_args(&mut process, &[buffer as u32, 0, 0, DSBPLAY_LOOPING]);
        assert!(matches!(
            method("IDirectSoundBuffer8", "Play").unwrap()(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "dsound" && name.contains("WebAudio output is unavailable")
        ));
    }

    #[test]
    fn looping_pcm_playback_updates_status_position_and_emits_audio_events() {
        const PLAY_OUT: u64 = 0x27000;
        const WRITE_OUT: u64 = 0x28000;
        let mut process = x86_process();
        assert!(process.mem.map(PLAY_OUT, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(WRITE_OUT, PAGE_SIZE, PROT_RW));
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(2, 44_100, 16);
        let buffer = create_secondary(
            &mut process,
            sound,
            DSBCAPS_CTRLVOLUME | DSBCAPS_CTRLFREQUENCY,
            0x2_0000,
            &format,
        );
        let memory = match com::kind_of(&process, buffer).unwrap() {
            Kind::SoundBuffer { memory, .. } => memory,
            _ => unreachable!(),
        };
        process.write_bytes(memory, &vec![0x80; 0x2_0000]).unwrap();
        process.extra.com.dsound_audio_enabled = true;

        set_args(&mut process, &[buffer as u32, 0, 0, DSBPLAY_LOOPING]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Play").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(
            matches!(process.extra.com.dsound_audio_events.as_slice(), [com::DSoundAudioEvent::Play { samples, looping: true, .. }] if samples.len() == 0x2_0000)
        );
        set_args(&mut process, &[buffer as u32, PLAY_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetStatus").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(
            process.rd32(PLAY_OUT).unwrap(),
            DSBSTATUS_PLAYING | DSBSTATUS_LOOPING
        );

        // Replaying a buffer that is already active must not reset its cursor or emit a second start.
        set_args(&mut process, &[buffer as u32, 0, 0, DSBPLAY_LOOPING]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Play").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.extra.com.dsound_audio_events.len(), 1);

        process.advance_time_100ns(1_000_000); // 100 ms at 44.1 kHz stereo 16-bit
        set_args(
            &mut process,
            &[buffer as u32, PLAY_OUT as u32, WRITE_OUT as u32],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "GetCurrentPosition").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(PLAY_OUT).unwrap(), 17_640);
        assert_eq!(
            process.rd32(WRITE_OUT).unwrap(),
            (17_640 + 1_764) % 0x2_0000
        );

        set_args(&mut process, &[buffer as u32, 4]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetCurrentPosition").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(
            matches!(process.extra.com.dsound_audio_events.last(), Some(com::DSoundAudioEvent::Seek { buffer: id, cursor: 4 }) if *id == buffer)
        );
        set_args(&mut process, &[buffer as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Stop").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert!(
            matches!(process.extra.com.dsound_audio_events.last(), Some(com::DSoundAudioEvent::Stop { buffer: id }) if *id == buffer)
        );
        set_args(&mut process, &[buffer as u32, PLAY_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetStatus").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(PLAY_OUT).unwrap(), 0);

        set_args(&mut process, &[buffer as u32, 0, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "Play").unwrap()(&mut process),
            Ok(S_OK)
        );
        process.advance_time_100ns(10_000_000); // one second runs past the 0.74 second buffer
        set_args(&mut process, &[buffer as u32, PLAY_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetStatus").unwrap()(&mut process),
            Ok(S_OK)
        );
        assert_eq!(process.rd32(PLAY_OUT).unwrap(), 0);
    }

    #[test]
    fn acquire_resources_uses_deferred_software_voice_flags_and_reports_unsupported_effects() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let format = pcm_format(1, 22_050, 8);
        let buffer = create_secondary(
            &mut process,
            sound,
            DSBCAPS_LOCDEFER | DSBCAPS_CTRLFX,
            0x1000,
            &format,
        );

        set_args(&mut process, &[buffer as u32, DSBPLAY_LOCSOFTWARE, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "AcquireResources").unwrap()(&mut process),
            Ok(S_OK)
        );
        set_args(
            &mut process,
            &[
                buffer as u32,
                DSBPLAY_LOCSOFTWARE | DSBPLAY_TERMINATEBY_TIME,
                0,
                0,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "AcquireResources").unwrap()(&mut process),
            Ok(DSERR_CONTROLUNAVAIL)
        );
        set_args(&mut process, &[buffer as u32, DSBPLAY_LOCHARDWARE, 0, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "AcquireResources").unwrap()(&mut process),
            Ok(DSERR_CONTROLUNAVAIL)
        );
        set_args(
            &mut process,
            &[buffer as u32, DSBPLAY_LOCSOFTWARE, 1, OUTPUT as u32],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "AcquireResources").unwrap()(&mut process),
            Ok(DSERR_CONTROLUNAVAIL)
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), DSFXR_UNALLOCATED);
    }

    #[test]
    fn unmodeled_sound_setters_report_coverage_gaps() {
        let mut process = x86_process();
        let buffer = com::create(
            &mut process,
            "IDirectSoundBuffer8",
            Kind::SoundBuffer {
                device: 1,
                memory: 0,
                size: 0,
                caps: DSBCAPS_CTRLVOLUME | DSBCAPS_CTRLPAN | DSBCAPS_CTRLFREQUENCY,
            },
        );
        for (method_name, args) in [
            ("SetPan", vec![buffer as u32, 0]),
            ("SetFormat", vec![buffer as u32, FORMAT as u32]),
        ] {
            set_args(&mut process, &args);
            let result = method("IDirectSoundBuffer8", method_name).unwrap()(&mut process);
            assert!(matches!(
                result,
                Err(Stop::Unimplemented { ref dll, ref name })
                    if dll == "dsound" && name.contains(method_name)
            ));
        }
    }

    #[test]
    fn direct_sound_create_resolves_the_virtual_default_playback_device() {
        let mut process = x86_process();
        set_args(&mut process, &[0, OUTPUT as u32, 0]);

        assert_eq!(lookup("#1").unwrap()(&mut process).unwrap(), S_OK);
        let object = process.rdptr(OUTPUT).unwrap();
        assert_ne!(object, 0);
        assert!(matches!(
            com::kind_of(&process, object),
            Some(Kind::DirectSound)
        ));
        assert_eq!(stdcall_stack_bytes("#1"), Some(12));
    }

    #[test]
    fn get_device_id_maps_playback_aliases_and_rejects_unmodeled_devices() {
        let mut process = x86_process();
        process
            .write_bytes(INPUT_GUID, &DSDEVID_DEFAULT_PLAYBACK)
            .unwrap();
        set_args(&mut process, &[INPUT_GUID as u32, OUTPUT as u32]);

        assert_eq!(lookup("#9").unwrap()(&mut process).unwrap(), S_OK);
        assert_eq!(
            process.read_bytes(OUTPUT, 16).unwrap(),
            VIRTUAL_PLAYBACK_DEVICE
        );
        assert_eq!(stdcall_stack_bytes("#9"), Some(8));

        process
            .write_bytes(INPUT_GUID, &DSDEVID_DEFAULT_VOICE_PLAYBACK)
            .unwrap();
        assert_eq!(lookup("#9").unwrap()(&mut process).unwrap(), S_OK);
        assert_eq!(
            process.read_bytes(OUTPUT, 16).unwrap(),
            VIRTUAL_PLAYBACK_DEVICE
        );

        process
            .write_bytes(INPUT_GUID, &DSDEVID_DEFAULT_CAPTURE)
            .unwrap();
        assert_eq!(
            lookup("#9").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );

        process.write_bytes(INPUT_GUID, &[0x77; 16]).unwrap();
        assert_eq!(
            lookup("#9").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );
    }

    #[test]
    fn direct_sound_create_rejects_non_playback_guids_and_aggregation() {
        let mut process = x86_process();
        process
            .write_bytes(INPUT_GUID, &DSDEVID_DEFAULT_CAPTURE)
            .unwrap();
        set_args(&mut process, &[INPUT_GUID as u32, OUTPUT as u32, 0]);

        assert_eq!(
            lookup("#1").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), 0);

        set_args(&mut process, &[0, OUTPUT as u32, 1]);
        assert_eq!(
            lookup("#1").unwrap()(&mut process).unwrap(),
            CLASS_E_NOAGGREGATION
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), 0);
    }

    #[test]
    fn set_cooperative_level_validates_and_persists_per_device_state() {
        const HWND_GAME: u64 = 0x30000;
        const HWND_CHILD: u64 = 0x30004;
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let window = |style| super::user32::Window {
            class: "GameWindow".into(),
            wndproc: 0,
            title: "Game".into(),
            x: 0,
            y: 0,
            w: 640,
            h: 480,
            client_x: 0,
            client_y: 0,
            client_w: 640,
            client_h: 480,
            visible: true,
            style,
            ex_style: 0,
            has_menu: false,
            menu: 0,
            hinstance: 0,
            user_data: 0,
            parent: 0,
            control_id: 0,
            unicode: false,
            needs_paint: false,
            gpu_id: 0,
        };
        process.extra.win.windows.insert(HWND_GAME, window(0));
        process
            .extra
            .win
            .windows
            .insert(HWND_CHILD, window(WS_CHILD));

        let set_level = method("IDirectSound8", "SetCooperativeLevel").unwrap();
        for level in [
            DSSCL_NORMAL,
            DSSCL_PRIORITY,
            DSSCL_EXCLUSIVE,
            DSSCL_WRITEPRIMARY,
        ] {
            set_args(&mut process, &[sound as u32, HWND_GAME as u32, level]);
            assert_eq!(set_level(&mut process).unwrap(), S_OK, "level {level}");
            assert_eq!(
                process.extra.com.dsound_cooperative.get(&sound),
                Some(&(HWND_GAME, level))
            );
        }

        // DirectSound documents GetDesktopWindow as the fallback when no app
        // foreground window is available.
        set_args(
            &mut process,
            &[sound as u32, HWND_DESKTOP as u32, DSSCL_PRIORITY],
        );
        assert_eq!(set_level(&mut process).unwrap(), S_OK);
        assert_eq!(
            process.extra.com.dsound_cooperative.get(&sound),
            Some(&(HWND_DESKTOP, DSSCL_PRIORITY))
        );

        for (hwnd, level) in [
            (0, DSSCL_PRIORITY),
            (0xdead_beef, DSSCL_PRIORITY),
            (HWND_CHILD as u32, DSSCL_PRIORITY),
            (HWND_GAME as u32, 0),
            (HWND_GAME as u32, 5),
        ] {
            set_args(&mut process, &[sound as u32, hwnd, level]);
            assert_eq!(
                set_level(&mut process).unwrap(),
                DSERR_INVALIDPARAM,
                "hwnd={hwnd:#x}, level={level}"
            );
            assert_eq!(
                process.extra.com.dsound_cooperative.get(&sound),
                Some(&(HWND_DESKTOP, DSSCL_PRIORITY)),
                "failed calls must preserve the prior cooperative state"
            );
        }

        set_args(
            &mut process,
            &[0xdead_beef, HWND_GAME as u32, DSSCL_PRIORITY],
        );
        assert_eq!(set_level(&mut process).unwrap(), DSERR_INVALIDPARAM);

        assert_eq!(com::release_obj(&mut process, sound), 0);
        assert!(!process.extra.com.dsound_cooperative.contains_key(&sound));
    }

    #[test]
    fn primary_buffer_has_device_semantics_and_requires_priority_to_change_format() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let primary = create_primary(&mut process, sound, DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D);
        assert!(matches!(
            com::kind_of(&process, primary),
            Some(Kind::SoundBuffer {
                device,
                memory: 0,
                size: 0,
                caps,
            }) if device == sound && caps == DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D
        ));
        assert_eq!(
            process.extra.com.objs.get(&primary).unwrap().iface,
            "IDirectSoundBuffer"
        );

        process.wr32(CAPS_OUT, DSBCAPS_SIZE).unwrap();
        set_args(&mut process, &[primary as u32, CAPS_OUT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "GetCaps").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(
            process.rd32(CAPS_OUT + 4).unwrap(),
            DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D
        );
        assert_eq!(process.rd32(CAPS_OUT + 8).unwrap(), 0);

        set_args(&mut process, &[sound as u32, DESC as u32, OUTPUT as u32, 0]);
        assert_eq!(
            method("IDirectSound8", "CreateSoundBuffer").unwrap()(&mut process).unwrap(),
            DSERR_ALLOCATED
        );
        assert_eq!(process.rdptr(OUTPUT).unwrap(), 0);

        let buffer8_iid = [
            0x49, 0xa4, 0x25, 0x68, 0x24, 0x75, 0x82, 0x4d, 0x92, 0x0f, 0x50, 0xe3, 0x6a, 0xb3,
            0xab, 0x1e,
        ];
        process.write_bytes(INPUT_GUID, &buffer8_iid).unwrap();
        set_args(
            &mut process,
            &[primary as u32, INPUT_GUID as u32, OUTPUT as u32],
        );
        assert_eq!(com::query_interface(&mut process).unwrap(), E_NOINTERFACE);
        assert_eq!(process.rdptr(OUTPUT).unwrap(), 0);

        set_args(
            &mut process,
            &[
                primary as u32,
                0,
                64,
                OUTPUT as u32,
                (OUTPUT + 4) as u32,
                (OUTPUT + 8) as u32,
                (OUTPUT + 12) as u32,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "Lock").unwrap()(&mut process).unwrap(),
            DSERR_CONTROLUNAVAIL
        );
        set_args(&mut process, &[primary as u32, 0]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetCurrentPosition").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDCALL
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "SetVolume").unwrap()(&mut process).unwrap(),
            DSERR_CONTROLUNAVAIL
        );
        set_args(&mut process, &[primary as u32, FORMAT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFormat").unwrap()(&mut process).unwrap(),
            DSERR_PRIOLEVELNEEDED
        );

        set_args(
            &mut process,
            &[sound as u32, HWND_DESKTOP as u32, DSSCL_PRIORITY],
        );
        assert_eq!(
            method("IDirectSound8", "SetCooperativeLevel").unwrap()(&mut process).unwrap(),
            S_OK
        );
        let mut format = default_primary_format();
        format[2..4].copy_from_slice(&1u16.to_le_bytes());
        format[4..8].copy_from_slice(&48_000u32.to_le_bytes());
        format[8..12].copy_from_slice(&96_000u32.to_le_bytes());
        format[12..14].copy_from_slice(&2u16.to_le_bytes());
        process.write_bytes(FORMAT, &format).unwrap();
        set_args(&mut process, &[primary as u32, FORMAT as u32]);
        assert_eq!(
            method("IDirectSoundBuffer8", "SetFormat").unwrap()(&mut process).unwrap(),
            S_OK
        );
        set_args(
            &mut process,
            &[
                primary as u32,
                FORMAT_OUT as u32,
                WAVEFORMATEX_PCM_SIZE as u32,
                OUTPUT as u32,
            ],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFormat").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), WAVEFORMATEX_PCM_SIZE as u32);
        assert_eq!(process.read_bytes(FORMAT_OUT, 18).unwrap(), format);

        set_args(
            &mut process,
            &[primary as u32, FORMAT_OUT as u32, 8, OUTPUT as u32],
        );
        assert_eq!(
            method("IDirectSoundBuffer8", "GetFormat").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );

        assert_eq!(com::release_obj(&mut process, primary), 0);
        assert!(process
            .extra
            .com
            .dsound_primary_formats
            .get(&sound)
            .is_none());
    }

    #[test]
    fn listener_parameters_are_stateful_and_deferred_updates_commit_together() {
        let mut process = x86_process();
        let sound = com::create(&mut process, "IDirectSound8", Kind::DirectSound);
        let primary = create_primary(&mut process, sound, DSBCAPS_PRIMARYBUFFER | DSBCAPS_CTRL3D);
        let listener = query_interface(&mut process, primary, listener_iid());
        assert_ne!(listener, primary);

        process.wr32(LISTENER_DATA, DS3DLISTENER_SIZE).unwrap();
        set_args(&mut process, &[listener as u32, LISTENER_DATA as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetAllParameters").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(LISTENER_DATA + 28).unwrap(), 0);
        assert_eq!(process.rd32(LISTENER_DATA + 36).unwrap(), float_bits(1.0));
        assert_eq!(process.rd32(LISTENER_DATA + 44).unwrap(), float_bits(1.0));
        assert_eq!(process.rd32(LISTENER_DATA + 52).unwrap(), float_bits(1.0));
        assert_eq!(process.rd32(LISTENER_DATA + 56).unwrap(), float_bits(1.0));
        assert_eq!(process.rd32(LISTENER_DATA + 60).unwrap(), float_bits(1.0));

        set_args(
            &mut process,
            &[
                listener as u32,
                float_bits(2.0),
                float_bits(3.0),
                float_bits(4.0),
                DS3D_DEFERRED,
            ],
        );
        assert_eq!(
            method("IDirectSound3DListener", "SetPosition").unwrap()(&mut process).unwrap(),
            S_OK
        );
        set_args(
            &mut process,
            &[
                listener as u32,
                float_bits(0.5),
                float_bits(1.5),
                float_bits(2.5),
                DS3D_DEFERRED,
            ],
        );
        assert_eq!(
            method("IDirectSound3DListener", "SetVelocity").unwrap()(&mut process).unwrap(),
            S_OK
        );

        set_args(&mut process, &[listener as u32, OUTPUT as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetPosition").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), float_bits(0.0));

        set_args(&mut process, &[listener as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "CommitDeferredSettings").unwrap()(&mut process)
                .unwrap(),
            S_OK
        );
        set_args(&mut process, &[listener as u32, OUTPUT as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetPosition").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), float_bits(2.0));
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), float_bits(3.0));
        assert_eq!(process.rd32(OUTPUT + 8).unwrap(), float_bits(4.0));
        set_args(&mut process, &[listener as u32, OUTPUT as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetVelocity").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), float_bits(0.5));
        assert_eq!(process.rd32(OUTPUT + 4).unwrap(), float_bits(1.5));
        assert_eq!(process.rd32(OUTPUT + 8).unwrap(), float_bits(2.5));

        set_args(
            &mut process,
            &[listener as u32, float_bits(2.25), DS3D_IMMEDIATE],
        );
        assert_eq!(
            method("IDirectSound3DListener", "SetDistanceFactor").unwrap()(&mut process).unwrap(),
            S_OK
        );
        set_args(&mut process, &[listener as u32, OUTPUT as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetDistanceFactor").unwrap()(&mut process).unwrap(),
            S_OK
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), float_bits(2.25));

        set_args(
            &mut process,
            &[listener as u32, float_bits(0.0), DS3D_IMMEDIATE],
        );
        assert_eq!(
            method("IDirectSound3DListener", "SetDistanceFactor").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );
        assert_eq!(process.rd32(OUTPUT).unwrap(), float_bits(2.25));
        set_args(&mut process, &[listener as u32, 1, DS3D_IMMEDIATE]);
        assert_eq!(
            method("IDirectSound3DListener", "SetDistanceFactor").unwrap()(&mut process).unwrap(),
            DSERR_INVALIDPARAM
        );

        process.wr32(LISTENER_DATA, 0).unwrap();
        set_args(&mut process, &[listener as u32, LISTENER_DATA as u32]);
        assert_eq!(
            method("IDirectSound3DListener", "GetAllParameters").unwrap()(&mut process).unwrap(),
            E_INVALIDARG
        );
        assert_eq!(com::release_obj(&mut process, primary), 0);
        assert_eq!(com::release_obj(&mut process, listener), 0);
    }
}
