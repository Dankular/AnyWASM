#![no_std]

use core::sync::atomic::{AtomicU32, Ordering};

#[no_mangle]
pub static SHARED_GUEST_WORD: AtomicU32 = AtomicU32::new(0);

#[no_mangle]
pub static mut RUNTIME_STATE: u32 = 7;

#[no_mangle]
pub static mut STAND_IN_CURSOR: u32 = 0x4000;

#[no_mangle]
pub extern "C" fn atomic_increment() -> u32 {
    SHARED_GUEST_WORD.fetch_add(1, Ordering::SeqCst) + 1
}

#[no_mangle]
pub extern "C" fn atomic_value() -> u32 {
    SHARED_GUEST_WORD.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn set_runtime_state(value: u32) {
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(RUNTIME_STATE), value) };
}

#[no_mangle]
pub extern "C" fn runtime_state() -> u32 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RUNTIME_STATE)) }
}

#[no_mangle]
pub extern "C" fn stand_in_cursor_probe() -> u32 {
    unsafe {
        let pointer = core::ptr::read_volatile(core::ptr::addr_of!(STAND_IN_CURSOR));
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(STAND_IN_CURSOR),
            pointer.wrapping_add(64),
        );
        pointer
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
