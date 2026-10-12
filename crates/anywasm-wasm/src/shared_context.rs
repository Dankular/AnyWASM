//! C-ABI bridge for handing quiescent guest CPU contexts to and from shared workers.

use anywasm_cpu_state::{Cpu, CpuMode};

const CPU_CONTEXT_BYTES: usize = 784;
const CPU_CONTEXT_ALIGN: usize = 16;
const _: [(); CPU_CONTEXT_BYTES] = [(); core::mem::size_of::<Cpu>()];
const _: [(); CPU_CONTEXT_ALIGN] = [(); core::mem::align_of::<Cpu>()];

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
