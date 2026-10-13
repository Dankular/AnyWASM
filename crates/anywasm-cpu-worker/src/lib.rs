#![cfg_attr(target_arch = "wasm32", no_std)]

use core::mem::size_of;
use core::slice;
use core::sync::atomic::Ordering;

use anywasm_cpu_state::exec_core::{self, Exit};
use anywasm_cpu_state::guest_memory::SharedGuestMemory;
use anywasm_cpu_state::shared::{Completion, CompletionState, SharedHeader, WorkerSlot};
use anywasm_cpu_state::Cpu;

pub const STOP_QUANTUM: u32 = 0;
pub const STOP_HLE: u32 = 1;
pub const STOP_ACCESS_VIOLATION: u32 = 2;
pub const STOP_DIVIDE_ERROR: u32 = 3;
pub const STOP_BREAKPOINT: u32 = 4;
pub const STOP_ILLEGAL_INSTRUCTION: u32 = 5;
pub const STOP_PRIVILEGED_INSTRUCTION: u32 = 6;
pub const STOP_SYSCALL: u32 = 7;
pub const STOP_PROCESS_EXIT: u32 = 8;
pub const STOP_THREAD_EXIT: u32 = 9;
pub const STOP_SOFTWARE_INTERRUPT_BASE: u32 = 0x100;
/// Retry this instruction on the coordinator's full interpreter.
pub const STOP_UNSUPPORTED: u32 = 0x200;
/// Coordinator fallback because the shared-memory backend cannot atomically access this alignment.
pub const STOP_UNSUPPORTED_ATOMIC_ALIGNMENT: u32 = 0x201;
/// Coordinator fallback because the shared-memory backend cannot atomically access this width.
pub const STOP_UNSUPPORTED_ATOMIC_WIDTH: u32 = 0x202;

const ABI_FAILURE: u32 = 2;
const DISPATCH_FAILURE: u32 = 3;
const WORKER_FAILURE: u32 = 4;

/// Claim and execute one bounded guest CPU slice from the shared ABI.
///
/// `shared_base` points to the ABI header, not necessarily to byte zero of the imported
/// WebAssembly memory. The coordinator reserves that aligned region after all worker modules'
/// initialized data and stacks. `trap_start..trap_end` is the coordinator's HLE thunk range.
/// The worker stops before executing any address in that range. `process_exit` and `thread_exit`
/// identify terminal addresses in this range; zero disables recognition for standalone users.
///
/// # Safety
///
/// `shared_base` must refer to a live shared Wasm memory region containing a valid ABI header,
/// slot array, CPU contexts, page map, and guest frames. The host must assign this instance a
/// disjoint Wasm stack and ensure no coordinator or other worker accesses the selected context
/// until the slot publishes completion.
#[no_mangle]
pub unsafe extern "C" fn aw_cpu_worker_slice(
    shared_base: u32,
    available_bytes: u32,
    slot_index: u32,
    trap_start: u64,
    trap_end: u64,
    process_exit: u64,
    thread_exit: u64,
) -> u32 {
    if shared_base == 0
        || trap_start >= trap_end
        || (process_exit != 0 && !(trap_start..trap_end).contains(&process_exit))
        || (thread_exit != 0 && !(trap_start..trap_end).contains(&thread_exit))
        || (process_exit != 0 && process_exit == thread_exit)
    {
        return ABI_FAILURE;
    }
    let base = shared_base as usize as *mut u8;
    if (base as usize) % core::mem::align_of::<SharedHeader>() != 0 {
        return ABI_FAILURE;
    }
    let header = &*base.cast::<SharedHeader>();
    if header.validate(available_bytes).is_err() {
        return ABI_FAILURE;
    }
    let Ok(slot_offset) = header.worker_slot_offset(slot_index) else {
        return ABI_FAILURE;
    };
    let slot = &*base.add(slot_offset as usize).cast::<WorkerSlot>();
    let Some(claim) = slot.try_claim() else {
        return 0;
    };
    let dispatch = claim.dispatch;
    if header
        .validate_context_offset(dispatch.context_offset)
        .is_err()
        || dispatch.mapping_generation != header.mapping_generation.load(Ordering::Acquire)
        || dispatch.code_generation != header.code_generation.load(Ordering::Acquire)
        || dispatch.dispatch_generation != header.dispatch_generation.load(Ordering::Acquire)
    {
        let completed = slot.complete(Completion {
            sequence: claim.sequence,
            state: CompletionState::Failed,
            retired_instructions: 0,
            stop_reason: DISPATCH_FAILURE,
            payload_address: 0,
            payload_value: 0,
        });
        return if completed {
            DISPATCH_FAILURE
        } else {
            WORKER_FAILURE
        };
    }

    let map_count = header.page_map_count.load(Ordering::Acquire) as usize;
    let mapping_bytes = map_count.checked_mul(size_of::<anywasm_cpu_state::shared::MappingEntry>());
    let Some(mapping_bytes) = mapping_bytes else {
        return fail_claim(slot, claim.sequence, ABI_FAILURE);
    };
    if mapping_bytes > header.page_map.length as usize {
        return fail_claim(slot, claim.sequence, ABI_FAILURE);
    }
    let mappings = slice::from_raw_parts(
        base.add(header.page_map.offset as usize)
            .cast::<anywasm_cpu_state::shared::MappingEntry>(),
        map_count,
    );
    if header.validate_mapping_table(mappings).is_err() {
        return fail_claim(slot, claim.sequence, ABI_FAILURE);
    }
    let context = &mut *base.add(dispatch.context_offset as usize).cast::<Cpu>();
    let Ok(mut memory) = SharedGuestMemory::new(base, available_bytes, header, mappings) else {
        return fail_claim(slot, claim.sequence, ABI_FAILURE);
    };

    let initial_icount = context.icount;
    let mut completion_state = CompletionState::Quantum;
    let mut stop_reason = STOP_QUANTUM;
    let mut payload_address = 0;
    let mut payload_value = 0;
    let mut retired = 0u64;
    while retired < dispatch.instruction_budget {
        if process_exit != 0 && context.rip == process_exit {
            completion_state = CompletionState::Exited;
            stop_reason = STOP_PROCESS_EXIT;
            payload_address = context.rip;
            payload_value = context.gpr[anywasm_cpu_state::RAX] as u32 as u64;
            break;
        }
        if thread_exit != 0 && context.rip == thread_exit {
            completion_state = CompletionState::Exited;
            stop_reason = STOP_THREAD_EXIT;
            payload_address = context.rip;
            break;
        }
        if (trap_start..trap_end).contains(&context.rip) {
            completion_state = CompletionState::Hle;
            stop_reason = STOP_HLE;
            payload_address = context.rip;
            break;
        }
        let instruction_rip = context.rip;
        match exec_core::step(context, &mut memory) {
            Ok(()) => retired = context.icount.saturating_sub(initial_icount),
            Err(exit) => {
                // Some interpreter exits, including INT3 and software interrupts, advance RIP
                // before returning. They did not retire in the worker: restore the instruction
                // boundary so the coordinator can dispatch the exception or fallback exactly once.
                context.rip = instruction_rip;
                completion_state = CompletionState::Fault;
                (stop_reason, payload_address, payload_value) = encode_exit(exit, context.rip);
                retired = context.icount.saturating_sub(initial_icount);
                break;
            }
        }
    }

    let completion = Completion {
        sequence: claim.sequence,
        state: completion_state,
        retired_instructions: retired,
        stop_reason,
        payload_address,
        payload_value,
    };
    if slot.complete(completion) {
        1
    } else {
        WORKER_FAILURE
    }
}

fn fail_claim(slot: &WorkerSlot, sequence: u32, reason: u32) -> u32 {
    if slot.complete(Completion {
        sequence,
        state: CompletionState::Failed,
        retired_instructions: 0,
        stop_reason: reason,
        payload_address: 0,
        payload_value: 0,
    }) {
        reason
    } else {
        WORKER_FAILURE
    }
}

fn encode_exit(exit: Exit, rip: u64) -> (u32, u64, u64) {
    match exit {
        Exit::Fault(fault) => {
            let stop_reason = match fault.kind {
                anywasm_cpu_state::memory::FaultKind::UnsupportedAtomicAlignment => {
                    STOP_UNSUPPORTED_ATOMIC_ALIGNMENT
                }
                anywasm_cpu_state::memory::FaultKind::UnsupportedAtomicWidth => {
                    STOP_UNSUPPORTED_ATOMIC_WIDTH
                }
                anywasm_cpu_state::memory::FaultKind::GuestMemory => STOP_ACCESS_VIOLATION,
            };
            (
                stop_reason,
                fault.addr,
                (match fault.access {
                    anywasm_cpu_state::Access::Read => 0,
                    anywasm_cpu_state::Access::Write => 1,
                    anywasm_cpu_state::Access::Exec => 8,
                }) | ((fault.protection as u64) << 8),
            )
        }
        Exit::DivideError => (STOP_DIVIDE_ERROR, rip, 0),
        Exit::Breakpoint => (STOP_BREAKPOINT, rip, 0),
        Exit::IllegalInstruction | Exit::Decode(_) => (STOP_ILLEGAL_INSTRUCTION, rip, 0),
        Exit::PrivilegedInstruction => (STOP_PRIVILEGED_INSTRUCTION, rip, 0),
        Exit::Syscall => (STOP_SYSCALL, rip, 0),
        Exit::Int(vector) => (STOP_SOFTWARE_INTERRUPT_BASE + vector as u32, rip, 0),
        Exit::Unimplemented(_) | Exit::Undefined => (STOP_UNSUPPORTED, rip, 0),
        Exit::Halt => (STOP_UNSUPPORTED, rip, 1),
    }
}

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
