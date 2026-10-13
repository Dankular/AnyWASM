//! Win64 PE unwind primitives used by ntdll's `RtlVirtualUnwind` export.

use crate::process::{GuestUnwind, Process, Stop, TEB_ADDR};
use anywasm_cpu::cpu::{Cpu, CpuMode, RBP, RBX, RDI, RSI, RSP};
use anywasm_cpu::mem::{Access, PROT_R, PROT_W, PROT_X};
use anywasm_cpu::{DecodeError, Exit};

const CONTEXT_GPR_BASE: u64 = 0x78;
const CONTEXT_MXCSR: u64 = 0x34;
const CONTEXT_RFLAGS: u64 = 0x44;
const CONTEXT_RIP: u64 = 0xf8;
const CONTEXT_XMM_BASE: u64 = 0x1a0;
const CONTEXT_SIZE: u64 = 0x4d0;
const EXCEPTION_RECORD_SIZE: u64 = 0x98;
const EXCEPTION_RECORD_PARAMETERS: u64 = 24;
const EXCEPTION_RECORD_ARGUMENTS: u64 = 32;
const DISPATCHER_CONTEXT_SIZE: u64 = 0x50;
const EXCEPTION_CONTINUE_EXECUTION: u64 = 0;
const EXCEPTION_CONTINUE_SEARCH: u64 = 1;
const EXCEPTION_CONTINUE_SEARCH_FILTER: i32 = 0;
const EXCEPTION_EXECUTE_HANDLER: i32 = 1;
const EXCEPTION_CONTINUE_EXECUTION_FILTER: i32 = -1;
const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const EXCEPTION_COLLIDED_UNWIND_FLAG: u32 = 0x40;
const DISPATCHER_CONTROL_PC: u64 = 0x00;
const DISPATCHER_IMAGE_BASE: u64 = 0x08;
const DISPATCHER_FUNCTION_ENTRY: u64 = 0x10;
const DISPATCHER_ESTABLISHER_FRAME: u64 = 0x18;
const DISPATCHER_TARGET_IP: u64 = 0x20;
const DISPATCHER_CONTEXT_RECORD: u64 = 0x28;
const DISPATCHER_LANGUAGE_HANDLER: u64 = 0x30;
const DISPATCHER_HANDLER_DATA: u64 = 0x38;
const DISPATCHER_HISTORY_TABLE: u64 = 0x40;
const DISPATCHER_SCOPE_INDEX: u64 = 0x48;
const UNWIND_HISTORY_TABLE_BYTES: u64 = 0xd8;
const UNWIND_HISTORY_TABLE_ENTRIES: u32 = 12;
const EXCEPTION_POINTERS_SIZE: u64 = 16;
const STATUS_ACCESS_VIOLATION: u32 = 0xc000_0005;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xc000_0094;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xc000_001d;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xc000_0096;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;
const MSVC_CPP_EXCEPTION: u32 = 0xe06d_7363;
const MSVC_CPP_MAGIC: u64 = 0x1993_0520;
const MAX_EXCEPTION_FRAMES: usize = 128;
const MAX_C_SCOPE_ENTRIES: u32 = 4096;
const UNW_FLAG_EHANDLER: u8 = 1;
const UNW_FLAG_UHANDLER: u8 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;
const EXCEPTION_NESTED_EXCEPTION: u64 = 2;
const EXCEPTION_NESTED_CALL: u32 = 0x10;
const EXCEPTION_COLLIDED_UNWIND_DISPOSITION: u32 = 3;

const WINDOWS_GPRS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Clone)]
struct Context64 {
    gpr: [u64; 16],
    xmm: [u128; 16],
    rip: u64,
    rflags: u64,
    mxcsr: u32,
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

/// Search guest frames for language handlers for a raised exception.
pub(crate) fn dispatch_raised_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    caller: u64,
) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return crate::exception32::dispatch_raised_exception(p, code, flags, args, caller);
    }
    if p.cpu.mode != CpuMode::X86_64 {
        return Err(Stop::Unimplemented {
            dll: "ntdll".into(),
            name: "PE32 structured exception dispatch".into(),
        });
    }
    if args.len() > 15 {
        return Err(Stop::Crash(format!(
            "RaiseException received {} parameters (maximum is 15)",
            args.len()
        )));
    }
    if code == MSVC_CPP_EXCEPTION {
        if args.len() != 4 || args[0] != MSVC_CPP_MAGIC {
            return Err(Stop::Crash(format!(
                "MSVC C++ exception code has an unsupported parameter layout ({} parameters)",
                args.len()
            )));
        }
        let throw_info = args[2];
        let image_base = args[3];
        if !p.modules.iter().any(|module| {
            module.hle_group.is_none()
                && module.base == image_base
                && throw_info >= module.base
                && throw_info < module.base.saturating_add(module.size)
        }) {
            return Err(Stop::Crash(format!(
                "MSVC C++ ThrowInfo {throw_info:#x} is outside loaded image {image_base:#x}"
            )));
        }
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

    // RaiseException is an HLE call. Model its guest frame as already popped before beginning
    // the search, leaving the original guest callsite PC and the caller's pre-call RSP.
    let context = context_from_cpu(&p.cpu, caller, hle_rsp + 8);
    dispatch_context_exception(p, code, flags, args, caller, context, true)
}

/// Convert selected CPU exits into Windows exception records and dispatch them through the
/// process's VEH, PE language handlers, vectored-continue handlers, and top-level filter.
pub(crate) fn dispatch_cpu_exception(p: &mut Process, exit: &Exit) -> Result<bool, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return crate::exception32::dispatch_cpu_exception(p, exit);
    }
    if p.cpu.mode != CpuMode::X86_64 {
        return Ok(false);
    }
    let (Ok(stack_base), Ok(stack_limit)) = (p.rd64(TEB_ADDR + 0x08), p.rd64(TEB_ADDR + 0x10))
    else {
        return Ok(false);
    };
    let rsp = p.cpu.gpr[RSP];
    if stack_limit >= stack_base || rsp < stack_limit || rsp >= stack_base {
        return Ok(false);
    }
    let (code, args, exception_address) = match exit {
        Exit::Fault(fault) => {
            let access = match fault.access {
                Access::Read => 0,
                Access::Write => 1,
                Access::Exec => 8,
            };
            (STATUS_ACCESS_VIOLATION, vec![access, fault.addr], p.cpu.rip)
        }
        Exit::DivideError => (STATUS_INTEGER_DIVIDE_BY_ZERO, Vec::new(), p.cpu.rip),
        Exit::Decode(DecodeError::Undefined(_)) => {
            (STATUS_ILLEGAL_INSTRUCTION, Vec::new(), p.cpu.rip)
        }
        Exit::IllegalInstruction => (STATUS_ILLEGAL_INSTRUCTION, Vec::new(), p.cpu.rip),
        Exit::PrivilegedInstruction => (STATUS_PRIVILEGED_INSTRUCTION, Vec::new(), p.cpu.rip),
        Exit::Breakpoint => (STATUS_BREAKPOINT, Vec::new(), p.cpu.rip.saturating_sub(1)),
        _ => return Ok(false),
    };
    let context = context_from_cpu(&p.cpu, p.cpu.rip, p.cpu.gpr[RSP]);
    dispatch_context_exception(p, code, 0, &args, exception_address, context, false)?;
    Ok(true)
}

fn dispatch_context_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    exception_address: u64,
    context: Context64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let thread_id = p.current_thread_id;
    let depth = p.guest_exception_depths.entry(thread_id).or_default();
    *depth = depth.saturating_add(1);

    let result = dispatch_context_exception_inner(
        p,
        code,
        flags,
        args,
        exception_address,
        context,
        skip_ret_on_resume,
    );

    let remove_depth = if let Some(depth) = p.guest_exception_depths.get_mut(&thread_id) {
        *depth = depth.saturating_sub(1);
        *depth == 0
    } else {
        false
    };
    if remove_depth {
        p.guest_exception_depths.remove(&thread_id);
    }
    result
}

fn dispatch_context_exception_inner(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    exception_address: u64,
    mut context: Context64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    let initial_rsp = context.gpr[RSP];
    if stack_limit >= stack_base || initial_rsp < stack_limit || initial_rsp >= stack_base {
        return Err(Stop::Crash(format!(
            "exception guest stack pointer {initial_rsp:#x} is outside TEB bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }

    let exception_record = crate::hle::kernel32::alloc_block(p, EXCEPTION_RECORD_SIZE, true);
    let context_record = crate::hle::kernel32::alloc_block(p, CONTEXT_SIZE, true);
    let dispatcher_frame_context = crate::hle::kernel32::alloc_block(p, CONTEXT_SIZE, true);
    let dispatcher_context = crate::hle::kernel32::alloc_block(p, DISPATCHER_CONTEXT_SIZE, true);
    write_exception_record(p, exception_record, code, flags, exception_address, args)?;
    p.wr32(context_record + 0x30, 0x0010_000b)?; // CONTEXT_AMD64 | CONTROL | INTEGER | FLOATING_POINT
    p.wr32(dispatcher_frame_context + 0x30, 0x0010_000b)?;

    let exception_context = context.clone();
    write_context(p, context_record, &context)?;
    let mut control_pc = context.rip;
    let mut last_rsp = context.gpr[RSP];
    let mut frames = Vec::new();
    let mut search_flags = flags;
    let mut search_history_table = 0;
    let mut nested_frame = 0;

    if dispatch_vectored_exception_handlers(p, exception_record, context_record)? {
        return resume_exception(p, exception_record, context_record, skip_ret_on_resume);
    }

    for frame_number in 0..MAX_EXCEPTION_FRAMES {
        if control_pc == 0 || last_rsp < stack_limit || last_rsp >= stack_base {
            return dispatch_unhandled_exception(
                p,
                exception_record,
                context_record,
                code,
                search_flags,
                skip_ret_on_resume,
                format!(
                    "search left guest stack after {frame_number} frames (pc={control_pc:#x}, rsp={last_rsp:#x})"
                ),
            );
        }
        let Some((module_base, function_entry)) = lookup_frame(p, control_pc)? else {
            return dispatch_unhandled_exception(
                p,
                exception_record,
                context_record,
                code,
                search_flags,
                skip_ret_on_resume,
                format!("search reached non-module PC {control_pc:#x} after {frame_number} frames"),
            );
        };
        let frame_context = context.clone();
        let mut caller_context = frame_context.clone();
        let mut establisher = if function_entry == 0 {
            frame_context.gpr[RSP]
        } else {
            read_frame_register(p, module_base, control_pc, function_entry, &frame_context)?
        };
        let mut result = if function_entry == 0 {
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
        let mut frame = ExceptionFrame {
            control_pc,
            image_base: module_base,
            function_entry,
            establisher,
            context: frame_context.clone(),
        };
        frames.push(frame.clone());

        if result.handler != 0 {
            // Windows passes the original exception context to every first-pass handler.
            // The per-frame context is carried separately by the dispatcher walk.
            let mut scope_index = 0;
            let mut collision_count = 0;
            let mut collision_redirected = false;
            let disposition = loop {
                p.wr32(exception_record + 4, search_flags)?;
                write_context(p, context_record, &exception_context)?;
                write_context(p, dispatcher_frame_context, &frame.context)?;
                write_dispatcher_context(
                    p,
                    dispatcher_context,
                    frame.control_pc,
                    frame.image_base,
                    frame.function_entry,
                    frame.establisher,
                    dispatcher_frame_context,
                    0,
                    scope_index,
                    result.handler,
                    result.handler_data,
                    search_history_table,
                )?;
                if p.trace_imports {
                    p.stderr.extend_from_slice(
                        format!(
                            "[seh] invoke frame={frame_number} pc={:#x} establisher={:#x} handler={:#x} data={:#x}\n",
                            frame.control_pc, frame.establisher, result.handler, result.handler_data
                        )
                        .as_bytes(),
                    );
                }
                let disposition = match p.call_guest(
                    result.handler,
                    &[
                        exception_record,
                        frame.establisher,
                        context_record,
                        dispatcher_context,
                    ],
                ) {
                    Ok(disposition) => disposition as u32,
                    Err(Stop::GuestUnwind(unwind)) => {
                        let context_record = unwind.context_record;
                        complete_guest_unwind(p, &frames, dispatcher_context, unwind)?;
                        // The continue handlers see the final context after phase-two unwind.
                        p.wr32(exception_record + 4, search_flags)?;
                        dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
                        restore_exception_context(p, context_record, skip_ret_on_resume)?;
                        return Ok(0);
                    }
                    Err(stop) => return Err(stop),
                };
                if p.trace_imports {
                    p.stderr.extend_from_slice(
                        format!(
                            "[seh] search frame={frame_number} pc={:#x} establisher={:#x} handler={:#x} disposition={disposition}\n",
                            frame.control_pc, frame.establisher, result.handler
                        )
                        .as_bytes(),
                    );
                }
                search_flags |= p.rd32(exception_record + 4)? & EXCEPTION_NONCONTINUABLE;
                if disposition != EXCEPTION_COLLIDED_UNWIND_DISPOSITION {
                    break disposition;
                }
                collision_count += 1;
                if collision_count > MAX_EXCEPTION_FRAMES {
                    return Err(Stop::Crash(format!(
                        "exception search exceeded {MAX_EXCEPTION_FRAMES} collided-unwind redirects"
                    )));
                }
                let (redirect_frame, redirect_result, redirect_scope_index) =
                    resolve_collided_search_frame(p, dispatcher_context)?;
                search_history_table = p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?;
                if let Some(redirect_index) = frames.iter().rposition(|searched| {
                    searched.image_base == redirect_frame.image_base
                        && searched.function_entry == redirect_frame.function_entry
                        && searched.establisher == redirect_frame.establisher
                }) {
                    frames.truncate(redirect_index + 1);
                    frames[redirect_index] = redirect_frame.clone();
                } else {
                    frames.clear();
                    frames.push(redirect_frame.clone());
                }
                frame = redirect_frame;
                establisher = frame.establisher;
                result = redirect_result;
                scope_index = redirect_scope_index;
                last_rsp = frame.context.gpr[RSP];
                search_flags |= EXCEPTION_COLLIDED_UNWIND_FLAG;
                collision_redirected = true;
            };
            if nested_frame == establisher {
                search_flags &= !EXCEPTION_NESTED_CALL;
                nested_frame = 0;
            }
            p.wr32(exception_record + 4, search_flags)?;
            match disposition as u64 {
                EXCEPTION_CONTINUE_SEARCH => {}
                EXCEPTION_CONTINUE_EXECUTION => {
                    if search_flags & EXCEPTION_NONCONTINUABLE != 0 {
                        return Err(Stop::Crash(format!(
                            "noncontinuable exception code={code:#x} cannot continue execution"
                        )));
                    }
                    return resume_exception(
                        p,
                        exception_record,
                        context_record,
                        skip_ret_on_resume,
                    );
                }
                EXCEPTION_NESTED_EXCEPTION => {
                    search_flags |= EXCEPTION_NESTED_CALL;
                    nested_frame = nested_frame
                        .max(p.rd64(dispatcher_context + DISPATCHER_ESTABLISHER_FRAME)?);
                }
                3 => unreachable!(
                    "collided unwind dispositions are consumed by the repeated-handler loop"
                ),
                value => {
                    return Err(Stop::Unimplemented {
                        dll: "ntdll".into(),
                        name: format!(
                            "exception handler disposition {value} at {:#x}",
                            result.handler
                        ),
                    })
                }
            }
            if collision_redirected && disposition == EXCEPTION_CONTINUE_SEARCH as u32 {
                search_flags &= !EXCEPTION_COLLIDED_UNWIND_FLAG;
                p.wr32(exception_record + 4, search_flags)?;
            }
            if collision_redirected {
                caller_context = frame.context.clone();
                if frame.function_entry == 0 {
                    unwind_leaf(p, &mut caller_context)?;
                } else {
                    unwind_function(
                        p,
                        frame.image_base,
                        frame.control_pc,
                        frame.function_entry,
                        1,
                        &mut caller_context,
                        0,
                        false,
                    )?;
                }
            }
        }

        let next_rsp = caller_context.gpr[RSP];
        let next_pc = caller_context.rip;
        if next_rsp <= last_rsp || next_rsp > stack_base {
            return Err(Stop::Crash(format!(
                "exception search made invalid stack progress {last_rsp:#x} -> {next_rsp:#x}"
            )));
        }
        if next_pc == 0 {
            return dispatch_unhandled_exception(
                p,
                exception_record,
                context_record,
                code,
                search_flags,
                skip_ret_on_resume,
                format!("search exhausted the guest stack after {frame_number} frames"),
            );
        }
        context = caller_context;
        control_pc = next_pc;
        last_rsp = next_rsp;
    }

    Err(Stop::Crash(format!(
        "exception search exceeded {MAX_EXCEPTION_FRAMES} frames"
    )))
}

/// Implement kernel32's UnhandledExceptionFilter using the process top-level filter.
pub(crate) fn unhandled_exception_filter(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return crate::exception32::unhandled_exception_filter(p);
    }
    let exception_pointers = p.arg(0);
    let exception_record = match p.cpu.mode {
        CpuMode::X86_32 => p.rd32(exception_pointers)? as u64,
        CpuMode::X86_64 => p.rd64(exception_pointers)?,
    };
    let exception_flags = p.rd32(exception_record + 4)?;
    let disposition = call_top_level_exception_filter(p, exception_pointers)?;
    if disposition == EXCEPTION_CONTINUE_EXECUTION_FILTER
        && exception_flags & EXCEPTION_NONCONTINUABLE != 0
    {
        return Err(Stop::Crash(format!(
            "noncontinuable exception code={:#x} cannot continue execution",
            p.rd32(exception_record)?
        )));
    }
    Ok(disposition as i64 as u64)
}

fn call_top_level_exception_filter(p: &mut Process, exception_pointers: u64) -> Result<i32, Stop> {
    if p.top_level_exception_filter == 0 {
        return Ok(EXCEPTION_EXECUTE_HANDLER);
    }
    let disposition = p.call_guest(p.top_level_exception_filter, &[exception_pointers])? as i32;
    if !matches!(
        disposition,
        EXCEPTION_CONTINUE_SEARCH_FILTER
            | EXCEPTION_EXECUTE_HANDLER
            | EXCEPTION_CONTINUE_EXECUTION_FILTER
    ) {
        return Err(Stop::Crash(format!(
            "top-level exception filter {:#x} returned invalid disposition {disposition}",
            p.top_level_exception_filter
        )));
    }
    Ok(disposition)
}

fn dispatch_unhandled_exception(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
    code: u32,
    flags: u32,
    skip_ret_on_resume: bool,
    reason: String,
) -> Result<u64, Stop> {
    let exception_pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS_SIZE, true);
    p.wr32(exception_record + 4, flags)?;
    dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
    p.wr64(exception_pointers, exception_record)?;
    p.wr64(exception_pointers + 8, context_record)?;
    let disposition = call_top_level_exception_filter(p, exception_pointers)?;
    if disposition == EXCEPTION_CONTINUE_EXECUTION_FILTER {
        if flags & EXCEPTION_NONCONTINUABLE != 0 {
            return Err(Stop::Crash(format!(
                "noncontinuable exception code={code:#x} cannot continue execution"
            )));
        }
        return restore_exception_context(p, context_record, skip_ret_on_resume);
    }
    Err(Stop::Crash(format!(
        "unhandled exception code={code:#x}, flags={flags:#x}: {reason}; top-level filter returned {disposition}"
    )))
}

fn resume_exception(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
    restore_exception_context(p, context_record, skip_ret_on_resume)
}

fn restore_exception_context(
    p: &mut Process,
    context_record: u64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let resumed = read_context(p, context_record)?;
    p.cpu.gpr = resumed.gpr;
    p.cpu.xmm = resumed.xmm;
    p.cpu.rip = resumed.rip;
    p.cpu.rflags = resumed.rflags;
    p.cpu.mxcsr = resumed.mxcsr;
    p.skip_ret = skip_ret_on_resume;
    Ok(0)
}

fn dispatch_vectored_exception_handlers(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
) -> Result<bool, Stop> {
    let handlers = p.vectored_exception_handlers.clone();
    if handlers.is_empty() {
        return Ok(false);
    }
    let exception_pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS_SIZE, true);
    p.wr64(exception_pointers, exception_record)?;
    p.wr64(exception_pointers + 8, context_record)?;
    let exception_flags = p.rd32(exception_record + 4)?;
    for (handle, callback) in handlers {
        let disposition = p.call_guest(callback, &[exception_pointers])? as i32;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh] vectored handler handle={handle:#x} callback={callback:#x} disposition={disposition}\n"
                )
                .as_bytes(),
            );
        }
        match disposition {
            EXCEPTION_CONTINUE_EXECUTION_FILTER => {
                if exception_flags & EXCEPTION_NONCONTINUABLE != 0 {
                    return Err(Stop::Crash(format!(
                        "noncontinuable exception code={:#x} reached vectored continue-execution handler {callback:#x}",
                        p.rd32(exception_record)?
                    )));
                }
                return Ok(true);
            }
            0 => {}
            value => {
                return Err(Stop::Unimplemented {
                    dll: "ntdll".into(),
                    name: format!("vectored exception handler {callback:#x} disposition {value}"),
                })
            }
        }
    }
    Ok(false)
}

fn dispatch_vectored_continue_handlers(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
) -> Result<(), Stop> {
    let handlers = p.vectored_continue_handlers.clone();
    if handlers.is_empty() {
        return Ok(());
    }
    let exception_pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS_SIZE, true);
    p.wr64(exception_pointers, exception_record)?;
    p.wr64(exception_pointers + 8, context_record)?;
    for (handle, callback) in handlers {
        // Windows ignores vectored continue handler return values; each registered callback runs.
        let disposition = p.call_guest(callback, &[exception_pointers])? as i32;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh] vectored continue handler handle={handle:#x} callback={callback:#x} disposition={disposition}\n"
                )
                .as_bytes(),
            );
        }
    }
    Ok(())
}

/// Implement the x64 scope-table personality used by `__try`, `__except`, and `__finally`.
pub(crate) fn c_specific_handler(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_64 {
        return Err(Stop::Unimplemented {
            dll: "crt".into(),
            name: "__C_specific_handler for PE32".into(),
        });
    }
    let (exception_record, establisher, context_record, dispatcher_context) =
        (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    let exception_flags = p.rd32(exception_record + 4)?;
    let control_pc = p.rd64(dispatcher_context + DISPATCHER_CONTROL_PC)?;
    let image_base = p.rd64(dispatcher_context + DISPATCHER_IMAGE_BASE)?;
    let target_ip = p.rd64(dispatcher_context + DISPATCHER_TARGET_IP)?;
    let handler_data = p.rd64(dispatcher_context + DISPATCHER_HANDLER_DATA)?;
    let mut scope_index = p.rd32(dispatcher_context + DISPATCHER_SCOPE_INDEX)?;
    let image_end = p
        .modules
        .iter()
        .find(|module| module.hle_group.is_none() && module.base == image_base)
        .map(|module| module.base.saturating_add(module.size))
        .or_else(|| {
            p.dynamic_function_tables
                .iter()
                .filter(|table| table.base == image_base)
                .find_map(|table| {
                    table
                        .callback
                        .and_then(|callback| table.base.checked_add(callback.length as u64))
                })
        });
    let Some(image_end) = image_end else {
        return Err(Stop::Crash(format!(
            "__C_specific_handler image base {image_base:#x} is not a loaded image or callback-managed range"
        )));
    };
    let table_end = handler_data
        .checked_add(4)
        .ok_or_else(|| Stop::Crash("__C_specific_handler scope table overflow".into()))?;
    if handler_data < image_base || table_end > image_end {
        return Err(Stop::Crash(format!(
            "__C_specific_handler scope table {handler_data:#x} is outside image {image_base:#x}..{image_end:#x}"
        )));
    }
    let count = p.rd32(handler_data)?;
    let entries_bytes = (count as u64)
        .checked_mul(16)
        .ok_or_else(|| Stop::Crash("__C_specific_handler scope count overflow".into()))?;
    if count > MAX_C_SCOPE_ENTRIES || table_end.saturating_add(entries_bytes) > image_end {
        return Err(Stop::Crash(format!(
            "__C_specific_handler scope table has invalid count {count}"
        )));
    }
    if scope_index > count {
        return Err(Stop::Crash(format!(
            "__C_specific_handler scope index {scope_index} exceeds count {count}"
        )));
    }
    let control_rva = control_pc.checked_sub(image_base).ok_or_else(|| {
        Stop::Crash(format!(
            "__C_specific_handler control PC {control_pc:#x} precedes image {image_base:#x}"
        ))
    })?;
    let target_rva = if target_ip == 0 {
        0
    } else {
        target_ip.checked_sub(image_base).ok_or_else(|| {
            Stop::Crash(format!(
                "__C_specific_handler target IP {target_ip:#x} precedes image {image_base:#x}"
            ))
        })?
    };
    let is_unwinding = exception_flags & EXCEPTION_UNWINDING != 0;

    while scope_index < count {
        let index = scope_index;
        scope_index += 1;
        p.wr32(dispatcher_context + DISPATCHER_SCOPE_INDEX, scope_index)?;
        let entry = handler_data + 4 + index as u64 * 16;
        let begin = p.rd32(entry)? as u64;
        let end = p.rd32(entry + 4)? as u64;
        let filter_or_finally = p.rd32(entry + 8)?;
        let jump_target = p.rd32(entry + 12)? as u64;
        if begin >= end || end > image_end - image_base {
            return Err(Stop::Crash(format!(
                "__C_specific_handler scope {index} has invalid image range {begin:#x}..{end:#x}"
            )));
        }
        if control_rva < begin || control_rva >= end {
            continue;
        }

        if is_unwinding {
            if exception_flags & EXCEPTION_TARGET_UNWIND != 0
                && target_rva >= begin
                && target_rva < end
            {
                return Ok(EXCEPTION_CONTINUE_SEARCH);
            }
            if jump_target == 0 {
                if filter_or_finally == 0 {
                    return Err(Stop::Crash(format!(
                        "__C_specific_handler scope {index} has a null termination handler"
                    )));
                }
                let handler = c_scope_image_address(
                    image_base,
                    image_end,
                    filter_or_finally as u64,
                    "termination handler",
                )?;
                p.call_guest(handler, &[1, establisher])?;
            } else if jump_target == target_rva {
                return Ok(EXCEPTION_CONTINUE_SEARCH);
            }
            continue;
        }

        if jump_target == 0 {
            continue;
        }
        let filter_result = if filter_or_finally == EXCEPTION_EXECUTE_HANDLER as u32 {
            EXCEPTION_EXECUTE_HANDLER
        } else {
            if filter_or_finally == 0 {
                return Err(Stop::Crash(format!(
                    "__C_specific_handler scope {index} has a null filter"
                )));
            }
            let filter =
                c_scope_image_address(image_base, image_end, filter_or_finally as u64, "filter")?;
            let exception_pointers =
                crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS_SIZE, true);
            p.wr64(exception_pointers, exception_record)?;
            p.wr64(exception_pointers + 8, context_record)?;
            p.call_guest(filter, &[exception_pointers, establisher])? as i32
        };
        match filter_result {
            EXCEPTION_CONTINUE_EXECUTION_FILTER => {
                if exception_flags & EXCEPTION_NONCONTINUABLE != 0 {
                    return Err(Stop::Crash(format!(
                        "noncontinuable exception filter at scope {index} requested continuation"
                    )));
                }
                return Ok(EXCEPTION_CONTINUE_EXECUTION);
            }
            EXCEPTION_EXECUTE_HANDLER => {
                let target_ip =
                    c_scope_image_address(image_base, image_end, jump_target, "landing pad")?;
                if lookup_frame(p, target_ip)?.is_none() {
                    return Err(Stop::Crash(format!(
                        "__C_specific_handler landing pad {target_ip:#x} is outside loaded guest images"
                    )));
                }
                return Err(Stop::GuestUnwind(GuestUnwind {
                    target_frame: establisher,
                    target_ip,
                    call_rsp: p.cpu.gpr[RSP],
                    exception_record,
                    return_value: p.rd32(exception_record)? as u64,
                    context_record,
                    history_table: p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?,
                }));
            }
            _ => {}
        }
    }

    Ok(EXCEPTION_CONTINUE_SEARCH)
}

fn c_scope_image_address(
    image_base: u64,
    image_end: u64,
    rva: u64,
    kind: &str,
) -> Result<u64, Stop> {
    image_base
        .checked_add(rva)
        .filter(|address| *address < image_end)
        .ok_or_else(|| {
            Stop::Crash(format!(
                "__C_specific_handler {kind} RVA {rva:#x} is outside image {image_base:#x}..{image_end:#x}"
            ))
        })
}

// UNWIND_HISTORY_TABLE is 0xd8 bytes on x64 (header plus twelve 16-byte entries). The runtime
// performs its own function-table search, so it validates and forwards this optional cache without
// reading or updating the cached entries.
fn validate_unwind_history_table(
    p: &Process,
    history_table: u64,
    operation: &str,
) -> Result<(), Stop> {
    if history_table == 0 {
        return Ok(());
    }
    if history_table & 7 != 0 {
        return Err(Stop::Crash(format!(
            "{operation} history table {history_table:#x} is not 8-byte aligned"
        )));
    }
    let last_byte = history_table
        .checked_add(UNWIND_HISTORY_TABLE_BYTES - 1)
        .ok_or_else(|| Stop::Crash(format!("{operation} history table range overflows")))?;
    for address in [history_table, last_byte] {
        let Some(protection) = p.mem.prot_of(address) else {
            return Err(Stop::Crash(format!(
                "{operation} history table {history_table:#x} is not fully mapped"
            )));
        };
        if protection & (PROT_R | PROT_W) != (PROT_R | PROT_W) {
            return Err(Stop::Crash(format!(
                "{operation} history table {history_table:#x} is not readable and writable"
            )));
        }
    }
    let count = p.rd32(history_table)?;
    if count > UNWIND_HISTORY_TABLE_ENTRIES {
        return Err(Stop::Crash(format!(
            "{operation} history table has invalid entry count {count}"
        )));
    }
    Ok(())
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
    validate_unwind_history_table(p, unwind.history_table, "RtlUnwindEx")?;
    if lookup_frame(p, unwind.target_ip)?.is_none() {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} is outside loaded guest images",
            unwind.target_ip
        )));
    }
    let context = read_context(p, unwind.context_record)?;
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    if lookup_frame(p, context.rip)?.is_none()
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
    mut unwind: GuestUnwind,
) -> Result<(), Stop> {
    validate_unwind_history_table(p, unwind.history_table, "RtlUnwindEx dispatch")?;
    let Some(mut target_index) = frames
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
    let Some((target_image_base, target_function_entry)) = lookup_frame(p, unwind.target_ip)?
    else {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} is outside loaded guest images",
            unwind.target_ip
        )));
    };
    let target_frame = &frames[target_index];
    let mut cpp_exception = p.rd32(unwind.exception_record)? == MSVC_CPP_EXCEPTION;
    let is_cpp_catch_funclet = cpp_exception
        && target_frame.image_base == target_image_base
        && target_frame.function_entry != target_function_entry
        && p.mem
            .prot_of(unwind.target_ip)
            .is_some_and(|protection| protection & PROT_X != 0);
    let mut cpp_funclet_transfer = is_cpp_catch_funclet;
    if target_frame.image_base != target_image_base
        || target_frame.function_entry != target_function_entry && !is_cpp_catch_funclet
    {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx target IP {:#x} does not belong to target frame {:#x}",
            unwind.target_ip, unwind.target_frame
        )));
    }
    let mut active_frames = frames.to_vec();
    let mut transfer_context = target_frame.context.clone();
    let mut target_scope_index = p.rd32(dispatcher_context + 0x48)?;
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
    let mut index = 0;
    let mut collided = false;
    let mut collided_count = 0;
    let mut redirected_scope_index = None;
    let mut nested_unwind_count = 0;
    while index <= target_index {
        let frame = &active_frames[index];
        if frame.function_entry == 0 {
            index += 1;
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
            index += 1;
            continue;
        }
        let scope_index = redirected_scope_index.take().unwrap_or_else(|| {
            if index == target_index {
                target_scope_index
            } else {
                0
            }
        });
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
            unwind.history_table,
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
            }
            | if collided {
                EXCEPTION_COLLIDED_UNWIND_FLAG
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
            Err(Stop::GuestUnwind(nested)) => {
                nested_unwind_count += 1;
                if nested_unwind_count > MAX_EXCEPTION_FRAMES {
                    return Err(Stop::Crash(format!(
                        "RtlUnwindEx exceeded {MAX_EXCEPTION_FRAMES} nested second-pass transfers"
                    )));
                }
                validate_unwind_history_table(p, nested.history_table, "nested RtlUnwindEx")?;
                if nested.context_record != unwind.context_record {
                    return Err(Stop::Unimplemented {
                        dll: "ntdll".into(),
                        name: "nested RtlUnwindEx using a different context record".into(),
                    });
                }
                let Some(nested_target_index) = active_frames
                    .iter()
                    .position(|active| active.establisher == nested.target_frame)
                else {
                    return Err(Stop::Crash(format!(
                        "nested RtlUnwindEx target frame {:#x} was not in the searched guest frames",
                        nested.target_frame
                    )));
                };
                if nested_target_index < index {
                    return Err(Stop::Unimplemented {
                        dll: "ntdll".into(),
                        name: "nested RtlUnwindEx target behind the active unwind frame".into(),
                    });
                }
                let nested_context = read_context(p, nested.context_record)?;
                if nested_context.rip != handler_context.rip
                    || nested_context.gpr[RSP] != handler_context.gpr[RSP]
                {
                    return Err(Stop::Crash(
                        "nested RtlUnwindEx context changed during the phase-two callback".into(),
                    ));
                }
                let stack_base = p.rd64(TEB_ADDR + 0x08)?;
                let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
                if nested.call_rsp < stack_limit || nested.call_rsp >= stack_base {
                    return Err(Stop::Crash(format!(
                        "nested RtlUnwindEx call stack pointer {:#x} is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}",
                        nested.call_rsp
                    )));
                }
                let Some((nested_image_base, nested_function_entry)) =
                    lookup_frame(p, nested.target_ip)?
                else {
                    return Err(Stop::Crash(format!(
                        "nested RtlUnwindEx target IP {:#x} is outside loaded guest images",
                        nested.target_ip
                    )));
                };
                let nested_target = &active_frames[nested_target_index];
                let nested_cpp_exception = p.rd32(nested.exception_record)? == MSVC_CPP_EXCEPTION;
                let nested_cpp_funclet = nested_cpp_exception
                    && nested_target.image_base == nested_image_base
                    && nested_target.function_entry != nested_function_entry
                    && p.mem
                        .prot_of(nested.target_ip)
                        .is_some_and(|protection| protection & PROT_X != 0);
                if nested_target.image_base != nested_image_base
                    || nested_target.function_entry != nested_function_entry && !nested_cpp_funclet
                {
                    return Err(Stop::Crash(format!(
                        "nested RtlUnwindEx target IP {:#x} does not belong to target frame {:#x}",
                        nested.target_ip, nested.target_frame
                    )));
                }

                // A nested RtlUnwindEx takes ownership of the active phase-two walk. The
                // current handler has already run, so continue with the next searched frame.
                unwind = nested;
                target_index = nested_target_index;
                target_scope_index = 0;
                cpp_exception = false;
                transfer_context = nested_target.context.clone();
                p.cpu.gpr[RSP] = unwind.call_rsp;
                p.wr32(unwind.exception_record + 4, EXCEPTION_UNWINDING)?;
                cpp_funclet_transfer = nested_cpp_funclet;
                redirected_scope_index = None;
                collided = false;
                index += 1;
                continue;
            }
            Err(stop) => return Err(stop),
        };
        if disposition == EXCEPTION_COLLIDED_UNWIND_DISPOSITION {
            collided_count += 1;
            if collided_count > MAX_EXCEPTION_FRAMES {
                return Err(Stop::Crash(format!(
                    "RtlUnwindEx exceeded {MAX_EXCEPTION_FRAMES} collided-unwind redirects"
                )));
            }
            let (redirect_index, restored_context, scope_index) = resolve_collided_unwind_frame(
                p,
                &active_frames,
                dispatcher_context,
                index,
                target_index,
            )?;
            unwind.history_table = p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?;
            active_frames[redirect_index].context = restored_context;
            for next_index in redirect_index + 1..=target_index {
                let previous_frame = &active_frames[next_index - 1];
                let mut caller_context = previous_frame.context.clone();
                if previous_frame.function_entry == 0 {
                    unwind_leaf(p, &mut caller_context)?;
                } else {
                    unwind_function(
                        p,
                        previous_frame.image_base,
                        previous_frame.control_pc,
                        previous_frame.function_entry,
                        2,
                        &mut caller_context,
                        0,
                        false,
                    )?;
                }
                let searched_frame = &frames[next_index];
                if caller_context.rip != searched_frame.control_pc
                    || caller_context.gpr[RSP] != searched_frame.context.gpr[RSP]
                {
                    return Err(Stop::Crash(format!(
                        "RtlUnwindEx collided frame walk diverged after {:#x}: next=({:#x},{:#x}), searched=({:#x},{:#x})",
                        previous_frame.control_pc,
                        caller_context.rip,
                        caller_context.gpr[RSP],
                        searched_frame.control_pc,
                        searched_frame.context.gpr[RSP]
                    )));
                }
                let establisher = if searched_frame.function_entry == 0 {
                    caller_context.gpr[RSP]
                } else {
                    read_frame_register(
                        p,
                        searched_frame.image_base,
                        searched_frame.control_pc,
                        searched_frame.function_entry,
                        &caller_context,
                    )?
                };
                if establisher != searched_frame.establisher {
                    return Err(Stop::Crash(format!(
                        "RtlUnwindEx collided frame walk changed establisher {:#x} to {establisher:#x}",
                        searched_frame.establisher
                    )));
                }
                active_frames[next_index].context = caller_context;
            }
            transfer_context = active_frames[target_index].context.clone();
            write_context(
                p,
                unwind.context_record,
                &active_frames[redirect_index].context,
            )?;
            index = redirect_index;
            redirected_scope_index = Some(scope_index);
            collided = true;
            continue;
        }
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
        collided = false;
        index += 1;
    }

    let consolidation_callback = if p.rd32(unwind.exception_record)? == STATUS_UNWIND_CONSOLIDATE {
        let parameter_count = p.rd32(unwind.exception_record + EXCEPTION_RECORD_PARAMETERS)?;
        if !(12..=15).contains(&parameter_count) {
            return Err(Stop::Crash(format!(
                "RtlUnwindEx consolidation record has invalid parameter count {parameter_count}"
            )));
        }
        let callback = p.rd64(unwind.exception_record + EXCEPTION_RECORD_ARGUMENTS)?;
        if callback == 0
            || p.mem
                .prot_of(callback)
                .is_none_or(|protection| protection & PROT_X == 0)
        {
            return Err(Stop::Crash(format!(
                "RtlUnwindEx consolidation callback {callback:#x} is not executable"
            )));
        }
        let callback_context =
            p.rd64(unwind.exception_record + EXCEPTION_RECORD_ARGUMENTS + 4 * 8)?;
        if callback_context != unwind.context_record {
            return Err(Stop::Crash(format!(
                "RtlUnwindEx consolidation context {callback_context:#x} does not match CONTEXT {:#x}",
                unwind.context_record
            )));
        }

        p.cpu.gpr = transfer_context.gpr;
        p.cpu.xmm = transfer_context.xmm;
        p.cpu.rip = transfer_context.rip;
        p.cpu.rflags = transfer_context.rflags;
        p.cpu.mxcsr = transfer_context.mxcsr;
        let continuation = p.call_guest(callback, &[unwind.exception_record])?;
        if continuation == 0 {
            return Err(Stop::Crash(
                "RtlUnwindEx consolidation callback returned a null continuation".into(),
            ));
        }
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh] consolidation callback={callback:#x} returned continuation={continuation:#x}\n"
                )
                .as_bytes(),
            );
        }
        Some(continuation)
    } else {
        None
    };
    let dispatcher_target_ip = p.rd64(dispatcher_context + DISPATCHER_TARGET_IP)?;
    let transfer_ip = consolidation_callback.unwrap_or_else(|| {
        if cpp_exception && dispatcher_target_ip != unwind.target_ip {
            dispatcher_target_ip
        } else {
            unwind.target_ip
        }
    });
    let Some((transfer_image_base, transfer_function_entry)) = lookup_frame(p, transfer_ip)? else {
        return Err(Stop::Crash(format!(
            "exception target continuation {transfer_ip:#x} is outside loaded guest images"
        )));
    };
    let transfer_target_frame = &active_frames[target_index];
    let transfer_is_cpp_funclet = cpp_funclet_transfer
        && transfer_image_base == transfer_target_frame.image_base
        && p.mem
            .prot_of(transfer_ip)
            .is_some_and(|protection| protection & PROT_X != 0);
    if transfer_image_base != transfer_target_frame.image_base
        || transfer_function_entry != transfer_target_frame.function_entry
            && !transfer_is_cpp_funclet
    {
        return Err(Stop::Crash(format!(
            "exception target continuation {transfer_ip:#x} does not belong to target frame {:#x}",
            unwind.target_frame
        )));
    }
    transfer_context.rip = transfer_ip;
    transfer_context.gpr[anywasm_cpu::cpu::RAX] = unwind.return_value;
    write_context(p, unwind.context_record, &transfer_context)?;
    p.cpu.gpr = transfer_context.gpr;
    p.cpu.xmm = transfer_context.xmm;
    p.cpu.rip = transfer_context.rip;
    p.cpu.rflags = transfer_context.rflags;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[seh] transfer frame={:#x} ip={transfer_ip:#x} requested={:#x} rsp={:#x} rax={:#x}\n",
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

fn resolve_collided_unwind_frame(
    p: &mut Process,
    frames: &[ExceptionFrame],
    dispatcher_context: u64,
    current_index: usize,
    target_index: usize,
) -> Result<(usize, Context64, u32), Stop> {
    let control_pc = p.rd64(dispatcher_context + DISPATCHER_CONTROL_PC)?;
    let image_base = p.rd64(dispatcher_context + DISPATCHER_IMAGE_BASE)?;
    let function_entry = p.rd64(dispatcher_context + DISPATCHER_FUNCTION_ENTRY)?;
    let establisher = p.rd64(dispatcher_context + DISPATCHER_ESTABLISHER_FRAME)?;
    let context_record = p.rd64(dispatcher_context + DISPATCHER_CONTEXT_RECORD)?;
    let language_handler = p.rd64(dispatcher_context + DISPATCHER_LANGUAGE_HANDLER)?;
    let handler_data = p.rd64(dispatcher_context + DISPATCHER_HANDLER_DATA)?;
    let history_table = p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?;
    let scope_index = p.rd32(dispatcher_context + DISPATCHER_SCOPE_INDEX)?;
    validate_unwind_history_table(p, history_table, "collided RtlUnwindEx")?;

    let mut restored_context = read_context(p, context_record)?;
    restored_context.rip = control_pc;
    let Some((redirect_index, _frame)) = frames
        .iter()
        .enumerate()
        .skip(current_index)
        .take(target_index.saturating_sub(current_index) + 1)
        .find(|(_, frame)| {
            frame.control_pc == control_pc
                && frame.image_base == image_base
                && frame.function_entry == function_entry
                && frame.establisher == establisher
        })
    else {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx collided dispatcher context does not identify a searched frame (pc={control_pc:#x}, establisher={establisher:#x})"
        )));
    };
    let calculated_establisher =
        read_frame_register(p, image_base, control_pc, function_entry, &restored_context)?;
    let mut caller_context = restored_context.clone();
    let result = unwind_function(
        p,
        image_base,
        control_pc,
        function_entry,
        2,
        &mut caller_context,
        0,
        false,
    )?;
    if calculated_establisher != establisher
        || result.handler != language_handler
        || result.handler_data != handler_data
    {
        return Err(Stop::Crash(format!(
            "RtlUnwindEx collided dispatcher metadata does not match frame {establisher:#x}"
        )));
    }
    Ok((redirect_index, restored_context, scope_index))
}

fn resolve_collided_search_frame(
    p: &mut Process,
    dispatcher_context: u64,
) -> Result<(ExceptionFrame, UnwindResult, u32), Stop> {
    let control_pc = p.rd64(dispatcher_context + DISPATCHER_CONTROL_PC)?;
    let image_base = p.rd64(dispatcher_context + DISPATCHER_IMAGE_BASE)?;
    let function_entry = p.rd64(dispatcher_context + DISPATCHER_FUNCTION_ENTRY)?;
    let establisher = p.rd64(dispatcher_context + DISPATCHER_ESTABLISHER_FRAME)?;
    let context_record = p.rd64(dispatcher_context + DISPATCHER_CONTEXT_RECORD)?;
    let language_handler = p.rd64(dispatcher_context + DISPATCHER_LANGUAGE_HANDLER)?;
    let handler_data = p.rd64(dispatcher_context + DISPATCHER_HANDLER_DATA)?;
    let history_table = p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?;
    let scope_index = p.rd32(dispatcher_context + DISPATCHER_SCOPE_INDEX)?;
    validate_unwind_history_table(p, history_table, "collided exception search")?;
    if function_entry == 0 {
        return Err(Stop::Crash(
            "collided exception search redirected to a frame without unwind metadata".into(),
        ));
    }

    let mut restored_context = read_context(p, context_record)?;
    restored_context.rip = control_pc;
    let Some((found_image_base, found_function_entry)) = lookup_frame(p, control_pc)? else {
        return Err(Stop::Crash(format!(
            "collided exception control PC {control_pc:#x} is outside loaded guest images"
        )));
    };
    if found_image_base != image_base || found_function_entry != function_entry {
        return Err(Stop::Crash(format!(
            "collided exception dispatcher function metadata does not match control PC {control_pc:#x}"
        )));
    }
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    if stack_limit >= stack_base
        || restored_context.gpr[RSP] < stack_limit
        || restored_context.gpr[RSP] >= stack_base
        || establisher < stack_limit
        || establisher >= stack_base
        || establisher & 7 != 0
    {
        return Err(Stop::Crash(format!(
            "collided exception context ({:#x},{:#x}) is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}",
            restored_context.gpr[RSP], establisher
        )));
    }
    let calculated_establisher =
        read_frame_register(p, image_base, control_pc, function_entry, &restored_context)?;
    let mut caller_context = restored_context.clone();
    let result = unwind_function(
        p,
        image_base,
        control_pc,
        function_entry,
        1,
        &mut caller_context,
        0,
        false,
    )?;
    if calculated_establisher != establisher
        || result.handler == 0
        || result.handler != language_handler
        || result.handler_data != handler_data
    {
        return Err(Stop::Crash(format!(
            "collided exception dispatcher metadata does not match searched frame {establisher:#x}"
        )));
    }
    let redirect_frame = ExceptionFrame {
        control_pc,
        image_base,
        function_entry,
        establisher,
        context: restored_context,
    };
    Ok((redirect_frame, result, scope_index))
}

fn context_from_cpu(cpu: &Cpu, rip: u64, rsp: u64) -> Context64 {
    let mut gpr = cpu.gpr;
    gpr[RSP] = rsp;
    Context64 {
        gpr,
        xmm: cpu.xmm,
        rip,
        rflags: cpu.rflags,
        mxcsr: cpu.mxcsr,
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
    history_table: u64,
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
        (0x40, history_table),
    ] {
        p.wr64(address + offset, value)?;
    }
    p.wr32(address + 0x48, scope_index)?;
    Ok(())
}

pub(crate) fn lookup_frame(p: &mut Process, control_pc: u64) -> Result<Option<(u64, u64)>, Stop> {
    for index in 0..p.dynamic_function_tables.len() {
        let table = p.dynamic_function_tables[index];
        let Some(rva) = control_pc.checked_sub(table.base) else {
            continue;
        };
        if let Some(callback) = table.callback {
            if rva >= callback.length as u64 {
                continue;
            }
            let entry = p.call_guest(callback.callback, &[control_pc, callback.context])?;
            if entry == 0 {
                return Ok(p
                    .mem
                    .prot_of(control_pc)
                    .is_some_and(|protection| protection & PROT_X != 0)
                    .then_some((table.base, 0)));
            }
            let Some(_entry_end) = entry.checked_add(12) else {
                return Err(Stop::Crash(format!(
                    "function table callback {:#x} returned overflowing entry {entry:#x}",
                    callback.callback
                )));
            };
            let begin = p.rd32(entry)?;
            let end = p.rd32(entry + 4)?;
            let unwind = p.rd32(entry + 8)?;
            if entry & 3 != 0
                || begin >= end
                || rva < begin as u64
                || rva >= end as u64
                || end as u64 > callback.length as u64
                || unwind == 0
                || unwind & 3 != 0
                || table.base.checked_add(end as u64).is_none()
                || table.base.checked_add(unwind as u64).is_none()
            {
                return Err(Stop::Crash(format!(
                    "function table callback {:#x} returned invalid entry {entry:#x} for PC {control_pc:#x}",
                    callback.callback
                )));
            }
            return Ok(Some((table.base, entry)));
        }
        let mut low = 0usize;
        let mut high = table.entry_count as usize;
        let mut valid = true;
        while low < high {
            let middle = low + (high - low) / 2;
            let Some(entry) = table.table.checked_add(middle as u64 * 12) else {
                valid = false;
                break;
            };
            let Ok(begin) = p.rd32(entry) else {
                valid = false;
                break;
            };
            let begin = begin as u64;
            if begin <= rva {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if !valid {
            continue;
        }
        let Some(index) = low.checked_sub(1) else {
            continue;
        };
        let Some(entry) = table.table.checked_add(index as u64 * 12) else {
            continue;
        };
        let Some(end_address) = entry.checked_add(4) else {
            continue;
        };
        let Ok(end) = p.rd32(end_address) else {
            continue;
        };
        let end = end as u64;
        if rva < end {
            return Ok(Some((table.base, entry)));
        }
    }

    let module = p.modules.iter().find(|module| {
        module.hle_group.is_none()
            && control_pc >= module.base
            && control_pc - module.base < module.size
    });
    let Some(module) = module else {
        return Ok(None);
    };
    let rva = control_pc - module.base;
    let index = module
        .runtime_functions
        .partition_point(|function| function.begin_address as u64 <= rva)
        .checked_sub(1)
        .filter(|index| rva < module.runtime_functions[*index].end_address as u64);
    if let Some(index) = index {
        return Ok(Some((
            module.base,
            module.base + module.exception_table_rva as u64 + index as u64 * 12,
        )));
    }

    // PE leaf functions have no RUNTIME_FUNCTION entry. Keep this internal marker distinct
    // from an unknown PC so exception dispatch can apply the ordinary one-return unwind. The
    // public RtlLookupFunctionEntry wrapper still turns entry 0 into a null result.
    Ok(p.mem
        .prot_of(control_pc)
        .is_some_and(|protection| protection & PROT_X != 0)
        .then_some((module.base, 0)))
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
        mxcsr: p.rd32(address + CONTEXT_MXCSR)?,
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
    p.wr32(address + CONTEXT_MXCSR, context.mxcsr)?;
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
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
    use anywasm_cpu::mem::{Fault, FaultKind, PAGE_SIZE, PROT_RW, PROT_RWX};

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
            mxcsr: 0x1f80,
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
            tls_callbacks: Vec::new(),
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
    fn context_roundtrip_preserves_mxcsr_and_honors_handler_edits() {
        const ORIGINAL_MXCSR: u32 = 0x5f80;
        const RESUMED_MXCSR: u32 = 0x3f80;
        let mut process = process_with_memory();
        process.cpu.mxcsr = ORIGINAL_MXCSR;
        let context = context_from_cpu(&process.cpu, 0x1400_0100, STACK + 0x800);

        write_context(&mut process, INFO, &context).unwrap();

        assert_eq!(process.rd32(INFO + 0x34).unwrap(), ORIGINAL_MXCSR);
        process.wr32(INFO + 0x34, RESUMED_MXCSR).unwrap();
        restore_exception_context(&mut process, INFO, false).unwrap();
        assert_eq!(process.cpu.mxcsr, RESUMED_MXCSR);
    }

    #[test]
    fn vectored_continue_execution_stops_later_first_chance_handlers() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const EXCEPTION_RECORD: u64 = INFO;
        const CONTEXT_RECORD: u64 = INFO + 0x100;
        const MARKER: u64 = INFO + 0x200;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x1000, PROT_RWX));
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            0xe123_4567,
            0,
            IMAGE_BASE + 0x10,
            &[],
        )
        .unwrap();
        process
            .write_bytes(
                IMAGE_BASE + 0x100,
                &[0xb8, 0xff, 0xff, 0xff, 0xff, 0xc3], // return EXCEPTION_CONTINUE_EXECUTION
            )
            .unwrap();
        let mut later_handler = vec![0x48, 0xb8]; // mov rax, MARKER
        later_handler.extend_from_slice(&MARKER.to_le_bytes());
        later_handler.extend_from_slice(&[
            0xc7, 0x00, 0x01, 0x00, 0x00, 0x00, // mov dword ptr [rax], 1
            0x31, 0xc0, 0xc3,
        ]);
        process
            .write_bytes(IMAGE_BASE + 0x200, &later_handler)
            .unwrap();
        process.vectored_exception_handlers = vec![
            (0x7000_0000, IMAGE_BASE + 0x100),
            (0x7000_0001, IMAGE_BASE + 0x200),
        ];
        process.cpu.gpr[RSP] = STACK + 0x800;

        assert!(dispatch_vectored_exception_handlers(
            &mut process,
            EXCEPTION_RECORD,
            CONTEXT_RECORD
        )
        .unwrap());
        assert_eq!(process.rd32(MARKER).unwrap(), 0);
    }

    #[test]
    fn unhandled_filter_can_resume_and_vectored_continue_handlers_all_run() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const CALLER: u64 = 0xdead_beef;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x700;
        const RAISE_RSP: u64 = STACK + 0x600;
        const FIRST_MARKER: u64 = INFO + 0x300;
        const SECOND_MARKER: u64 = INFO + 0x304;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x1000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        process.wr64(RAISE_RSP, CALLER).unwrap();

        let mut first_vch = vec![0x48, 0xb8]; // mov rax, FIRST_MARKER
        first_vch.extend_from_slice(&FIRST_MARKER.to_le_bytes());
        first_vch.extend_from_slice(&[
            0xc7, 0x00, 1, 0, 0, 0, // mov dword ptr [rax], 1
            0xb8, 0xff, 0xff, 0xff,
            0xff, // return EXCEPTION_CONTINUE_EXECUTION (ignored for VCH)
            0xc3,
        ]);
        process.write_bytes(IMAGE_BASE + 0x100, &first_vch).unwrap();

        let mut second_vch = vec![0x48, 0xb8]; // mov rax, SECOND_MARKER
        second_vch.extend_from_slice(&SECOND_MARKER.to_le_bytes());
        second_vch.extend_from_slice(&[
            0xc7, 0x00, 1, 0, 0, 0, // mov dword ptr [rax], 1
            0x31, 0xc0, // return EXCEPTION_CONTINUE_SEARCH
            0xc3,
        ]);
        process
            .write_bytes(IMAGE_BASE + 0x200, &second_vch)
            .unwrap();

        let mut top_filter = vec![0x48, 0x8b, 0x41, 0x08]; // mov rax, [rcx + ContextRecord]
        top_filter.extend_from_slice(&[0x48, 0xba]); // mov rdx, LANDING_PAD
        top_filter.extend_from_slice(&LANDING_PAD.to_le_bytes());
        top_filter.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // return EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        process
            .write_bytes(IMAGE_BASE + 0x300, &top_filter)
            .unwrap();
        process.top_level_exception_filter = IMAGE_BASE + 0x300;
        process.vectored_continue_handlers = vec![
            (0x7000_0000, IMAGE_BASE + 0x100),
            (0x7000_0001, IMAGE_BASE + 0x200),
        ];
        process.cpu.gpr[RSP] = RAISE_RSP;

        let result = dispatch_raised_exception(&mut process, 0xe123_4567, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(FIRST_MARKER).unwrap(), 1);
        assert_eq!(process.rd32(SECOND_MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
    }

    #[test]
    fn cpu_fault_records_use_windows_codes_arguments_and_instruction_context() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const FAULT_RIP: u64 = IMAGE_BASE + 0x500;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x700;
        const MARKER: u64 = INFO + 0x300;
        const FAULT_ADDRESS: u64 = 0xdead_0000;

        let mut handler = vec![
            0x48, 0x8b, 0x01, // mov rax, [rcx] -- EXCEPTION_RECORD
            0x49, 0x89, 0xcd, // mov r13, rcx -- preserve EXCEPTION_POINTERS
            0x8b, 0x10, // mov edx, [rax] -- ExceptionCode
            0x48, 0xbb, // mov rbx, MARKER
        ];
        handler.extend_from_slice(&MARKER.to_le_bytes());
        handler.extend_from_slice(&[
            0x89, 0x13, // mov [rbx], edx
            0x48, 0x8b, 0x50, 0x10, // mov rdx, [rax + 16] -- ExceptionAddress
            0x48, 0x89, 0x53, 0x08, // mov [rbx + 8], rdx
            0x8b, 0x48, 0x18, // mov ecx, [rax + 24] -- NumberParameters
            0x89, 0x4b, 0x10, // mov [rbx + 16], ecx
            0x48, 0x8b, 0x50, 0x20, // mov rdx, [rax + 32] -- ExceptionInformation[0]
            0x48, 0x89, 0x53, 0x18, // mov [rbx + 24], rdx
            0x48, 0x8b, 0x50, 0x28, // mov rdx, [rax + 40] -- ExceptionInformation[1]
            0x48, 0x89, 0x53, 0x20, // mov [rbx + 32], rdx
            0x49, 0x8b, 0x45, 0x08, // mov rax, [r13 + 8] -- CONTEXT
            0x48, 0x8b, 0x90, 0xf8, 0, 0, 0, // mov rdx, [rax + CONTEXT.Rip]
            0x48, 0xbb, // mov rbx, MARKER + 40
        ]);
        handler.extend_from_slice(&(MARKER + 40).to_le_bytes());
        handler.extend_from_slice(&[
            0x48, 0x89, 0x13, // mov [rbx], rdx
            0x49, 0x8b, 0x45, 0x08, // mov rax, [r13 + 8] -- CONTEXT
            0x48, 0xba, // mov rdx, LANDING_PAD
        ]);
        handler.extend_from_slice(&LANDING_PAD.to_le_bytes());
        handler.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // return EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);

        let cases = [
            (
                Exit::Fault(Fault {
                    addr: FAULT_ADDRESS,
                    access: Access::Write,
                    protection: false,
                    kind: FaultKind::GuestMemory,
                }),
                STATUS_ACCESS_VIOLATION,
                FAULT_RIP,
                2,
                1,
                FAULT_ADDRESS,
                FAULT_RIP,
            ),
            (
                Exit::DivideError,
                STATUS_INTEGER_DIVIDE_BY_ZERO,
                FAULT_RIP,
                0,
                0,
                0,
                FAULT_RIP,
            ),
            (
                Exit::Breakpoint,
                STATUS_BREAKPOINT,
                FAULT_RIP,
                0,
                0,
                0,
                FAULT_RIP + 1,
            ),
            (
                Exit::IllegalInstruction,
                STATUS_ILLEGAL_INSTRUCTION,
                FAULT_RIP,
                0,
                0,
                0,
                FAULT_RIP,
            ),
            (
                Exit::PrivilegedInstruction,
                STATUS_PRIVILEGED_INSTRUCTION,
                FAULT_RIP,
                0,
                0,
                0,
                FAULT_RIP,
            ),
        ];

        for (exit, code, address, count, access, fault_address, context_rip) in cases {
            let mut process = process_with_memory();
            assert!(process.mem.map(IMAGE_BASE, 0x1000, PROT_RWX));
            assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
            assert!(process.mem.map(MARKER, PAGE_SIZE, PROT_RW));
            process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
            process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
            process.write_bytes(IMAGE_BASE + 0x100, &handler).unwrap();
            process.vectored_exception_handlers = vec![(0x7000_0000, IMAGE_BASE + 0x100)];
            process.cpu.rip = match &exit {
                Exit::Breakpoint => FAULT_RIP + 1,
                _ => FAULT_RIP,
            };
            process.cpu.gpr[RSP] = STACK + 0x800;

            assert!(dispatch_cpu_exception(&mut process, &exit).unwrap());
            assert_eq!(process.rd32(MARKER).unwrap(), code);
            assert_eq!(process.rd64(MARKER + 8).unwrap(), address);
            assert_eq!(process.rd32(MARKER + 16).unwrap(), count);
            assert_eq!(process.rd64(MARKER + 24).unwrap(), access);
            assert_eq!(process.rd64(MARKER + 32).unwrap(), fault_address);
            assert_eq!(process.rd64(MARKER + 40).unwrap(), context_rip);
            assert_eq!(process.cpu.rip, LANDING_PAD);
            assert!(!process.skip_ret);
        }
    }

    #[test]
    fn c_scope_table_catch_all_requests_unwind_to_its_landing_pad() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const EXCEPTION_RECORD: u64 = INFO;
        const CONTEXT_RECORD: u64 = INFO + 0x100;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x200;
        const SCOPE_TABLE: u64 = IMAGE_BASE + 0x700;
        const ESTABLISHER: u64 = STACK + 0x180;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        let mut module = test_module();
        module.runtime_functions[0].end_address = 0x400;
        process.modules.push(module);
        process
            .write_bytes(EXCEPTION_RECORD, &{
                let mut bytes = vec![0; EXCEPTION_RECORD_SIZE as usize];
                bytes[0..4].copy_from_slice(&CODE.to_le_bytes());
                bytes[16..24].copy_from_slice(&(IMAGE_BASE + 0x110).to_le_bytes());
                bytes
            })
            .unwrap();
        process.wr32(SCOPE_TABLE, 1).unwrap();
        process.wr32(SCOPE_TABLE + 4, 0x100).unwrap();
        process.wr32(SCOPE_TABLE + 8, 0x120).unwrap();
        process.wr32(SCOPE_TABLE + 12, 1).unwrap(); // Catch-all filter.
        process.wr32(SCOPE_TABLE + 16, 0x300).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT, IMAGE_BASE + 0x110)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 8, IMAGE_BASE).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 24, ESTABLISHER).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT + 40, CONTEXT_RECORD)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 64, 0).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 56, SCOPE_TABLE).unwrap();
        process.wr32(DISPATCHER_CONTEXT + 72, 0).unwrap();
        process.cpu.gpr[anywasm_cpu::cpu::RCX] = EXCEPTION_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::RDX] = ESTABLISHER;
        process.cpu.gpr[anywasm_cpu::cpu::R8] = CONTEXT_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::R9] = DISPATCHER_CONTEXT;
        process.cpu.gpr[RSP] = STACK + 0x400;

        let result = c_specific_handler(&mut process);

        let Err(Stop::GuestUnwind(unwind)) = result else {
            panic!("catch-all scope should request an unwind, got {result:?}");
        };
        assert_eq!(unwind.target_frame, ESTABLISHER);
        assert_eq!(unwind.target_ip, IMAGE_BASE + 0x300);
        assert_eq!(unwind.exception_record, EXCEPTION_RECORD);
        assert_eq!(unwind.return_value, CODE as u64);
        assert_eq!(unwind.context_record, CONTEXT_RECORD);
        assert_eq!(unwind.history_table, 0);
        assert_eq!(process.rd32(DISPATCHER_CONTEXT + 72).unwrap(), 1);
    }

    #[test]
    fn c_scope_filter_receives_exception_pointers_and_frame() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const EXCEPTION_RECORD: u64 = INFO;
        const CONTEXT_RECORD: u64 = INFO + 0x100;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x200;
        const FILTER_OUTPUT: u64 = INFO + 0x400;
        const SCOPE_TABLE: u64 = IMAGE_BASE + 0x700;
        const ESTABLISHER: u64 = STACK + 0x180;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        let mut module = test_module();
        module.runtime_functions[0].end_address = 0x400;
        process.modules.push(module);
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            0xe123_4567,
            0,
            IMAGE_BASE + 0x110,
            &[],
        )
        .unwrap();
        process.wr32(SCOPE_TABLE, 1).unwrap();
        process.wr32(SCOPE_TABLE + 4, 0x100).unwrap();
        process.wr32(SCOPE_TABLE + 8, 0x120).unwrap();
        process.wr32(SCOPE_TABLE + 12, 0x500).unwrap();
        process.wr32(SCOPE_TABLE + 16, 0x300).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT, IMAGE_BASE + 0x110)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 8, IMAGE_BASE).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 24, ESTABLISHER).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT + 40, CONTEXT_RECORD)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 56, SCOPE_TABLE).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 64, 0).unwrap();
        process.wr32(DISPATCHER_CONTEXT + 72, 0).unwrap();

        let mut filter = vec![0x48, 0xb8]; // mov rax, FILTER_OUTPUT
        filter.extend_from_slice(&FILTER_OUTPUT.to_le_bytes());
        filter.extend_from_slice(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        filter.extend_from_slice(&[0x48, 0xb8]);
        filter.extend_from_slice(&(FILTER_OUTPUT + 8).to_le_bytes());
        filter.extend_from_slice(&[
            0x48, 0x89, 0x10, // mov [rax], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // mov eax, EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        process.write_bytes(IMAGE_BASE + 0x500, &filter).unwrap();
        process.cpu.gpr[anywasm_cpu::cpu::RCX] = EXCEPTION_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::RDX] = ESTABLISHER;
        process.cpu.gpr[anywasm_cpu::cpu::R8] = CONTEXT_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::R9] = DISPATCHER_CONTEXT;
        process.cpu.gpr[RSP] = STACK + 0x400;

        let result = c_specific_handler(&mut process);

        assert_eq!(result, Ok(EXCEPTION_CONTINUE_EXECUTION));
        let exception_pointers = process.rd64(FILTER_OUTPUT).unwrap();
        assert_eq!(process.rd64(exception_pointers).unwrap(), EXCEPTION_RECORD);
        assert_eq!(
            process.rd64(exception_pointers + 8).unwrap(),
            CONTEXT_RECORD
        );
        assert_eq!(process.rd64(FILTER_OUTPUT + 8).unwrap(), ESTABLISHER);
    }

    #[test]
    fn c_scope_table_runs_finally_during_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const EXCEPTION_RECORD: u64 = INFO;
        const CONTEXT_RECORD: u64 = INFO + 0x100;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x200;
        const FINALLY_OUTPUT: u64 = INFO + 0x400;
        const SCOPE_TABLE: u64 = IMAGE_BASE + 0x700;
        const ESTABLISHER: u64 = STACK + 0x180;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        let mut module = test_module();
        module.runtime_functions[0].end_address = 0x400;
        process.modules.push(module);
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            0xe123_4567,
            EXCEPTION_UNWINDING,
            IMAGE_BASE + 0x110,
            &[],
        )
        .unwrap();
        process.wr32(SCOPE_TABLE, 1).unwrap();
        process.wr32(SCOPE_TABLE + 4, 0x100).unwrap();
        process.wr32(SCOPE_TABLE + 8, 0x120).unwrap();
        process.wr32(SCOPE_TABLE + 12, 0x500).unwrap(); // Finally funclet.
        process.wr32(SCOPE_TABLE + 16, 0).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT, IMAGE_BASE + 0x110)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 8, IMAGE_BASE).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 24, ESTABLISHER).unwrap();
        process
            .wr64(DISPATCHER_CONTEXT + 32, IMAGE_BASE + 0x300)
            .unwrap();
        process
            .wr64(DISPATCHER_CONTEXT + 40, CONTEXT_RECORD)
            .unwrap();
        process.wr64(DISPATCHER_CONTEXT + 56, SCOPE_TABLE).unwrap();
        process.wr64(DISPATCHER_CONTEXT + 64, 0).unwrap();
        process.wr32(DISPATCHER_CONTEXT + 72, 0).unwrap();

        let mut finally_body = vec![0x48, 0xb8]; // mov rax, FINALLY_OUTPUT
        finally_body.extend_from_slice(&FINALLY_OUTPUT.to_le_bytes());
        finally_body.extend_from_slice(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        finally_body.extend_from_slice(&[0x48, 0xb8]);
        finally_body.extend_from_slice(&(FINALLY_OUTPUT + 8).to_le_bytes());
        finally_body.extend_from_slice(&[
            0x48, 0x89, 0x10, // mov [rax], rdx
            0x31, 0xc0, // xor eax, eax
            0xc3,
        ]);
        process
            .write_bytes(IMAGE_BASE + 0x500, &finally_body)
            .unwrap();
        process.cpu.gpr[anywasm_cpu::cpu::RCX] = EXCEPTION_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::RDX] = ESTABLISHER;
        process.cpu.gpr[anywasm_cpu::cpu::R8] = CONTEXT_RECORD;
        process.cpu.gpr[anywasm_cpu::cpu::R9] = DISPATCHER_CONTEXT;
        process.cpu.gpr[RSP] = STACK + 0x400;

        assert_eq!(
            c_specific_handler(&mut process),
            Ok(EXCEPTION_CONTINUE_SEARCH)
        );
        assert_eq!(process.rd64(FINALLY_OUTPUT).unwrap(), 1);
        assert_eq!(process.rd64(FINALLY_OUTPUT + 8).unwrap(), ESTABLISHER);
        assert_eq!(process.rd32(DISPATCHER_CONTEXT + 72).unwrap(), 1);
    }

    #[test]
    fn raise_exception_dispatches_a_non_msvc_record_to_a_catch_all_scope() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const CODE: u32 = 0xe123_4567;
        const RAISE_RSP: u64 = STACK + 0x600;
        const VEH_MARKER: u64 = INFO + 0x500;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions[0].end_address = 0x180;
        process.modules.push(module);
        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, IMAGE_BASE + 0x120).unwrap();

        // One x64 runtime-function entry with both exception and unwind handlers.
        process.wr32(IMAGE_BASE + 0x500, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0x504, 0x120).unwrap();
        process.wr32(IMAGE_BASE + 0x508, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x19).unwrap(); // version 1, EHANDLER | UHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process.wr32(IMAGE_BASE + 0x304, 0x600).unwrap();

        let scope_table = IMAGE_BASE + 0x308;
        process.wr32(scope_table, 1).unwrap();
        process.wr32(scope_table + 4, 0x100).unwrap();
        process.wr32(scope_table + 8, 0x180).unwrap();
        process.wr32(scope_table + 12, 1).unwrap();
        process.wr32(scope_table + 16, 0x160).unwrap();

        let c_handler = process
            .thunk_for("crt", "__C_specific_handler", true)
            .unwrap();
        let mut shim = vec![0x48, 0xb8]; // mov rax, __C_specific_handler thunk
        shim.extend_from_slice(&c_handler.to_le_bytes());
        shim.extend_from_slice(&[0xff, 0xe0]); // jmp rax
        process.write_bytes(IMAGE_BASE + 0x600, &shim).unwrap();

        let mut vectored_handler = vec![0x48, 0xb8]; // mov rax, VEH_MARKER
        vectored_handler.extend_from_slice(&VEH_MARKER.to_le_bytes());
        vectored_handler.extend_from_slice(&[
            0xc7, 0x00, 0x01, 0x00, 0x00, 0x00, // mov dword ptr [rax], 1
            0x31, 0xc0, // xor eax, eax (EXCEPTION_CONTINUE_SEARCH)
            0xc3,
        ]);
        process
            .write_bytes(IMAGE_BASE + 0x580, &vectored_handler)
            .unwrap();
        process
            .vectored_exception_handlers
            .push((0x7000_0000, IMAGE_BASE + 0x580));

        process.cpu.gpr[RSP] = RAISE_RSP;

        let result = dispatch_raised_exception(&mut process, CODE, 0, &[0xaaaa, 0xbbbb], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], CODE as u64);
        assert!(process.skip_ret);
        assert_eq!(process.rd32(VEH_MARKER).unwrap(), 1);
    }

    #[test]
    fn nested_exception_disposition_marks_outer_x64_handler() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const INNER: u64 = IMAGE_BASE + 0x110;
        const OUTER: u64 = IMAGE_BASE + 0x210;
        const TOP: u64 = IMAGE_BASE + 0x310;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x380;
        const INNER_HANDLER: u64 = IMAGE_BASE + 0x600;
        const OUTER_HANDLER: u64 = IMAGE_BASE + 0x680;
        const TOP_HANDLER: u64 = IMAGE_BASE + 0x700;
        const RAISE_RSP: u64 = STACK + 0x600;
        const OUTER_FLAGS: u64 = INFO + 0x300;
        const TOP_FLAGS: u64 = INFO + 0x304;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions = vec![
            anywasm_pe::RuntimeFunction {
                begin_address: 0x100,
                end_address: 0x120,
                unwind_info_address: 0x400,
            },
            anywasm_pe::RuntimeFunction {
                begin_address: 0x200,
                end_address: 0x240,
                unwind_info_address: 0x420,
            },
            anywasm_pe::RuntimeFunction {
                begin_address: 0x300,
                end_address: 0x340,
                unwind_info_address: 0x440,
            },
        ];
        process.modules.push(module);

        for (entry, begin, end, unwind) in [
            (IMAGE_BASE + 0x500, 0x100, 0x120, 0x400),
            (IMAGE_BASE + 0x50c, 0x200, 0x240, 0x420),
            (IMAGE_BASE + 0x518, 0x300, 0x340, 0x440),
        ] {
            process.wr32(entry, begin).unwrap();
            process.wr32(entry + 4, end).unwrap();
            process.wr32(entry + 8, unwind).unwrap();
        }
        for (info, handler) in [
            (IMAGE_BASE + 0x400, INNER_HANDLER),
            (IMAGE_BASE + 0x420, OUTER_HANDLER),
            (IMAGE_BASE + 0x440, TOP_HANDLER),
        ] {
            process.wr8(info, 0x09).unwrap(); // version 1, EHANDLER
            process.wr8(info + 1, 0).unwrap();
            process.wr8(info + 2, 0).unwrap();
            process.wr8(info + 3, 0).unwrap();
            process
                .wr32(info + 4, (handler - IMAGE_BASE) as u32)
                .unwrap();
        }
        let outer_establisher = RAISE_RSP + 16;
        let mut inner_handler = vec![0x4c, 0x89, 0xc8]; // mov rax, r9 (DISPATCHER_CONTEXT)
        inner_handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, outer establisher
        inner_handler.extend_from_slice(&outer_establisher.to_le_bytes());
        inner_handler.extend_from_slice(&[
            0x48, 0x89, 0x50, 0x18, // mov [rax + DISPATCHER_CONTEXT.EstablisherFrame], rdx
            0xb8, 2, 0, 0, 0, // ExceptionNestedException
            0xc3,
        ]);
        process.write_bytes(INNER_HANDLER, &inner_handler).unwrap();

        let mut outer_handler = vec![0x8b, 0x41, 0x04]; // mov eax, [rcx + ExceptionFlags]
        outer_handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, OUTER_FLAGS
        outer_handler.extend_from_slice(&OUTER_FLAGS.to_le_bytes());
        outer_handler.extend_from_slice(&[
            0x89, 0x02, // mov [rdx], eax
            0xb8, 1, 0, 0, 0, // ExceptionContinueSearch
            0xc3,
        ]);
        process.write_bytes(OUTER_HANDLER, &outer_handler).unwrap();

        let mut top_handler = vec![0x8b, 0x41, 0x04]; // mov eax, [rcx + ExceptionFlags]
        top_handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, TOP_FLAGS
        top_handler.extend_from_slice(&TOP_FLAGS.to_le_bytes());
        top_handler.extend_from_slice(&[0x89, 0x02]); // mov [rdx], eax
        top_handler.extend_from_slice(&[0x4c, 0x89, 0xc0]); // mov rax, r8 (CONTEXT)
        top_handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, LANDING_PAD
        top_handler.extend_from_slice(&LANDING_PAD.to_le_bytes());
        top_handler.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0x31, 0xc0, // ExceptionContinueExecution
            0xc3,
        ]);
        process.write_bytes(TOP_HANDLER, &top_handler).unwrap();

        process.wr64(RAISE_RSP, INNER).unwrap();
        process.wr64(RAISE_RSP + 8, OUTER).unwrap();
        process.wr64(RAISE_RSP + 16, TOP).unwrap();
        process.wr64(RAISE_RSP + 24, 0).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], INNER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(OUTER_FLAGS).unwrap(), EXCEPTION_NESTED_CALL);
        assert_eq!(process.rd32(TOP_FLAGS).unwrap(), 0);
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
    }

    #[test]
    fn collided_x64_search_retries_the_saved_dispatcher_frame() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const REDIRECT_PC: u64 = IMAGE_BASE + 0x118;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x170;
        const HANDLER: u64 = IMAGE_BASE + 0x600;
        const RAISE_RSP: u64 = STACK + 0x600;
        const FIRST_FLAGS: u64 = INFO + 0x300;
        const RETRY_FLAGS: u64 = INFO + 0x304;
        const DISPATCHER_CONTEXT_RECORD: u64 = INFO + 0x308;
        const HANDLER_CONTEXT_RECORD: u64 = INFO + 0x310;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions[0].end_address = 0x180;
        process.modules.push(module);
        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, 0).unwrap();
        process.wr32(IMAGE_BASE + 0x500, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0x504, 0x180).unwrap();
        process.wr32(IMAGE_BASE + 0x508, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x09).unwrap(); // version 1, EHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process.wr32(IMAGE_BASE + 0x304, 0x600).unwrap();

        let mut handler = vec![0x8b, 0x41, 0x04]; // mov eax, [rcx + ExceptionFlags]
        handler.extend_from_slice(&[0xa9, 0x40, 0, 0, 0]); // test eax, EXCEPTION_COLLIDED_UNWIND
        handler.extend_from_slice(&[0x75, 0]); // jnz collided (relative byte patched below)
        handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, FIRST_FLAGS
        handler.extend_from_slice(&FIRST_FLAGS.to_le_bytes());
        handler.extend_from_slice(&[0x89, 0x02]); // mov [rdx], eax
        handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, DISPATCHER_CONTEXT_RECORD
        handler.extend_from_slice(&DISPATCHER_CONTEXT_RECORD.to_le_bytes());
        handler.extend_from_slice(&[0x49, 0x8b, 0x41, 0x28]); // mov rax, [r9 + ContextRecord]
        handler.extend_from_slice(&[0x48, 0x89, 0x02]); // mov [rdx], rax
        handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, HANDLER_CONTEXT_RECORD
        handler.extend_from_slice(&HANDLER_CONTEXT_RECORD.to_le_bytes());
        handler.extend_from_slice(&[0x4c, 0x89, 0x02]); // mov [rdx], r8
        handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, REDIRECT_PC
        handler.extend_from_slice(&REDIRECT_PC.to_le_bytes());
        handler.extend_from_slice(&[0x49, 0x89, 0x11]); // mov [r9 + ControlPc], rdx
        handler.extend_from_slice(&[0xb8, 3, 0, 0, 0, 0xc3]); // ExceptionCollidedUnwind; ret
        let collided = handler.len();
        handler[9] = (collided - 10) as u8;
        handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, RETRY_FLAGS
        handler.extend_from_slice(&RETRY_FLAGS.to_le_bytes());
        handler.extend_from_slice(&[0x89, 0x02]); // mov [rdx], eax
        handler.extend_from_slice(&[0x49, 0xb9]); // mov r9, LANDING_PAD
        handler.extend_from_slice(&LANDING_PAD.to_le_bytes());
        handler.extend_from_slice(&[
            0x4d, 0x89, 0x88, 0xf8, 0, 0, 0, // mov [r8 + CONTEXT.Rip], r9
            0x31, 0xc0, // ExceptionContinueExecution
            0xc3,
        ]);
        process.write_bytes(HANDLER, &handler).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;

        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(FIRST_FLAGS).unwrap(), 0);
        assert_eq!(
            process.rd32(RETRY_FLAGS).unwrap(),
            EXCEPTION_COLLIDED_UNWIND_FLAG
        );
        assert_ne!(
            process.rd64(DISPATCHER_CONTEXT_RECORD).unwrap(),
            process.rd64(HANDLER_CONTEXT_RECORD).unwrap()
        );
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
    }

    #[test]
    fn callback_managed_function_table_participates_in_exception_search() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const TABLE_IDENTIFIER: u64 = 0x200003;
        const TABLE_LENGTH: u64 = 0x1000;
        const CALLBACK: u64 = IMAGE_BASE + 0x700;
        const CALLBACK_CONTEXT: u64 = 0xfeed_beef;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const FUNCTION_ENTRY: u64 = IMAGE_BASE + 0x500;
        const EXCEPTION_HANDLER: u64 = IMAGE_BASE + 0x600;
        const TOP_LEVEL_FILTER: u64 = IMAGE_BASE + 0x780;
        const RAISE_RSP: u64 = STACK + 0x600;
        const SEEN_PC: u64 = INFO + 0x300;
        const SEEN_CONTEXT: u64 = INFO + 0x308;
        const HANDLER_MARKER: u64 = INFO + 0x310;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions.clear();
        process.modules.push(module);

        process.wr32(FUNCTION_ENTRY, 0x100).unwrap();
        process.wr32(FUNCTION_ENTRY + 4, 0x120).unwrap();
        process.wr32(FUNCTION_ENTRY + 8, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x09).unwrap(); // version 1, EHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process
            .wr32(IMAGE_BASE + 0x304, (EXCEPTION_HANDLER - IMAGE_BASE) as u32)
            .unwrap();

        let mut callback_code = vec![0x48, 0xb8]; // mov rax, SEEN_PC
        callback_code.extend_from_slice(&SEEN_PC.to_le_bytes());
        callback_code.extend_from_slice(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        callback_code.extend_from_slice(&[0x48, 0xb8]); // mov rax, SEEN_CONTEXT
        callback_code.extend_from_slice(&SEEN_CONTEXT.to_le_bytes());
        callback_code.extend_from_slice(&[0x48, 0x89, 0x10]); // mov [rax], rdx
        callback_code.extend_from_slice(&[0x48, 0xb8]); // mov rax, FUNCTION_ENTRY
        callback_code.extend_from_slice(&FUNCTION_ENTRY.to_le_bytes());
        callback_code.push(0xc3);
        process.write_bytes(CALLBACK, &callback_code).unwrap();

        let mut exception_handler = vec![0x48, 0xb8]; // mov rax, HANDLER_MARKER
        exception_handler.extend_from_slice(&HANDLER_MARKER.to_le_bytes());
        exception_handler.extend_from_slice(&[
            0xc7, 0x00, 1, 0, 0, 0, // mov dword ptr [rax], 1
            0xb8, 1, 0, 0, 0, // EXCEPTION_CONTINUE_SEARCH
            0xc3,
        ]);
        process
            .write_bytes(EXCEPTION_HANDLER, &exception_handler)
            .unwrap();

        let mut top_filter = vec![0x48, 0x8b, 0x41, 0x08]; // mov rax, [rcx + ContextRecord]
        top_filter.extend_from_slice(&[0x48, 0xba]); // mov rdx, LANDING_PAD
        top_filter.extend_from_slice(&LANDING_PAD.to_le_bytes());
        top_filter.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        process.write_bytes(TOP_LEVEL_FILTER, &top_filter).unwrap();
        process.top_level_exception_filter = TOP_LEVEL_FILTER;

        let install = crate::hle::lookup("ntdll", "RtlInstallFunctionTableCallback").unwrap();
        process.cpu.gpr[RSP] = STACK + 0x500;
        process.cpu.gpr[RCX] = TABLE_IDENTIFIER;
        process.cpu.gpr[RDX] = IMAGE_BASE;
        process.cpu.gpr[R8] = TABLE_LENGTH;
        process.cpu.gpr[R9] = CALLBACK;
        process
            .wr64(process.cpu.gpr[RSP] + 0x28, CALLBACK_CONTEXT)
            .unwrap();
        assert_eq!(install(&mut process), Ok(1));

        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, 0).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd64(SEEN_PC).unwrap(), CALLER);
        assert_eq!(process.rd64(SEEN_CONTEXT).unwrap(), CALLBACK_CONTEXT);
        assert_eq!(process.rd32(HANDLER_MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
    }

    #[test]
    fn exception_search_uses_a_grown_dynamic_function_table() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const TABLE: u64 = IMAGE_BASE + 0x900;
        const ADD_OUTPUT: u64 = INFO + 0x280;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const EXCEPTION_HANDLER: u64 = IMAGE_BASE + 0x600;
        const TOP_LEVEL_FILTER: u64 = IMAGE_BASE + 0x780;
        const RAISE_RSP: u64 = STACK + 0x600;
        const HANDLER_MARKER: u64 = INFO + 0x2a0;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process
            .mem
            .map(crate::process::TEB_ADDR, PAGE_SIZE, PROT_RW));
        process
            .wr64(crate::process::TEB_ADDR + 0x08, STACK + PAGE_SIZE)
            .unwrap();
        process
            .wr64(crate::process::TEB_ADDR + 0x10, STACK)
            .unwrap();
        let mut module = test_module();
        module.runtime_functions.clear();
        process.modules.push(module);

        let add = crate::hle::lookup("ntdll", "RtlAddGrowableFunctionTable").unwrap();
        let grow = crate::hle::lookup("ntdll", "RtlGrowFunctionTable").unwrap();
        process.cpu.gpr[RSP] = STACK + 0x500;
        process.cpu.gpr[RCX] = ADD_OUTPUT;
        process.cpu.gpr[RDX] = TABLE;
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 4;
        process.wr64(STACK + 0x528, IMAGE_BASE).unwrap();
        process.wr64(STACK + 0x530, IMAGE_BASE + 0x2000).unwrap();
        assert_eq!(add(&mut process), Ok(0));
        let table_handle = process.rd64(ADD_OUTPUT).unwrap();
        assert_ne!(table_handle, 0);

        process.wr32(TABLE, 0x100).unwrap();
        process.wr32(TABLE + 4, 0x120).unwrap();
        process.wr32(TABLE + 8, 0x300).unwrap();
        process.cpu.gpr[RCX] = table_handle;
        process.cpu.gpr[RDX] = 1;
        assert_eq!(grow(&mut process), Ok(0));

        process.wr8(IMAGE_BASE + 0x300, 0x09).unwrap(); // version 1, EHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process
            .wr32(IMAGE_BASE + 0x304, (EXCEPTION_HANDLER - IMAGE_BASE) as u32)
            .unwrap();

        let mut exception_handler = vec![0x48, 0xb8]; // mov rax, HANDLER_MARKER
        exception_handler.extend_from_slice(&HANDLER_MARKER.to_le_bytes());
        exception_handler.extend_from_slice(&[
            0xc7, 0x00, 1, 0, 0, 0, // mov dword ptr [rax], 1
            0xb8, 1, 0, 0, 0, // EXCEPTION_CONTINUE_SEARCH
            0xc3,
        ]);
        process
            .write_bytes(EXCEPTION_HANDLER, &exception_handler)
            .unwrap();

        let mut top_filter = vec![0x48, 0x8b, 0x41, 0x08]; // mov rax, [rcx + ContextRecord]
        top_filter.extend_from_slice(&[0x48, 0xba]); // mov rdx, LANDING_PAD
        top_filter.extend_from_slice(&LANDING_PAD.to_le_bytes());
        top_filter.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        process.write_bytes(TOP_LEVEL_FILTER, &top_filter).unwrap();
        process.top_level_exception_filter = TOP_LEVEL_FILTER;

        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, IMAGE_BASE + 0x120).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(HANDLER_MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
    }

    #[test]
    fn callback_managed_function_table_participates_in_phase_two_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const TABLE_IDENTIFIER: u64 = 0x200007;
        const TABLE_LENGTH: u64 = 0x1000;
        const CALLBACK: u64 = IMAGE_BASE + 0x700;
        const CALLBACK_CONTEXT: u64 = 0xfeed_beef;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const FUNCTION_ENTRY: u64 = IMAGE_BASE + 0x500;
        const EXCEPTION_HANDLER: u64 = IMAGE_BASE + 0x600;
        const RAISE_RSP: u64 = STACK + 0x600;
        const CALLBACK_COUNT: u64 = INFO + 0x300;
        const HANDLER_COUNT: u64 = INFO + 0x304;
        const LAST_PC: u64 = INFO + 0x308;
        const LAST_CONTEXT: u64 = INFO + 0x310;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions.clear();
        process.modules.push(module);

        process.wr32(FUNCTION_ENTRY, 0x100).unwrap();
        process.wr32(FUNCTION_ENTRY + 4, 0x180).unwrap();
        process.wr32(FUNCTION_ENTRY + 8, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x19).unwrap(); // version 1, EHANDLER | UHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process
            .wr32(IMAGE_BASE + 0x304, (EXCEPTION_HANDLER - IMAGE_BASE) as u32)
            .unwrap();

        let scope_table = IMAGE_BASE + 0x308;
        process.wr32(scope_table, 1).unwrap();
        process.wr32(scope_table + 4, 0x100).unwrap();
        process.wr32(scope_table + 8, 0x180).unwrap();
        process.wr32(scope_table + 12, 1).unwrap(); // Catch-all filter.
        process.wr32(scope_table + 16, 0x160).unwrap();

        let c_handler = process
            .thunk_for("crt", "__C_specific_handler", true)
            .unwrap();
        let mut exception_handler = vec![0x48, 0xb8]; // mov rax, HANDLER_COUNT
        exception_handler.extend_from_slice(&HANDLER_COUNT.to_le_bytes());
        exception_handler.extend_from_slice(&[
            0x83, 0x00, 0x01, // add dword ptr [rax], 1
            0x48, 0xb8,
        ]);
        exception_handler.extend_from_slice(&c_handler.to_le_bytes());
        exception_handler.extend_from_slice(&[0xff, 0xe0]); // jmp to __C_specific_handler
        process
            .write_bytes(EXCEPTION_HANDLER, &exception_handler)
            .unwrap();

        let mut callback = vec![0x48, 0xb8]; // mov rax, CALLBACK_COUNT
        callback.extend_from_slice(&CALLBACK_COUNT.to_le_bytes());
        callback.extend_from_slice(&[
            0x83, 0x00, 0x01, // add dword ptr [rax], 1
            0x48, 0xb8,
        ]);
        callback.extend_from_slice(&LAST_PC.to_le_bytes());
        callback.extend_from_slice(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        callback.extend_from_slice(&[0x48, 0xb8]);
        callback.extend_from_slice(&LAST_CONTEXT.to_le_bytes());
        callback.extend_from_slice(&[0x48, 0x89, 0x10]); // mov [rax], rdx
        callback.extend_from_slice(&[0x48, 0xb8]);
        callback.extend_from_slice(&FUNCTION_ENTRY.to_le_bytes());
        callback.push(0xc3);
        process.write_bytes(CALLBACK, &callback).unwrap();

        let install = crate::hle::lookup("ntdll", "RtlInstallFunctionTableCallback").unwrap();
        process.cpu.gpr[RSP] = STACK + 0x500;
        process.cpu.gpr[RCX] = TABLE_IDENTIFIER;
        process.cpu.gpr[RDX] = IMAGE_BASE;
        process.cpu.gpr[R8] = TABLE_LENGTH;
        process.cpu.gpr[R9] = CALLBACK;
        process
            .wr64(process.cpu.gpr[RSP] + 0x28, CALLBACK_CONTEXT)
            .unwrap();
        assert_eq!(install(&mut process), Ok(1));

        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, IMAGE_BASE + 0x120).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], CODE as u64);
        assert!(process.skip_ret);
        assert_eq!(process.rd32(HANDLER_COUNT).unwrap(), 2);
        assert_eq!(process.rd32(CALLBACK_COUNT).unwrap(), 4);
        assert_eq!(process.rd64(LAST_PC).unwrap(), LANDING_PAD);
        assert_eq!(process.rd64(LAST_CONTEXT).unwrap(), CALLBACK_CONTEXT);
    }

    #[test]
    fn grown_function_table_participates_in_phase_two_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const TABLE: u64 = IMAGE_BASE + 0x900;
        const ADD_OUTPUT: u64 = INFO + 0x280;
        const CALLER: u64 = IMAGE_BASE + 0x110;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const EXCEPTION_HANDLER: u64 = IMAGE_BASE + 0x600;
        const RAISE_RSP: u64 = STACK + 0x600;
        const HANDLER_COUNT: u64 = INFO + 0x304;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions.clear();
        process.modules.push(module);

        let add = crate::hle::lookup("ntdll", "RtlAddGrowableFunctionTable").unwrap();
        let grow = crate::hle::lookup("ntdll", "RtlGrowFunctionTable").unwrap();
        process.cpu.gpr[RSP] = STACK + 0x500;
        process.cpu.gpr[RCX] = ADD_OUTPUT;
        process.cpu.gpr[RDX] = TABLE;
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 4;
        process.wr64(STACK + 0x528, IMAGE_BASE).unwrap();
        process.wr64(STACK + 0x530, IMAGE_BASE + 0x2000).unwrap();
        assert_eq!(add(&mut process), Ok(0));
        let table_handle = process.rd64(ADD_OUTPUT).unwrap();
        assert_ne!(table_handle, 0);

        process.wr32(TABLE, 0x100).unwrap();
        process.wr32(TABLE + 4, 0x180).unwrap();
        process.wr32(TABLE + 8, 0x300).unwrap();
        process.cpu.gpr[RCX] = table_handle;
        process.cpu.gpr[RDX] = 1;
        assert_eq!(grow(&mut process), Ok(0));

        process.wr8(IMAGE_BASE + 0x300, 0x19).unwrap(); // version 1, EHANDLER | UHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process
            .wr32(IMAGE_BASE + 0x304, (EXCEPTION_HANDLER - IMAGE_BASE) as u32)
            .unwrap();
        let scope_table = IMAGE_BASE + 0x308;
        process.wr32(scope_table, 1).unwrap();
        process.wr32(scope_table + 4, 0x100).unwrap();
        process.wr32(scope_table + 8, 0x180).unwrap();
        process.wr32(scope_table + 12, 1).unwrap(); // Catch-all filter.
        process.wr32(scope_table + 16, 0x160).unwrap();

        let c_handler = process
            .thunk_for("crt", "__C_specific_handler", true)
            .unwrap();
        let mut exception_handler = vec![0x48, 0xb8]; // mov rax, HANDLER_COUNT
        exception_handler.extend_from_slice(&HANDLER_COUNT.to_le_bytes());
        exception_handler.extend_from_slice(&[
            0x83, 0x00, 0x01, // add dword ptr [rax], 1
            0x48, 0xb8,
        ]);
        exception_handler.extend_from_slice(&c_handler.to_le_bytes());
        exception_handler.extend_from_slice(&[0xff, 0xe0]); // jmp to __C_specific_handler
        process
            .write_bytes(EXCEPTION_HANDLER, &exception_handler)
            .unwrap();

        process.wr64(RAISE_RSP, CALLER).unwrap();
        process.wr64(RAISE_RSP + 8, IMAGE_BASE + 0x120).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], CODE as u64);
        assert!(process.skip_ret);
        assert_eq!(process.rd32(HANDLER_COUNT).unwrap(), 2);
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
            0,
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
    fn validates_non_null_unwind_history_tables() {
        let mut process = process_with_memory();
        process.wr32(INFO, UNWIND_HISTORY_TABLE_ENTRIES).unwrap();
        assert!(validate_unwind_history_table(&process, INFO, "test").is_ok());

        process
            .wr32(INFO, UNWIND_HISTORY_TABLE_ENTRIES + 1)
            .unwrap();
        assert!(matches!(
            validate_unwind_history_table(&process, INFO, "test"),
            Err(Stop::Crash(_))
        ));
        assert!(matches!(
            validate_unwind_history_table(&process, INFO + 1, "test"),
            Err(Stop::Crash(_))
        ));
        assert!(matches!(
            validate_unwind_history_table(&process, INFO + PAGE_SIZE as u64 - 8, "test"),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn lookup_frame_uses_sorted_runtime_metadata_and_end_exclusive_ranges() {
        let mut process = process_with_memory();
        process.modules.push(test_module());

        assert_eq!(
            lookup_frame(&mut process, 0x1400_00110).unwrap(),
            Some((0x1400_0000_0, 0x1400_00500))
        );
        assert_eq!(lookup_frame(&mut process, 0x1400_00120).unwrap(), None);
    }

    #[test]
    fn exception_search_unwinds_through_a_mapped_leaf_without_pdata() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const LEAF_PC: u64 = IMAGE_BASE + 0x220;
        const OUTER_PC: u64 = IMAGE_BASE + 0x110;
        const EXCEPTION_HANDLER: u64 = IMAGE_BASE + 0x600;
        const TOP_LEVEL_FILTER: u64 = IMAGE_BASE + 0x780;
        const RAISE_RSP: u64 = STACK + 0x600;
        const HANDLER_MARKER: u64 = INFO + 0x280;
        const LANDING_PAD: u64 = IMAGE_BASE + 0x160;
        const CODE: u32 = 0xe123_4567;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        process.modules.push(test_module());

        process.wr32(IMAGE_BASE + 0x500, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0x504, 0x120).unwrap();
        process.wr32(IMAGE_BASE + 0x508, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x09).unwrap(); // version 1, EHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process
            .wr32(IMAGE_BASE + 0x304, (EXCEPTION_HANDLER - IMAGE_BASE) as u32)
            .unwrap();

        let mut exception_handler = vec![0x48, 0xb8]; // mov rax, HANDLER_MARKER
        exception_handler.extend_from_slice(&HANDLER_MARKER.to_le_bytes());
        exception_handler.extend_from_slice(&[
            0xc7, 0x00, 1, 0, 0, 0, // mov dword ptr [rax], 1
            0xb8, 1, 0, 0, 0, // EXCEPTION_CONTINUE_SEARCH
            0xc3,
        ]);
        process
            .write_bytes(EXCEPTION_HANDLER, &exception_handler)
            .unwrap();

        let mut top_filter = vec![0x48, 0x8b, 0x41, 0x08]; // mov rax, [rcx + ContextRecord]
        top_filter.extend_from_slice(&[0x48, 0xba]); // mov rdx, LANDING_PAD
        top_filter.extend_from_slice(&LANDING_PAD.to_le_bytes());
        top_filter.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        process.write_bytes(TOP_LEVEL_FILTER, &top_filter).unwrap();
        process.top_level_exception_filter = TOP_LEVEL_FILTER;

        process.wr64(RAISE_RSP, LEAF_PC).unwrap();
        process.wr64(RAISE_RSP + 8, OUTER_PC).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        let result = dispatch_raised_exception(&mut process, CODE, 0, &[], LEAF_PC);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(HANDLER_MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, LANDING_PAD);
        assert!(process.skip_ret);
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
    fn unwind_consolidation_calls_the_runtime_catch_callback() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const TARGET_PC: u64 = IMAGE_BASE + 0x110;
        const CONTINUATION: u64 = IMAGE_BASE + 0x120;
        const CALLBACK: u64 = IMAGE_BASE + 0x180;
        const MARKER: u64 = STACK + 0x300;
        const TARGET_FRAME: u64 = STACK + 0x800;
        const CALL_RSP: u64 = STACK + 0x600;
        const EXCEPTION_RECORD: u64 = INFO;
        const CONTEXT_RECORD: u64 = INFO + 0x100;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x200;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK + PAGE_SIZE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK).unwrap();
        let mut module = test_module();
        module.runtime_functions = vec![anywasm_pe::RuntimeFunction {
            begin_address: 0x100,
            end_address: 0x200,
            unwind_info_address: 0x300,
        }];
        process.modules.push(module);
        process.wr32(IMAGE_BASE + 0x500, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0x504, 0x200).unwrap();
        process.wr32(IMAGE_BASE + 0x508, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 1).unwrap(); // version 1, no handler
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();

        let mut callback_code = vec![0x48, 0xb8]; // mov rax, MARKER
        callback_code.extend_from_slice(&MARKER.to_le_bytes());
        callback_code.extend_from_slice(&[
            0xc7, 0x00, 0x01, 0x00, 0x00, 0x00, // mov dword ptr [rax], 1
            0x48, 0xb8, // mov rax, CONTINUATION
        ]);
        callback_code.extend_from_slice(&CONTINUATION.to_le_bytes());
        callback_code.push(0xc3); // ret
        process.write_bytes(CALLBACK, &callback_code).unwrap();
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            0x8000_0029, // STATUS_UNWIND_CONSOLIDATE
            EXCEPTION_NONCONTINUABLE,
            TARGET_PC,
            &[CALLBACK, 0, 0, 0, CONTEXT_RECORD, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();

        let mut target_context = context();
        target_context.rip = TARGET_PC;
        target_context.gpr[RSP] = TARGET_FRAME;
        write_context(&mut process, CONTEXT_RECORD, &target_context).unwrap();
        process.wr32(DISPATCHER_CONTEXT + 0x48, 0).unwrap();
        process.cpu.gpr[RSP] = CALL_RSP;
        let frames = [ExceptionFrame {
            control_pc: TARGET_PC,
            image_base: IMAGE_BASE,
            function_entry: IMAGE_BASE + 0x500,
            establisher: TARGET_FRAME,
            context: target_context,
        }];
        let unwind = GuestUnwind {
            target_frame: TARGET_FRAME,
            target_ip: TARGET_PC,
            call_rsp: CALL_RSP,
            exception_record: EXCEPTION_RECORD,
            return_value: 0,
            context_record: CONTEXT_RECORD,
            history_table: 0,
        };

        complete_guest_unwind(&mut process, &frames, DISPATCHER_CONTEXT, unwind).unwrap();

        assert_eq!(process.rd32(MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, CONTINUATION);
        assert_eq!(process.cpu.gpr[RSP], TARGET_FRAME);
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
        const HISTORY_TABLE: u64 = INFO + 0x300;
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
            history_table: HISTORY_TABLE,
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
        assert_eq!(
            process
                .rd64(DISPATCHER_CONTEXT + DISPATCHER_HISTORY_TABLE)
                .unwrap(),
            HISTORY_TABLE
        );
        assert_eq!(process.rd32(HISTORY_TABLE).unwrap(), 0);
    }

    #[test]
    fn nested_rtl_unwind_ex_from_phase_two_handler_takes_over_the_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const GUEST_STACK: u64 = 0x80000;
        const STACK_LIMIT: u64 = GUEST_STACK;
        const STACK_BASE: u64 = GUEST_STACK + 0x10000;
        const ORIGINAL_RSP: u64 = GUEST_STACK + 0x4000;
        const UNWIND_CALL_RSP: u64 = GUEST_STACK + 0x2000;
        const EXCEPTION_RECORD: u64 = GUEST_STACK + 0x2f00;
        const CONTEXT_RECORD: u64 = INFO + 0x200;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x100;
        const FIRST_MARKER: u64 = INFO + 0x180;
        const TARGET_MARKER: u64 = INFO + 0x188;
        const FIRST_HANDLER_RVA: u32 = 0x800;
        const TARGET_HANDLER_RVA: u32 = 0x900;

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
        for (entry_rva, begin, end, unwind_rva, handler_rva) in [
            (0x500, 0x100, 0x120, 0x300, FIRST_HANDLER_RVA),
            (0x50c, 0x200, 0x220, 0x320, TARGET_HANDLER_RVA),
        ] {
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
            process.wr32(unwind_info + 4, handler_rva).unwrap();
        }

        let rtl_unwind_ex = process.thunk_for("ntdll", "RtlUnwindEx", false).unwrap();
        let mut first_handler = vec![0x48, 0xb8]; // mov rax, FIRST_MARKER
        first_handler.extend_from_slice(&FIRST_MARKER.to_le_bytes());
        first_handler.extend_from_slice(&[
            0x83, 0x00, 0x01, // add dword ptr [rax], 1
            0x48, 0x83, 0xec, 0x38, // sub rsp, 0x38
            0x48, 0xb8, // mov rax, CONTEXT_RECORD
        ]);
        first_handler.extend_from_slice(&CONTEXT_RECORD.to_le_bytes());
        first_handler.extend_from_slice(&[
            0x48, 0x89, 0x44, 0x24, 0x20, // mov [rsp+0x20], rax (RtlUnwindEx arg 5)
            0x48, 0xc7, 0x44, 0x24, 0x28, 0, 0, 0, 0, // arg 6 = null
            0x48, 0xb9, // mov rcx, target frame
        ]);
        first_handler.extend_from_slice(&(ORIGINAL_RSP + 8).to_le_bytes());
        first_handler.extend_from_slice(&[0x48, 0xba]); // mov rdx, target IP
        first_handler.extend_from_slice(&(IMAGE_BASE + 0x210).to_le_bytes());
        first_handler.extend_from_slice(&[0x49, 0xb8]); // mov r8, exception record
        first_handler.extend_from_slice(&EXCEPTION_RECORD.to_le_bytes());
        first_handler.extend_from_slice(&[0x49, 0xb9]); // mov r9, return value
        first_handler.extend_from_slice(&0x77u64.to_le_bytes());
        first_handler.extend_from_slice(&[0x48, 0xb8]); // mov rax, RtlUnwindEx
        first_handler.extend_from_slice(&rtl_unwind_ex.to_le_bytes());
        first_handler.extend_from_slice(&[
            0xff, 0xd0, // call rax
            0xc3, // ret (unreachable)
        ]);
        process
            .write_bytes(IMAGE_BASE + FIRST_HANDLER_RVA as u64, &first_handler)
            .unwrap();

        let mut target_handler = vec![0x48, 0xb8]; // mov rax, TARGET_MARKER
        target_handler.extend_from_slice(&TARGET_MARKER.to_le_bytes());
        target_handler.extend_from_slice(&[
            0x83, 0x00, 0x01, // add dword ptr [rax], 1
            0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, ExceptionContinueSearch
            0xc3, // ret
        ]);
        process
            .write_bytes(IMAGE_BASE + TARGET_HANDLER_RVA as u64, &target_handler)
            .unwrap();

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
            target_frame: ORIGINAL_RSP,
            target_ip: first_pc,
            call_rsp: UNWIND_CALL_RSP,
            exception_record: EXCEPTION_RECORD,
            return_value: 0x55,
            context_record: CONTEXT_RECORD,
            history_table: 0,
        };
        process.cpu.gpr[RSP] = ORIGINAL_RSP;

        complete_guest_unwind(&mut process, &frames, DISPATCHER_CONTEXT, unwind).unwrap();

        assert_eq!(process.rd32(FIRST_MARKER).unwrap(), 1);
        assert_eq!(process.rd32(TARGET_MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, target_pc);
        assert_eq!(process.cpu.gpr[RSP], ORIGINAL_RSP + 8);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], 0x77);
    }

    #[test]
    fn selected_cpp_catch_funclet_can_redirect_the_phase_two_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const GUEST_STACK: u64 = 0x80000;
        const STACK_LIMIT: u64 = GUEST_STACK;
        const STACK_BASE: u64 = GUEST_STACK + 0x10000;
        const FRAME_RSP: u64 = GUEST_STACK + 0x4000;
        const EXCEPTION_RECORD: u64 = GUEST_STACK + 0x2f00;
        const CONTEXT_RECORD: u64 = INFO + 0x200;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x100;
        const CXX_HANDLER_RVA: u32 = 0x800;
        const CATCH_FUNCLET_RVA: u64 = 0x900;
        const CONTINUATION_RVA: u64 = 0x980;
        const FUNC_INFO_RVA: u64 = 0xa00;
        const MARKER: u64 = INFO + 0x180;
        const THROW_INFO_RVA: u64 = 0xc00;
        const OBJECT_RVA: u64 = 0xc80;

        let mut process = process_with_memory();
        assert!(process.mem.map(IMAGE_BASE, 0x2000, PROT_RWX));
        assert!(process.mem.map(GUEST_STACK, 0x10000, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr64(TEB_ADDR + 0x08, STACK_BASE).unwrap();
        process.wr64(TEB_ADDR + 0x10, STACK_LIMIT).unwrap();

        let mut module = test_module();
        module.runtime_functions = vec![anywasm_pe::RuntimeFunction {
            begin_address: 0x100,
            end_address: 0x200,
            unwind_info_address: 0x300,
        }];
        process.modules.push(module);
        process.wr32(IMAGE_BASE + 0x500, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0x504, 0x200).unwrap();
        process.wr32(IMAGE_BASE + 0x508, 0x300).unwrap();
        process.wr8(IMAGE_BASE + 0x300, 0x19).unwrap(); // version 1, EHANDLER | UHANDLER
        process.wr8(IMAGE_BASE + 0x301, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x302, 0).unwrap();
        process.wr8(IMAGE_BASE + 0x303, 0).unwrap();
        process.wr32(IMAGE_BASE + 0x304, CXX_HANDLER_RVA).unwrap();
        process
            .wr32(IMAGE_BASE + 0x308, FUNC_INFO_RVA as u32)
            .unwrap();

        let cxx_frame_handler = process
            .thunk_for("crt", "__CxxFrameHandler3", true)
            .unwrap();
        process
            .write_bytes(
                IMAGE_BASE + CXX_HANDLER_RVA as u64,
                &[0x48, 0xb8]
                    .into_iter()
                    .chain(cxx_frame_handler.to_le_bytes())
                    .chain([0xff, 0xe0]) // mov rax, thunk; jmp rax
                    .collect::<Vec<_>>(),
            )
            .unwrap();

        // EH3 FuncInfo: one active try, a typed catch, and no destructor actions.
        process
            .wr32(IMAGE_BASE + FUNC_INFO_RVA, 0x1993_0522)
            .unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 4, 1).unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 8, 0xb00).unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 12, 1).unwrap();
        process
            .wr32(IMAGE_BASE + FUNC_INFO_RVA + 16, 0xb10)
            .unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 20, 1).unwrap();
        process
            .wr32(IMAGE_BASE + FUNC_INFO_RVA + 24, 0xb30)
            .unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 28, 0).unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 32, 0).unwrap();
        process.wr32(IMAGE_BASE + FUNC_INFO_RVA + 36, 0).unwrap();
        process.wr32(IMAGE_BASE + 0xb00, u32::MAX).unwrap(); // unwind to state -1
        process.wr32(IMAGE_BASE + 0xb04, 0).unwrap();
        for (offset, value) in [(0, 0), (4, 0), (8, 0), (12, 1), (16, 0xb40)] {
            process.wr32(IMAGE_BASE + 0xb10 + offset, value).unwrap();
        }
        for (offset, value) in [
            (0, 0),
            (4, 0xb60),
            (8, u32::MAX - 7), // catch object at establisher - 8
            (12, CATCH_FUNCLET_RVA as u32),
            (16, 0),
        ] {
            process.wr32(IMAGE_BASE + 0xb40 + offset, value).unwrap();
        }
        process.wr64(IMAGE_BASE + 0xb60, 0).unwrap();
        process
            .write_bytes(IMAGE_BASE + 0xb70, b".?AUDerived@@\0")
            .unwrap();
        process.wr32(IMAGE_BASE + 0xb30, 0x100).unwrap();
        process.wr32(IMAGE_BASE + 0xb34, 0).unwrap();

        // ThrowInfo and its single CatchableType match the EH3 handler above.
        process.wr32(IMAGE_BASE + THROW_INFO_RVA, 0).unwrap();
        process.wr32(IMAGE_BASE + THROW_INFO_RVA + 4, 0).unwrap();
        process.wr32(IMAGE_BASE + THROW_INFO_RVA + 8, 0).unwrap();
        process
            .wr32(IMAGE_BASE + THROW_INFO_RVA + 12, 0xc20)
            .unwrap();
        process.wr32(IMAGE_BASE + 0xc20, 1).unwrap();
        process.wr32(IMAGE_BASE + 0xc24, 0xc40).unwrap();
        process.wr32(IMAGE_BASE + 0xc40, 0).unwrap();
        process.wr32(IMAGE_BASE + 0xc44, 0xc60).unwrap();
        process.wr32(IMAGE_BASE + 0xc48, 0).unwrap();
        process.wr32(IMAGE_BASE + 0xc4c, u32::MAX).unwrap();
        process.wr32(IMAGE_BASE + 0xc50, 0).unwrap();
        process.wr32(IMAGE_BASE + 0xc54, 16).unwrap();
        process.wr32(IMAGE_BASE + 0xc58, 0).unwrap();
        process.wr64(IMAGE_BASE + 0xc60, 0).unwrap();
        process
            .write_bytes(IMAGE_BASE + 0xc70, b".?AUDerived@@\0")
            .unwrap();
        process
            .write_bytes(IMAGE_BASE + OBJECT_RVA, &[0x69; 16])
            .unwrap();

        let rtl_unwind_ex = process.thunk_for("ntdll", "RtlUnwindEx", false).unwrap();
        let mut catch_funclet = vec![0x48, 0xb8]; // mov rax, MARKER
        catch_funclet.extend_from_slice(&MARKER.to_le_bytes());
        catch_funclet.extend_from_slice(&[
            0xc7, 0x00, 0x01, 0x00, 0x00, 0x00, // mov dword ptr [rax], 1
            0x48, 0x83, 0xec, 0x38, // sub rsp, 0x38
            0x48, 0xb8, // mov rax, CONTEXT_RECORD
        ]);
        catch_funclet.extend_from_slice(&CONTEXT_RECORD.to_le_bytes());
        catch_funclet.extend_from_slice(&[
            0x48, 0x89, 0x44, 0x24, 0x20, // mov [rsp+0x20], rax (arg 5)
            0x48, 0xc7, 0x44, 0x24, 0x28, 0, 0, 0, 0, // arg 6 = null
            0x48, 0xb9, // mov rcx, current frame
        ]);
        catch_funclet.extend_from_slice(&FRAME_RSP.to_le_bytes());
        catch_funclet.extend_from_slice(&[0x48, 0xba]); // mov rdx, catch continuation
        catch_funclet.extend_from_slice(&(IMAGE_BASE + CONTINUATION_RVA).to_le_bytes());
        catch_funclet.extend_from_slice(&[0x49, 0xb8]); // mov r8, exception record
        catch_funclet.extend_from_slice(&EXCEPTION_RECORD.to_le_bytes());
        catch_funclet.extend_from_slice(&[0x49, 0xb9]); // mov r9, return value
        catch_funclet.extend_from_slice(&0x99u64.to_le_bytes());
        catch_funclet.extend_from_slice(&[0x48, 0xb8]); // mov rax, RtlUnwindEx
        catch_funclet.extend_from_slice(&rtl_unwind_ex.to_le_bytes());
        catch_funclet.extend_from_slice(&[0xff, 0xd0, 0xc3]); // call rax; unreachable ret
        process
            .write_bytes(IMAGE_BASE + CATCH_FUNCLET_RVA, &catch_funclet)
            .unwrap();

        let first_pc = IMAGE_BASE + 0x110;
        let catch_funclet_ip = IMAGE_BASE + CATCH_FUNCLET_RVA;
        let continuation = IMAGE_BASE + CONTINUATION_RVA;
        let mut frame_context = context();
        frame_context.rip = first_pc;
        frame_context.gpr[RSP] = FRAME_RSP;
        process.wr64(FRAME_RSP, IMAGE_BASE + 0x700).unwrap();
        write_context(&mut process, CONTEXT_RECORD, &frame_context).unwrap();
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            MSVC_CPP_EXCEPTION,
            EXCEPTION_NONCONTINUABLE,
            first_pc,
            &[
                MSVC_CPP_MAGIC,
                IMAGE_BASE + OBJECT_RVA,
                IMAGE_BASE + THROW_INFO_RVA,
                IMAGE_BASE,
            ],
        )
        .unwrap();
        process
            .wr32(DISPATCHER_CONTEXT + DISPATCHER_SCOPE_INDEX, 1)
            .unwrap();

        let frames = [ExceptionFrame {
            control_pc: first_pc,
            image_base: IMAGE_BASE,
            function_entry: IMAGE_BASE + 0x500,
            establisher: FRAME_RSP,
            context: frame_context,
        }];
        let unwind = GuestUnwind {
            target_frame: FRAME_RSP,
            target_ip: catch_funclet_ip,
            call_rsp: GUEST_STACK + 0x2000,
            exception_record: EXCEPTION_RECORD,
            return_value: 0,
            context_record: CONTEXT_RECORD,
            history_table: 0,
        };

        complete_guest_unwind(&mut process, &frames, DISPATCHER_CONTEXT, unwind).unwrap();

        assert_eq!(process.rd32(MARKER).unwrap(), 1);
        assert_eq!(process.cpu.rip, continuation);
        assert_eq!(process.cpu.gpr[RSP], FRAME_RSP);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], 0x99);
        assert_eq!(process.read_bytes(FRAME_RSP - 8, 16).unwrap(), [0x69; 16]);
    }

    #[test]
    fn second_pass_resumes_at_the_dispatcher_context_after_collided_unwind() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const GUEST_STACK: u64 = 0x80000;
        const STACK_LIMIT: u64 = GUEST_STACK;
        const STACK_BASE: u64 = GUEST_STACK + 0x10000;
        const ORIGINAL_RSP: u64 = GUEST_STACK + 0x4000;
        const UNWIND_CALL_RSP: u64 = GUEST_STACK + 0x2000;
        const EXCEPTION_RECORD: u64 = GUEST_STACK + 0x2f00;
        const CONTEXT_RECORD: u64 = INFO + 0x200;
        const REDIRECT_CONTEXT: u64 = INFO + 0x800;
        const DISPATCHER_CONTEXT: u64 = INFO + 0x100;
        const SEEN_FLAGS: u64 = INFO + 0x180;
        const FIRST_HANDLER_RVA: u32 = 0x800;
        const TARGET_HANDLER_RVA: u32 = 0x900;

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
        for (entry_rva, begin, end, unwind_rva, handler_rva) in [
            (0x500, 0x100, 0x120, 0x300, FIRST_HANDLER_RVA),
            (0x50c, 0x200, 0x220, 0x320, TARGET_HANDLER_RVA),
        ] {
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
            process.wr32(unwind_info + 4, handler_rva).unwrap();
        }

        let first_pc = IMAGE_BASE + 0x110;
        let target_pc = IMAGE_BASE + 0x210;
        let target_frame = ORIGINAL_RSP + 8;
        let target_function_entry = IMAGE_BASE + 0x50c;
        let mut first_context = context();
        first_context.rip = first_pc;
        first_context.gpr[RSP] = ORIGINAL_RSP;
        process.wr64(ORIGINAL_RSP, target_pc).unwrap();
        let mut target_context = first_context.clone();
        target_context.rip = target_pc;
        target_context.gpr[RSP] = target_frame;
        let mut redirected_context = target_context.clone();
        redirected_context.gpr[RBX] = 0xfeed_beef;
        write_context(&mut process, CONTEXT_RECORD, &first_context).unwrap();
        write_context(&mut process, REDIRECT_CONTEXT, &redirected_context).unwrap();
        write_exception_record(
            &mut process,
            EXCEPTION_RECORD,
            MSVC_CPP_EXCEPTION,
            0,
            first_pc,
            &[MSVC_CPP_MAGIC, 0, 0, IMAGE_BASE],
        )
        .unwrap();

        let first_handler = IMAGE_BASE + FIRST_HANDLER_RVA as u64;
        let target_handler = IMAGE_BASE + TARGET_HANDLER_RVA as u64;
        let mut first_handler_code = vec![0x4c, 0x89, 0xc8]; // mov rax, r9 (DISPATCHER_CONTEXT)
        for (offset, value) in [
            (0x00, target_pc),
            (0x08, IMAGE_BASE),
            (0x10, target_function_entry),
            (0x18, target_frame),
            (0x28, REDIRECT_CONTEXT),
            (0x30, target_handler),
            (0x38, IMAGE_BASE + 0x328), // target UNWIND_INFO handler data
        ] {
            first_handler_code.extend_from_slice(&[0x48, 0xba]); // mov rdx, value
            first_handler_code.extend_from_slice(&value.to_le_bytes());
            first_handler_code.extend_from_slice(&[0x48, 0x89, 0x50, offset]); // mov [rax+offset], rdx
        }
        first_handler_code.extend_from_slice(&[
            0xb8, 3, 0, 0, 0, // ExceptionCollidedUnwind
            0xc3,
        ]);
        process
            .write_bytes(first_handler, &first_handler_code)
            .unwrap();

        let mut target_handler_code = vec![0x8b, 0x41, 0x04]; // mov eax, [rcx + ExceptionFlags]
        target_handler_code.extend_from_slice(&[0x48, 0xba]); // mov rdx, SEEN_FLAGS
        target_handler_code.extend_from_slice(&SEEN_FLAGS.to_le_bytes());
        target_handler_code.extend_from_slice(&[
            0x89, 0x02, // mov [rdx], eax
            0xb8, 1, 0, 0, 0, // ExceptionContinueSearch
            0xc3,
        ]);
        process
            .write_bytes(target_handler, &target_handler_code)
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
                function_entry: target_function_entry,
                establisher: target_frame,
                context: target_context,
            },
        ];
        let unwind = GuestUnwind {
            target_frame,
            target_ip: target_pc,
            call_rsp: UNWIND_CALL_RSP,
            exception_record: EXCEPTION_RECORD,
            return_value: 0x55,
            context_record: CONTEXT_RECORD,
            history_table: 0,
        };

        complete_guest_unwind(&mut process, &frames, DISPATCHER_CONTEXT, unwind).unwrap();

        assert_eq!(process.rd32(SEEN_FLAGS).unwrap(), 0x62);
        assert_eq!(process.cpu.rip, target_pc);
        assert_eq!(process.cpu.gpr[RSP], target_frame);
        assert_eq!(process.cpu.gpr[RBX], 0xfeed_beef);
        assert_eq!(process.cpu.gpr[anywasm_cpu::cpu::RAX], 0x55);
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
