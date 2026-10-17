//! EAX DirectSound export compatibility.
//!
//! The shipped EAX DLL's ordinal #6 creates a DirectSound 8 device. The runtime's DirectSound
//! implementation owns the returned COM object; EAX property/effect interfaces remain explicit
//! unsupported COM queries instead of being fabricated here.

use super::*;
use crate::process::{HleFn, Process};
use anywasm_cpu::CpuMode;

const S_OK: u64 = 0;
const E_INVALIDARG: u64 = 0x8007_0057;
const CLASS_E_NOAGGREGATION: u64 = 0x8004_0110;
const DSERR_INVALIDPARAM: u64 = 0x8878_000a;

const DSDEVID_DEFAULT_PLAYBACK: [u8; 16] = [
    0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_VOICE_PLAYBACK: [u8; 16] = [
    0x02, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const VIRTUAL_PLAYBACK_DEVICE: [u8; 16] = [
    0xa4, 0x7d, 0x39, 0x2c, 0x8a, 0x13, 0x4f, 0x40, 0x9d, 0x2a, 0x41, 0x57, 0x53, 0x4d, 0x00, 0x01,
];

pub fn lookup(name: &str) -> Option<HleFn> {
    (name == "#6").then_some(eax_direct_sound_create8)
}

/// `EAXDirectSoundCreate8` is WINAPI/stdcall with three x86 pointer arguments.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    (name == "#6").then_some(12)
}

fn eax_direct_sound_create8(p: &mut Process) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(crate::process::Stop::Unimplemented {
            dll: "eax".into(),
            name: "EAXDirectSoundCreate8 only supports its 32-bit export ABI".into(),
        });
    }
    let (device_guid, output, outer) = (p.arg(0), p.arg(1), p.arg(2));
    if output == 0 {
        return Ok(E_INVALIDARG);
    }
    p.wrptr(output, 0)?;
    if outer != 0 {
        return Ok(CLASS_E_NOAGGREGATION);
    }
    if device_guid != 0 {
        let device_guid = p.read_bytes(device_guid, 16)?;
        if device_guid != DSDEVID_DEFAULT_PLAYBACK
            && device_guid != DSDEVID_DEFAULT_VOICE_PLAYBACK
            && device_guid != VIRTUAL_PLAYBACK_DEVICE
        {
            return Ok(DSERR_INVALIDPARAM);
        }
    }

    let object = com::create(p, "IDirectSound8", com::Kind::DirectSound);
    p.wrptr(output, object)?;
    Ok(S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Process;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::PROT_RW;

    fn invoke_x86(p: &mut Process, args: &[u32]) -> u64 {
        let stack = p.alloc_virtual(0x1000, PROT_RW) + 0x800;
        p.wr32(stack, 0xfeed).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(stack + 4 + index as u64 * 4, *value).unwrap();
        }
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = stack;
        eax_direct_sound_create8(p).unwrap()
    }

    #[test]
    fn ordinal_six_creates_the_existing_virtual_directsound_device() {
        assert!(lookup("#6").is_some());
        assert_eq!(stdcall_stack_bytes("#6"), Some(12));

        let mut process = Process::new(Vfs::new());
        process.vmem_next = 0x1000_0000;
        let output = process.alloc_virtual(4, PROT_RW);
        assert_eq!(invoke_x86(&mut process, &[0, output as u32, 0]), S_OK);
        let object = process.rd32(output).unwrap() as u64;
        assert_ne!(object, 0);
        assert!(matches!(
            com::kind_of(&process, object),
            Some(com::Kind::DirectSound)
        ));
    }

    #[test]
    fn ordinal_six_rejects_aggregation_and_unknown_devices() {
        let mut process = Process::new(Vfs::new());
        process.vmem_next = 0x1000_0000;
        let output = process.alloc_virtual(4, PROT_RW);
        assert_eq!(
            invoke_x86(&mut process, &[0, output as u32, 0x1234]),
            CLASS_E_NOAGGREGATION
        );
        assert_eq!(process.rd32(output).unwrap(), 0);

        let unknown_guid = process.alloc_virtual(16, PROT_RW);
        process.write_bytes(unknown_guid, &[0x44; 16]).unwrap();
        assert_eq!(
            invoke_x86(&mut process, &[unknown_guid as u32, output as u32, 0]),
            DSERR_INVALIDPARAM
        );
        assert_eq!(process.rd32(output).unwrap(), 0);
    }
}
