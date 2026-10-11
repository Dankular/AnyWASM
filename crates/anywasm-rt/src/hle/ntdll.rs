//! ntdll subset (functions programs and CRTs import directly or via kernel32 forwarding).

use super::*;
use crate::process::*;
use anywasm_cpu::cpu::*;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "RtlCaptureContext" => rtl_capture_context,
        "RtlLookupFunctionEntry" => |_| Ok(0),
        "RtlAllocateHeap" => |p| {
            let n = p.arg(2);
            Ok(kernel32::alloc_block(p, n, true))
        },
        "RtlFreeHeap" => |p| {
            let a = p.arg(2);
            Ok((a == 0 || kernel32::free_block(p, a)) as u64)
        },
        "RtlReAllocateHeap" => |p| {
            let (fl, a, n) = (p.arg(1), p.arg(2), p.arg(3));
            kernel32::realloc_block(p, a, n, fl | 8)
        },
        "RtlGetVersion" => |p| {
            let a = p.arg(0);
            p.wr32(a + 4, 10)?;
            p.wr32(a + 8, 0)?;
            p.wr32(a + 12, 19045)?;
            p.wr32(a + 16, 2)?;
            Ok(0)
        },
        "RtlInitializeCriticalSection" | "RtlEnterCriticalSection" | "RtlLeaveCriticalSection" | "RtlDeleteCriticalSection" => |_| Ok(0),
        "RtlInitUnicodeString" => |p| {
            let (d, s) = (p.arg(0), p.arg(1));
            let n = if s == 0 { 0 } else { p.read_wstr(s)?.encode_utf16().count() as u16 * 2 };
            p.wr16(d, n)?;
            p.wr16(d + 2, if s == 0 { 0 } else { n + 2 })?;
            p.wr64(d + 8, s)?;
            Ok(0)
        },
        "NtClose" => |p| {
            let h = p.arg(0);
            p.handles.remove(&h);
            Ok(0)
        },
        "NtTerminateProcess" => |p| {
            let code = p.arg(1) as u32;
            p.finish_exit(code).map(|_| 0)
        },
        "NtQueryPerformanceCounter" => |p| {
            let a = p.arg(0);
            let t = (p.cpu.icount + p.extra.sleep_adjust) / 10;
            p.wr64(a, t)?;
            Ok(0)
        },
        "RtlNtStatusToDosError" => |_| Ok(317),
        "RtlUnwind" | "RtlUnwindEx" | "RtlVirtualUnwind" => |p| Err(Stop::Unimplemented { dll: "ntdll".into(), name: "RtlUnwind (exception unwinding is not implemented)".into() }.also(p)),
        _ => return None,
    })
}

trait Also {
    fn also(self, p: &mut Process) -> Self;
}
impl Also for Stop {
    fn also(self, p: &mut Process) -> Self {
        if let Stop::Unimplemented { dll, name } = &self {
            p.unimplemented_log.push((dll.clone(), name.clone()));
        }
        self
    }
}

/// Fill a CONTEXT (x64, 0x4d0 bytes) from the current register state.
pub fn rtl_capture_context(p: &mut Process) -> R {
    let a = p.arg(0);
    let mut b = vec![0u8; 0x4d0];
    b[0x30..0x34].copy_from_slice(&0x10_000fu32.to_le_bytes()); // ContextFlags: CONTROL|INTEGER|SEGMENTS|FLOATING_POINT
    b[0x34..0x38].copy_from_slice(&p.cpu.mxcsr.to_le_bytes());
    let sp = p.cpu.gpr[RSP];
    let order = [RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, 8, 9, 10, 11, 12, 13, 14, 15];
    for (i, r) in order.iter().enumerate() {
        let v = if *r == RSP { sp + 8 } else { p.cpu.gpr[*r] };
        b[0x78 + 8 * i..0x80 + 8 * i].copy_from_slice(&v.to_le_bytes());
    }
    let ra = p.rd64(sp)?;
    b[0xf8..0x100].copy_from_slice(&ra.to_le_bytes()); // Rip = return address
    b[0x44..0x48].copy_from_slice(&(p.cpu.rflags as u32).to_le_bytes());
    p.write_bytes(a, &b)?;
    Ok(0)
}
