#![no_std]
#![no_main]

use core::panic::PanicInfo;

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
    unsafe { core::hint::black_box(memory.add(2048).read_volatile()); }
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

#[cfg(not(any(bench_call, bench_memory, bench_sse)))]
compile_error!("select one benchmark workload with --cfg bench_call, bench_memory, or bench_sse");

#[no_mangle]
pub extern "system" fn mainCRTStartup() -> u32 {
    run_workload();
    0
}
