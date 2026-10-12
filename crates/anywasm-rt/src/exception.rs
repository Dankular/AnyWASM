//! Win64 PE unwind primitives used by ntdll's `RtlVirtualUnwind` export.

use crate::process::{GuestUnwind, Process, Stop, TEB_ADDR};
use anywasm_cpu::cpu::{Cpu, RBP, RBX, RDI, RSI, RSP};

const CONTEXT_GPR_BASE: u64 = 0x78;
const CONTEXT_RFLAGS: u64 = 0x44;
const CONTEXT_RIP: u64 = 0xf8;
const CONTEXT_XMM_BASE: u64 = 0x1a0;
const CONTEXT_SIZE: u64 = 0x4d0;
const EXCEPTION_RECORD_SIZE: u64 = 0x98;
const DISPATCHER_CONTEXT_SIZE: u64 = 0x50;
const EXCEPTION_CONTINUE_EXECUTION: u64 = 0;
const EXCEPTION_CONTINUE_SEARCH: u64 = 1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const MSVC_CPP_EXCEPTION: u32 = 0xe06d_7363;
const MSVC_CPP_MAGIC: u64 = 0x1993_0520;
const MAX_EXCEPTION_FRAMES: usize = 128;
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

#[derive(Clone)]
struct ExceptionFrame {
    control_pc: u64,
    image_base: u64,
    function_entry: u64,
    establisher: u64,
    context: Context64,
}

/// Search guest frames for a handler for the selected target's MSVC C++ exception form.
pub(crate) fn dispatch_raised_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    caller: u64,
) -> Result<u64, Stop> {
    if code != MSVC_CPP_EXCEPTION || args.len() != 4 || args[0] != MSVC_CPP_MAGIC {
        return Err(Stop::Crash(format!(
            "RaiseException code={code:#x} flags={flags:#x} is outside the supported MSVC C++ form"
        )));
    }
    let image_base = args[3];
    let throw_info = args[2];
    let module = p
        .modules
        .iter()
        .find(|module| {
            module.hle_group.is_none()
                && module.base == image_base
                && throw_info >= module.base
                && throw_info < module.base.saturating_add(module.size)
        })
        .ok_or_else(|| {
            Stop::Crash(format!(
                "MSVC C++ ThrowInfo {throw_info:#x} is outside loaded image {image_base:#x}"
            ))
        })?;
    if module.runtime_functions.is_empty() {
        return Err(Stop::Crash(format!(
            "MSVC C++ throw image {} has no PE runtime-function table",
            module.name
        )));
    }

    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    let hle_rsp = p.cpu.gpr[RSP];
    if stack_limit >= stack_base || hle_rsp < stack_limit || hle_rsp >= stack_base {
        return Err(Stop::Crash(format!(
            "RaiseException guest stack pointer {hle_rsp:#x} is outside TEB bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }
    if p.rd64(hle_rsp)? != caller {
        return Err(Stop::Crash(format!(
            "RaiseException caller {caller:#x} does not match guest return slot at {hle_rsp:#x}"
        )));
    }

    let exception_record = crate::hle::kernel32::alloc_block(p, EXCEPTION_RECORD_SIZE, true);
    let context_record = crate::hle::kernel32::alloc_block(p, CONTEXT_SIZE, true);
    let dispatcher_context = crate::hle::kernel32::alloc_block(p, DISPATCHER_CONTEXT_SIZE, true);
    write_exception_record(p, exception_record, code, flags, caller, args)?;
    p.wr32(context_record + 0x30, 0x0010_000b)?; // CONTEXT_AMD64 | CONTROL | INTEGER | FLOATING_POINT
    p.wr32(context_record + 0x34, 0x1f80)?; // Default guest MXCSR.

    // RaiseException is an HLE call. Model its guest frame as already popped before beginning
    // the search, leaving the original guest callsite PC and the caller's pre-call RSP.
    let mut context = context_from_cpu(&p.cpu, caller, hle_rsp + 8);
    let exception_context = context.clone();
    write_context(p, context_record, &context)?;
    let mut control_pc = caller;
    let mut last_rsp = context.gpr[RSP];
    let mut frames = Vec::new();

    for frame_number in 0..MAX_EXCEPTION_FRAMES {
        if control_pc == 0 || last_rsp < stack_limit || last_rsp >= stack_base {
            return Err(Stop::Crash(format!(
                "MSVC C++ exception search left guest stack after {frame_number} frames (pc={control_pc:#x}, rsp={last_rsp:#x})"
            )));
        }
        let Some((module_base, function_entry)) = lookup_frame(p, control_pc) else {
            return Err(Stop::Crash(format!(
                "MSVC C++ exception search reached non-module PC {control_pc:#x} after {frame_number} frames"
            )));
        };
        let frame_context = context.clone();
        let mut caller_context = frame_context.clone();
        let establisher = if function_entry == 0 {
            frame_context.gpr[RSP]
        } else {
            read_frame_register(p, module_base, control_pc, function_entry, &frame_context)?
        };
        let result = if function_entry == 0 {
            unwind_leaf(p, &mut caller_context)?
        } else {
            unwind_function(
                p,
                module_base,
                control_pc,
                function_entry,
                1,
                &mut caller_context,
                0,
                false,
            )?
        };
        frames.push(ExceptionFrame {
            control_pc,
            image_base: module_base,
            function_entry,
            establisher,
            context: frame_context.clone(),
        });

        if result.handler != 0 {
            // Windows passes the original exception context to every first-pass handler.
            // The per-frame context is carried separately by the dispatcher walk.
            write_context(p, context_record, &exception_context)?;
            write_dispatcher_context(
                p,
                dispatcher_context,
                control_pc,
                module_base,
                function_entry,
                establisher,
                context_record,
                0,
                0,
                result.handler,
                result.handler_data,
            )?;
            if p.trace_imports {
                p.stderr.extend_from_slice(
                    format!(
                        "[seh] invoke frame={frame_number} pc={control_pc:#x} establisher={establisher:#x} handler={:#x} data={:#x}\n",
                        result.handler, result.handler_data
                    )
                    .as_bytes(),
                );
            }
            let disposition = match p.call_guest(
                result.handler,
                &[
                    exception_record,
                    establisher,
                    context_record,
                    dispatcher_context,
                ],
            ) {
                Ok(disposition) => disposition as u32,
                Err(Stop::GuestUnwind(unwind)) => {
                    complete_guest_unwind(p, &frames, dispatcher_context, unwind)?;
                    p.skip_ret = true;
                    return Ok(0);
                }
                Err(stop) => return Err(stop),
            };
            if p.trace_imports {
                p.stderr.extend_from_slice(
                    format!(
                        "[seh] search frame={frame_number} pc={control_pc:#x} establisher={establisher:#x} handler={:#x} disposition={disposition}\n",
                        result.handler
                    )
                    .as_bytes(),
                );
            }
            match disposition as u64 {
                EXCEPTION_CONTINUE_SEARCH => {}
                EXCEPTION_CONTINUE_EXECUTION => {
                    return Err(Stop::Crash(format!(
                        "noncontinuable MSVC C++ exception handler at {:#x} returned ExceptionContinueExecution",
                        result.handler
                    )))
                }
                value => {
                    return Err(Stop::Unimplemented {
                        dll: "ntdll".into(),
                        name: format!(
                            "MSVC C++ exception handler disposition {value} at {:#x}",
                            result.handler
                        ),
                    })
                }
            }
        }

        let next_rsp = caller_context.gpr[RSP];
        let next_pc = caller_context.rip;
        if next_rsp <= last_rsp || next_rsp > stack_base {
            return Err(Stop::Crash(format!(
                "MSVC C++ exception search made invalid stack progress {last_rsp:#x} -> {next_rsp:#x}"
            )));
        }
        if next_pc == 0 {
            return Err(Stop::Crash(format!(
                "MSVC C++ exception search exhausted the guest stack after {frame_number} frames"
            )));
        }
        context = caller_context;
        control_pc = next_pc;
        last_rsp = next_rsp;
    }

    Err(Stop::Crash(format!(
        "MSVC C++ exception search exceeded {MAX_EXCEPTION_FRAMES} frames"
    )))
}

/// Capture the target's non-returning `RtlUnwindEx` request for the active dispatcher.
pub(crate) fn rtl_unwind_ex(p: &mut Process) -> Result<u64, Stop> {
    let unwind = GuestUnwind {
        target_frame: p.arg(0),
        target_ip: p.arg(1),
        call_rsp: p.cpu.gpr[RSP],
        exception_record: p.arg(2),
        return_value: p.arg(3),
        context_record: p.arg(4),
        history_table: p.arg(5),
    };
    if unwind.target_frame == 0 || unwind.target_ip == 0 || unwind.exception_record == 0 {
        return Err(Stop::Crash(
            "RtlUnwindEx received a null target frame, target IP, or exception record".into(),
        ));
    }
    if unwind.history_table != 0 {
        return unsupported(
            p,
            "non-null unwind history tables are not implemented".into(),
        );
    }
    if lookup_frame(p, unwind.target_ip).is_none() {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} is outside loaded guest images",
            unwind.target_ip
        )));
    }
    let context = read_context(p, unwind.context_record)?;
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    if lookup_frame(p, context.rip).is_none()
        || context.gpr[RSP] < stack_limit
        || context.gpr[RSP] >= stack_base
    {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx start context ({:#x},{:#x}) is outside the guest image or TEB stack",
            context.rip, context.gpr[RSP]
        )));
    }
    if p.trace_imports {
        let args = (0..6)
            .map(|index| format!("{:#x}", p.arg(index)))
            .collect::<Vec<_>>()
            .join(", ");
        let caller = p.rd64(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[seh] RtlUnwindEx args=[{args}] caller={caller:#x} context={:#x} context_rip={:#x} context_rsp={:#x}\n",
                unwind.context_record, context.rip, context.gpr[RSP]
            )
            .as_bytes(),
        );
    }
    Err(Stop::GuestUnwind(unwind))
}

fn complete_guest_unwind(
    p: &mut Process,
    frames: &[ExceptionFrame],
    dispatcher_context: u64,
    unwind: GuestUnwind,
) -> Result<(), Stop> {
    if unwind.history_table != 0 {
        return Err(Stop::Crash(
            "RtlUnwindEx unexpectedly supplied a history table during dispatch".into(),
        ));
    }
    let Some(target_index) = frames
        .iter()
        .position(|frame| frame.establisher == unwind.target_frame)
    else {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target frame {:#x} was not in the searched guest frames",
            unwind.target_frame
        )));
    };
    let original_context = read_context(p, unwind.context_record)?;
    let Some(first_frame) = frames.first() else {
        return Err(Stop::Crash(
            "RtlUnwindEx was requested with an empty exception stack".into(),
        ));
    };
    if original_context.rip != first_frame.context.rip
        || original_context.gpr[RSP] != first_frame.context.gpr[RSP]
    {
        return Err(Stop::Crash(
            "RtlUnwindEx exception context changed before the second pass".into(),
        ));
    }
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    if unwind.target_frame < stack_limit || unwind.target_frame >= stack_base {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target frame {:#x} is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}",
            unwind.target_frame
        )));
    }
    if unwind.call_rsp < stack_limit || unwind.call_rsp >= stack_base {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx call stack pointer {:#x} is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}",
            unwind.call_rsp
        )));
    }
    let Some((target_image_base, target_function_entry)) = lookup_frame(p, unwind.target_ip) else {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} is outside loaded guest images",
            unwind.target_ip
        )));
    };
    let target_frame = &frames[target_index];
    if target_frame.image_base != target_image_base
        || target_frame.function_entry != target_function_entry
    {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} does not belong to target frame {:#x}",
            unwind.target_ip, unwind.target_frame
        )));
    }
    let mut transfer_context = target_frame.context.clone();
    let target_scope_index = p.rd32(dispatcher_context + 0x48)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[seh] unwind begin target_frame={:#x} target_ip={:#x} call_rsp={:#x} scope={target_scope_index} start=({:#x},{:#x})\n",
                unwind.target_frame,
                unwind.target_ip,
                unwind.call_rsp,
                original_context.rip,
                original_context.gpr[RSP]
            )
            .as_bytes(),
        );
    }

    // Second-pass handlers run as nested calls from RtlUnwindEx. Preserve that active stack
    // position so their frames grow below the exception record and handler locals.
    p.cpu.gpr[RSP] = unwind.call_rsp;
    for (index, frame) in frames.iter().take(target_index + 1).enumerate() {
        if frame.function_entry == 0 {
            continue;
        }
        let mut caller_context = frame.context.clone();
        let result = unwind_function(
            p,
            frame.image_base,
            frame.control_pc,
            frame.function_entry,
            2,
            &mut caller_context,
            0,
            false,
        )?;
        if result.handler == 0 {
            continue;
        }
        let scope_index = if index == target_index {
            target_scope_index
        } else {
            0
        };
        if index < target_index {
            let next_frame = &frames[index + 1];
            if caller_context.rip != next_frame.context.rip
                || caller_context.gpr[RSP] != next_frame.context.gpr[RSP]
            {
                return Err(Stop::Crash(format!(
                    "RtlUnwindEx frame walk diverged after {:#x}: next=({:#x},{:#x}), searched=({:#x},{:#x})",
                    frame.control_pc,
                    caller_context.rip,
                    caller_context.gpr[RSP],
                    next_frame.context.rip,
                    next_frame.context.gpr[RSP]
                )));
            }
        }
        // During phase two each termination handler receives the current frame's
        // unwind context; the next frame's context is installed after it returns.
        let handler_context = frame.context.clone();
        write_context(p, unwind.context_record, &handler_context)?;
        write_dispatcher_context(
            p,
            dispatcher_context,
            frame.control_pc,
            frame.image_base,
            frame.function_entry,
            frame.establisher,
            unwind.context_record,
            unwind.target_ip,
            scope_index,
            result.handler,
            result.handler_data,
        )?;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh] unwind frame={index} pc={:#x} establisher={:#x} handler={:#x} target={:#x} context=({:#x},{:#x}) rbx={:#x} rbp={:#x} rsi={:#x} rdi={:#x}\n",
                    frame.control_pc,
                    frame.establisher,
                    result.handler,
                    unwind.target_ip,
                    handler_context.rip,
                    handler_context.gpr[RSP],
                    handler_context.gpr[RBX],
                    handler_context.gpr[RBP],
                    handler_context.gpr[RSI],
                    handler_context.gpr[RDI]
                )
                .as_bytes(),
            );
        }
        let phase_two_flags = EXCEPTION_UNWINDING
            | if index == target_index {
                EXCEPTION_TARGET_UNWIND
            } else {
                0
            };
        p.wr32(unwind.exception_record + 4, phase_two_flags)?;
        let disposition = match p.call_guest(
            result.handler,
            &[
                unwind.exception_record,
                frame.establisher,
                unwind.context_record,
                dispatcher_context,
            ],
        ) {
            Ok(disposition) => disposition as u32,
            Err(Stop::GuestUnwind(_)) => {
                return Err(Stop::Crash(
                    "nested RtlUnwindEx during the selected second-pass unwind".into(),
                ))
            }
            Err(stop) => return Err(stop),
        };
        if disposition != EXCEPTION_CONTINUE_SEARCH as u32 {
            return Err(Stop::Unimplemented {
                dll: "ntdll".into(),
                name: format!(
                    "second-pass exception handler disposition {disposition} at {:#x}",
                    result.handler
                ),
            });
        }
        p.wr32(unwind.exception_record + 4, EXCEPTION_UNWINDING)?;
    }

    transfer_context.rip = unwind.target_ip;
    transfer_context.gpr[anywasm_cpu::cpu::RAX] = unwind.return_value;
    write_context(p, unwind.context_record, &transfer_context)?;
    p.cpu.gpr = transfer_context.gpr;
    p.cpu.xmm = transfer_context.xmm;
    p.cpu.rip = transfer_context.rip;
    p.cpu.rflags = transfer_context.rflags;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[seh] transfer frame={:#x} ip={:#x} rsp={:#x} rax={:#x}\n",
                unwind.target_frame,
                unwind.target_ip,
                p.cpu.gpr[RSP],
                p.cpu.gpr[anywasm_cpu::cpu::RAX]
            )
            .as_bytes(),
        );
    }
    Ok(())
}

fn context_from_cpu(cpu: &Cpu, rip: u64, rsp: u64) -> Context64 {
    let mut gpr = cpu.gpr;
    gpr[RSP] = rsp;
    Context64 {
        gpr,
        xmm: cpu.xmm,
        rip,
        rflags: cpu.rflags,
    }
}

fn write_exception_record(
    p: &mut Process,
    address: u64,
    code: u32,
    flags: u32,
    caller: u64,
    args: &[u64],
) -> Result<(), Stop> {
    p.wr32(address, code)?;
    p.wr32(address + 4, flags)?;
    p.wr64(address + 8, 0)?;
    p.wr64(address + 16, caller)?;
    p.wr32(address + 24, args.len().min(15) as u32)?;
    for (index, argument) in args.iter().take(15).enumerate() {
        p.wr64(address + 32 + index as u64 * 8, *argument)?;
    }
    Ok(())
}

fn write_dispatcher_context(
    p: &mut Process,
    address: u64,
    control_pc: u64,
    image_base: u64,
    function_entry: u64,
    establisher: u64,
    context_record: u64,
    target_ip: u64,
    scope_index: u32,
    handler: u64,
    handler_data: u64,
) -> Result<(), Stop> {
    for (offset, value) in [
        (0x00, control_pc),
        (0x08, image_base),
        (0x10, function_entry),
        (0x18, establisher),
        (0x20, target_ip),
        (0x28, context_record),
        (0x30, handler),
        (0x38, handler_data),
        (0x40, 0), // HistoryTable is not used by the selected image.
    ] {
        p.wr64(address + offset, value)?;
    }
    p.wr32(address + 0x48, scope_index)?;
    Ok(())
}

fn lookup_frame(p: &Process, control_pc: u64) -> Option<(u64, u64)> {
    let module = p.modules.iter().find(|module| {
        module.hle_group.is_none()
            && control_pc >= module.base
            && control_pc - module.base < module.size
    })?;
    let rva = control_pc - module.base;
    let index = module
        .runtime_functions
        .partition_point(|function| function.begin_address as u64 <= rva)
        .checked_sub(1)
        .filter(|index| rva < module.runtime_functions[*index].end_address as u64)?;
    Some((
        module.base,
        module.base + module.exception_table_rva as u64 + index as u64 * 12,
    ))
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
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

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

    fn test_module() -> crate::process::Module {
        crate::process::Module {
            name: "guest.exe".into(),
            path: "C:\\guest.exe".into(),
            base: 0x1400_0000_0,
            size: 0x2000,
            entry: 0x1400_00100,
            is_dll: false,
            exports: Default::default(),
            ordinals: Default::default(),
            exception_table_rva: 0x500,
            runtime_functions: vec![anywasm_pe::RuntimeFunction {
                begin_address: 0x100,
                end_address: 0x120,
                unwind_info_address: 0x300,
            }],
            resource_rva: 0,
            resource_size: 0,
            hle_group: None,
        }
    }

    #[test]
    fn exception_record_preserves_raise_exception_parameters() {
        let mut process = process_with_memory();
        write_exception_record(
            &mut process,
            INFO,
            MSVC_CPP_EXCEPTION,
            1,
            0x1400_1234,
            &[MSVC_CPP_MAGIC, 0x1234, 0x5678, 0x1400_0000_0],
        )
        .unwrap();

        assert_eq!(process.rd32(INFO).unwrap(), MSVC_CPP_EXCEPTION);
        assert_eq!(process.rd32(INFO + 4).unwrap(), 1);
        assert_eq!(process.rd64(INFO + 8).unwrap(), 0);
        assert_eq!(process.rd64(INFO + 16).unwrap(), 0x1400_1234);
        assert_eq!(process.rd32(INFO + 24).unwrap(), 4);
        assert_eq!(process.rd64(INFO + 32).unwrap(), MSVC_CPP_MAGIC);
        assert_eq!(process.rd64(INFO + 56).unwrap(), 0x1400_0000_0);
    }

    #[test]
    fn dispatcher_context_uses_the_x64_field_offsets_observed_in_the_target() {
        let mut process = process_with_memory();
        write_dispatcher_context(
            &mut process,
            INFO,
            0x1400_1234,
            0x1400_0000_0,
            0x1400_0500,
            STACK + 0x40,
            INFO + 0x200,
            0,
            0,
            0x1400_6789,
            INFO + 0x80,
        )
        .unwrap();

        assert_eq!(process.rd64(INFO).unwrap(), 0x1400_1234);
        assert_eq!(process.rd64(INFO + 8).unwrap(), 0x1400_0000_0);
        assert_eq!(process.rd64(INFO + 16).unwrap(), 0x1400_0500);
        assert_eq!(process.rd64(INFO + 24).unwrap(), STACK + 0x40);
        assert_eq!(process.rd64(INFO + 32).unwrap(), 0);
        assert_eq!(process.rd64(INFO + 40).unwrap(), INFO + 0x200);
        assert_eq!(process.rd64(INFO + 48).unwrap(), 0x1400_6789);
        assert_eq!(process.rd64(INFO + 56).unwrap(), INFO + 0x80);
        assert_eq!(process.rd64(INFO + 64).unwrap(), 0);
        assert_eq!(process.rd32(INFO + 72).unwrap(), 0);
    }

    #[test]
    fn lookup_frame_uses_sorted_runtime_metadata_and_end_exclusive_ranges() {
        let mut process = process_with_memory();
        process.modules.push(test_module());

        assert_eq!(
            lookup_frame(&process, 0x1400_00110),
            Some((0x1400_0000_0, 0x1400_00500))
        );
        assert_eq!(lookup_frame(&process, 0x1400_00120), None);
    }

    #[test]
    fn exception_context_starts_at_guest_caller_after_hle_return_slot() {
        let mut cpu = Cpu::new();
        cpu.gpr[RSP] = STACK - 8;

        let context = context_from_cpu(&cpu, 0x1400_1234, STACK);

        assert_eq!(context.rip, 0x1400_1234);
        assert_eq!(context.gpr[RSP], STACK);
    }

    #[test]
    fn second_pass_calls_termination_handlers_on_the_active_unwind_stack() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const GUEST_STACK: u64 = 0x80000;
        const STACK_LIMIT: u64 = GUEST_STACK;
        const STACK_BASE: u64 = GUEST_STACK + 0x10000;
        const ORIGINAL_RSP: u64 = GUEST_STACK + 0x4000;
        const UNWIND_CALL_RSP: u64 = GUEST_STACK + 0x2000;
        const EXCEPTION_RECORD: u64 = GUEST_STACK + 0x2f00;
        const CONTEXT_RECORD: u64 = INFO + 0x200;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x100;
        const HANDLER_RSP_OUT: u64 = INFO + 0x180;
        const HANDLER_RVA: u32 = 0x800;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(GUEST_STACK, 0x10000, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK_BASE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK_LIMIT).unwrap();

        let mut module = test_module();
        module.runtime_functions = vec![
            anywasm_pe::RuntimeFunction {
                begin_address: 0x100,
                end_address: 0x120,
                unwind_info_address: 0x300,
            },
            anywasm_pe::RuntimeFunction {
                begin_address: 0x200,
                end_address: 0x220,
                unwind_info_address: 0x320,
            },
        ];
        process.modules.push(module);
        for (entry_rva, begin, end, unwind_rva) in
            [(0x500, 0x100, 0x120, 0x300), (0x50c, 0x200, 0x220, 0x320)]
        {
            process.wr32(IMAGE_BASE + entry_rva, begin).unwrap();
            process.wr32(IMAGE_BASE + entry_rva + 4, end).unwrap();
            process
                .wr32(IMAGE_BASE + entry_rva + 8, unwind_rva)
                .unwrap();
            let unwind_info = IMAGE_BASE + unwind_rva as u64;
            process.wr8(unwind_info, 0x11).unwrap(); // version 1, UHANDLER
            process.wr8(unwind_info + 1, 0).unwrap();
            process.wr8(unwind_info + 2, 0).unwrap();
            process.wr8(unwind_info + 3, 0).unwrap();
            process.wr32(unwind_info + 4, HANDLER_RVA).unwrap();
        }

        let handler = IMAGE_BASE + HANDLER_RVA as u64;
        let mut handler_code = vec![0x48, 0xb8]; // mov rax, HANDLER_RSP_OUT
        handler_code.extend_from_slice(&HANDLER_RSP_OUT.to_le_bytes());
        handler_code.extend_from_slice(&[
            0x48, 0x89, 0x20, // mov [rax], rsp
            0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, ExceptionContinueSearch
            0xc3, // ret
        ]);
        process.write_bytes(handler, &handler_code).unwrap();

        let first_pc = IMAGE_BASE + 0x110;
        let target_pc = IMAGE_BASE + 0x210;
        let mut first_context = context();
        first_context.rip = first_pc;
        first_context.gpr[RSP] = ORIGINAL_RSP;
        process.wr64(ORIGINAL_RSP, target_pc).unwrap();
        process.wr64(ORIGINAL_RSP + 8, IMAGE_BASE + 0x300).unwrap();
        let mut target_context = first_context.clone();
        target_context.rip = target_pc;
        target_context.gpr[RSP] = ORIGINAL_RSP + 8;
        write_context(&mut process, CONTEXT_RECORD, &first_context).unwrap();
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            MSVC_CPP_EXCEPTION,
            1,
            first_pc,
            &[MSVC_CPP_MAGIC, 0, 0, IMAGE_BASE],
        )
        .unwrap();

        let frames = vec![
            ExceptionFrame {
                control_pc: first_pc,
                image_base: IMAGE_BASE,
                function_entry: IMAGE_BASE + 0x500,
                establisher: ORIGINAL_RSP,
                context: first_context,
            },
            ExceptionFrame {
                control_pc: target_pc,
                image_base: IMAGE_BASE,
                function_entry: IMAGE_BASE + 0x50c,
                establisher: ORIGINAL_RSP + 8,
                context: target_context,
            },
        ];
        let unwind = GuestUnwind {
            target_frame: ORIGINAL_RSP + 8,
            target_ip: target_pc,
            call_rsp: UNWIND_CALL_RSP,
            exception_record: EXCEPTION_RECORD,
            return_value: 0x55,
            context_record: CONTEXT_RECORD,
            history_table: 0,
        };
        process.cpu.gpr[RSP] = ORIGINAL_RSP;

        complete_guest_unwind(&mut process, &frames, DISPATCHER_CONTEXT, unwind).unwrap();

        let mut expected_handler_rsp = (UNWIND_CALL_RSP - 0x200) & !0xf;
        expected_handler_rsp = (expected_handler_rsp - 0x20) & !0xf;
        expected_handler_rsp -= 8;
        assert_eq!(process.rd64(HANDLER_RSP_OUT).unwrap(), expected_handler_rsp);
        assert_eq!(process.cpu.rip, target_pc);
        assert_eq!(process.cpu.gpr[RSP], ORIGINAL_RSP + 8);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], 0x55);
        assert_eq!(
            process.rd32(EXCEPTION_RECORD + 4).unwrap(),
            EXCEPTION_UNWINDING
        );
        assert_eq!(
            process.rd64(CONTEXT_RECORD + CONTEXT_RIP).unwrap(),
            target_pc
        );
        assert_eq!(
            process
                .rd64(CONTEXT_RECORD + CONTEXT_GPR_BASE + 8 * RSP as u64)
                .unwrap(),
            ORIGINAL_RSP + 8
        );
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
