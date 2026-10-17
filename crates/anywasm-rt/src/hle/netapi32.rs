//! NetBIOS import behavior for environments without a NetBIOS transport.
//!
//! The runtime does not expose a NetBIOS adapter or session service. Valid network commands
//! therefore receive the documented driver-open failure instead of fabricated network results;
//! invalid commands, no-op unlink, and cancellation-with-no-pending-work retain their own status.

use crate::process::{HleFn, Process, Stop};
use anywasm_cpu::CpuMode;

const NRC_GOODRET: u8 = 0x00;
const NRC_ILLCMD: u8 = 0x03;
const NRC_CANCEL: u8 = 0x26;
const NRC_BUFLEN: u8 = 0x01;
const NRC_INVADDRESS: u8 = 0x39;
const NRC_OPENERR: u8 = 0x3f;
const NCB_ASYNCH: u8 = 0x80;
const NCB_UNLINK: u8 = 0x70;
const NCB_TRACE: u8 = 0x79;
const NCB_CANCEL: u8 = 0x35;

const KNOWN_COMMANDS: &[u8] = &[
    0x10, 0x11, 0x12, 0x14, 0x15, 0x16, 0x17, 0x20, 0x21, 0x22, 0x23, 0x30, 0x31, 0x32, 0x33, 0x34,
    NCB_CANCEL, 0x36, 0x37, NCB_UNLINK, 0x71, 0x72, 0x73, 0x77, 0x78,
];

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "Netbios" => Some(netbios),
        _ => None,
    }
}

/// x86 WINAPI stack cleanup for NETAPI32!Netbios(PNCB).
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    match name {
        "Netbios" => Some(4),
        _ => None,
    }
}

fn netbios(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "netapi32".into(),
            name: "Netbios requires the 32-bit NCB layout; no x64 NCB transport is modeled".into(),
        });
    }
    let ncb = p.arg(0);
    if ncb == 0 {
        // NRC_BADNCB in the Netbios docs corresponds to the SDK's NRC_INVADDRESS value.
        return Ok(NRC_INVADDRESS as u64);
    }

    let Ok(command) = p.mem.read_u8(ncb) else {
        return Ok(NRC_INVADDRESS as u64);
    };
    let base_command = command & !NCB_ASYNCH;
    let is_async = command & NCB_ASYNCH != 0;
    let Some(post_address) = ncb.checked_add(44) else {
        return Ok(NRC_INVADDRESS as u64);
    };
    let Some(event_address) = ncb.checked_add(60) else {
        return Ok(NRC_INVADDRESS as u64);
    };
    let (Ok(post), Ok(event)) = (p.mem.read_u32(post_address), p.mem.read_u32(event_address))
    else {
        return Ok(NRC_INVADDRESS as u64);
    };
    let status = if !KNOWN_COMMANDS.contains(&base_command) || base_command == NCB_TRACE {
        NRC_ILLCMD
    } else if event != 0 && (!is_async || post != 0) {
        // The documented NCB layout requires an event only for asynchronous requests that
        // do not also use a post callback.
        NRC_ILLCMD
    } else if is_async {
        // This runtime has neither an async NetBIOS queue nor guest callback/event dispatch.
        NRC_OPENERR
    } else {
        match base_command {
            // Windows documents NCBUNLINK as a compatibility no-op.
            NCB_UNLINK => NRC_GOODRET,
            // No NetBIOS request can be pending because the runtime never submits one.
            NCB_CANCEL => NRC_CANCEL,
            _ => NRC_OPENERR,
        }
    };

    let Some(retcode) = ncb.checked_add(1) else {
        return Ok(NRC_INVADDRESS as u64);
    };
    if p.mem.write_u8(retcode, status).is_err() {
        // The API documents NRC_BUFLEN when the return-code field is not writable.
        return Ok(NRC_BUFLEN as u64);
    }
    let Some(command_complete) = ncb.checked_add(49) else {
        return Ok(NRC_INVADDRESS as u64);
    };
    if p.mem.write_u8(command_complete, status).is_err() {
        return Ok(NRC_BUFLEN as u64);
    }
    Ok(status as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::{
        cpu::RSP,
        mem::{PAGE_SIZE, PROT_RW},
    };

    const STACK: u64 = 0x10000;
    const NCB: u64 = 0x20000;

    fn x86_process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        for base in [STACK, NCB] {
            assert!(process.mem.map(base, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK + 4, NCB as u32).unwrap();
        process
    }

    #[test]
    fn netbios_import_has_one_x86_pointer_argument() {
        assert_eq!(stdcall_stack_bytes("Netbios"), Some(4));
        assert_eq!(stdcall_stack_bytes("other"), None);
    }

    #[test]
    fn valid_network_commands_return_driver_unavailable_and_update_ncb_status() {
        let mut process = x86_process();
        process.wr8(NCB, 0x32).unwrap(); // NCBRESET
        process.wr8(NCB + 1, 0).unwrap();
        process.wr8(NCB + 49, 0).unwrap();

        assert_eq!(netbios(&mut process).unwrap(), NRC_OPENERR as u64);
        assert_eq!(process.rd8(NCB + 1).unwrap(), NRC_OPENERR);
        assert_eq!(process.rd8(NCB + 49).unwrap(), NRC_OPENERR);
    }

    #[test]
    fn compatibility_noop_and_cancel_have_documented_failure_semantics() {
        let mut process = x86_process();
        process.wr8(NCB, NCB_UNLINK).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_GOODRET as u64);

        process.wr8(NCB, NCB_CANCEL).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_CANCEL as u64);
        assert_eq!(process.rd8(NCB + 1).unwrap(), NRC_CANCEL);
    }

    #[test]
    fn invalid_or_async_commands_never_report_success() {
        let mut process = x86_process();
        process.wr8(NCB, 0xff).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_ILLCMD as u64);

        process.wr8(NCB, 0x80 | 0x32).unwrap(); // NCBRESET|ASYNCH, but there is no queue.
        assert_eq!(netbios(&mut process).unwrap(), NRC_OPENERR as u64);
    }

    #[test]
    fn ncb_event_must_be_used_only_for_async_without_post_callback() {
        let mut process = x86_process();
        process.wr8(NCB, 0x32).unwrap();
        process.wr32(NCB + 60, 0x1234).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_ILLCMD as u64);
        assert_eq!(process.rd8(NCB + 1).unwrap(), NRC_ILLCMD);

        process.wr8(NCB, 0x80 | 0x32).unwrap();
        process.wr32(NCB + 44, 0x5678).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_ILLCMD as u64);
    }

    #[test]
    fn null_ncb_returns_documented_invalid_address_error() {
        let mut process = x86_process();
        process.wr32(STACK + 4, 0).unwrap();
        assert_eq!(netbios(&mut process).unwrap(), NRC_INVADDRESS as u64);
    }
}
