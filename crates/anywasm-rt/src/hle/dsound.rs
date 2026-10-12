//! DirectSound 8 compatibility used to keep legacy Win32 startup moving.
//!
//! Sound buffers are guest-memory backed for startup compatibility. No audio is emitted to the
//! browser yet, and playback reports `E_NOTIMPL` instead of claiming to play silently.

use super::com::{self, Kind, S_OK};
use super::*;
use crate::process::Process;

const E_INVALIDARG: u64 = 0x8007_0057;
const E_NOINTERFACE: u64 = 0x8000_4002;
const E_NOTIMPL: u64 = 0x8000_4001;
const DSBUFFERDESC_SIZE: u32 = 36;
const DSBCAPS_SIZE: u32 = 20;
const MAX_SOUND_BUFFER: u32 = 16 * 1024 * 1024;
pub const DSBCAPS_PRIMARYBUFFER: u32 = 0x1;
pub const DSBCAPS_CTRL3D: u32 = 0x10;
const IID_IDIRECTSOUND3DLISTENER: &str = "279afa84-4981-11ce-a521-0020af0be560";

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

pub fn methods(interface: &str) -> Option<&'static [&'static str]> {
    (interface == "IDirectSound3DListener").then_some(LISTENER_METHODS)
}

pub fn iid_name(iid: &str) -> Option<&'static str> {
    (iid == IID_IDIRECTSOUND3DLISTENER).then_some("IDirectSound3DListener")
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "#11" => direct_sound_create8,
        _ => return None,
    })
}

pub fn method(iface: &str, name: &str) -> Option<HleFn> {
    Some(match (iface, name) {
        ("IDirectSound8", "CreateSoundBuffer") => create_sound_buffer,
        ("IDirectSound8", "GetCaps") => get_sound_caps,
        ("IDirectSound8", "DuplicateSoundBuffer") => duplicate_sound_buffer,
        ("IDirectSound8", "GetSpeakerConfig") => get_speaker_config,
        ("IDirectSound8", "VerifyCertification") => verify_certification,
        ("IDirectSound8", "SetCooperativeLevel")
        | ("IDirectSound8", "Compact")
        | ("IDirectSound8", "SetSpeakerConfig")
        | ("IDirectSound8", "Initialize") => |_| Ok(S_OK),
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
        ("IDirectSound3DListener", "SetAllParameters")
        | ("IDirectSound3DListener", "SetDistanceFactor")
        | ("IDirectSound3DListener", "SetDopplerFactor")
        | ("IDirectSound3DListener", "SetOrientation")
        | ("IDirectSound3DListener", "SetPosition")
        | ("IDirectSound3DListener", "SetRolloffFactor")
        | ("IDirectSound3DListener", "SetVelocity")
        | ("IDirectSound3DListener", "CommitDeferredSettings") => |_| Ok(S_OK),
        ("IDirectSoundBuffer8", "Initialize")
        | ("IDirectSoundBuffer8", "SetCurrentPosition")
        | ("IDirectSoundBuffer8", "SetFormat")
        | ("IDirectSoundBuffer8", "SetVolume")
        | ("IDirectSoundBuffer8", "SetPan")
        | ("IDirectSoundBuffer8", "SetFrequency")
        | ("IDirectSoundBuffer8", "Stop")
        | ("IDirectSoundBuffer8", "Unlock")
        | ("IDirectSoundBuffer8", "Restore") => |_| Ok(S_OK),
        ("IDirectSoundBuffer8", "Play") => playback_not_implemented,
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

fn create_sound_buffer(p: &mut Process) -> R {
    let (desc, out, outer) = (p.arg(1), p.arg(2), p.arg(3));
    if desc == 0 || out == 0 || outer != 0 {
        return Ok(E_INVALIDARG);
    }
    let desc_size = p.rd32(desc)?;
    if desc_size < DSBUFFERDESC_SIZE {
        p.wrptr(out, 0)?;
        return Ok(E_INVALIDARG);
    }
    let caps = p.rd32(desc + 4)?;
    let size = p.rd32(desc + 8)?.clamp(1, MAX_SOUND_BUFFER);
    let memory = super::kernel32::alloc_block(p, size as u64, true);
    let buffer = com::create(
        p,
        "IDirectSoundBuffer8",
        Kind::SoundBuffer { memory, size, caps },
    );
    p.wrptr(out, buffer)?;
    Ok(S_OK)
}

fn duplicate_sound_buffer(p: &mut Process) -> R {
    let (source, out) = (p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    let Some(Kind::SoundBuffer { memory, size, caps }) = com::kind_of(p, source) else {
        p.wrptr(out, 0)?;
        return Ok(E_INVALIDARG);
    };
    let duplicate_memory = super::kernel32::alloc_block(p, size as u64, true);
    let bytes = p.read_bytes(memory, size as usize)?;
    p.write_bytes(duplicate_memory, &bytes)?;
    let duplicate = com::create(
        p,
        "IDirectSoundBuffer8",
        Kind::SoundBuffer {
            memory: duplicate_memory,
            size,
            caps,
        },
    );
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
    let caps = match com::kind_of(p, p.arg(0)) {
        Some(Kind::SoundBuffer { caps, .. }) => caps,
        _ => 0,
    };
    p.wr32(out + 4, caps)?;
    let buffer_size = match com::kind_of(p, p.arg(0)) {
        Some(Kind::SoundBuffer { size, .. }) => size,
        _ => 0,
    };
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
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_INVALIDARG);
    }
    p.write_bytes(out, &[0; 64])?;
    p.wr32(out, 64)?;
    p.wr32(out + 36, 0x3f80_0000)?;
    p.wr32(out + 44, 0x3f80_0000)?;
    p.wr32(out + 52, 0x3f80_0000)?;
    p.wr32(out + 56, 0x3f80_0000)?;
    p.wr32(out + 60, 0x3f80_0000)?;
    Ok(S_OK)
}

fn get_listener_distance_factor(p: &mut Process) -> R {
    write_float(p, p.arg(1), 1.0)
}

fn get_listener_doppler_factor(p: &mut Process) -> R {
    write_float(p, p.arg(1), 1.0)
}

fn get_listener_orientation(p: &mut Process) -> R {
    write_vector(p, p.arg(1), [0.0, 0.0, 1.0])?;
    write_vector(p, p.arg(2), [0.0, 1.0, 0.0])
}

fn get_listener_position(p: &mut Process) -> R {
    write_vector(p, p.arg(1), [0.0; 3])
}

fn get_listener_rolloff_factor(p: &mut Process) -> R {
    write_float(p, p.arg(1), 1.0)
}

fn get_listener_velocity(p: &mut Process) -> R {
    write_vector(p, p.arg(1), [0.0; 3])
}

fn get_current_position(p: &mut Process) -> R {
    for out in [p.arg(1), p.arg(2)] {
        if out != 0 {
            p.wr32(out, 0)?;
        }
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
    write_u32(p, 0)
}

fn get_pan(p: &mut Process) -> R {
    write_u32(p, 0)
}

fn get_frequency(p: &mut Process) -> R {
    write_u32(p, 44_100)
}

fn get_status(p: &mut Process) -> R {
    write_u32(p, 0)
}

fn get_format(p: &mut Process) -> R {
    let (out, capacity, written) = (p.arg(1), p.arg(2) as usize, p.arg(3));
    const FORMAT_SIZE: usize = 18;
    if out != 0 && capacity >= FORMAT_SIZE {
        let mut format = [0u8; FORMAT_SIZE];
        format[0..2].copy_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
        format[2..4].copy_from_slice(&2u16.to_le_bytes());
        format[4..8].copy_from_slice(&44_100u32.to_le_bytes());
        format[8..12].copy_from_slice(&176_400u32.to_le_bytes());
        format[12..14].copy_from_slice(&4u16.to_le_bytes());
        format[14..16].copy_from_slice(&16u16.to_le_bytes());
        p.write_bytes(out, &format)?;
    }
    if written != 0 {
        p.wr32(written, FORMAT_SIZE as u32)?;
    }
    Ok(S_OK)
}

fn lock_buffer(p: &mut Process) -> R {
    let (this, offset, bytes) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u32);
    let (first_ptr, first_len, second_ptr, second_len) = (p.arg(3), p.arg(4), p.arg(5), p.arg(6));
    if first_ptr == 0 || first_len == 0 {
        return Ok(E_INVALIDARG);
    }
    let Some(Kind::SoundBuffer { memory, size, .. }) = com::kind_of(p, this) else {
        return Ok(E_INVALIDARG);
    };
    let offset = offset % size;
    let first_bytes = bytes.min(size - offset);
    let second_bytes = bytes.saturating_sub(first_bytes).min(size);
    p.wrptr(first_ptr, memory + offset as u64)?;
    p.wr32(first_len, first_bytes)?;
    if second_ptr != 0 {
        p.wrptr(second_ptr, if second_bytes != 0 { memory } else { 0 })?;
    }
    if second_len != 0 {
        p.wr32(second_len, second_bytes)?;
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

fn set_fx(p: &mut Process) -> R {
    let (count, results) = (p.arg(1) as usize, p.arg(3));
    if results != 0 && count <= 256 {
        for i in 0..count {
            p.wr32(results + (i * 4) as u64, 0)?;
        }
    }
    Ok(S_OK)
}

fn acquire_resources(p: &mut Process) -> R {
    let (count, results) = (p.arg(2) as usize, p.arg(3));
    if results != 0 && count <= 256 {
        for i in 0..count {
            p.wr32(results + (i * 4) as u64, 0)?;
        }
    }
    Ok(S_OK)
}

fn playback_not_implemented(_p: &mut Process) -> R {
    Ok(E_NOTIMPL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_reports_not_implemented_without_an_audio_backend() {
        let mut process = Process::new(crate::vfs::Vfs::new());
        let play = method("IDirectSoundBuffer8", "Play").unwrap();

        assert_eq!(play(&mut process).unwrap(), 0x8000_4001);
    }
}
