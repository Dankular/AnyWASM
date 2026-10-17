//! ntdll subset (functions programs and CRTs import directly or via kernel32 forwarding).

use super::*;
use crate::process::*;
use anywasm_cpu::cpu::*;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "RtlCaptureContext" => rtl_capture_context,
        "RtlPcToFileHeader" => rtl_pc_to_file_header,
        "RtlLookupFunctionEntry" => rtl_lookup_function_entry,
        "RtlAddFunctionTable" => rtl_add_function_table,
        "RtlDeleteFunctionTable" => rtl_delete_function_table,
        "RtlInstallFunctionTableCallback" => rtl_install_function_table_callback,
        "RtlAddGrowableFunctionTable" => rtl_add_growable_function_table,
        "RtlGrowFunctionTable" => rtl_grow_function_table,
        "RtlDeleteGrowableFunctionTable" => rtl_delete_growable_function_table,
        "RtlVerifyVersionInfo" => rtl_verify_version_info,
        "RtlAllocateHeap" => rtl_allocate_heap,
        "RtlFreeHeap" => rtl_free_heap,
        "RtlReAllocateHeap" => rtl_reallocate_heap,
        "RtlGetVersion" => |p| {
            let a = p.arg(0);
            p.wr32(a + 4, 10)?;
            p.wr32(a + 8, 0)?;
            p.wr32(a + 12, 19045)?;
            p.wr32(a + 16, 2)?;
            Ok(0)
        },
        "RtlInitializeCriticalSection" => rtl_initialize_critical_section,
        "RtlEnterCriticalSection" => rtl_enter_critical_section,
        "RtlLeaveCriticalSection" => rtl_leave_critical_section,
        "RtlDeleteCriticalSection" => rtl_delete_critical_section,
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
        "NtClose" => nt_close,
        "NtTerminateProcess" => |p| {
            let process = p.arg(0);
            if process != 0 && process != u64::MAX && process != u32::MAX as u64 {
                return Ok(0xC000_0008); // STATUS_INVALID_HANDLE
            }
            let code = p.arg(1) as u32;
            p.finish_exit(code).map(|_| 0)
        },
        "NtQueryPerformanceCounter" => nt_query_performance_counter,
        "RtlNtStatusToDosError" => rtl_nt_status_to_dos_error,
        "RtlVirtualUnwind" => crate::exception::rtl_virtual_unwind,
        "RtlUnwind" => crate::exception32::rtl_unwind,
        "RtlUnwindEx" => crate::exception::rtl_unwind_ex,
        _ => return None,
    })
}

fn unsupported_ntdll_behavior(name: &str, detail: &str) -> Stop {
    Stop::Unimplemented {
        dll: "ntdll".into(),
        name: format!("{name}: {detail}"),
    }
}

fn rtl_allocate_heap(p: &mut Process) -> R {
    let (heap, flags, size) = (p.arg(0), p.arg(1), p.arg(2));
    if heap != p.heap.process_heap {
        return Ok(0);
    }
    if flags & !0x0d != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlAllocateHeap",
            "unsupported heap flags",
        ));
    }
    let allocation = kernel32::alloc_block(p, size, flags & 0x8 != 0);
    if allocation == 0 && flags & 0x4 != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlAllocateHeap",
            "HEAP_GENERATE_EXCEPTIONS allocation failure is not translated",
        ));
    }
    Ok(allocation)
}

fn rtl_free_heap(p: &mut Process) -> R {
    let (heap, flags, allocation) = (p.arg(0), p.arg(1), p.arg(2));
    if heap != p.heap.process_heap || allocation == 0 {
        return Ok(0);
    }
    if flags & !0x1 != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlFreeHeap",
            "unsupported heap flags",
        ));
    }
    Ok(kernel32::free_block(p, allocation) as u64)
}

fn rtl_reallocate_heap(p: &mut Process) -> R {
    let (heap, flags, allocation, size) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if heap != p.heap.process_heap {
        return Ok(0);
    }
    if flags & !0x1d != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlReAllocateHeap",
            "unsupported heap flags",
        ));
    }
    if flags & 0x10 != 0 && allocation != 0 {
        let Some(old_size) = p.heap.size_of(allocation) else {
            return Ok(0);
        };
        if size > old_size {
            return Ok(0);
        }
    }
    let result = kernel32::realloc_block(p, allocation, size, flags & 0x8);
    if matches!(result, Ok(0)) && flags & 0x4 != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlReAllocateHeap",
            "HEAP_GENERATE_EXCEPTIONS allocation failure is not translated",
        ));
    }
    result
}

fn rtl_initialize_critical_section(p: &mut Process) -> R {
    let address = p.arg(0);
    if address == 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlInitializeCriticalSection",
            "null critical-section pointer",
        ));
    }
    p.critical_sections.entry(address).or_default();
    Ok(0) // STATUS_SUCCESS
}

fn rtl_enter_critical_section(p: &mut Process) -> R {
    let address = p.arg(0);
    let id = p.current_thread_id;
    if p.try_acquire_critical(address, id) {
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(unsupported_ntdll_behavior(
            "RtlEnterCriticalSection",
            "contended critical section from a nested synchronous guest callback cannot be scheduled",
        ));
    }
    let waiters = &mut p.critical_sections.entry(address).or_default().waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::CriticalAcquire { address });
    Ok(0)
}

fn rtl_leave_critical_section(p: &mut Process) -> R {
    let address = p.arg(0);
    if !p.release_critical(address, p.current_thread_id) {
        return Err(unsupported_ntdll_behavior(
            "RtlLeaveCriticalSection",
            "current thread does not own the critical section",
        ));
    }
    Ok(0)
}

fn rtl_delete_critical_section(p: &mut Process) -> R {
    let address = p.arg(0);
    let Some(state) = p.critical_sections.get(&address) else {
        return Err(unsupported_ntdll_behavior(
            "RtlDeleteCriticalSection",
            "critical section was not initialized",
        ));
    };
    if state.owner.is_some() || !state.waiters.is_empty() {
        return Err(unsupported_ntdll_behavior(
            "RtlDeleteCriticalSection",
            "critical section is still owned or has waiters",
        ));
    }
    p.critical_sections.remove(&address);
    Ok(0)
}

fn nt_close(p: &mut Process) -> R {
    const STATUS_INVALID_HANDLE: u64 = 0xC000_0008;
    Ok(if p.handles.remove(&p.arg(0)).is_some() {
        0
    } else {
        STATUS_INVALID_HANDLE
    })
}

fn nt_query_performance_counter(p: &mut Process) -> R {
    const STATUS_ACCESS_VIOLATION: u64 = 0xC000_0005;
    let counter = p.arg(0);
    let frequency = p.arg(1);
    if counter == 0 {
        return Ok(STATUS_ACCESS_VIOLATION);
    }
    p.wr64(counter, kernel32::query_performance_counter_value(p))?;
    if frequency != 0 {
        p.wr64(frequency, kernel32::QUERY_PERFORMANCE_FREQUENCY)?;
    }
    Ok(0) // STATUS_SUCCESS
}

fn rtl_nt_status_to_dos_error(p: &mut Process) -> R {
    let status = p.arg(0) as u32;
    let error = match status {
        0x0000_0000 => 0,           // STATUS_SUCCESS
        0x0000_0102 => 1460,        // STATUS_TIMEOUT
        0x0000_0103 => 997,         // STATUS_PENDING
        0x4000_0000 => 698,         // STATUS_OBJECT_NAME_EXISTS
        0x8000_0005 => 234,         // STATUS_BUFFER_OVERFLOW
        0x8000_0006 => 18,          // STATUS_NO_MORE_FILES
        0xc000_0002 => 1,           // STATUS_NOT_IMPLEMENTED
        0xc000_0005 => 998,         // STATUS_ACCESS_VIOLATION
        0xc000_0008 => 6,           // STATUS_INVALID_HANDLE
        0xc000_000d => 87,          // STATUS_INVALID_PARAMETER
        0xc000_000f => 2,           // STATUS_NO_SUCH_FILE
        0xc000_0011 => 38,          // STATUS_END_OF_FILE
        0xc000_0017 => 8,           // STATUS_NO_MEMORY
        0xc000_0022 => 5,           // STATUS_ACCESS_DENIED
        0xc000_0023 => 122,         // STATUS_BUFFER_TOO_SMALL
        0xc000_0033 => 123,         // STATUS_OBJECT_NAME_INVALID
        0xc000_0034 => 2,           // STATUS_OBJECT_NAME_NOT_FOUND
        0xc000_0035 => 183,         // STATUS_OBJECT_NAME_COLLISION
        0xc000_003a => 3,           // STATUS_OBJECT_PATH_NOT_FOUND
        0xc000_003b => 161,         // STATUS_OBJECT_PATH_SYNTAX_BAD
        0xc000_0043 => 32,          // STATUS_SHARING_VIOLATION
        0xc000_007b => 193,         // STATUS_INVALID_IMAGE_FORMAT
        0xc000_007f => 112,         // STATUS_DISK_FULL
        0xc000_0094 => 0xc000_0094, // STATUS_INTEGER_DIVIDE_BY_ZERO (native passthrough)
        0xc000_00a2 => 19,          // STATUS_MEDIA_WRITE_PROTECTED
        0xc000_011f => 4,           // STATUS_TOO_MANY_OPENED_FILES
        0xc000_0135 => 126,         // STATUS_DLL_NOT_FOUND
        0xc000_0139 => 127,         // STATUS_ENTRYPOINT_NOT_FOUND
        0xc000_0142 => 1114,        // STATUS_DLL_INIT_FAILED
        0xc000_0059 => 1306,        // STATUS_REVISION_MISMATCH
        _ => 317,                   // ERROR_MR_MID_NOT_FOUND, the documented fallback
    };
    Ok(error)
}

fn rtl_lookup_function_entry(p: &mut Process) -> R {
    let control_pc = p.arg(0);
    let image_base_out = p.arg(1);
    let Some((base, entry)) = crate::exception::lookup_frame(p, control_pc)? else {
        if image_base_out != 0 {
            p.wr64(image_base_out, 0)?;
        }
        return Ok(0);
    };
    if entry == 0 {
        if image_base_out != 0 {
            p.wr64(image_base_out, 0)?;
        }
        return Ok(0);
    }
    if image_base_out != 0 {
        p.wr64(image_base_out, base)?;
    }
    Ok(entry)
}

fn rtl_add_function_table(p: &mut Process) -> R {
    const MAX_ENTRIES: u32 = 1 << 20;

    let table = p.arg(0);
    let entry_count = p.arg(1) as u32;
    let base = p.arg(2);
    let table_bytes = entry_count as u64 * 12;
    if table == 0
        || entry_count == 0
        || entry_count > MAX_ENTRIES
        || table.checked_add(table_bytes).is_none()
        || p.dynamic_function_tables
            .iter()
            .any(|registered| registered.table == table)
    {
        return Ok(0);
    }

    let mut previous_end = None;
    for index in 0..entry_count {
        let entry = table + index as u64 * 12;
        let begin = p.rd32(entry)?;
        let end = p.rd32(entry + 4)?;
        let unwind = p.rd32(entry + 8)?;
        if begin >= end
            || previous_end.is_some_and(|previous| begin < previous)
            || unwind == 0
            || unwind & 3 != 0
            || base.checked_add(end as u64).is_none()
            || base.checked_add(unwind as u64).is_none()
        {
            return Ok(0);
        }
        previous_end = Some(end);
    }

    p.dynamic_function_tables.push(DynamicFunctionTable {
        table,
        entry_count,
        base,
        growable: None,
        callback: None,
    });
    Ok(1)
}

fn rtl_delete_function_table(p: &mut Process) -> R {
    let table = p.arg(0);
    let Some(index) = p
        .dynamic_function_tables
        .iter()
        .position(|registered| registered.table == table && registered.growable.is_none())
    else {
        return Ok(0);
    };
    p.dynamic_function_tables.remove(index);
    Ok(1)
}

fn rtl_install_function_table_callback(p: &mut Process) -> R {
    let table_identifier = p.arg(0);
    let base = p.arg(1);
    let length = p.arg(2) as u32;
    let callback = p.arg(3);
    let context = p.arg(4);
    let Some(range_end) = base.checked_add(length as u64) else {
        return Ok(0);
    };

    if table_identifier & 3 != 3
        || length == 0
        || range_end <= base
        || callback == 0
        || !p
            .mem
            .prot_of(callback)
            .is_some_and(|protection| protection & anywasm_cpu::mem::PROT_X != 0)
        || p.dynamic_function_tables
            .iter()
            .any(|registered| registered.table == table_identifier)
    {
        return Ok(0);
    }

    // OutOfProcessCallbackDll is only used by an external debugger. The in-process
    // unwinder resolves entries through the guest callback below.
    p.dynamic_function_tables.push(DynamicFunctionTable {
        table: table_identifier,
        entry_count: 0,
        base,
        growable: None,
        callback: Some(CallbackFunctionTable {
            length,
            callback,
            context,
        }),
    });
    Ok(1)
}

fn rtl_add_growable_function_table(p: &mut Process) -> R {
    const STATUS_INVALID_PARAMETER: u64 = 0xC000_000D;
    const STATUS_NO_MEMORY: u64 = 0xC000_0017;
    const MAX_ENTRIES: u32 = 1 << 20;

    let output = p.arg(0);
    let table = p.arg(1);
    let entry_count = p.arg(2) as u32;
    let maximum_entry_count = p.arg(3) as u32;
    let range_base = p.arg(4);
    let range_end = p.arg(5);
    let table_bytes = maximum_entry_count as u64 * 12;
    if output == 0
        || table == 0
        || table & 3 != 0
        || entry_count > maximum_entry_count
        || maximum_entry_count == 0
        || maximum_entry_count > MAX_ENTRIES
        || table.checked_add(table_bytes).is_none()
        || range_end <= range_base
        || p.dynamic_function_tables
            .iter()
            .any(|registered| registered.table == table)
        || !valid_dynamic_function_entries(p, table, entry_count, range_base, range_end)
    {
        return Ok(STATUS_INVALID_PARAMETER);
    }

    let handle = p.next_growable_function_table_handle;
    let Some(next_handle) = handle.checked_add(8) else {
        return Ok(STATUS_NO_MEMORY);
    };
    if handle == 0
        || p.dynamic_function_tables.iter().any(|registered| {
            registered
                .growable
                .is_some_and(|growable| growable.handle == handle)
        })
    {
        return Ok(STATUS_NO_MEMORY);
    }

    p.wr64(output, handle)?;
    p.dynamic_function_tables.push(DynamicFunctionTable {
        table,
        entry_count,
        base: range_base,
        growable: Some(GrowableFunctionTable {
            handle,
            maximum_entry_count,
            range_end,
        }),
        callback: None,
    });
    p.next_growable_function_table_handle = next_handle;
    Ok(0)
}

fn rtl_grow_function_table(p: &mut Process) -> R {
    let handle = p.arg(0);
    let new_entry_count = p.arg(1) as u32;
    let Some(index) = p.dynamic_function_tables.iter().position(|registered| {
        registered
            .growable
            .is_some_and(|growable| growable.handle == handle)
    }) else {
        return Ok(0);
    };
    let registered = p.dynamic_function_tables[index];
    let Some(growable) = registered.growable else {
        return Ok(0);
    };
    if new_entry_count <= registered.entry_count
        || new_entry_count > growable.maximum_entry_count
        || !valid_dynamic_function_entries(
            p,
            registered.table,
            new_entry_count,
            registered.base,
            growable.range_end,
        )
    {
        return Ok(0);
    }
    p.dynamic_function_tables[index].entry_count = new_entry_count;
    Ok(0)
}

fn rtl_delete_growable_function_table(p: &mut Process) -> R {
    let handle = p.arg(0);
    if let Some(index) = p.dynamic_function_tables.iter().position(|registered| {
        registered
            .growable
            .is_some_and(|growable| growable.handle == handle)
    }) {
        p.dynamic_function_tables.remove(index);
    }
    Ok(0)
}

fn valid_dynamic_function_entries(
    p: &Process,
    table: u64,
    entry_count: u32,
    range_base: u64,
    range_end: u64,
) -> bool {
    let Some(range_size) = range_end.checked_sub(range_base) else {
        return false;
    };
    let Some(table_bytes) = (entry_count as u64).checked_mul(12) else {
        return false;
    };
    if range_size == 0 || table.checked_add(table_bytes).is_none() {
        return false;
    }

    let mut previous_end = None;
    for index in 0..entry_count {
        let Some(entry) = table.checked_add(index as u64 * 12) else {
            return false;
        };
        let (Ok(begin), Ok(end), Ok(unwind)) =
            (p.rd32(entry), p.rd32(entry + 4), p.rd32(entry + 8))
        else {
            return false;
        };
        if begin >= end
            || end as u64 > range_size
            || previous_end.is_some_and(|previous| begin < previous)
            || unwind == 0
            || unwind & 3 != 0
            || range_base.checked_add(unwind as u64).is_none()
        {
            return false;
        }
        previous_end = Some(end);
    }
    true
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
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};
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
            tls_callbacks: Vec::new(),
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
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
        process.wr64(STACK + 0x30, 0x72400).unwrap();
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
                history_table: 0x72400,
            }))
        );
        assert!(String::from_utf8(process.take_stderr())
            .unwrap()
            .contains("[seh] RtlUnwindEx args=[0x71000, 0x140000110, 0x72000, 0x73000, 0x76000, 0x72400] caller=0x14005678 context=0x76000 context_rip=0x140000110 context_rsp=0x71000"));
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
    fn callback_function_table_runs_guest_callback_for_rtl_lookup() {
        const STACK: u64 = 0x60000;
        const TABLE_IDENTIFIER: u64 = 0x200003;
        const TABLE_BASE: u64 = IMAGE_BASE + 0x1000;
        const TABLE_LENGTH: u64 = 0x400;
        const CALLBACK: u64 = IMAGE_BASE + 0x180;
        const CONTEXT: u64 = 0xfeed_beef_cafe_1234;
        const CONTROL_PC: u64 = TABLE_BASE + 0x140;
        const ENTRY: u64 = OUTPUT + 0x100;
        const SEEN_PC: u64 = OUTPUT;
        const SEEN_CONTEXT: u64 = OUTPUT + 8;
        const IMAGE_BASE_OUT: u64 = OUTPUT + 16;

        let mut process = process_with_guest_image();
        assert!(process.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        process.cpu.gpr[RSP] = STACK + 0x800;
        process.wr32(ENTRY, 0x100).unwrap();
        process.wr32(ENTRY + 4, 0x200).unwrap();
        process.wr32(ENTRY + 8, 0x300).unwrap();

        let mut callback_code = vec![0x48, 0xb8]; // mov rax, SEEN_PC
        callback_code.extend_from_slice(&SEEN_PC.to_le_bytes());
        callback_code.extend_from_slice(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        callback_code.extend_from_slice(&[0x48, 0xb8]); // mov rax, SEEN_CONTEXT
        callback_code.extend_from_slice(&SEEN_CONTEXT.to_le_bytes());
        callback_code.extend_from_slice(&[0x48, 0x89, 0x10]); // mov [rax], rdx
        callback_code.extend_from_slice(&[0x48, 0xb8]); // mov rax, ENTRY
        callback_code.extend_from_slice(&ENTRY.to_le_bytes());
        callback_code.push(0xc3); // ret
        process.write_bytes(CALLBACK, &callback_code).unwrap();

        let install = crate::hle::lookup("ntdll", "RtlInstallFunctionTableCallback").unwrap();
        process.cpu.gpr[RCX] = TABLE_IDENTIFIER;
        process.cpu.gpr[RDX] = TABLE_BASE;
        process.cpu.gpr[R8] = TABLE_LENGTH;
        process.cpu.gpr[R9] = CALLBACK;
        process.wr64(process.cpu.gpr[RSP] + 0x28, CONTEXT).unwrap();
        assert_eq!(install(&mut process), Ok(1));

        let lookup = crate::hle::lookup("ntdll", "RtlLookupFunctionEntry").unwrap();
        process.cpu.gpr[RCX] = CONTROL_PC;
        process.cpu.gpr[RDX] = IMAGE_BASE_OUT;
        assert_eq!(lookup(&mut process), Ok(ENTRY));

        assert_eq!(process.rd64(SEEN_PC), Ok(CONTROL_PC));
        assert_eq!(process.rd64(SEEN_CONTEXT), Ok(CONTEXT));
        assert_eq!(process.rd64(IMAGE_BASE_OUT), Ok(TABLE_BASE));

        process.cpu.gpr[RCX] = TABLE_BASE + TABLE_LENGTH;
        process.cpu.gpr[RDX] = IMAGE_BASE_OUT;
        assert_eq!(lookup(&mut process), Ok(0));
        assert_eq!(process.rd64(SEEN_PC), Ok(CONTROL_PC));
        assert_eq!(process.rd64(IMAGE_BASE_OUT), Ok(0));

        let delete = crate::hle::lookup("ntdll", "RtlDeleteFunctionTable").unwrap();
        process.cpu.gpr[RCX] = TABLE_IDENTIFIER;
        assert_eq!(delete(&mut process), Ok(1));
        process.cpu.gpr[RCX] = CONTROL_PC;
        process.cpu.gpr[RDX] = IMAGE_BASE_OUT;
        assert_eq!(lookup(&mut process), Ok(0));
        assert_eq!(process.rd64(SEEN_PC), Ok(CONTROL_PC));
        assert_eq!(process.rd64(IMAGE_BASE_OUT), Ok(0));
    }

    #[test]
    fn growable_function_table_can_start_empty_grow_and_be_deleted() {
        const STACK: u64 = 0x60000;
        const TABLE: u64 = 0x80000;
        const RANGE_BASE: u64 = 0x3000_0000;
        const RANGE_END: u64 = RANGE_BASE + 0x1000;
        let mut process = process_with_guest_image();
        for address in [STACK, TABLE, RANGE_BASE] {
            assert!(process.mem.map(address, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process.cpu.gpr[RCX] = OUTPUT;
        process.cpu.gpr[RDX] = TABLE;
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 4;
        process.wr64(STACK + 0x28, RANGE_BASE).unwrap();
        process.wr64(STACK + 0x30, RANGE_END).unwrap();
        let add = crate::hle::lookup("ntdll", "RtlAddGrowableFunctionTable").unwrap();
        let grow = crate::hle::lookup("ntdll", "RtlGrowFunctionTable").unwrap();
        let delete = crate::hle::lookup("ntdll", "RtlDeleteGrowableFunctionTable").unwrap();
        let lookup = crate::hle::lookup("ntdll", "RtlLookupFunctionEntry").unwrap();

        assert_eq!(add(&mut process), Ok(0));
        let handle = process.rd64(OUTPUT).unwrap();
        assert_ne!(handle, 0);
        process.wr32(TABLE, 0x100).unwrap();
        process.wr32(TABLE + 4, 0x1100).unwrap();
        process.wr32(TABLE + 8, 0x400).unwrap();

        process.cpu.gpr[RCX] = handle;
        process.cpu.gpr[RDX] = 0;
        assert_eq!(grow(&mut process), Ok(0));
        process.cpu.gpr[RDX] = 5;
        assert_eq!(grow(&mut process), Ok(0));
        process.cpu.gpr[RDX] = 1;
        assert_eq!(grow(&mut process), Ok(0));

        process.cpu.gpr[RCX] = RANGE_BASE + 0x120;
        process.cpu.gpr[RDX] = OUTPUT;
        assert_eq!(lookup(&mut process), Ok(0));
        assert_eq!(process.rd64(OUTPUT), Ok(0));

        process.wr32(TABLE + 4, 0x180).unwrap();
        process.cpu.gpr[RCX] = handle;
        process.cpu.gpr[RDX] = 1;
        assert_eq!(grow(&mut process), Ok(0));
        process.cpu.gpr[RCX] = RANGE_BASE + 0x120;
        process.cpu.gpr[RDX] = OUTPUT;
        assert_eq!(lookup(&mut process), Ok(TABLE));
        assert_eq!(process.rd64(OUTPUT), Ok(RANGE_BASE));

        process.cpu.gpr[RCX] = handle;
        assert_eq!(delete(&mut process), Ok(0));
        process.cpu.gpr[RCX] = RANGE_BASE + 0x120;
        process.cpu.gpr[RDX] = OUTPUT;
        assert_eq!(lookup(&mut process), Ok(0));
        assert_eq!(process.rd64(OUTPUT), Ok(0));
    }

    #[test]
    fn growable_function_table_rejects_entries_outside_its_code_range() {
        const STACK: u64 = 0x60000;
        const TABLE: u64 = 0x80000;
        const RANGE_BASE: u64 = 0x3000_0000;
        const RANGE_END: u64 = RANGE_BASE + 0x1000;
        let mut process = process_with_guest_image();
        for address in [STACK, TABLE] {
            assert!(process.mem.map(address, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process.cpu.gpr[RCX] = OUTPUT;
        process.cpu.gpr[RDX] = TABLE;
        process.cpu.gpr[R8] = 1;
        process.cpu.gpr[R9] = 2;
        process.wr64(STACK + 0x28, RANGE_BASE).unwrap();
        process.wr64(STACK + 0x30, RANGE_END).unwrap();
        process.wr32(TABLE, 0x100).unwrap();
        process.wr32(TABLE + 4, 0x1100).unwrap();
        process.wr32(TABLE + 8, 0x400).unwrap();
        process.wr64(OUTPUT, 0xfeed).unwrap();
        let add = crate::hle::lookup("ntdll", "RtlAddGrowableFunctionTable").unwrap();

        assert_eq!(add(&mut process), Ok(0xC000_000D));
        assert_eq!(process.rd64(OUTPUT), Ok(0xfeed));
        assert!(process.dynamic_function_tables.is_empty());
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

#[cfg(test)]
mod ntdll_api_behavior_tests {
    use super::*;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const OUTPUT: u64 = 0x50000;

    fn process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        assert!(process.mem.map(OUTPUT, PAGE_SIZE, PROT_RW));
        process
    }

    #[test]
    fn rtl_critical_section_calls_update_the_scheduler_lock_state() {
        let mut process = process();
        let address = OUTPUT + 0x100;
        process.cpu.gpr[RCX] = address;
        let initialize = lookup("RtlInitializeCriticalSection").unwrap();
        let enter = lookup("RtlEnterCriticalSection").unwrap();
        let leave = lookup("RtlLeaveCriticalSection").unwrap();
        let delete = lookup("RtlDeleteCriticalSection").unwrap();

        assert_eq!(initialize(&mut process), Ok(0));
        assert_eq!(enter(&mut process), Ok(0));
        assert_eq!(enter(&mut process), Ok(0));
        let state = process.critical_sections.get(&address).unwrap();
        assert_eq!(state.owner, Some(process.current_thread_id));
        assert_eq!(state.recursion, 2);

        assert_eq!(leave(&mut process), Ok(0));
        assert_eq!(process.critical_sections[&address].recursion, 1);
        assert_eq!(leave(&mut process), Ok(0));
        assert_eq!(process.critical_sections[&address].owner, None);
        assert_eq!(delete(&mut process), Ok(0));
        assert!(!process.critical_sections.contains_key(&address));
    }

    #[test]
    fn rtl_critical_section_reports_nested_contention_instead_of_faking_acquisition() {
        let mut process = process();
        let address = OUTPUT + 0x100;
        process.critical_sections.entry(address).or_default().owner = Some(0x7777);
        process.nested = 1;
        process.cpu.gpr[RCX] = address;
        let enter = lookup("RtlEnterCriticalSection").unwrap();

        assert!(matches!(
            enter(&mut process),
            Err(Stop::Unimplemented { dll, name })
                if dll == "ntdll" && name.contains("contended critical section")
        ));
        assert_eq!(process.critical_sections[&address].owner, Some(0x7777));
    }

    #[test]
    fn nt_close_returns_invalid_handle_for_unknown_handles() {
        let mut process = process();
        let close = lookup("NtClose").unwrap();

        process.handles.insert(0x1234, Handle::Other);
        process.cpu.gpr[RCX] = 0x1234;
        assert_eq!(close(&mut process), Ok(0));
        assert!(!process.handles.contains_key(&0x1234));

        process.cpu.gpr[RCX] = 0xdead;
        assert_eq!(close(&mut process), Ok(0xC000_0008));
    }

    #[test]
    fn nt_terminate_process_does_not_terminate_for_an_unmodeled_process_handle() {
        let mut process = process();
        process.cpu.gpr[RCX] = 0xdead;
        process.cpu.gpr[RDX] = 9;
        let terminate = lookup("NtTerminateProcess").unwrap();

        assert_eq!(terminate(&mut process), Ok(0xC000_0008));
        assert_eq!(process.exit_code, None);
    }

    #[test]
    fn nt_query_performance_counter_writes_counter_and_optional_frequency() {
        let mut process = process();
        let counter = OUTPUT;
        let frequency = OUTPUT + 8;
        process.cpu.gpr[RCX] = counter;
        process.cpu.gpr[RDX] = frequency;
        let query = lookup("NtQueryPerformanceCounter").unwrap();

        assert_eq!(query(&mut process), Ok(0));
        assert_eq!(
            process.rd64(counter),
            Ok(kernel32::query_performance_counter_value(&process))
        );
        assert_eq!(
            process.rd64(frequency),
            Ok(kernel32::QUERY_PERFORMANCE_FREQUENCY)
        );
    }

    #[test]
    fn rtl_ntstatus_to_dos_error_translates_known_statuses_and_uses_documented_unknown_code() {
        let mut process = process();
        let convert = lookup("RtlNtStatusToDosError").unwrap();

        process.cpu.gpr[RCX] = 0;
        assert_eq!(convert(&mut process), Ok(0));
        process.cpu.gpr[RCX] = 0xC000_000D;
        assert_eq!(convert(&mut process), Ok(87));
        process.cpu.gpr[RCX] = 0x4000_0000;
        assert_eq!(convert(&mut process), Ok(698));
        process.cpu.gpr[RCX] = 0xC000_0002;
        assert_eq!(convert(&mut process), Ok(1));
        process.cpu.gpr[RCX] = 0xC000_0094;
        assert_eq!(convert(&mut process), Ok(0xC000_0094));
        process.cpu.gpr[RCX] = 0xDEAD_BEEF;
        assert_eq!(convert(&mut process), Ok(317));
    }

    #[test]
    fn rtl_heap_calls_reject_non_process_heaps_and_do_not_force_reallocation_zeroing() {
        let mut process = process();
        let alloc = lookup("RtlAllocateHeap").unwrap();
        let free = lookup("RtlFreeHeap").unwrap();
        let realloc = lookup("RtlReAllocateHeap").unwrap();

        process.cpu.gpr[RCX] = 0xdead;
        process.cpu.gpr[RDX] = 0;
        process.cpu.gpr[R8] = 32;
        let bytes_before = process.heap.bytes_in_use;
        assert_eq!(alloc(&mut process), Ok(0));
        assert_eq!(process.heap.bytes_in_use, bytes_before);
        assert_eq!(free(&mut process), Ok(0));

        let heap = process.heap.process_heap;
        process.cpu.gpr[RCX] = heap;
        process.cpu.gpr[RDX] = 0;
        process.cpu.gpr[R8] = 16;
        let old = alloc(&mut process).unwrap();
        process.write_bytes(old, &[0xAA; 16]).unwrap();

        process.cpu.gpr[RCX] = heap;
        process.cpu.gpr[RDX] = 0;
        process.cpu.gpr[R8] = 64;
        let recycled = alloc(&mut process).unwrap();
        process.write_bytes(recycled, &[0xBB; 64]).unwrap();
        process.cpu.gpr[RCX] = heap;
        process.cpu.gpr[RDX] = 0;
        process.cpu.gpr[R8] = recycled;
        assert_eq!(free(&mut process), Ok(1));

        process.cpu.gpr[RCX] = heap;
        process.cpu.gpr[RDX] = 0;
        process.cpu.gpr[R8] = old;
        process.cpu.gpr[R9] = 64;
        let grown = realloc(&mut process).unwrap();
        assert_ne!(grown, 0);
        assert_eq!(process.read_bytes(grown, 16).unwrap(), vec![0xAA; 16]);
        assert_eq!(process.read_bytes(grown + 16, 48).unwrap(), vec![0xBB; 48]);
    }
}
