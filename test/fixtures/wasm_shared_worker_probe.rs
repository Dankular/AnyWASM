#![no_std]

use core::sync::atomic::{AtomicU32, Ordering};

#[no_mangle]
pub extern "C" fn worker_slice(
    context_address: u32,
    counter_address: u32,
    active_address: u32,
    maximum_active_address: u32,
    seed: u32,
) -> u32 {
    let mut scratch = [0_u32; 256];
    let active = unsafe { &*(active_address as *const AtomicU32) };
    let maximum_active = unsafe { &*(maximum_active_address as *const AtomicU32) };
    let current_active = active.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    let mut observed_maximum = maximum_active.load(Ordering::SeqCst);
    while current_active > observed_maximum {
        match maximum_active.compare_exchange_weak(
            observed_maximum,
            current_active,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => break,
            Err(actual) => observed_maximum = actual,
        }
    }
    for (index, value) in scratch.iter_mut().enumerate() {
        unsafe {
            core::ptr::write_volatile(value, seed.wrapping_add(index as u32));
        }
    }

    let context = context_address as *mut u32;
    let counter = unsafe { &*(counter_address as *const AtomicU32) };
    let checksum = scratch.iter().fold(0_u32, |sum, value| {
        sum.wrapping_add(unsafe { core::ptr::read_volatile(value) })
    });
    unsafe {
        core::ptr::write_volatile(context, seed);
        core::ptr::write_volatile(context.add(1), checksum);
        core::ptr::write_volatile(context.add(2), core::ptr::read_volatile(&scratch[255]));
    }

    let result = counter.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    active.fetch_sub(1, Ordering::SeqCst);
    result
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
