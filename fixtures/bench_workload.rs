#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[cfg(callback_budget)]
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumDisplayMonitors(
        dc: *mut core::ffi::c_void,
        clip: *const core::ffi::c_void,
        callback: extern "system" fn(
            monitor: *mut core::ffi::c_void,
            dc: *mut core::ffi::c_void,
            rect: *mut core::ffi::c_void,
            data: isize,
        ) -> i32,
        data: isize,
    ) -> i32;
}

#[cfg(callback_budget)]
static mut CALLBACK_DONE: u32 = 0;
#[cfg(callback_budget)]
static mut CHECKSUM: u64 = 0;

#[cfg(jit_state)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "mov rax, qword ptr [rip + JIT_STATE_INPUT]",
    "mov qword ptr [rip + JIT_STATE_OUTPUT], rax",
    "cmp rax, rax",
    "jmp JIT_STATE_MOV_PREFIX",
    "JIT_STATE_MOV_PREFIX:",
    "nop",
    "mov r12, qword ptr [rip + JIT_STATE_INPUT]",
    "movabs rax, 0x1122334455667788",
    "mov rbx, rax",
    "mov ecx, eax",
    "mov rdx, rcx",
    "mov r8d, 0xa1b2c3d4",
    "mov r9, r8",
    "mov qword ptr [rip + JIT_STATE_OUTPUT + 48], r12",
    "lea r10, [rip + JIT_STATE_OUTPUT]",
    "mov qword ptr [r10], rax",
    "mov qword ptr [r10 + 8], rbx",
    "mov qword ptr [r10 + 16], rcx",
    "mov qword ptr [r10 + 24], rdx",
    "mov qword ptr [r10 + 32], r8",
    "mov qword ptr [r10 + 40], r9",
    "movabs r11, 0x1122334455667788",
    "cmp qword ptr [r10], r11",
    "jne JIT_STATE_FAIL",
    "cmp qword ptr [r10 + 8], r11",
    "jne JIT_STATE_FAIL",
    "mov r11d, 0x55667788",
    "cmp qword ptr [r10 + 16], r11",
    "jne JIT_STATE_FAIL",
    "cmp qword ptr [r10 + 24], r11",
    "jne JIT_STATE_FAIL",
    "mov r11d, 0xa1b2c3d4",
    "cmp qword ptr [r10 + 32], r11",
    "jne JIT_STATE_FAIL",
    "cmp qword ptr [r10 + 40], r11",
    "jne JIT_STATE_FAIL",
    "movabs r11, 0x1122334455667788",
    "cmp qword ptr [r10 + 48], r11",
    "jne JIT_STATE_FAIL",
    "movabs r11, 0x8877665544332211",
    "jmp JIT_STATE_MISS_PREFIX",
    "JIT_STATE_MISS_PREFIX:",
    "nop",
    "mov r14, qword ptr [rip + JIT_STATE_MISS_INPUT]",
    "mov r15, r15",
    "mov r13, r13",
    "mov r12, r12",
    "mov r9, r9",
    "mov r8, r8",
    "mov r11, r11",
    "mov r10, r10",
    "mov r13d, 0x89abcdef",
    "mov r15d, 0x76543210",
    "mov ebp, 0x13579bdf",
    "mov esi, 0x2468ace0",
    "mov edi, 0x10203040",
    "cmp r14, r11",
    "jne JIT_STATE_FAIL",
    "nop",
    "mov r15, r15",
    "jmp JIT_STATE_BRANCH_PREFIX",
    "JIT_STATE_BRANCH_PREFIX:",
    "nop",
    "mov r15, r15",
    "je JIT_STATE_SUCCESS",
    "JIT_STATE_FAIL:",
    "mov eax, 1",
    "ret",
    "JIT_STATE_SUCCESS:",
    "lea rax, [rip + JIT_STATE_OUTPUT]",
    "cmp r10, rax",
    "jne JIT_STATE_FAIL",
    "movabs rax, 0x8877665544332211",
    "cmp r11, rax",
    "jne JIT_STATE_FAIL",
    "cmp r12, qword ptr [rip + JIT_STATE_INPUT]",
    "jne JIT_STATE_FAIL",
    "cmp r13d, 0x89abcdef",
    "jne JIT_STATE_FAIL",
    "cmp r14, rax",
    "jne JIT_STATE_FAIL",
    "cmp r15d, 0x76543210",
    "jne JIT_STATE_FAIL",
    "cmp ebp, 0x13579bdf",
    "jne JIT_STATE_FAIL",
    "cmp esi, 0x2468ace0",
    "jne JIT_STATE_FAIL",
    "cmp edi, 0x10203040",
    "jne JIT_STATE_FAIL",
    "mov r14d, 1",
    "jmp JIT_STATE_INDIRECT_TARGET",
    "JIT_STATE_INDIRECT_TARGET:",
    "nop",
    "test r14d, r14d",
    "jz JIT_STATE_INDIRECT_DONE",
    "mov r14d, 0",
    "jmp JIT_STATE_INDIRECT_SOURCE",
    "JIT_STATE_INDIRECT_SOURCE:",
    "lea rax, [rip + JIT_STATE_INDIRECT_TARGET]",
    "jmp rax",
    "JIT_STATE_INDIRECT_DONE:",
    "mov ecx, 100000",
    "JIT_STATE_CHAIN_CHECK:",
    "dec ecx",
    "jz JIT_STATE_CHAIN_DONE",
    "JIT_STATE_CHAIN_A:",
    "nop",
    "mov rbx, rbx",
    "jmp JIT_STATE_CHAIN_B",
    "JIT_STATE_CHAIN_B:",
    "nop",
    "mov r12, r12",
    "jmp JIT_STATE_CHAIN_CHECK",
    "JIT_STATE_CHAIN_DONE:",
    "xor eax, eax",
    "ret",
    ".data",
    ".p2align 3",
    ".globl JIT_STATE_OUTPUT",
    "JIT_STATE_OUTPUT:",
    ".quad 0, 0, 0, 0, 0, 0, 0",
    ".globl JIT_STATE_INPUT",
    "JIT_STATE_INPUT:",
    ".quad 0x1122334455667788",
    ".p2align 12",
    ".globl JIT_STATE_MISS_INPUT",
    "JIT_STATE_MISS_INPUT:",
    ".quad 0x8877665544332211",
    ".text",
);

#[cfg(jit_indirect)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "mov r14d, 1",
    "jmp JIT_INDIRECT_TARGET",
    "JIT_INDIRECT_TARGET:",
    "nop",
    "test r14d, r14d",
    "jz JIT_INDIRECT_DONE",
    "mov r14d, 0",
    "jmp JIT_INDIRECT_SOURCE",
    "JIT_INDIRECT_SOURCE:",
    "lea rax, [rip + JIT_INDIRECT_TARGET]",
    "jmp rax",
    "JIT_INDIRECT_DONE:",
    "xor eax, eax",
    "ret",
    ".text",
);

#[cfg(jit_badjmp)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "mov rax, 0x0000800000000000",
    "jmp rax",
    ".text",
);

#[cfg(jit_hle)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetLastError(error: u32);
}

#[cfg(jit_hle)]
#[no_mangle]
#[used]
pub static JIT_HLE_IMPORT: unsafe extern "system" fn(u32) = SetLastError;

#[cfg(jit_hle)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "xor ecx, ecx",
    "mov rax, qword ptr [rip + JIT_HLE_IMPORT]",
    "jmp JIT_HLE_PREFIX",
    "JIT_HLE_PREFIX:",
    "jmp rax",
    ".text",
);

#[cfg(jit_sse)]
unsafe extern "system" {
    fn jit_sse_entry() -> u32;
}

#[cfg(jit_sse)]
core::arch::global_asm!(
    ".text",
    ".globl jit_sse_entry",
    "jit_sse_entry:",
    "mov dword ptr [rip + JIT_SSE_RESULT], 0",
    "movupd xmm0, xmmword ptr [rip + JIT_SSE_INPUT0]",
    "movupd xmm1, xmmword ptr [rip + JIT_SSE_INPUT1]",
    "jmp JIT_SSE_OPS",
    "JIT_SSE_OPS:",
    "movapd xmm2, xmm0",
    "mulpd xmm2, xmm1",
    "addpd xmm2, xmm1",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT], xmm2",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT]",
    "movabs rdx, 0x4016800000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 8]",
    "movabs rdx, 0x401e000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "movapd xmm3, xmm0",
    "subpd xmm3, xmm1",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT + 16], xmm3",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 16]",
    "movabs rdx, 0xbfe8000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 24]",
    "movabs rdx, 0xbff8000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "movapd xmm4, xmm1",
    "divpd xmm4, xmm0",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT + 32], xmm4",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 32]",
    "movabs rdx, 0x3ff8000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 40]",
    "movabs rdx, 0x4000000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "movupd xmm6, xmmword ptr [rip + JIT_SSE_ZERO]",
    "addpd xmm6, xmm1",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT + 48], xmm6",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 48]",
    "movabs rdx, 0x4002000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 56]",
    "movabs rdx, 0x4008000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "movupd xmm7, xmmword ptr [rip + JIT_SSE_NAN]",
    "addpd xmm7, xmm1",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT + 64], xmm7",
    "movupd xmm8, xmmword ptr [rip + JIT_SSE_INPUT0]",
    "addpd xmm8, xmmword ptr [rip + JIT_SSE_INPUT1]",
    "movupd xmmword ptr [rip + JIT_SSE_RESULT + 80], xmm8",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 80]",
    "movabs rdx, 0x400e000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "mov rax, qword ptr [rip + JIT_SSE_RESULT + 88]",
    "movabs rdx, 0x4012000000000000",
    "cmp rax, rdx",
    "jne JIT_SSE_FAIL",
    "movupd xmm5, xmmword ptr [rip + JIT_SSE_EXCEPTION]",
    "addpd xmm5, xmm1",
    "xor eax, eax",
    "ret",
    "JIT_SSE_FAIL:",
    "mov eax, 1",
    "ret",
    ".data",
    ".p2align 4",
    ".globl JIT_SSE_RESULT",
    "JIT_SSE_RESULT:",
    ".quad 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0",
    ".globl JIT_SSE_INPUT0",
    "JIT_SSE_INPUT0:",
    ".quad 0x3ff8000000000000, 0x3ff8000000000000",
    ".globl JIT_SSE_INPUT1",
    "JIT_SSE_INPUT1:",
    ".quad 0x4002000000000000, 0x4008000000000000",
    ".globl JIT_SSE_EXCEPTION",
    "JIT_SSE_EXCEPTION:",
    ".quad 0x7ff0000000000000, 0x0000000000000001",
    ".globl JIT_SSE_ZERO",
    "JIT_SSE_ZERO:",
    ".quad 0x0000000000000000, 0x8000000000000000",
    ".globl JIT_SSE_NAN",
    "JIT_SSE_NAN:",
    ".quad 0x7ff8000000000001, 0xfff8000000000002",
    ".text",
);

#[cfg(jit_sse_mxcsr)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "ldmxcsr dword ptr [rip + JIT_SSE_MXCSR]",
    "jmp JIT_SSE_MXCSR_OPS",
    "JIT_SSE_MXCSR_OPS:",
    "movupd xmm0, xmmword ptr [rip + JIT_SSE_MXCSR_INPUT0]",
    "movupd xmm1, xmmword ptr [rip + JIT_SSE_MXCSR_INPUT1]",
    "addpd xmm0, xmm1",
    "movupd xmmword ptr [rip + JIT_SSE_MXCSR_RESULT], xmm0",
    "xor eax, eax",
    "ret",
    ".data",
    ".p2align 4",
    "JIT_SSE_MXCSR:",
    ".long 0x3f80",
    "JIT_SSE_MXCSR_INPUT0:",
    ".quad 0x3ff0000000000000, 0x3ff0000000000000",
    "JIT_SSE_MXCSR_INPUT1:",
    ".quad 0x3ca8000000000000, 0x3ca8000000000000",
    "JIT_SSE_MXCSR_RESULT:",
    ".quad 0, 0",
    ".text",
);

#[cfg(jit_x87)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "fld qword ptr [rip + JIT_X87_INPUT0]",
    "fld qword ptr [rip + JIT_X87_INPUT1]",
    "jmp JIT_X87_STACK_OPS",
    "JIT_X87_STACK_OPS:",
    "fadd st(0), st(1)",
    "fxch st(1)",
    "fstp st(1)",
    "fld st(0)",
    "fmul st(0), st(1)",
    "fsub st(0), st(1)",
    "fsubr st(0), st(1)",
    "fdiv st(0), st(1)",
    "fdivr st(0), st(1)",
    "fadd st(0), st(1)",
    "xor eax, eax",
    "ret",
    ".data",
    ".p2align 3",
    ".globl JIT_X87_INPUT0",
    "JIT_X87_INPUT0:",
    ".quad 0x3ff8000000000000",
    ".globl JIT_X87_INPUT1",
    "JIT_X87_INPUT1:",
    ".quad 0x4002000000000000",
    ".text",
);

#[cfg(jit_x87_exception)]
core::arch::global_asm!(
    ".text",
    ".globl mainCRTStartup",
    "mainCRTStartup:",
    "fld qword ptr [rip + JIT_X87_EXCEPTION_INF]",
    "fld qword ptr [rip + JIT_X87_EXCEPTION_TWO]",
    "jmp JIT_X87_EXCEPTION_OPS",
    "JIT_X87_EXCEPTION_OPS:",
    "fadd st(0), st(1)",
    "xor eax, eax",
    "ret",
    ".data",
    ".p2align 3",
    "JIT_X87_EXCEPTION_INF:",
    ".quad 0x7ff0000000000000",
    "JIT_X87_EXCEPTION_TWO:",
    ".quad 0x4000000000000000",
    ".text",
);

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(bench_call)]
#[inline(never)]
fn call_leaf(value: u64) -> u64 {
    value.wrapping_mul(0x9e37_79b1_85eb_ca87).rotate_left(11) ^ value.rotate_right(7)
}

#[cfg(bench_call)]
fn run_workload() {
    let mut value = 0u64;
    for i in 0..500_000u64 {
        value = call_leaf(value.wrapping_add(i));
    }
    core::hint::black_box(value);
}

#[cfg(bench_memory)]
static mut MEMORY: [u64; 4096] = [0; 4096];

#[cfg(bench_memory)]
fn run_workload() {
    let memory = core::ptr::addr_of_mut!(MEMORY).cast::<u64>();
    for pass in 0..128u64 {
        for index in 0..4096usize {
            unsafe {
                let cell = memory.add(index);
                let value = cell.read_volatile();
                cell.write_volatile(value.wrapping_add(pass ^ index as u64));
            }
        }
    }
    unsafe {
        core::hint::black_box(memory.add(2048).read_volatile());
    }
}

#[cfg(bench_sse)]
fn run_workload() -> u32 {
    use core::arch::x86_64::{_mm_add_pd, _mm_loadu_pd, _mm_mul_pd, _mm_set_pd, _mm_storeu_pd};

    let mut lanes = [1.0f64, 2.0f64];
    unsafe {
        let multiplier = _mm_set_pd(1.000000001, 1.000000002);
        let increment = _mm_set_pd(0.0000001, 0.0000002);
        let mut value = _mm_loadu_pd(lanes.as_ptr());
        for _ in 0..500_000 {
            value = _mm_add_pd(_mm_mul_pd(value, multiplier), increment);
        }
        _mm_storeu_pd(lanes.as_mut_ptr(), value);
    }
    core::hint::black_box(lanes);
    if lanes[0].to_bits() == 0x3ff1_9de7_2584_5e7b && lanes[1].to_bits() == 0x4000_6879_5faf_a5af {
        0
    } else {
        1
    }
}

#[cfg(callback_budget)]
extern "system" fn monitor_callback(
    _monitor: *mut core::ffi::c_void,
    _dc: *mut core::ffi::c_void,
    _rect: *mut core::ffi::c_void,
    _data: isize,
) -> i32 {
    for i in 0..800_000u64 {
        unsafe {
            let checksum = core::ptr::addr_of_mut!(CHECKSUM);
            checksum.write_volatile(checksum.read_volatile().wrapping_add(i));
        }
    }
    unsafe { core::ptr::addr_of_mut!(CALLBACK_DONE).write_volatile(1) };
    1
}

#[cfg(callback_budget)]
fn run_workload() -> u32 {
    let result = unsafe {
        EnumDisplayMonitors(
            core::ptr::null_mut(),
            core::ptr::null(),
            monitor_callback,
            0x1234,
        )
    };
    let done = unsafe { core::ptr::addr_of!(CALLBACK_DONE).read_volatile() };
    let checksum = unsafe { core::ptr::addr_of!(CHECKSUM).read_volatile() };
    if result != 0 && done == 1 && checksum != 0 {
        0
    } else {
        1
    }
}

#[cfg(not(any(
    bench_call,
    bench_memory,
    bench_sse,
    callback_budget,
    jit_state,
    jit_sse,
    jit_sse_mxcsr,
    jit_x87,
    jit_x87_exception,
    jit_indirect,
    jit_badjmp,
    jit_hle
)))]
compile_error!(
    "select one workload with --cfg bench_call, bench_memory, bench_sse, callback_budget, jit_state, jit_sse, jit_sse_mxcsr, jit_x87, jit_x87_exception, jit_indirect, jit_badjmp, or jit_hle"
);

#[cfg(not(any(callback_budget, jit_state, jit_sse, jit_sse_mxcsr, jit_x87, jit_x87_exception, jit_indirect, jit_badjmp, jit_hle, bench_sse)))]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload();
    0
}

#[cfg(bench_sse)]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload()
}

#[cfg(jit_sse)]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    unsafe { jit_sse_entry() }
}

#[cfg(callback_budget)]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload()
}
