//! Win32 exception records, CONTEXT records, and linked SEH registration chains.

use std::collections::HashSet;

use crate::process::{GuestSehTransfer32, Process, Stop, TEB_ADDR};
use anywasm_cpu::cpu::{Cpu, CpuMode, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP};
use anywasm_cpu::mem::{Access, PROT_X};
use anywasm_cpu::{DecodeError, Exit};

const EXCEPTION_CONTINUE_EXECUTION: u32 = 0;
const EXCEPTION_CONTINUE_SEARCH: u32 = 1;
const EXCEPTION_NESTED_EXCEPTION: u32 = 2;
const EXCEPTION_COLLIDED_UNWIND: u32 = 3;
const EXCEPTION_CONTINUE_EXECUTION_FILTER: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH_FILTER: i32 = 0;
const EXCEPTION_NONCONTINUABLE: u32 = 1;
const EXCEPTION_NESTED_CALL: u32 = 0x10;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_EXIT_UNWIND: u32 = 0x4;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const EXCEPTION_COLLIDED_UNWIND_FLAG: u32 = 0x40;
const EXCEPTION_CHAIN_END: u32 = u32::MAX;
const EH4_TOPMOST_TRY_LEVEL: u32 = u32::MAX - 1;
const EH4_NO_GS_COOKIE: u32 = u32::MAX - 1;
const EH4_REGISTRATION_NODE_SIZE: u64 = 24;
const EH4_SCOPE_TABLE_HEADER_SIZE: u64 = 16;
const EH4_SCOPE_RECORD_SIZE: u64 = 12;
const MAX_EH4_SCOPE_LEVELS: u32 = 65_536;
const CONTEXT32_SIZE: u64 = 0x2cc;
const EXCEPTION_RECORD32_SIZE: u64 = 80;
const EXCEPTION_POINTERS32_SIZE: u64 = 8;
const CONTEXT32_FLAGS: u32 = 0x0001_0007; // CONTEXT_i386 | CONTROL | INTEGER | SEGMENTS
const CONTEXT32_EDI: u64 = 0x9c;
const CONTEXT32_ESI: u64 = 0xa0;
const CONTEXT32_EBX: u64 = 0xa4;
const CONTEXT32_EDX: u64 = 0xa8;
const CONTEXT32_ECX: u64 = 0xac;
const CONTEXT32_EAX: u64 = 0xb0;
const CONTEXT32_EBP: u64 = 0xb4;
const CONTEXT32_EIP: u64 = 0xb8;
const CONTEXT32_EFLAGS: u64 = 0xc0;
const CONTEXT32_ESP: u64 = 0xc4;
const STATUS_ACCESS_VIOLATION: u32 = 0xc000_0005;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xc000_0094;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xc000_001d;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xc000_0096;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const MAX_SEH_FRAMES: usize = 128;

#[derive(Clone)]
struct Context32 {
    gpr: [u64; 16],
    eip: u32,
    eflags: u32,
}

#[derive(Clone, Copy)]
struct SearchFrame32 {
    registration: u32,
    next: u32,
    handler: u32,
}

#[derive(Clone, Copy)]
struct Eh4ScopeRecord32 {
    enclosing_level: u32,
    filter: u32,
    handler: u32,
}

#[derive(Clone, Copy)]
struct Eh4Registration32 {
    node: u64,
    frame_pointer: u64,
    saved_esp: u64,
    scope_table: u64,
    try_level: u32,
}

/// MSVC's x86 EH4 helper used by the image-local `_except_handler4` thunk.
pub(crate) fn except_handler4_common(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: "_except_handler4_common for non-PE32 process".into(),
        });
    }

    let cookie_pointer = p.arg(0) as u32 as u64;
    let cookie_check = p.arg(1) as u32 as u64;
    let exception_record = p.arg(2) as u32 as u64;
    let establisher_frame = p.arg(3) as u32 as u64;
    let context_record = p.arg(4) as u32 as u64;
    if cookie_pointer == 0
        || cookie_check == 0
        || exception_record == 0
        || establisher_frame == 0
        || context_record == 0
    {
        return Err(Stop::Crash(
            "_except_handler4_common received a null required argument".into(),
        ));
    }

    let (image_base, image_end) = eh4_image_bounds(p, cookie_pointer, "security cookie")?;
    eh4_check_image_range(image_base, image_end, cookie_pointer, 4, "security cookie")?;
    eh4_check_executable(p, image_base, image_end, cookie_check, "cookie check")?;

    let registration = eh4_registration32(p, cookie_pointer, establisher_frame)?;
    eh4_check_image_range(
        image_base,
        image_end,
        registration.scope_table,
        EH4_SCOPE_TABLE_HEADER_SIZE,
        "scope table header",
    )?;
    validate_eh4_cookies(
        p,
        cookie_pointer,
        cookie_check,
        registration.scope_table,
        registration.frame_pointer,
    )?;

    let exception_flags = p.rd32(exception_record + 4)?;
    let is_dispatching = exception_flags
        & (EXCEPTION_UNWINDING
            | EXCEPTION_EXIT_UNWIND
            | EXCEPTION_TARGET_UNWIND
            | EXCEPTION_COLLIDED_UNWIND_FLAG)
        == 0;
    if !is_dispatching {
        eh4_local_unwind_to(
            p,
            cookie_pointer,
            cookie_check,
            registration,
            EH4_TOPMOST_TRY_LEVEL,
            true,
        )?;
        validate_eh4_cookies(
            p,
            cookie_pointer,
            cookie_check,
            registration.scope_table,
            registration.frame_pointer,
        )?;
        return Ok(EXCEPTION_CONTINUE_SEARCH as u64);
    }

    let pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS32_SIZE, true);
    p.wr32(pointers, exception_record as u32)?;
    p.wr32(pointers + 4, context_record as u32)?;
    p.wr32(registration.node + 4, pointers as u32)?;

    let mut level = registration.try_level;
    let mut visited = HashSet::new();
    while level != EH4_TOPMOST_TRY_LEVEL {
        if !visited.insert(level) {
            return Err(Stop::Crash(format!(
                "EH4 scope table contains an enclosing-level cycle at {level}"
            )));
        }
        let scope = eh4_scope_record(p, image_base, image_end, registration.scope_table, level)?;
        if scope.filter != 0 {
            eh4_check_executable(p, image_base, image_end, scope.filter as u64, "filter")?;
            let previous_ebp = p.cpu.gpr[RBP];
            p.cpu.gpr[RBP] = registration.frame_pointer;
            let filter_result = p.call_guest(scope.filter as u64, &[]);
            p.cpu.gpr[RBP] = previous_ebp;
            let filter_result = filter_result? as u32 as i32;
            validate_eh4_cookies(
                p,
                cookie_pointer,
                cookie_check,
                registration.scope_table,
                registration.frame_pointer,
            )?;
            if filter_result < 0 {
                if exception_flags & EXCEPTION_NONCONTINUABLE != 0 {
                    return Err(Stop::Crash(format!(
                        "noncontinuable exception {:#x} filter requested continuation",
                        p.rd32(exception_record)?
                    )));
                }
                return Ok(EXCEPTION_CONTINUE_EXECUTION as u64);
            }
            if filter_result > 0 {
                if scope.handler == 0 {
                    return Err(Stop::Crash(format!(
                        "EH4 scope {level} accepted an exception but has no handler"
                    )));
                }
                eh4_check_executable(
                    p,
                    image_base,
                    image_end,
                    scope.handler as u64,
                    "exception handler",
                )?;
                if p.trace_imports {
                    p.stderr.extend_from_slice(
                        format!(
                            "[seh32] EH4 accepted exception={:#x} frame={establisher_frame:#010x} scope={level} handler={:#010x}\n",
                            p.rd32(exception_record)?, scope.handler
                        )
                        .as_bytes(),
                    );
                }
                return Err(Stop::GuestSehTransfer32(GuestSehTransfer32 {
                    target_frame: establisher_frame,
                    target_ip: scope.handler as u64,
                    scope_table: registration.scope_table,
                    cookie_pointer,
                    cookie_check,
                    frame_pointer: registration.frame_pointer,
                    saved_esp: registration.saved_esp,
                    selected_level: level,
                    enclosing_level: scope.enclosing_level,
                }));
            }
        }
        level = scope.enclosing_level;
    }

    Ok(EXCEPTION_CONTINUE_SEARCH as u64)
}

fn eh4_image_bounds(p: &Process, address: u64, what: &str) -> Result<(u64, u64), Stop> {
    p.modules
        .iter()
        .find(|module| {
            module.hle_group.is_none()
                && address >= module.base
                && address < module.base.saturating_add(module.size)
        })
        .map(|module| (module.base, module.base.saturating_add(module.size)))
        .ok_or_else(|| {
            Stop::Crash(format!(
                "EH4 {what} address {address:#x} is outside loaded guest images"
            ))
        })
}

fn eh4_check_image_range(
    image_base: u64,
    image_end: u64,
    address: u64,
    size: u64,
    what: &str,
) -> Result<(), Stop> {
    let end = address
        .checked_add(size)
        .ok_or_else(|| Stop::Crash(format!("EH4 {what} range overflows")))?;
    if address < image_base || end > image_end {
        return Err(Stop::Crash(format!(
            "EH4 {what} {address:#x}..{end:#x} is outside image {image_base:#x}..{image_end:#x}"
        )));
    }
    Ok(())
}

fn eh4_check_executable(
    p: &Process,
    image_base: u64,
    image_end: u64,
    address: u64,
    what: &str,
) -> Result<(), Stop> {
    if address < image_base
        || address >= image_end
        || p.mem
            .prot_of(address)
            .is_none_or(|protection| protection & PROT_X == 0)
    {
        return Err(Stop::Crash(format!(
            "EH4 {what} {address:#x} is not executable in image {image_base:#x}..{image_end:#x}"
        )));
    }
    Ok(())
}

fn eh4_registration32(
    p: &Process,
    cookie_pointer: u64,
    establisher_frame: u64,
) -> Result<Eh4Registration32, Stop> {
    let node = establisher_frame.checked_sub(8).ok_or_else(|| {
        Stop::Crash("EH4 establisher frame precedes its registration node".into())
    })?;
    let stack_base = p.rd32(TEB_ADDR + 4)? as u64;
    let stack_limit = p.rd32(TEB_ADDR + 8)? as u64;
    let node_end = node
        .checked_add(EH4_REGISTRATION_NODE_SIZE)
        .ok_or_else(|| Stop::Crash("EH4 registration node range overflows".into()))?;
    if node < stack_limit || node_end > stack_base {
        return Err(Stop::Crash(format!(
            "EH4 registration node {node:#x}..{node_end:#x} is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }
    let cookie = p.rd32(cookie_pointer)?;
    let encoded_scope_table = p.rd32(node + 16)?;
    let scope_table = (encoded_scope_table ^ cookie) as u64;
    let frame_pointer = node + EH4_REGISTRATION_NODE_SIZE;
    let saved_esp = p.rd32(node)? as u64;
    if saved_esp < stack_limit || saved_esp > stack_base {
        return Err(Stop::Crash(format!(
            "EH4 saved ESP {saved_esp:#x} is outside TEB stack bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }
    Ok(Eh4Registration32 {
        node,
        frame_pointer,
        saved_esp,
        scope_table,
        try_level: p.rd32(node + 20)?,
    })
}

fn eh4_scope_record(
    p: &Process,
    image_base: u64,
    image_end: u64,
    scope_table: u64,
    level: u32,
) -> Result<Eh4ScopeRecord32, Stop> {
    if level >= MAX_EH4_SCOPE_LEVELS {
        return Err(Stop::Crash(format!(
            "EH4 scope level {level} exceeds the supported bound"
        )));
    }
    let table_records = scope_table
        .checked_add(EH4_SCOPE_TABLE_HEADER_SIZE)
        .ok_or_else(|| Stop::Crash("EH4 scope table header overflows".into()))?;
    let record = table_records
        .checked_add(level as u64 * EH4_SCOPE_RECORD_SIZE)
        .ok_or_else(|| Stop::Crash("EH4 scope record address overflows".into()))?;
    eh4_check_image_range(
        image_base,
        image_end,
        record,
        EH4_SCOPE_RECORD_SIZE,
        "scope record",
    )?;
    let enclosing_level = p.rd32(record)?;
    if enclosing_level != EH4_TOPMOST_TRY_LEVEL && enclosing_level >= MAX_EH4_SCOPE_LEVELS {
        return Err(Stop::Crash(format!(
            "EH4 scope {level} has invalid enclosing level {enclosing_level}"
        )));
    }
    Ok(Eh4ScopeRecord32 {
        enclosing_level,
        filter: p.rd32(record + 4)?,
        handler: p.rd32(record + 8)?,
    })
}

fn eh4_cookie_slot(frame_pointer: u64, offset: u32) -> u64 {
    (frame_pointer as u32).wrapping_add(offset) as u64
}

fn validate_eh4_cookies(
    p: &mut Process,
    cookie_pointer: u64,
    cookie_check: u64,
    scope_table: u64,
    frame_pointer: u64,
) -> Result<(), Stop> {
    let global_cookie = p.rd32(cookie_pointer)?;
    let gs_offset = p.rd32(scope_table)?;
    let gs_xor_offset = p.rd32(scope_table + 4)?;
    if gs_offset != EH4_NO_GS_COOKIE {
        let stored = p.rd32(eh4_cookie_slot(frame_pointer, gs_offset))?;
        let expected = stored ^ eh4_cookie_slot(frame_pointer, gs_xor_offset) as u32;
        if expected != global_cookie {
            return Err(Stop::Crash(
                "EH4 local GS cookie validation failed during exception dispatch".into(),
            ));
        }
        call_eh4_cookie_check(p, cookie_check, expected as u64)?;
    }
    let eh_offset = p.rd32(scope_table + 8)?;
    let eh_xor_offset = p.rd32(scope_table + 12)?;
    let stored = p.rd32(eh4_cookie_slot(frame_pointer, eh_offset))?;
    let expected = stored ^ eh4_cookie_slot(frame_pointer, eh_xor_offset) as u32;
    if expected != global_cookie {
        return Err(Stop::Crash(
            "EH4 local exception cookie validation failed during exception dispatch".into(),
        ));
    }
    call_eh4_cookie_check(p, cookie_check, expected as u64)?;
    Ok(())
}

fn call_eh4_cookie_check(p: &mut Process, cookie_check: u64, value: u64) -> Result<(), Stop> {
    let previous_ecx = p.cpu.gpr[RCX];
    p.cpu.gpr[RCX] = value;
    let result = p.call_guest(cookie_check, &[]);
    p.cpu.gpr[RCX] = previous_ecx;
    result.map(|_| ())
}

fn eh4_local_unwind_to(
    p: &mut Process,
    cookie_pointer: u64,
    cookie_check: u64,
    registration: Eh4Registration32,
    target_level: u32,
    abnormal_termination: bool,
) -> Result<(), Stop> {
    let (image_base, image_end) = eh4_image_bounds(p, registration.scope_table, "scope table")?;
    let mut level = p.rd32(registration.node + 20)?;
    let mut visited = HashSet::new();
    while level != target_level {
        if level == EH4_TOPMOST_TRY_LEVEL || !visited.insert(level) {
            return Err(Stop::Crash(format!(
                "EH4 local unwind cannot reach target level {target_level} from {level}"
            )));
        }
        let scope = eh4_scope_record(p, image_base, image_end, registration.scope_table, level)?;
        p.wr32(registration.node + 20, scope.enclosing_level)?;
        if scope.filter == 0 {
            eh4_check_executable(
                p,
                image_base,
                image_end,
                scope.handler as u64,
                "termination handler",
            )?;
            let previous_ebp = p.cpu.gpr[RBP];
            let previous_ecx = p.cpu.gpr[RCX];
            p.cpu.gpr[RBP] = registration.frame_pointer;
            p.cpu.gpr[RCX] = abnormal_termination as u64;
            let result = p.call_guest(scope.handler as u64, &[]);
            p.cpu.gpr[RBP] = previous_ebp;
            p.cpu.gpr[RCX] = previous_ecx;
            result?;
            validate_eh4_cookies(
                p,
                cookie_pointer,
                cookie_check,
                registration.scope_table,
                registration.frame_pointer,
            )?;
        }
        level = scope.enclosing_level;
    }
    Ok(())
}

pub(crate) fn dispatch_raised_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    caller: u64,
) -> Result<u64, Stop> {
    dispatch_raised_exception_with_cleanup(p, code, flags, args, caller, 20)
}

pub(crate) fn dispatch_cxx_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    caller: u64,
) -> Result<u64, Stop> {
    dispatch_raised_exception_with_cleanup(p, code, flags, args, caller, 12)
}

fn dispatch_raised_exception_with_cleanup(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    caller: u64,
    stack_cleanup: u32,
) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Crash(
            "PE32 exception dispatcher called for a non-x86 process".into(),
        ));
    }
    if args.len() > 15 {
        return Err(Stop::Crash(format!(
            "RaiseException received {} parameters (maximum is 15)",
            args.len()
        )));
    }
    let hle_esp = p.cpu.gpr[RSP] as u32 as u64;
    validate_stack(p, hle_esp)?;
    if p.rd32(hle_esp)? != caller as u32 {
        return Err(Stop::Crash(format!(
            "RaiseException caller {caller:#x} does not match guest return slot at {hle_esp:#x}"
        )));
    }
    // The exception source is stdcall: resume after its return address and arguments.
    let context = context_from_cpu(
        &p.cpu,
        caller as u32,
        hle_esp.wrapping_add(stack_cleanup as u64) as u32,
    );
    dispatch_exception(p, code, flags, args, caller as u32, context, true)
}

pub(crate) fn dispatch_cpu_exception(p: &mut Process, exit: &Exit) -> Result<bool, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Ok(false);
    }
    let rsp = p.cpu.gpr[RSP] as u32 as u64;
    if validate_stack(p, rsp).is_err() {
        return Ok(false);
    }
    let (code, args, exception_address) = match exit {
        Exit::Fault(fault) => {
            let access = match fault.access {
                Access::Read => 0,
                Access::Write => 1,
                Access::Exec => 8,
            };
            (
                STATUS_ACCESS_VIOLATION,
                vec![access, fault.addr as u32 as u64],
                p.cpu.rip as u32,
            )
        }
        Exit::DivideError => (STATUS_INTEGER_DIVIDE_BY_ZERO, Vec::new(), p.cpu.rip as u32),
        Exit::Decode(DecodeError::Undefined(_)) | Exit::IllegalInstruction => {
            (STATUS_ILLEGAL_INSTRUCTION, Vec::new(), p.cpu.rip as u32)
        }
        Exit::PrivilegedInstruction => {
            (STATUS_PRIVILEGED_INSTRUCTION, Vec::new(), p.cpu.rip as u32)
        }
        Exit::Breakpoint => (
            STATUS_BREAKPOINT,
            Vec::new(),
            (p.cpu.rip as u32).wrapping_sub(1),
        ),
        _ => return Ok(false),
    };
    let context = context_from_cpu(&p.cpu, p.cpu.rip as u32, rsp as u32);
    dispatch_exception(p, code, 0, &args, exception_address, context, false)?;
    Ok(true)
}

pub(crate) fn unhandled_exception_filter(p: &mut Process) -> Result<u64, Stop> {
    let exception_pointers = p.arg(0) as u32 as u64;
    let exception_record = p.rd32(exception_pointers)? as u64;
    let context_record = p.rd32(exception_pointers + 4)? as u64;
    let flags = p.rd32(exception_record + 4)?;
    let disposition = call_top_level_filter(p, exception_pointers)?;
    if disposition == EXCEPTION_CONTINUE_EXECUTION_FILTER {
        if flags & EXCEPTION_NONCONTINUABLE != 0 {
            return Err(Stop::Crash(format!(
                "noncontinuable exception code={:#x} cannot continue execution",
                p.rd32(exception_record)?
            )));
        }
        dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
        restore_context(p, context_record, false)?;
    }
    Ok(disposition as u32 as u64)
}

fn dispatch_exception(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    exception_address: u32,
    mut context: Context32,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let thread_id = p.current_thread_id;
    let depth = p.guest_exception_depths.entry(thread_id).or_default();
    if *depth >= 32 {
        return Err(Stop::Crash(
            "nested PE32 exception dispatch exceeded 32 levels".into(),
        ));
    }
    *depth += 1;
    let result = dispatch_exception_inner(
        p,
        code,
        flags,
        args,
        exception_address,
        &mut context,
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

fn dispatch_exception_inner(
    p: &mut Process,
    code: u32,
    flags: u32,
    args: &[u64],
    exception_address: u32,
    context: &mut Context32,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    validate_stack(p, context.gpr[RSP] as u32 as u64)?;
    let exception_record = crate::hle::kernel32::alloc_block(p, EXCEPTION_RECORD32_SIZE, true);
    let context_record = crate::hle::kernel32::alloc_block(p, CONTEXT32_SIZE, true);
    write_exception_record(p, exception_record, code, flags, exception_address, args)?;
    write_context(p, context_record, context)?;

    if dispatch_vectored_exception_handlers(p, exception_record, context_record)? {
        return resume_exception(p, exception_record, context_record, skip_ret_on_resume);
    }

    let stack_base = p.rd32(TEB_ADDR + 4)? as u64;
    let stack_limit = p.rd32(TEB_ADDR + 8)? as u64;
    if stack_limit >= stack_base {
        return Err(Stop::Crash(format!(
            "invalid PE32 TEB stack bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }
    let mut registration = p.rd32(TEB_ADDR)?;
    let mut visited = HashSet::new();
    let mut searched_frames = Vec::new();
    for frame_number in 0..MAX_SEH_FRAMES {
        if registration == EXCEPTION_CHAIN_END {
            return unhandled_exception(
                p,
                exception_record,
                context_record,
                skip_ret_on_resume,
                format!("search exhausted after {frame_number} registration frames"),
            );
        }
        let frame = registration as u64;
        if frame < stack_limit || frame.checked_add(8).is_none_or(|end| end > stack_base) {
            return Err(Stop::Crash(format!(
                "PE32 exception registration frame {frame:#x} lies outside TEB stack bounds"
            )));
        }
        if !visited.insert(registration) {
            return Err(Stop::Crash(format!(
                "PE32 exception registration chain contains a cycle at {frame:#x}"
            )));
        }
        let next = p.rd32(frame)?;
        let handler = p.rd32(frame + 4)?;
        if handler == 0 {
            return Err(Stop::Crash(format!(
                "PE32 exception registration frame {frame:#x} has a null handler"
            )));
        }
        searched_frames.push(SearchFrame32 {
            registration,
            next,
            handler,
        });
        let disposition = match p.call_guest(
            handler as u64,
            &[
                exception_record,
                frame,
                context_record,
                0, // The x86 dispatcher does not expose the x64 UNWIND_HISTORY_TABLE context.
            ],
        ) {
            Ok(disposition) => disposition as u32,
            Err(Stop::GuestCatch32(catch)) => {
                return unwind_to_catch32(
                    p,
                    catch,
                    &searched_frames,
                    exception_record,
                    context_record,
                    flags,
                    skip_ret_on_resume,
                );
            }
            Err(Stop::GuestSehTransfer32(transfer)) => {
                return unwind_to_seh_transfer32(
                    p,
                    transfer,
                    &searched_frames,
                    exception_record,
                    context_record,
                    flags,
                    skip_ret_on_resume,
                );
            }
            Err(stop) => return Err(stop),
        };
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh32] frame={frame:#010x} handler={handler:#010x} disposition={disposition}\n"
                )
                .as_bytes(),
            );
        }
        match disposition {
            EXCEPTION_CONTINUE_EXECUTION => {
                if flags & EXCEPTION_NONCONTINUABLE != 0 {
                    return Err(Stop::Crash(format!(
                        "noncontinuable exception code={code:#x} cannot continue execution"
                    )));
                }
                return resume_exception(p, exception_record, context_record, skip_ret_on_resume);
            }
            EXCEPTION_CONTINUE_SEARCH => registration = next,
            EXCEPTION_NESTED_EXCEPTION => {
                let exception_flags = p.rd32(exception_record + 4)?;
                p.wr32(
                    exception_record + 4,
                    exception_flags | EXCEPTION_NESTED_CALL,
                )?;
                registration = next;
            }
            EXCEPTION_COLLIDED_UNWIND => {
                return Err(Stop::Unimplemented {
                    dll: "ntdll".into(),
                    name: format!("PE32 exception disposition {disposition}"),
                });
            }
            value => {
                return Err(Stop::Crash(format!(
                    "PE32 exception handler {handler:#x} returned invalid disposition {value}"
                )));
            }
        }
    }
    unhandled_exception(
        p,
        exception_record,
        context_record,
        skip_ret_on_resume,
        format!("search exceeded {MAX_SEH_FRAMES} registration frames"),
    )
}

fn unwind_to_catch32(
    p: &mut Process,
    catch: crate::process::GuestCatch32,
    searched_frames: &[SearchFrame32],
    exception_record: u64,
    context_record: u64,
    original_flags: u32,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let target_index = searched_frames
        .iter()
        .position(|frame| frame.registration as u64 == catch.target_frame)
        .ok_or_else(|| {
            Stop::Crash(format!(
                "PE32 C++ handler selected registration {:#x} outside the searched chain",
                catch.target_frame
            ))
        })?;
    let target = searched_frames[target_index];
    let transfer = exception_record + 32;
    for (offset, value) in [
        0x4332_5846,
        target.registration,
        catch.target_ip as u32,
        catch.target_state as u32,
        catch.catch_state as u32,
        catch.try_index as u32,
        catch.handler_index as u32,
    ]
    .into_iter()
    .enumerate()
    {
        p.wr32(transfer + offset as u64 * 4, value)?;
    }

    for (index, frame) in searched_frames[..=target_index].iter().enumerate() {
        let target_unwind = index == target_index;
        p.wr32(
            exception_record + 4,
            original_flags
                | EXCEPTION_UNWINDING
                | if target_unwind {
                    EXCEPTION_TARGET_UNWIND
                } else {
                    0
                },
        )?;
        let frame_address = frame.registration as u64;
        if p.rd32(frame_address)? != frame.next || p.rd32(frame_address + 4)? != frame.handler {
            return Err(Stop::Crash(format!(
                "PE32 registration frame {frame_address:#x} changed during exception search"
            )));
        }
        let previous_ebp = p.cpu.gpr[RBP];
        p.cpu.gpr[RBP] = frame_address + 12;
        let result = p.call_guest(
            frame.handler as u64,
            &[
                exception_record,
                frame_address,
                context_record,
                0, // The x86 dispatcher does not expose an UNWIND_HISTORY_TABLE.
            ],
        );
        p.cpu.gpr[RBP] = previous_ebp;
        let disposition = result? as u32;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh32] unwind frame={frame_address:#010x} handler={:#010x} target={target_unwind} disposition={disposition}\n",
                    frame.handler
                )
                .as_bytes(),
            );
        }
        if disposition != EXCEPTION_CONTINUE_SEARCH {
            return Err(Stop::Unimplemented {
                dll: "ntdll".into(),
                name: format!("PE32 unwind disposition {disposition}"),
            });
        }
        let expected_head = frame.registration;
        if p.rd32(TEB_ADDR)? != expected_head {
            return Err(Stop::Crash(format!(
                "PE32 ExceptionList changed while unwinding frame {frame_address:#x}"
            )));
        }
        if !target_unwind {
            p.wr32(TEB_ADDR, frame.next)?;
        }
    }

    if p.rd32(TEB_ADDR)? != target.registration {
        return Err(Stop::Crash(format!(
            "PE32 catch target {:#x} is not the active registration after unwind",
            target.registration
        )));
    }
    // Clear the private transfer payload before user catch code or a continue handler can see it.
    for offset in 0..28 {
        p.wr8(transfer + offset, 0)?;
    }
    p.wr32(exception_record + 4, original_flags)?;
    crate::exception32::resume_cxx_catch(p, exception_record, context_record, skip_ret_on_resume)
}

fn unwind_to_seh_transfer32(
    p: &mut Process,
    transfer: GuestSehTransfer32,
    searched_frames: &[SearchFrame32],
    exception_record: u64,
    context_record: u64,
    original_flags: u32,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let target_index = searched_frames
        .iter()
        .position(|frame| frame.registration as u64 == transfer.target_frame)
        .ok_or_else(|| {
            Stop::Crash(format!(
                "PE32 SEH handler selected registration {:#x} outside the searched chain",
                transfer.target_frame
            ))
        })?;
    let target = searched_frames[target_index];

    // RtlUnwind removes registrations above the target without calling the
    // target's language handler. EH4's local unwind then handles nested scopes
    // in that target frame before control enters the selected __except block.
    for frame in &searched_frames[..target_index] {
        p.wr32(exception_record + 4, original_flags | EXCEPTION_UNWINDING)?;
        let frame_address = frame.registration as u64;
        if p.rd32(frame_address)? != frame.next || p.rd32(frame_address + 4)? != frame.handler {
            return Err(Stop::Crash(format!(
                "PE32 registration frame {frame_address:#x} changed during SEH unwind"
            )));
        }
        let previous_ebp = p.cpu.gpr[RBP];
        p.cpu.gpr[RBP] = frame_address + 12;
        let result = p.call_guest(
            frame.handler as u64,
            &[
                exception_record,
                frame_address,
                context_record,
                0, // The x86 dispatcher does not expose an UNWIND_HISTORY_TABLE.
            ],
        );
        p.cpu.gpr[RBP] = previous_ebp;
        let disposition = result? as u32;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh32] unwind frame={frame_address:#010x} handler={:#010x} target=false disposition={disposition}\n",
                    frame.handler
                )
                .as_bytes(),
            );
        }
        if disposition != EXCEPTION_CONTINUE_SEARCH {
            return Err(Stop::Unimplemented {
                dll: "ntdll".into(),
                name: format!("PE32 unwind disposition {disposition}"),
            });
        }
        if p.rd32(TEB_ADDR)? != frame.registration {
            return Err(Stop::Crash(format!(
                "PE32 ExceptionList changed while unwinding frame {frame_address:#x}"
            )));
        }
        p.wr32(TEB_ADDR, frame.next)?;
    }

    if p.rd32(TEB_ADDR)? != target.registration {
        return Err(Stop::Crash(format!(
            "PE32 SEH target {:#x} is not the active registration after unwind",
            target.registration
        )));
    }
    if p.rd32(target.registration as u64)? != target.next
        || p.rd32(target.registration as u64 + 4)? != target.handler
    {
        return Err(Stop::Crash(format!(
            "PE32 SEH target frame {:#x} changed during unwind",
            target.registration
        )));
    }
    let target_node = transfer.target_frame.checked_sub(8).ok_or_else(|| {
        Stop::Crash("PE32 EH4 target frame precedes its registration node".into())
    })?;
    let registration = Eh4Registration32 {
        node: target_node,
        frame_pointer: transfer.frame_pointer,
        saved_esp: transfer.saved_esp,
        scope_table: transfer.scope_table,
        try_level: p.rd32(target_node + 20)?,
    };
    p.wr32(
        exception_record + 4,
        original_flags | EXCEPTION_UNWINDING | EXCEPTION_TARGET_UNWIND,
    )?;
    validate_eh4_cookies(
        p,
        transfer.cookie_pointer,
        transfer.cookie_check,
        transfer.scope_table,
        transfer.frame_pointer,
    )?;
    eh4_local_unwind_to(
        p,
        transfer.cookie_pointer,
        transfer.cookie_check,
        registration,
        transfer.selected_level,
        true,
    )?;
    p.wr32(registration.node + 20, transfer.enclosing_level)?;
    validate_eh4_cookies(
        p,
        transfer.cookie_pointer,
        transfer.cookie_check,
        transfer.scope_table,
        transfer.frame_pointer,
    )?;

    let mut context = read_context(p, context_record)?;
    context.gpr[RSP] = transfer.saved_esp;
    context.gpr[RBP] = transfer.frame_pointer;
    context.eip = transfer.target_ip as u32;
    write_context(p, context_record, &context)?;
    p.wr32(exception_record + 4, original_flags)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[seh32] transfer frame={:#010x} ip={:#010x} esp={:#010x} scope_level={} enclosing={}\n",
                transfer.target_frame,
                transfer.target_ip,
                transfer.saved_esp,
                transfer.selected_level,
                transfer.enclosing_level
            )
            .as_bytes(),
        );
    }
    resume_exception(p, exception_record, context_record, skip_ret_on_resume)
}

fn unhandled_exception(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
    skip_ret_on_resume: bool,
    reason: String,
) -> Result<u64, Stop> {
    let pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS32_SIZE, true);
    p.wr32(pointers, exception_record as u32)?;
    p.wr32(pointers + 4, context_record as u32)?;
    let disposition = call_top_level_filter(p, pointers)?;
    if disposition == EXCEPTION_CONTINUE_EXECUTION_FILTER {
        let flags = p.rd32(exception_record + 4)?;
        if flags & EXCEPTION_NONCONTINUABLE != 0 {
            return Err(Stop::Crash(format!(
                "noncontinuable exception code={:#x} cannot continue execution",
                p.rd32(exception_record)?
            )));
        }
        dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
        return resume_exception(p, exception_record, context_record, skip_ret_on_resume);
    }
    let code = p.rd32(exception_record)?;
    let flags = p.rd32(exception_record + 4)?;
    let exception_address = p.rd32(exception_record + 12)?;
    let details = if code == STATUS_ACCESS_VIOLATION && p.rd32(exception_record + 16)? >= 2 {
        let esp = p.rd32(context_record + CONTEXT32_ESP)? as u64;
        let stack = (0..12)
            .map(|index| p.rd32(esp + index * 4).map(|word| format!("{word:08x}")))
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        format!(
            " eip={:#x} access={} fault={:#x} eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} ebp={:#x} esp={:#x} stack=[{}]",
            p.rd32(context_record + CONTEXT32_EIP)?,
            p.rd32(exception_record + 20)?,
            p.rd32(exception_record + 24)?,
            p.rd32(context_record + CONTEXT32_EAX)?,
            p.rd32(context_record + CONTEXT32_EBX)?,
            p.rd32(context_record + CONTEXT32_ECX)?,
            p.rd32(context_record + CONTEXT32_EDX)?,
            p.rd32(context_record + CONTEXT32_ESI)?,
            p.rd32(context_record + CONTEXT32_EDI)?,
            p.rd32(context_record + CONTEXT32_EBP)?,
            esp,
            stack,
        )
    } else {
        format!(" eip={:#x}", p.rd32(context_record + CONTEXT32_EIP)?)
    };
    Err(Stop::Crash(format!(
        "unhandled PE32 exception code={code:#x} flags={flags:#x} address={exception_address:#x}:{details}: {reason}"
    )))
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
    let pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS32_SIZE, true);
    p.wr32(pointers, exception_record as u32)?;
    p.wr32(pointers + 4, context_record as u32)?;
    for (handle, callback) in handlers {
        let disposition = p.call_guest(callback, &[pointers])? as i32;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh32] vectored handler handle={handle:#x} callback={callback:#x} disposition={disposition}\n"
                )
                .as_bytes(),
            );
        }
        match disposition {
            EXCEPTION_CONTINUE_EXECUTION_FILTER => {
                if p.rd32(exception_record + 4)? & EXCEPTION_NONCONTINUABLE != 0 {
                    return Err(Stop::Crash(format!(
                        "noncontinuable exception code={:#x} reached a continue-execution VEH",
                        p.rd32(exception_record)?
                    )));
                }
                return Ok(true);
            }
            EXCEPTION_CONTINUE_SEARCH_FILTER => {}
            value => {
                return Err(Stop::Unimplemented {
                    dll: "kernel32".into(),
                    name: format!("PE32 vectored exception disposition {value}"),
                });
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
    let pointers = crate::hle::kernel32::alloc_block(p, EXCEPTION_POINTERS32_SIZE, true);
    p.wr32(pointers, exception_record as u32)?;
    p.wr32(pointers + 4, context_record as u32)?;
    for (handle, callback) in handlers {
        let _ = p.call_guest(callback, &[pointers])?;
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!(
                    "[seh32] vectored continue handler handle={handle:#x} callback={callback:#x}\n"
                )
                .as_bytes(),
            );
        }
    }
    Ok(())
}

fn call_top_level_filter(p: &mut Process, pointers: u64) -> Result<i32, Stop> {
    if p.top_level_exception_filter == 0 {
        return Ok(EXCEPTION_CONTINUE_SEARCH_FILTER);
    }
    Ok(p.call_guest(p.top_level_exception_filter, &[pointers])? as i32)
}

fn resume_exception(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    dispatch_vectored_continue_handlers(p, exception_record, context_record)?;
    restore_context(p, context_record, skip_ret_on_resume)
}

pub(crate) fn resume_cxx_catch(
    p: &mut Process,
    exception_record: u64,
    context_record: u64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    resume_exception(p, exception_record, context_record, skip_ret_on_resume)
}

fn restore_context(
    p: &mut Process,
    context_record: u64,
    skip_ret_on_resume: bool,
) -> Result<u64, Stop> {
    let resumed = read_context(p, context_record)?;
    p.cpu.gpr = resumed.gpr;
    p.cpu.rip = resumed.eip as u64;
    p.cpu.rflags = resumed.eflags as u64;
    p.skip_ret = skip_ret_on_resume;
    Ok(0)
}

fn validate_stack(p: &Process, rsp: u64) -> Result<(), Stop> {
    let stack_base = p.rd32(TEB_ADDR + 4)? as u64;
    let stack_limit = p.rd32(TEB_ADDR + 8)? as u64;
    if stack_limit >= stack_base || rsp < stack_limit || rsp >= stack_base {
        return Err(Stop::Crash(format!(
            "PE32 exception stack pointer {rsp:#x} is outside TEB bounds {stack_limit:#x}..{stack_base:#x}"
        )));
    }
    Ok(())
}

fn context_from_cpu(cpu: &Cpu, eip: u32, esp: u32) -> Context32 {
    let mut gpr = cpu.gpr;
    gpr[RSP] = esp as u64;
    Context32 {
        gpr,
        eip,
        eflags: cpu.rflags as u32,
    }
}

fn write_exception_record(
    p: &mut Process,
    address: u64,
    code: u32,
    flags: u32,
    exception_address: u32,
    args: &[u64],
) -> Result<(), Stop> {
    p.wr32(address, code)?;
    p.wr32(address + 4, flags)?;
    p.wr32(address + 8, 0)?;
    p.wr32(address + 12, exception_address)?;
    p.wr32(address + 16, args.len().min(15) as u32)?;
    for (index, argument) in args.iter().take(15).enumerate() {
        p.wr32(address + 20 + index as u64 * 4, *argument as u32)?;
    }
    Ok(())
}

fn write_context(p: &mut Process, address: u64, context: &Context32) -> Result<(), Stop> {
    p.wr32(address, CONTEXT32_FLAGS)?;
    for (offset, register) in [
        (CONTEXT32_EDI, RDI),
        (CONTEXT32_ESI, RSI),
        (CONTEXT32_EBX, RBX),
        (CONTEXT32_EDX, RDX),
        (CONTEXT32_ECX, RCX),
        (CONTEXT32_EAX, RAX),
        (CONTEXT32_EBP, RBP),
    ] {
        p.wr32(address + offset, context.gpr[register] as u32)?;
    }
    p.wr32(address + CONTEXT32_EIP, context.eip)?;
    p.wr32(address + CONTEXT32_EFLAGS, context.eflags)?;
    p.wr32(address + CONTEXT32_ESP, context.gpr[RSP] as u32)?;
    // The guest uses flat x86 segments. CONTEXT_FULL excludes the floating-point and extended
    // register areas; the emulator keeps those states live while guest callbacks run.
    for (offset, selector) in [
        (0x8c, 0x23),
        (0x90, 0x3b),
        (0x94, 0x23),
        (0x98, 0x23),
        (0xc8, 0x23),
    ] {
        p.wr32(address + offset, selector)?;
    }
    p.wr32(address + 0xbc, 0x1b)?; // SegCs
    Ok(())
}

fn read_context(p: &Process, address: u64) -> Result<Context32, Stop> {
    let mut gpr = p.cpu.gpr;
    for (offset, register) in [
        (CONTEXT32_EDI, RDI),
        (CONTEXT32_ESI, RSI),
        (CONTEXT32_EBX, RBX),
        (CONTEXT32_EDX, RDX),
        (CONTEXT32_ECX, RCX),
        (CONTEXT32_EAX, RAX),
        (CONTEXT32_EBP, RBP),
        (CONTEXT32_ESP, RSP),
    ] {
        gpr[register] = p.rd32(address + offset)? as u64;
    }
    Ok(Context32 {
        gpr,
        eip: p.rd32(address + CONTEXT32_EIP)?,
        eflags: p.rd32(address + CONTEXT32_EFLAGS)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::TEB_ADDR;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    const STACK: u64 = 0x70000;
    const CODE: u64 = 0x50000;
    const FRAME: u64 = STACK + 0x100;
    const RAISE_RSP: u64 = STACK + 0x400;
    const MARKER: u64 = 0x30000;
    const LANDING: u64 = CODE + 0x80;
    const HANDLER: u64 = CODE + 0x100;

    fn process_with_frame_handler() -> Process {
        let mut process = Process::new(Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.fs_base = TEB_ADDR;
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(STACK, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(CODE, PAGE_SIZE, PROT_RWX));
        assert!(process.mem.map(MARKER, PAGE_SIZE, PROT_RW));
        process.wr32(TEB_ADDR, FRAME as u32).unwrap();
        process
            .wr32(TEB_ADDR + 4, (STACK + PAGE_SIZE) as u32)
            .unwrap();
        process.wr32(TEB_ADDR + 8, STACK as u32).unwrap();
        process.wr32(FRAME, u32::MAX).unwrap();
        process.wr32(FRAME + 4, HANDLER as u32).unwrap();

        // __EXCEPTION_ROUTINE(record, frame, context, dispatcher) stores the code,
        // redirects CONTEXT.Eip, then returns ExceptionContinueExecution.
        let mut handler = vec![
            0x8b, 0x44, 0x24, 0x04, // mov eax, [esp + 4] -- EXCEPTION_RECORD
            0x8b, 0x10, // mov edx, [eax] -- ExceptionCode
            0xb9, // mov ecx, MARKER
        ];
        handler.extend_from_slice(&(MARKER as u32).to_le_bytes());
        handler.extend_from_slice(&[
            0x89, 0x11, // mov [ecx], edx
            0x8b, 0x50, 0x14, // mov edx, [eax + 20] -- ExceptionInformation[0]
            0xb9, // mov ecx, MARKER + 4
        ]);
        handler.extend_from_slice(&((MARKER + 4) as u32).to_le_bytes());
        handler.extend_from_slice(&[
            0x89, 0x11, // mov [ecx], edx
            0x8b, 0x44, 0x24, 0x0c, // mov eax, [esp + 12] -- CONTEXT
            0xba, // mov edx, LANDING
        ]);
        handler.extend_from_slice(&(LANDING as u32).to_le_bytes());
        handler.extend_from_slice(&[
            0x89, 0x90, 0xb8, 0x00, 0x00, 0x00, // mov [eax + CONTEXT.Eip], edx
            0x31, 0xc0, // xor eax, eax -- ExceptionContinueExecution
            0xc2, 0x10, 0x00, // ret 16
        ]);
        process.write_bytes(HANDLER, &handler).unwrap();

        const CALLER: u64 = CODE + 0x20;
        process.wr32(RAISE_RSP, CALLER as u32).unwrap();
        process.cpu.gpr[RSP] = RAISE_RSP;
        process.cpu.rip = CALLER;
        process
    }

    #[test]
    fn raised_exception_runs_x86_registration_handler_and_resumes_context() {
        const CODE_VALUE: u32 = 0xe123_4567;
        const CALLER: u64 = CODE + 0x20;
        let mut process = process_with_frame_handler();

        let result = dispatch_raised_exception(&mut process, CODE_VALUE, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(MARKER).unwrap(), CODE_VALUE);
        assert_eq!(process.cpu.rip, LANDING);
        assert_eq!(process.cpu.gpr[RSP], RAISE_RSP + 20);
        assert_eq!(process.rd32(MARKER + 4).unwrap(), 0);
        assert!(process.skip_ret);
    }

    #[test]
    fn nested_exception_disposition_marks_the_record_and_continues_search() {
        const OUTER_FRAME: u64 = STACK + 0x180;
        const OUTER_HANDLER: u64 = CODE + 0x180;
        const CALLER: u64 = CODE + 0x20;
        let mut process = process_with_frame_handler();
        process.wr32(FRAME, OUTER_FRAME as u32).unwrap();
        process.wr32(FRAME + 4, HANDLER as u32).unwrap();
        process.wr32(OUTER_FRAME, u32::MAX).unwrap();
        process
            .wr32(OUTER_FRAME + 4, OUTER_HANDLER as u32)
            .unwrap();
        process
            .write_bytes(HANDLER, &[0xb8, 2, 0, 0, 0, 0xc2, 0x10, 0x00])
            .unwrap(); // return ExceptionNestedException

        let mut outer_handler = vec![
            0x8b, 0x44, 0x24, 0x04, // mov eax, [esp + 4] -- EXCEPTION_RECORD
            0x8b, 0x50, 0x04, // mov edx, [eax + 4] -- ExceptionFlags
            0xb9, // mov ecx, MARKER
        ];
        outer_handler.extend_from_slice(&(MARKER as u32).to_le_bytes());
        outer_handler.extend_from_slice(&[
            0x89, 0x11, // mov [ecx], edx
            0x8b, 0x44, 0x24, 0x0c, // mov eax, [esp + 12] -- CONTEXT
            0xba, // mov edx, LANDING
        ]);
        outer_handler.extend_from_slice(&(LANDING as u32).to_le_bytes());
        outer_handler.extend_from_slice(&[
            0x89, 0x90, 0xb8, 0, 0, 0, // mov [eax + CONTEXT.Eip], edx
            0x31, 0xc0, // xor eax, eax -- ExceptionContinueExecution
            0xc2, 0x10, 0x00, // ret 16
        ]);
        process
            .write_bytes(OUTER_HANDLER, &outer_handler)
            .unwrap();

        let result = dispatch_raised_exception(&mut process, 0xe123_4567, 0, &[], CALLER);

        assert_eq!(result, Ok(0));
        assert_eq!(process.rd32(MARKER).unwrap(), 0x10);
        assert_eq!(process.cpu.rip, LANDING);
    }

    #[test]
    fn x86_cpu_instruction_exceptions_reach_veh_and_resume_the_guest() {
        use crate::process::EXIT_MAGIC;

        const LANDING_RETURN: u64 = CODE + 0x80;
        let cases: &[(&[u8], u32)] = &[
            (&[0x0f, 0x0b], STATUS_ILLEGAL_INSTRUCTION), // UD2
            (&[0x0f, 0x0c], STATUS_ILLEGAL_INSTRUCTION), // reserved encoding
            (&[0xfa], STATUS_PRIVILEGED_INSTRUCTION),    // CLI
            (&[0xfb], STATUS_PRIVILEGED_INSTRUCTION),    // STI
            (&[0x31, 0xc0, 0xf7, 0xf0], STATUS_INTEGER_DIVIDE_BY_ZERO), // DIV EAX, EAX
            (&[0xcc], STATUS_BREAKPOINT),                // INT3
        ];

        for (instruction, expected_code) in cases {
            let mut process = Process::new(Vfs::new());
            process.cpu.mode = CpuMode::X86_32;
            process.cpu.fs_base = TEB_ADDR;
            assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
            assert!(process.mem.map(STACK, PAGE_SIZE, PROT_RW));
            assert!(process.mem.map(CODE, PAGE_SIZE, PROT_RWX));
            assert!(process.mem.map(MARKER, PAGE_SIZE, PROT_RW));
            process.wr32(TEB_ADDR, u32::MAX).unwrap();
            process
                .wr32(TEB_ADDR + 4, (STACK + PAGE_SIZE) as u32)
                .unwrap();
            process.wr32(TEB_ADDR + 8, STACK as u32).unwrap();
            process.write_bytes(CODE, instruction).unwrap();
            process
                .write_bytes(LANDING_RETURN, &[0xb8, 9, 0, 0, 0, 0xc3]) // mov eax, 9; ret
                .unwrap();

            let mut handler = vec![
                0x8b, 0x44, 0x24, 0x04, // mov eax, [esp + 4] -- EXCEPTION_POINTERS
                0x8b, 0x08, // mov ecx, [eax] -- EXCEPTION_RECORD
                0x8b, 0x11, // mov edx, [ecx] -- ExceptionCode
                0xbb, // mov ebx, MARKER
            ];
            handler.extend_from_slice(&(MARKER as u32).to_le_bytes());
            handler.extend_from_slice(&[
                0x89, 0x13, // mov [ebx], edx
                0x8b, 0x40, 0x04, // mov eax, [eax + 4] -- CONTEXT
                0xba, // mov edx, landing
            ]);
            handler.extend_from_slice(&(LANDING_RETURN as u32).to_le_bytes());
            handler.extend_from_slice(&[
                0x89, 0x90, 0xb8, 0, 0, 0, // mov [eax + CONTEXT.Eip], edx
                0xb8, 0xff, 0xff, 0xff, 0xff, // return EXCEPTION_CONTINUE_EXECUTION
                0xc2, 0x04, 0x00, // ret 4
            ]);
            process.write_bytes(HANDLER, &handler).unwrap();
            process
                .vectored_exception_handlers
                .push((0x7000_0000, HANDLER));

            let initial_esp = STACK + PAGE_SIZE - 4;
            process.wr32(initial_esp, EXIT_MAGIC as u32).unwrap();
            process.cpu.gpr[RSP] = initial_esp;
            process.cpu.rip = CODE;

            assert_eq!(process.run(100), Stop::Exit(9));
            assert_eq!(process.rd32(MARKER).unwrap(), *expected_code);
            assert!(!process.skip_ret);
        }
    }

    #[test]
    fn raise_exception_import_reads_x86_arguments_and_cleans_its_stdcall_frame() {
        let mut process = process_with_frame_handler();
        const ARGUMENTS: u64 = 0x40000;
        assert!(process.mem.map(ARGUMENTS, PAGE_SIZE, PROT_RW));
        process.wr32(ARGUMENTS, 0x1234_5678).unwrap();
        process.wr32(RAISE_RSP + 4, 0xe123_4567).unwrap();
        process.wr32(RAISE_RSP + 8, 0).unwrap();
        process.wr32(RAISE_RSP + 12, 1).unwrap();
        process.wr32(RAISE_RSP + 16, ARGUMENTS as u32).unwrap();
        process
            .write_bytes(LANDING, &[0xb8, 9, 0, 0, 0, 0xc3])
            .unwrap();
        process
            .wr32(RAISE_RSP + 20, crate::process::EXIT_MAGIC as u32)
            .unwrap();
        let thunk = process
            .thunk_for("kernel32", "RaiseException", true)
            .unwrap();
        process.cpu.rip = thunk;

        assert_eq!(process.run(100), Stop::Exit(9));
        assert_eq!(process.rd32(MARKER).unwrap(), 0xe123_4567);
        assert_eq!(process.rd32(MARKER + 4).unwrap(), 0x1234_5678);
        assert!(!process.skip_ret);
    }
}
