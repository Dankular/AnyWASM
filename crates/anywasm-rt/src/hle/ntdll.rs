//! ntdll subset (functions programs and CRTs import directly or via kernel32 forwarding).

use super::*;
use crate::process::*;
use anywasm_cpu::cpu::*;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "RtlCaptureContext" => rtl_capture_context,
        "RtlLookupFunctionEntry" => |_| Ok(0),
        "RtlVerifyVersionInfo" => rtl_verify_version_info,
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

pub(crate) fn rtl_verify_version_info(p: &mut Process) -> R {
    const STATUS_INVALID_PARAMETER: u64 = 0xC000_000D;
    const STATUS_REVISION_MISMATCH: u64 = 0xC000_0059;
    const VER_TYPE_MASK: u32 = 0xFF;
    const CONDITION_MASK_INITIALIZED: u64 = 1 << 63;

    let (version, type_mask, condition_mask) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if version == 0 || type_mask == 0 || type_mask & !VER_TYPE_MASK != 0
        || condition_mask & CONDITION_MASK_INITIALIZED == 0
        || p.rd32(version)? < 284
    {
        return Ok(STATUS_INVALID_PARAMETER);
    }

    // (VER_* flag, RTL_OSVERSIONINFOEXW offset, width, host value)
    let fields: [(u32, u64, u8, u64); 8] = [
        (0x01, 4, 4, 10),    // major version
        (0x02, 8, 4, 0),     // minor version
        (0x04, 12, 4, 19045), // build number
        (0x08, 16, 4, 2),    // platform ID
        (0x10, 276, 2, 0),   // service pack major
        (0x20, 278, 2, 0),   // service pack minor
        (0x40, 280, 2, 0),   // product suite mask
        (0x80, 282, 1, 1),   // product type (workstation)
    ];

    for (flag, offset, width, current) in fields {
        if type_mask & flag == 0 {
            continue;
        }
        let shift = flag.trailing_zeros() * 3;
        let condition = ((condition_mask >> shift) & 7) as u8;
        let requested = match width {
            1 => p.rd8(version + offset)? as u64,
            2 => p.rd16(version + offset)? as u64,
            _ => p.rd32(version + offset)? as u64,
        };

        let matches = if flag == 0x40 {
            match condition {
                6 => current & requested == requested, // VER_AND
                7 => current & requested != 0,          // VER_OR
                _ => return Ok(STATUS_INVALID_PARAMETER),
            }
        } else {
            match condition {
                1 => current == requested, // VER_EQUAL
                2 => current > requested,  // VER_GREATER
                3 => current >= requested, // VER_GREATER_EQUAL
                4 => current < requested,  // VER_LESS
                5 => current <= requested, // VER_LESS_EQUAL
                _ => return Ok(STATUS_INVALID_PARAMETER),
            }
        };
        if !matches {
            return Ok(STATUS_REVISION_MISMATCH);
        }
    }
    Ok(0)
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
