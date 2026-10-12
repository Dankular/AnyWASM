//! C-ABI bridge for handing quiescent guest CPU contexts to and from shared workers.

use anywasm_cpu_state::{Cpu, CpuMode};

const CPU_CONTEXT_BYTES: usize = 784;
const CPU_CONTEXT_ALIGN: usize = 16;
const SHARED_CPU_QUANTUM: u64 = 10_000;
const _: [(); CPU_CONTEXT_BYTES] = [(); core::mem::size_of::<Cpu>()];
const _: [(); CPU_CONTEXT_ALIGN] = [(); core::mem::align_of::<Cpu>()];

/// Number of active and saved guest CPU contexts in the loaded process.
#[no_mangle]
pub extern "C" fn aw_shared_context_count() -> u32 {
    super::state()
        .map(|s| s.p.guest_threads.len() as u32 + u32::from(!s.p.current_thread_saved))
        .unwrap_or(0)
}

/// Thread ID at an enumeration index. The active context comes first, then saved IDs sorted
/// ascending. Returns zero when the index is outside the process's context table.
#[no_mangle]
pub extern "C" fn aw_shared_context_thread_id(index: u32) -> u32 {
    let Some(s) = super::state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved {
        if index == 0 {
            return p.current_thread_id;
        }
        let mut ids: Vec<u32> = p
            .guest_threads
            .keys()
            .copied()
            .filter(|id| *id != p.current_thread_id)
            .collect();
        ids.sort_unstable();
        return ids.get((index - 1) as usize).copied().unwrap_or(0);
    }
    let mut ids: Vec<u32> = p.guest_threads.keys().copied().collect();
    ids.sort_unstable();
    ids.get(index as usize).copied().unwrap_or(0)
}

/// Active coordinator context when it is safe to dispatch, or zero when scheduling must run first.
#[no_mangle]
pub extern "C" fn aw_shared_context_active_thread_id() -> u32 {
    use anywasm_rt::process::ThreadState;
    let Some(s) = super::state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved
        && !p.schedule_requested
        && p.current_thread_state == ThreadState::Running
        && p.quantum_used < SHARED_CPU_QUANTUM // matches Process::run_with_budget's thread quantum
    {
        p.current_thread_id
    } else {
        0
    }
}

/// Remaining worker budget in the active thread's scheduler quantum, or zero when it must yield.
#[no_mangle]
pub extern "C" fn aw_shared_context_quantum_remaining() -> u32 {
    let Some(s) = super::state() else { return 0 };
    let p = &s.p;
    if !p.current_thread_saved
        && !p.schedule_requested
        && p.current_thread_state == anywasm_rt::process::ThreadState::Running
        && p.quantum_used < SHARED_CPU_QUANTUM
    {
        (SHARED_CPU_QUANTUM - p.quantum_used) as u32
    } else {
        0
    }
}

/// Start address of the coordinator-owned HLE thunk range.
#[no_mangle]
pub extern "C" fn aw_shared_hle_range_start() -> u64 {
    anywasm_rt::process::THUNK_BASE
}

/// Exclusive end address of the coordinator-owned HLE thunk range.
#[no_mangle]
pub extern "C" fn aw_shared_hle_range_end() -> u64 {
    anywasm_rt::process::THUNK_BASE + anywasm_rt::process::THUNK_BYTES
}

/// Copy one guest thread's architectural context to a buffer for a shared CPU worker.
///
/// The buffer uses the `Cpu` ABI from `anywasm-cpu-state`, including its 16-byte alignment.
/// Call only while the coordinator is quiescent and the selected thread is not executing.
/// Returns the context size (784 bytes), or zero for an invalid buffer or unknown thread ID.
///
/// # Safety
/// `output` must point to at least `capacity` writable bytes and be 16-byte aligned.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_context_export(
    thread_id: u32,
    output: *mut u8,
    capacity: usize,
) -> usize {
    let Some(s) = super::state() else { return 0 };
    if output.is_null()
        || capacity < CPU_CONTEXT_BYTES
        || output as usize % CPU_CONTEXT_ALIGN != 0
    {
        s.message = b"invalid shared CPU context output buffer".to_vec();
        return 0;
    }
    let p = &s.p;
    let cpu = if !p.current_thread_saved && thread_id == p.current_thread_id {
        Some(&p.cpu)
    } else {
        p.guest_threads.get(&thread_id).map(|thread| &thread.cpu)
    };
    let Some(cpu) = cpu else {
        s.message = b"unknown guest thread for shared CPU context export".to_vec();
        return 0;
    };
    output.cast::<Cpu>().write(cpu.clone());
    s.message.clear();
    CPU_CONTEXT_BYTES
}

/// Import one worker-completed context into the coordinator's guest thread table.
///
/// The instruction-count increase is added to the process retirement counter. For the active
/// thread it also advances the current scheduling quantum; saved contexts have no independent
/// quantum counter in the cooperative scheduler.
/// Call only after the worker has published completion and the coordinator owns this context.
/// Returns 0 on success, 1 without a process, 2 for an invalid buffer/context, 3 for an unknown
/// thread ID, and 4 if the worker context's instruction count moved backwards.
///
/// # Safety
/// `input` must point to at least `length` readable bytes and be 16-byte aligned.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_context_import(
    thread_id: u32,
    input: *const u8,
    length: usize,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    if input.is_null()
        || length != CPU_CONTEXT_BYTES
        || input as usize % CPU_CONTEXT_ALIGN != 0
        || *input > CpuMode::X86_64 as u8
        || *input.add(core::mem::offset_of!(Cpu, fpu_top)) > 7
    {
        s.message = b"invalid shared CPU context input".to_vec();
        return 2;
    }
    let imported = input.cast::<Cpu>().read();
    let p = &mut s.p;
    let is_active = !p.current_thread_saved && thread_id == p.current_thread_id;
    let target = if is_active {
        Some(&mut p.cpu)
    } else {
        p.guest_threads
            .get_mut(&thread_id)
            .map(|thread| &mut thread.cpu)
    };
    let Some(target) = target else {
        s.message = b"unknown guest thread for shared CPU context import".to_vec();
        return 3;
    };
    if imported.mode != target.mode || imported.icount < target.icount {
        s.message = b"shared CPU context mode changed or instruction count moved backwards".to_vec();
        return 4;
    }
    let retired = imported.icount - target.icount;
    *target = imported;
    if is_active {
        p.quantum_used = p.quantum_used.saturating_add(retired);
    }
    p.retired_instructions = p.retired_instructions.saturating_add(retired);
    s.message.clear();
    0
}
