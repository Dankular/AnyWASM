//! Win64 PE unwind primitives used by ntdll's `RtlVirtualUnwind` export.

use crate::process::{Process, Stop};
use anywasm_cpu::cpu::RSP;

const CONTEXT_GPR_BASE: u64 = 0x78;
const CONTEXT_RFLAGS: u64 = 0x44;
const CONTEXT_RIP: u64 = 0xf8;
const CONTEXT_XMM_BASE: u64 = 0x1a0;
const UNW_FLAG_EHANDLER: u8 = 1;
const UNW_FLAG_UHANDLER: u8 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;

const WINDOWS_GPRS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Clone)]
struct Context64 {
    gpr: [u64; 16],
    xmm: [u128; 16],
    rip: u64,
    rflags: u64,
}

#[derive(Default)]
struct UnwindResult {
    handler: u64,
    handler_data: u64,
    machine_frame: bool,
}

/// Emulate the x64 `RtlVirtualUnwind` subset described in the runtime design.
pub(crate) fn rtl_virtual_unwind(p: &mut Process) -> Result<u64, Stop> {
    let (handler_type, image_base, control_pc, function_entry, context_ptr) =
        (p.arg(0) as u32, p.arg(1), p.arg(2), p.arg(3), p.arg(4));
    let handler_data_out = p.arg(5);
    let establisher_out = p.arg(6);
    let context_pointers = p.arg(7);
    if context_ptr == 0 {
        return Err(Stop::Crash(
            "RtlVirtualUnwind received a null CONTEXT".into(),
        ));
    }
    if context_pointers != 0 {
        return unsupported(p, "ContextPointers output is not implemented".into());
    }
    if handler_type > 2 {
        return Err(Stop::Crash(format!(
            "RtlVirtualUnwind received unsupported handler type {handler_type}"
        )));
    }

    let mut context = read_context(p, context_ptr)?;
    let establisher = if function_entry == 0 {
        context.gpr[RSP]
    } else {
        read_frame_register(p, image_base, control_pc, function_entry, &context)?
    };
    let result = if function_entry == 0 {
        unwind_leaf(p, &mut context)?
    } else {
        unwind_function(
            p,
            image_base,
            control_pc,
            function_entry,
            handler_type as u8,
            &mut context,
            0,
            false,
        )?
    };

    write_context(p, context_ptr, &context)?;
    if handler_data_out != 0 {
        p.wr64(handler_data_out, result.handler_data)?;
    }
    if establisher_out != 0 {
        p.wr64(establisher_out, establisher)?;
    }
    Ok(result.handler)
}

fn read_context(p: &Process, address: u64) -> Result<Context64, Stop> {
    let mut gpr = [0u64; 16];
    for (index, register) in WINDOWS_GPRS.iter().enumerate() {
        gpr[*register] = p.rd64(address + CONTEXT_GPR_BASE + index as u64 * 8)?;
    }
    let mut xmm = [0u128; 16];
    for (index, value) in xmm.iter_mut().enumerate() {
        *value = p
            .mem
            .read_u128(address + CONTEXT_XMM_BASE + index as u64 * 16)
            .map_err(|error| {
                Stop::Crash(format!(
                    "RtlVirtualUnwind CONTEXT XMM read fault: {error:?}"
                ))
            })?;
    }
    Ok(Context64 {
        gpr,
        xmm,
        rip: p.rd64(address + CONTEXT_RIP)?,
        rflags: p.rd32(address + CONTEXT_RFLAGS)? as u64,
    })
}

fn write_context(p: &mut Process, address: u64, context: &Context64) -> Result<(), Stop> {
    for (index, register) in WINDOWS_GPRS.iter().enumerate() {
        p.wr64(
            address + CONTEXT_GPR_BASE + index as u64 * 8,
            context.gpr[*register],
        )?;
    }
    for (index, value) in context.xmm.iter().enumerate() {
        p.mem
            .write_u128(address + CONTEXT_XMM_BASE + index as u64 * 16, *value)
            .map_err(|error| {
                Stop::Crash(format!(
                    "RtlVirtualUnwind CONTEXT XMM write fault: {error:?}"
                ))
            })?;
    }
    p.wr64(address + CONTEXT_RIP, context.rip)?;
    p.wr32(address + CONTEXT_RFLAGS, context.rflags as u32)?;
    Ok(())
}

fn read_frame_register(
    p: &Process,
    image_base: u64,
    control_pc: u64,
    function_entry: u64,
    context: &Context64,
) -> Result<u64, Stop> {
    let unwind_rva = p.rd32(function_entry + 8)?;
    if unwind_rva == 0 || unwind_rva & 3 != 0 {
        return Err(Stop::Crash(format!(
            "RtlVirtualUnwind invalid UNWIND_INFO RVA {unwind_rva:#x}"
        )));
    }
    let frame_byte = p.rd8(image_base + unwind_rva as u64 + 3)?;
    let frame_register = (frame_byte & 0x0f) as usize;
    let frame_offset = (frame_byte >> 4) as u64 * 16;
    if frame_register == 0 {
        Ok(context.gpr[RSP])
    } else {
        let begin = image_base + p.rd32(function_entry)? as u64;
        let offset = control_pc.saturating_sub(begin);
        let info = image_base + unwind_rva as u64;
        let prolog_size = p.rd8(info + 1)? as u64;
        if offset < prolog_size {
            let code_count = p.rd8(info + 2)? as usize;
            let mut slot = 0usize;
            while slot < code_count {
                let code = info + 4 + slot as u64 * 2;
                let code_offset = p.rd8(code)? as u64;
                let operation = p.rd8(code + 1)?;
                let op = operation & 0x0f;
                let op_info = operation >> 4;
                let consumed = match op {
                    1 if op_info == 0 => 2,
                    1 if op_info == 1 => 3,
                    4 | 8 => 2,
                    5 | 9 => 3,
                    _ => 1,
                };
                if op == 3 && code_offset > offset {
                    return Ok(context.gpr[RSP]);
                }
                slot = slot.saturating_add(consumed);
            }
        }
        Ok(context.gpr[WINDOWS_GPRS[frame_register]].wrapping_sub(frame_offset))
    }
}

fn unwind_leaf(p: &Process, context: &mut Context64) -> Result<UnwindResult, Stop> {
    let return_address = p.rd64(context.gpr[RSP])?;
    context.gpr[RSP] = context.gpr[RSP].wrapping_add(8);
    context.rip = return_address;
    Ok(UnwindResult::default())
}

fn unwind_function(
    p: &mut Process,
    image_base: u64,
    control_pc: u64,
    function_entry: u64,
    handler_type: u8,
    context: &mut Context64,
    depth: u8,
    chained: bool,
) -> Result<UnwindResult, Stop> {
    if depth >= 16 {
        return Err(Stop::Crash(
            "RtlVirtualUnwind chained-info depth exceeded".into(),
        ));
    }
    let begin_rva = p.rd32(function_entry)?;
    let end_rva = p.rd32(function_entry + 4)?;
    let unwind_rva = p.rd32(function_entry + 8)?;
    let begin = image_base
        .checked_add(begin_rva as u64)
        .ok_or_else(|| Stop::Crash("RtlVirtualUnwind function begin address overflow".into()))?;
    let end = image_base
        .checked_add(end_rva as u64)
        .ok_or_else(|| Stop::Crash("RtlVirtualUnwind function end address overflow".into()))?;
    if begin >= end
        || (!chained && (control_pc < begin || control_pc >= end))
        || unwind_rva == 0
        || unwind_rva & 3 != 0
    {
        return Err(Stop::Crash(format!(
            "RtlVirtualUnwind invalid runtime-function range {begin:#x}..{end:#x} for {control_pc:#x}"
        )));
    }
    let info = image_base
        .checked_add(unwind_rva as u64)
        .ok_or_else(|| Stop::Crash("RtlVirtualUnwind UNWIND_INFO address overflow".into()))?;
    let version_flags = p.rd8(info)?;
    let version = version_flags & 7;
    let flags = version_flags >> 3;
    if version != 1 {
        return unsupported(
            p,
            format!("UNWIND_INFO version {version} is not implemented"),
        );
    }
    if flags & !(UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER | UNW_FLAG_CHAININFO) != 0
        || flags & UNW_FLAG_CHAININFO != 0 && flags & (UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER) != 0
    {
        return Err(Stop::Crash(format!(
            "RtlVirtualUnwind invalid UNWIND_INFO flags {flags:#x}"
        )));
    }
    let prolog_size = p.rd8(info + 1)? as u64;
    let code_count = p.rd8(info + 2)? as usize;
    let frame_byte = p.rd8(info + 3)?;
    let frame_register = (frame_byte & 0x0f) as usize;
    let frame_offset = (frame_byte >> 4) as u64 * 16;
    if frame_register >= WINDOWS_GPRS.len()
        || frame_register != 0 && !is_nonvolatile_gpr(frame_register)
    {
        return Err(Stop::Crash(
            "RtlVirtualUnwind invalid frame register".into(),
        ));
    }
    let function_offset = if chained {
        0
    } else {
        control_pc.saturating_sub(begin)
    };
    let in_prolog = !chained && function_offset < prolog_size;
    if !chained && !in_prolog && looks_like_epilogue(p, control_pc, end)? {
        return unsupported(
            p,
            "epilogue instruction emulation is not implemented".into(),
        );
    }
    let mut slot = 0usize;
    let mut previous_code_offset = u64::MAX;
    let mut machine_frame = false;
    while slot < code_count {
        let code_address = info + 4 + (slot as u64) * 2;
        let code_offset = p.rd8(code_address)? as u64;
        if code_offset > prolog_size || code_offset > previous_code_offset {
            return Err(Stop::Crash(
                "RtlVirtualUnwind malformed unwind-code ordering".into(),
            ));
        }
        previous_code_offset = code_offset;
        let operation = p.rd8(code_address + 1)?;
        let op = operation & 0x0f;
        let op_info = operation >> 4;
        if op == 3 && op_info != 0 {
            return Err(Stop::Crash(
                "RtlVirtualUnwind SET_FPREG has nonzero operation info".into(),
            ));
        }
        let consumed = match op {
            0 | 2 | 3 | 10 => 1,
            1 => match op_info {
                0 => 2,
                1 => 3,
                _ => {
                    return Err(Stop::Crash(
                        "RtlVirtualUnwind invalid ALLOC_LARGE info".into(),
                    ))
                }
            },
            4 | 8 => 2,
            5 | 9 => 3,
            _ => return unsupported(p, format!("unwind opcode {op} is not implemented")),
        };
        if slot + consumed > code_count {
            return Err(Stop::Crash(
                "RtlVirtualUnwind truncated unwind-code operands".into(),
            ));
        }
        if !in_prolog || code_offset <= function_offset {
            apply_unwind_code(
                p,
                info,
                slot,
                op,
                op_info,
                frame_register,
                frame_offset,
                context,
                &mut machine_frame,
            )?;
        }
        slot += consumed;
    }

    let aligned_slots = (code_count + 1) & !1;
    let tail = info + 4 + aligned_slots as u64 * 2;
    if flags & UNW_FLAG_CHAININFO != 0 {
        let chained_begin = p.rd32(tail)?;
        let chained_end = p.rd32(tail + 4)?;
        let chained_unwind = p.rd32(tail + 8)?;
        if chained_begin >= chained_end || chained_unwind == 0 {
            return Err(Stop::Crash(
                "RtlVirtualUnwind malformed chained runtime function".into(),
            ));
        }
        let chained = unwind_function(
            p,
            image_base,
            control_pc,
            tail,
            handler_type,
            context,
            depth + 1,
            true,
        )?;
        if chained.handler != 0 || chained.machine_frame {
            return Ok(chained);
        }
        return Ok(UnwindResult {
            machine_frame,
            ..chained
        });
    }

    if machine_frame {
        return Ok(UnwindResult {
            machine_frame: true,
            ..UnwindResult::default()
        });
    }
    let return_address = p.rd64(context.gpr[RSP])?;
    context.gpr[RSP] = context.gpr[RSP].wrapping_add(8);
    context.rip = return_address;

    let handler_flag = match handler_type {
        1 => UNW_FLAG_EHANDLER,
        2 => UNW_FLAG_UHANDLER,
        _ => 0,
    };
    if !in_prolog && handler_flag != 0 && flags & handler_flag != 0 {
        let handler_rva = p.rd32(tail)?;
        if handler_rva == 0 {
            return Err(Stop::Crash(
                "RtlVirtualUnwind null language-handler RVA".into(),
            ));
        }
        return Ok(UnwindResult {
            handler: image_base + handler_rva as u64,
            handler_data: tail + 4,
            machine_frame: false,
        });
    }
    Ok(UnwindResult::default())
}

fn apply_unwind_code(
    p: &Process,
    info: u64,
    slot: usize,
    op: u8,
    op_info: u8,
    frame_register: usize,
    frame_offset: u64,
    context: &mut Context64,
    machine_frame: &mut bool,
) -> Result<(), Stop> {
    let slot_value = |index: usize| p.rd16(info + 4 + index as u64 * 2);
    match op {
        0 => {
            if !is_nonvolatile_gpr(op_info as usize) {
                return Err(Stop::Crash(
                    "RtlVirtualUnwind PUSH_NONVOL names a volatile register".into(),
                ));
            }
            let register = WINDOWS_GPRS[op_info as usize];
            context.gpr[register] = p.rd64(context.gpr[RSP])?;
            context.gpr[RSP] = context.gpr[RSP].wrapping_add(8);
        }
        1 => {
            let size = match op_info {
                0 => slot_value(slot + 1)? as u64 * 8,
                1 => slot_value(slot + 1)? as u64 | (slot_value(slot + 2)? as u64) << 16,
                _ => unreachable!(),
            };
            context.gpr[RSP] = context.gpr[RSP].wrapping_add(size);
        }
        2 => {
            let size = op_info as u64 * 8 + 8;
            context.gpr[RSP] = context.gpr[RSP].wrapping_add(size);
        }
        3 => {
            if frame_register == 0 {
                return Err(Stop::Crash(
                    "RtlVirtualUnwind SET_FPREG without frame register".into(),
                ));
            }
            context.gpr[RSP] = context.gpr[WINDOWS_GPRS[frame_register]].wrapping_sub(frame_offset);
        }
        4 | 5 => {
            if !is_nonvolatile_gpr(op_info as usize) {
                return Err(Stop::Crash(
                    "RtlVirtualUnwind SAVE_NONVOL names a volatile register".into(),
                ));
            }
            let register = WINDOWS_GPRS[op_info as usize];
            let offset = if op == 4 {
                slot_value(slot + 1)? as u64 * 8
            } else {
                slot_value(slot + 1)? as u64 | (slot_value(slot + 2)? as u64) << 16
            };
            let save_base = if frame_register == 0 {
                context.gpr[RSP]
            } else {
                context.gpr[WINDOWS_GPRS[frame_register]].wrapping_sub(frame_offset)
            };
            context.gpr[register] = p.rd64(save_base.wrapping_add(offset))?;
        }
        8 | 9 => {
            if op_info < 6 {
                return Err(Stop::Crash(
                    "RtlVirtualUnwind SAVE_XMM128 names a volatile register".into(),
                ));
            }
            let offset = if op == 8 {
                slot_value(slot + 1)? as u64 * 16
            } else {
                slot_value(slot + 1)? as u64 | (slot_value(slot + 2)? as u64) << 16
            };
            let save_base = if frame_register == 0 {
                context.gpr[RSP]
            } else {
                context.gpr[WINDOWS_GPRS[frame_register]].wrapping_sub(frame_offset)
            };
            context.xmm[op_info as usize] = p
                .mem
                .read_u128(save_base.wrapping_add(offset))
                .map_err(|error| {
                    Stop::Crash(format!("RtlVirtualUnwind XMM restore fault: {error:?}"))
                })?;
        }
        10 => {
            if op_info > 1 {
                return Err(Stop::Crash(
                    "RtlVirtualUnwind invalid PUSH_MACHFRAME info".into(),
                ));
            }
            let frame = context.gpr[RSP].wrapping_add(if op_info == 1 { 8 } else { 0 });
            context.rip = p.rd64(frame)?;
            context.rflags = p.rd64(frame + 16)?;
            context.gpr[RSP] = p.rd64(frame + 24)?;
            *machine_frame = true;
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn is_nonvolatile_gpr(register: usize) -> bool {
    matches!(register, 3 | 5 | 6 | 7 | 12..=15)
}

fn looks_like_epilogue(p: &Process, pc: u64, function_end: u64) -> Result<bool, Stop> {
    let mut cursor = pc;
    let mut saw_stack_adjust = false;
    let mut saw_pop = false;
    for _ in 0..16 {
        if cursor >= function_end {
            return Ok(false);
        }
        let b0 = p.rd8(cursor)?;
        if b0 == 0xc3 || b0 == 0xc2 || b0 == 0xf3 && p.rd8(cursor + 1)? == 0xc3 {
            return Ok(true);
        }
        if b0 == 0x48 && p.rd8(cursor + 1)? == 0x83 && p.rd8(cursor + 2)? == 0xc4 {
            if saw_stack_adjust || saw_pop {
                return Ok(false);
            }
            saw_stack_adjust = true;
            cursor += 4;
            continue;
        }
        if b0 == 0x48 && p.rd8(cursor + 1)? == 0x81 && p.rd8(cursor + 2)? == 0xc4 {
            if saw_stack_adjust || saw_pop {
                return Ok(false);
            }
            saw_stack_adjust = true;
            cursor += 7;
            continue;
        }
        if b0 == 0x48 && p.rd8(cursor + 1)? == 0x8d && matches!(p.rd8(cursor + 2)?, 0x65 | 0xa5) {
            if saw_stack_adjust || saw_pop {
                return Ok(false);
            }
            saw_stack_adjust = true;
            cursor += if p.rd8(cursor + 2)? == 0x65 { 4 } else { 7 };
            continue;
        }
        if (0x58..=0x5f).contains(&b0) {
            saw_pop = true;
            cursor += 1;
            continue;
        }
        if b0 == 0x41 && (0x58..=0x5f).contains(&p.rd8(cursor + 1)?) {
            saw_pop = true;
            cursor += 2;
            continue;
        }
        return Ok(false);
    }
    Ok(false)
}

fn unsupported<T>(p: &mut Process, detail: String) -> Result<T, Stop> {
    let name = format!("RtlVirtualUnwind: {detail}");
    p.unimplemented_log.push(("ntdll".into(), name.clone()));
    Err(Stop::Unimplemented {
        dll: "ntdll".into(),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::Vfs;
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const INFO: u64 = 0x10000;
    const STACK: u64 = 0x20000;

    fn process_with_memory() -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(INFO, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(STACK, PAGE_SIZE, PROT_RW));
        process
    }

    fn context() -> Context64 {
        Context64 {
            gpr: [0; 16],
            xmm: [0; 16],
            rip: 0,
            rflags: 0,
        }
    }

    #[test]
    fn leaf_unwind_pops_only_the_return_address() {
        let process = process_with_memory();
        let mut context = context();
        context.gpr[RSP] = STACK;
        let mut process = process;
        process.wr64(STACK, 0x1400_4321).unwrap();

        unwind_leaf(&process, &mut context).unwrap();

        assert_eq!(context.rip, 0x1400_4321);
        assert_eq!(context.gpr[RSP], STACK + 8);
    }

    #[test]
    fn prologue_unwind_skips_operations_that_have_not_executed() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RW));
        let entry = IMAGE_BASE + 0x300;
        let info = IMAGE_BASE + 0x400;
        process.wr32(entry, 0x100).unwrap();
        process.wr32(entry + 4, 0x200).unwrap();
        process.wr32(entry + 8, 0x400).unwrap();
        process.wr8(info, 1).unwrap();
        process.wr8(info + 1, 8).unwrap();
        process.wr8(info + 2, 2).unwrap();
        process.wr8(info + 4, 8).unwrap();
        process.wr8(info + 5, 0x32).unwrap(); // allocation completes at offset 8
        process.wr8(info + 6, 1).unwrap();
        process.wr8(info + 7, 0x30).unwrap(); // RBX push completed at offset 1
        process.wr64(STACK, 0xfeed_beef).unwrap();
        process.wr64(STACK + 8, 0x1400_7654).unwrap();
        process.wr8(IMAGE_BASE + 0x104, 0x90).unwrap();
        let mut context = context();
        context.gpr[RSP] = STACK;

        let result = unwind_function(
            &mut process,
            IMAGE_BASE,
            IMAGE_BASE + 0x104,
            entry,
            1,
            &mut context,
            0,
            false,
        )
        .unwrap();

        assert_eq!(result.handler, 0);
        assert_eq!(context.gpr[3], 0xfeed_beef);
        assert_eq!(context.rip, 0x1400_7654);
        assert_eq!(context.gpr[RSP], STACK + 16);
    }

    #[test]
    fn unsupported_unwind_versions_fail_loudly_without_changing_context() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RW));
        let entry = IMAGE_BASE + 0x300;
        let info = IMAGE_BASE + 0x400;
        process.wr32(entry, 0x100).unwrap();
        process.wr32(entry + 4, 0x200).unwrap();
        process.wr32(entry + 8, 0x400).unwrap();
        process.wr8(info, 2).unwrap(); // unsupported version 2
        process.wr8(IMAGE_BASE + 0x110, 0x90).unwrap();
        let mut context = context();
        context.gpr[RSP] = STACK;
        let original_context = context.clone();

        assert!(matches!(
            unwind_function(
                &mut process,
                IMAGE_BASE,
                IMAGE_BASE + 0x110,
                entry,
                1,
                &mut context,
                0,
                false,
            ),
            Err(Stop::Unimplemented { .. })
        ));
        assert_eq!(context.gpr, original_context.gpr);
        assert_eq!(context.rip, original_context.rip);
        assert_eq!(process.unimplemented_log.len(), 1);
    }

    #[test]
    fn epilogue_unwind_is_explicitly_unimplemented() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RW));
        let entry = IMAGE_BASE + 0x300;
        let info = IMAGE_BASE + 0x400;
        process.wr32(entry, 0x100).unwrap();
        process.wr32(entry + 4, 0x200).unwrap();
        process.wr32(entry + 8, 0x400).unwrap();
        process.wr8(info, 1).unwrap();
        process.wr8(info + 1, 4).unwrap();
        process.wr8(IMAGE_BASE + 0x110, 0xc3).unwrap();
        let mut context = context();
        context.gpr[RSP] = STACK;

        assert!(matches!(
            unwind_function(
                &mut process,
                IMAGE_BASE,
                IMAGE_BASE + 0x110,
                entry,
                1,
                &mut context,
                0,
                false,
            ),
            Err(Stop::Unimplemented { .. })
        ));
        assert_eq!(context.gpr[RSP], STACK);
    }

    #[test]
    fn restores_nonvolatile_gpr_and_xmm_slots_and_frame_register() {
        let mut process = process_with_memory();
        let mut context = context();
        context.gpr[RSP] = STACK;
        context.gpr[5] = STACK + 0x90;
        process.wr16(INFO + 6, 2).unwrap(); // RSP + 16
        process.wr16(INFO + 10, 0x18).unwrap(); // far offset 0x18
        process.wr16(INFO + 14, 2).unwrap(); // XMM offset 32
        process.wr16(INFO + 18, 0x40).unwrap(); // far XMM offset 0x40
        process.wr64(STACK + 16, 0xfeed_beef).unwrap();
        process.wr64(STACK + 0x18, 0xcafe_babe).unwrap();
        process
            .mem
            .write_u128(STACK + 32, 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00)
            .unwrap();
        process
            .mem
            .write_u128(STACK + 0x40, 0xaabb_ccdd_eeff_0011_2233_4455_6677_8899)
            .unwrap();

        apply_unwind_code(&process, INFO, 0, 4, 3, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.gpr[3], 0xfeed_beef);
        apply_unwind_code(&process, INFO, 2, 5, 3, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.gpr[3], 0xcafe_babe);
        apply_unwind_code(&process, INFO, 4, 8, 6, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.xmm[6], 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        apply_unwind_code(&process, INFO, 6, 9, 7, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.xmm[7], 0xaabb_ccdd_eeff_0011_2233_4455_6677_8899);
        apply_unwind_code(&process, INFO, 0, 3, 0, 5, 0x10, &mut context, &mut false).unwrap();
        assert_eq!(context.gpr[RSP], STACK + 0x80);
    }

    #[test]
    fn handles_large_allocations_and_machine_frames() {
        let mut process = process_with_memory();
        let mut context = context();
        context.gpr[RSP] = STACK;
        process.wr16(INFO + 6, 4).unwrap();
        apply_unwind_code(&process, INFO, 0, 1, 0, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.gpr[RSP], STACK + 32);

        context.gpr[RSP] = STACK;
        process.wr16(INFO + 6, 0x1234).unwrap();
        process.wr16(INFO + 8, 1).unwrap();
        apply_unwind_code(&process, INFO, 0, 1, 1, 0, 0, &mut context, &mut false).unwrap();
        assert_eq!(context.gpr[RSP], STACK + 0x11234);

        context.gpr[RSP] = STACK;
        process.wr64(STACK, 0x1400_1234).unwrap();
        process.wr64(STACK + 16, 0x202).unwrap();
        process.wr64(STACK + 24, STACK + 0x800).unwrap();
        let mut machine_frame = false;
        apply_unwind_code(
            &process,
            INFO,
            0,
            10,
            0,
            0,
            0,
            &mut context,
            &mut machine_frame,
        )
        .unwrap();
        assert!(machine_frame);
        assert_eq!(context.rip, 0x1400_1234);
        assert_eq!(context.rflags, 0x202);
        assert_eq!(context.gpr[RSP], STACK + 0x800);
    }

    #[test]
    fn chained_records_apply_both_frames_before_popping_return_address() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const CONTEXT: u64 = 0x30000;
        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RW));
        assert!(process.mem.map(CONTEXT, PAGE_SIZE, PROT_RW));
        let entry = IMAGE_BASE + 0x300;
        let child_info = IMAGE_BASE + 0x400;
        let parent_info = IMAGE_BASE + 0x500;
        process.wr32(entry, 0x100).unwrap();
        process.wr32(entry + 4, 0x120).unwrap();
        process.wr32(entry + 8, 0x400).unwrap();
        process.wr8(child_info, 0x21).unwrap(); // version 1, chained info
        process.wr8(child_info + 1, 4).unwrap();
        process.wr8(child_info + 2, 1).unwrap();
        process.wr8(child_info + 4, 4).unwrap();
        process.wr8(child_info + 5, 0x32).unwrap(); // 32-byte allocation
        process.wr32(child_info + 8, 0x200).unwrap();
        process.wr32(child_info + 12, 0x220).unwrap();
        process.wr32(child_info + 16, 0x500).unwrap();
        process.wr8(parent_info, 1).unwrap();
        process.wr8(parent_info + 1, 4).unwrap();
        process.wr8(parent_info + 2, 1).unwrap();
        process.wr8(parent_info + 4, 4).unwrap();
        process.wr8(parent_info + 5, 0x12).unwrap(); // 16-byte allocation
        process
            .wr64(CONTEXT + CONTEXT_GPR_BASE + 8 * RSP as u64, STACK)
            .unwrap();
        process.wr64(STACK + 48, IMAGE_BASE + 0x777).unwrap();
        process.wr8(IMAGE_BASE + 0x105, 0x90).unwrap();
        let mut context = read_context(&process, CONTEXT).unwrap();

        let result = unwind_function(
            &mut process,
            IMAGE_BASE,
            IMAGE_BASE + 0x105,
            entry,
            0,
            &mut context,
            0,
            false,
        )
        .unwrap();

        assert_eq!(result.handler, 0);
        assert_eq!(context.rip, IMAGE_BASE + 0x777);
        assert_eq!(context.gpr[RSP], STACK + 56);
    }
}
