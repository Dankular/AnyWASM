//! ntdll subset (functions programs and CRTs import directly or via kernel32 forwarding).

use super::*;
use crate::process::*;
use anywasm_cpu::cpu::*;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "RtlCaptureContext" => rtl_capture_context,
        "RtlPcToFileHeader" => rtl_pc_to_file_header,
        "RtlLookupFunctionEntry" => rtl_lookup_function_entry,
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
        "RtlInitializeCriticalSection"
        | "RtlEnterCriticalSection"
        | "RtlLeaveCriticalSection"
        | "RtlDeleteCriticalSection" => |_| Ok(0),
        "RtlInitUnicodeString" => |p| {
            let (d, s) = (p.arg(0), p.arg(1));
            let n = if s == 0 {
                0
            } else {
                p.read_wstr(s)?.encode_utf16().count() as u16 * 2
            };
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
        "RtlVirtualUnwind" => crate::exception::rtl_virtual_unwind,
        "RtlUnwind" => |p| rtl_unwind_unimplemented(p, "RtlUnwind", 4),
        "RtlUnwindEx" => crate::exception::rtl_unwind_ex,
        _ => return None,
    })
}

fn rtl_unwind_unimplemented(p: &mut Process, name: &str, arg_count: usize) -> R {
    if p.trace_imports {
        let args = (0..arg_count)
            .map(|index| format!("{:#x}", p.arg(index)))
            .collect::<Vec<_>>()
            .join(", ");
        let caller = p.rd64(p.cpu.gpr[RSP]).unwrap_or(0);
        let context = (arg_count >= 5).then(|| p.arg(4));
        let context_state = context
            .map(|address| {
                format!(
                    " context={address:#x} context_rip={:#x} context_rsp={:#x}",
                    p.rd64(address + 0xf8).unwrap_or(0),
                    p.rd64(address + 0x98).unwrap_or(0)
                )
            })
            .unwrap_or_default();
        p.stderr.extend_from_slice(
            format!("[seh] {name} args=[{args}] caller={caller:#x}{context_state}\n").as_bytes(),
        );
    }
    Err(Stop::Unimplemented {
        dll: "ntdll".into(),
        name: format!("{name} (exception unwinding is not implemented)"),
    }
    .also(p))
}

fn rtl_lookup_function_entry(p: &mut Process) -> R {
    let control_pc = p.arg(0);
    let entry = p.modules.iter().find_map(|module| {
        if module.hle_group.is_some()
            || control_pc < module.base
            || control_pc - module.base >= module.size
        {
            return None;
        }
        let rva = control_pc - module.base;
        let index = module
            .runtime_functions
            .partition_point(|function| function.begin_address as u64 <= rva)
            .checked_sub(1)?;
        if rva >= module.runtime_functions[index].end_address as u64 {
            return None;
        }
        Some((module.base, module.exception_table_rva, index))
    });
    let Some((base, table_rva, index)) = entry else {
        return Ok(0);
    };
    if p.arg(1) != 0 {
        p.wr64(p.arg(1), base)?;
    }
    Ok(base + table_rva as u64 + index as u64 * 12)
}

pub(crate) fn rtl_verify_version_info(p: &mut Process) -> R {
    const STATUS_INVALID_PARAMETER: u64 = 0xC000_000D;
    const STATUS_REVISION_MISMATCH: u64 = 0xC000_0059;
    const VER_TYPE_MASK: u32 = 0xFF;
    const CONDITION_MASK_INITIALIZED: u64 = 1 << 63;

    let (version, type_mask, condition_mask) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if version == 0
        || type_mask == 0
        || type_mask & !VER_TYPE_MASK != 0
        || condition_mask & CONDITION_MASK_INITIALIZED == 0
        || p.rd32(version)? < 284
    {
        return Ok(STATUS_INVALID_PARAMETER);
    }

    // (VER_* flag, RTL_OSVERSIONINFOEXW offset, width, host value)
    let fields: [(u32, u64, u8, u64); 8] = [
        (0x01, 4, 4, 10),     // major version
        (0x02, 8, 4, 0),      // minor version
        (0x04, 12, 4, 19045), // build number
        (0x08, 16, 4, 2),     // platform ID
        (0x10, 276, 2, 0),    // service pack major
        (0x20, 278, 2, 0),    // service pack minor
        (0x40, 280, 2, 0),    // product suite mask
        (0x80, 282, 1, 1),    // product type (workstation)
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
                7 => current & requested != 0,         // VER_OR
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

pub(crate) fn rtl_pc_to_file_header(p: &mut Process) -> R {
    let (pc, base_out) = (p.arg(0), p.arg(1));
    let image_base = p
        .modules
        .iter()
        .find(|module| {
            module.hle_group.is_none()
                && pc >= module.base
                && pc.wrapping_sub(module.base) < module.size
        })
        .map(|module| module.base);

    if let Some(image_base) = image_base {
        p.wr64(base_out, image_base)?;
        Ok(image_base)
    } else {
        Ok(0)
    }
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
    let order = [
        RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, 8, 9, 10, 11, 12, 13, 14, 15,
    ];
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

#[cfg(test)]
mod rtl_pc_to_file_header_tests {
    use super::*;
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
    use std::collections::HashMap;

    const IMAGE_BASE: u64 = 0x1400_0000_0;
    const OUTPUT: u64 = 0x50000;

    fn process_with_guest_image() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        assert!(process.mem.map(OUTPUT, PAGE_SIZE, PROT_RW));
        process.modules.push(Module {
            name: "sample.dll".into(),
            path: "C:\\sample.dll".into(),
            base: IMAGE_BASE,
            size: 0x2000,
            entry: 0,
            is_dll: true,
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            hle_group: None,
        });
        process
    }

    #[test]
    fn rtl_unwind_ex_trace_captures_all_six_arguments_and_guest_caller() {
        let mut process = process_with_guest_image();
        process.modules[0].runtime_functions = vec![anywasm_pe::RuntimeFunction {
            begin_address: 0x100,
            end_address: 0x120,
            unwind_info_address: 0,
        }];
        const STACK: u64 = 0x60000;
        assert!(process.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(0x72000, PAGE_SIZE, PROT_RW));
        assert!(process
            .mem
            .map(crate::process::TEB_ADDR, PAGE_SIZE, PROT_RW));
        process
            .wr64(crate::process::TEB_ADDR + 0x08, 0x80000)
            .unwrap();
        process
            .wr64(crate::process::TEB_ADDR + 0x10, 0x50000)
            .unwrap();
        const CONTEXT: u64 = 0x76000;
        assert!(process.mem.map(CONTEXT, PAGE_SIZE, PROT_RW));
        process.cpu.gpr[RSP] = STACK;
        process.cpu.gpr[RCX] = 0x71000;
        process.cpu.gpr[RDX] = 0x1400_00110;
        process.cpu.gpr[R8] = 0x72000;
        process.cpu.gpr[R9] = 0x73000;
        process.wr64(STACK, 0x1400_5678).unwrap();
        process.wr64(STACK + 0x28, CONTEXT).unwrap();
        process.wr64(STACK + 0x30, 0).unwrap();
        process.wr64(CONTEXT + 0x98, 0x71000).unwrap();
        process.wr64(CONTEXT + 0xf8, 0x1400_00110).unwrap();
        process.wr32(0x72000 + 4, 1).unwrap();
        process.trace_imports = true;
        let rtl_unwind = crate::hle::lookup("ntdll", "RtlUnwindEx").unwrap();

        assert_eq!(
            rtl_unwind(&mut process),
            Err(Stop::GuestUnwind(crate::process::GuestUnwind {
                target_frame: 0x71000,
                target_ip: 0x1400_00110,
                call_rsp: STACK,
                exception_record: 0x72000,
                return_value: 0x73000,
                context_record: CONTEXT,
                history_table: 0,
            }))
        );
        assert!(String::from_utf8(process.take_stderr())
            .unwrap()
            .contains("[seh] RtlUnwindEx args=[0x71000, 0x140000110, 0x72000, 0x73000, 0x76000, 0x0] caller=0x14005678 context=0x76000 context_rip=0x140000110 context_rsp=0x71000"));
    }

    #[test]
    fn resolves_a_pc_inside_a_guest_image_and_writes_its_base() {
        let mut process = process_with_guest_image();
        process.cpu.gpr[RCX] = IMAGE_BASE + 0x1fff;
        process.cpu.gpr[RDX] = OUTPUT;
        let rtl_pc_to_file_header = crate::hle::lookup("kernel32", "RtlPcToFileHeader").unwrap();

        assert_eq!(rtl_pc_to_file_header(&mut process), Ok(IMAGE_BASE));
        assert_eq!(process.rd64(OUTPUT), Ok(IMAGE_BASE));
    }

    #[test]
    fn returns_null_and_preserves_output_for_addresses_without_a_guest_image() {
        let mut process = process_with_guest_image();
        let hle_module = process.register_hle_module("kernel32.dll", "kernel32");
        process.wr64(OUTPUT, 0xfeed).unwrap();
        process.cpu.gpr[RCX] = 0x5000_0000;
        process.cpu.gpr[RDX] = OUTPUT;
        let rtl_pc_to_file_header = crate::hle::lookup("kernel32", "RtlPcToFileHeader").unwrap();

        assert_eq!(rtl_pc_to_file_header(&mut process), Ok(0));
        assert_eq!(process.rd64(OUTPUT), Ok(0xfeed));

        process.cpu.gpr[RCX] = process.modules[hle_module].base + 0x200;
        assert_eq!(rtl_pc_to_file_header(&mut process), Ok(0));
        assert_eq!(process.rd64(OUTPUT), Ok(0xfeed));
    }

    #[test]
    fn finds_function_entry_for_pc_and_reports_image_base() {
        let mut process = process_with_guest_image();
        let module = &mut process.modules[0];
        module.exception_table_rva = 0x1000;
        module.runtime_functions = vec![
            anywasm_pe::RuntimeFunction {
                begin_address: 0x100,
                end_address: 0x180,
                unwind_info_address: 0x300,
            },
            anywasm_pe::RuntimeFunction {
                begin_address: 0x180,
                end_address: 0x220,
                unwind_info_address: 0x340,
            },
        ];
        process.cpu.gpr[RCX] = IMAGE_BASE + 0x1a0;
        process.cpu.gpr[RDX] = OUTPUT;
        let lookup = crate::hle::lookup("ntdll", "RtlLookupFunctionEntry").unwrap();

        assert_eq!(lookup(&mut process), Ok(IMAGE_BASE + 0x1000 + 12));
        assert_eq!(process.rd64(OUTPUT), Ok(IMAGE_BASE));
    }

    #[test]
    fn virtual_unwind_applies_stack_allocation_and_returns_handler_data() {
        const CONTEXT: u64 = 0x60000;
        const HLE_STACK: u64 = 0x61000;
        const GUEST_STACK: u64 = 0x70000;
        const FUNCTION_ENTRY: u64 = IMAGE_BASE + 0x300;
        const UNWIND_INFO: u64 = IMAGE_BASE + 0x400;
        let mut process = process_with_guest_image();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RW));
        assert!(process.mem.map(CONTEXT, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(HLE_STACK, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(GUEST_STACK, PAGE_SIZE, PROT_RW));
        process.wr32(FUNCTION_ENTRY, 0x100).unwrap();
        process.wr32(FUNCTION_ENTRY + 4, 0x200).unwrap();
        process.wr32(FUNCTION_ENTRY + 8, 0x400).unwrap();
        process.wr8(UNWIND_INFO, 0x09).unwrap(); // version 1, exception handler
        process.wr8(UNWIND_INFO + 1, 4).unwrap();
        process.wr8(UNWIND_INFO + 2, 1).unwrap();
        process.wr8(UNWIND_INFO + 4, 4).unwrap();
        process.wr8(UNWIND_INFO + 5, 0x32).unwrap(); // UWOP_ALLOC_SMALL, 32 bytes
        process.wr32(UNWIND_INFO + 8, 0x600).unwrap();
        process.wr32(UNWIND_INFO + 12, 0x1234).unwrap();
        process.wr64(CONTEXT + 0x98, GUEST_STACK).unwrap(); // CONTEXT.Rsp
        process.wr64(GUEST_STACK + 32, IMAGE_BASE + 0x777).unwrap();
        process.cpu.gpr[RSP] = HLE_STACK;
        process.cpu.gpr[RCX] = 1; // UNW_FLAG_EHANDLER
        process.cpu.gpr[RDX] = IMAGE_BASE;
        process.cpu.gpr[R8] = IMAGE_BASE + 0x150;
        process.cpu.gpr[R9] = FUNCTION_ENTRY;
        process.wr64(HLE_STACK + 40, CONTEXT).unwrap();
        process.wr64(HLE_STACK + 48, OUTPUT).unwrap();
        process.wr64(HLE_STACK + 56, OUTPUT + 8).unwrap();
        process.wr64(HLE_STACK + 64, 0).unwrap();
        let unwind = crate::hle::lookup("ntdll", "RtlVirtualUnwind").unwrap();

        assert_eq!(unwind(&mut process), Ok(IMAGE_BASE + 0x600));
        assert_eq!(process.rd64(CONTEXT + 0xf8), Ok(IMAGE_BASE + 0x777));
        assert_eq!(process.rd64(CONTEXT + 0x98), Ok(GUEST_STACK + 40));
        assert_eq!(process.rd64(OUTPUT), Ok(UNWIND_INFO + 12));
        assert_eq!(process.rd64(OUTPUT + 8), Ok(GUEST_STACK));
    }
}
