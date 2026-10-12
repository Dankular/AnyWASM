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
    "cmp rax, rax",
    "jmp JIT_STATE_MOV_PREFIX",
    "JIT_STATE_MOV_PREFIX:",
    "nop",
    "movabs rax, 0x1122334455667788",
    "mov rbx, rax",
    "mov ecx, eax",
    "mov rdx, rcx",
    "mov r8d, 0xa1b2c3d4",
    "mov r9, r8",
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
    "xor eax, eax",
    "ret",
    "JIT_STATE_FAIL:",
    "mov eax, 1",
    "ret",
    ".data",
    ".p2align 3",
    ".globl JIT_STATE_OUTPUT",
    "JIT_STATE_OUTPUT:",
    ".quad 0, 0, 0, 0, 0, 0",
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
fn run_workload() {
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

#[cfg(not(any(bench_call, bench_memory, bench_sse, callback_budget, jit_state)))]
compile_error!(
    "select one workload with --cfg bench_call, bench_memory, bench_sse, callback_budget, or jit_state"
);

#[cfg(not(any(callback_budget, jit_state)))]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload();
    0
}

#[cfg(callback_budget)]
#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload()
}
