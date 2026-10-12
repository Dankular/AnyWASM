use anywasm_cpu::cpu::{R8, R9, RCX, RDX};
use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};
use anywasm_rt::{hle, Process, Vfs};

fn call(name: &str, process: &mut Process, args: &[u64]) -> u64 {
    let registers = [RCX, RDX, R8, R9];
    for (index, &value) in args.iter().take(registers.len()).enumerate() {
        process.cpu.gpr[registers[index]] = value;
    }
    hle::lookup("user32", name)
        .unwrap_or_else(|| panic!("user32!{name} should be implemented"))(process)
        .unwrap_or_else(|stop| panic!("user32!{name} stopped: {stop:?}"))
}

#[test]
fn desktop_window_is_a_stable_virtual_window_with_display_bounds() {
    const RECT: u64 = 0x20_000;
    let mut process = Process::new(Vfs::new());
    assert!(process.mem.map(RECT, PAGE_SIZE, PROT_RW));

    let desktop = call("GetDesktopWindow", &mut process, &[]);
    assert_ne!(desktop, 0);
    assert_eq!(call("GetDesktopWindow", &mut process, &[]), desktop);
    assert_eq!(call("IsWindow", &mut process, &[desktop]), 1);
    assert_eq!(call("IsWindowVisible", &mut process, &[desktop]), 1);

    assert_eq!(call("GetClientRect", &mut process, &[desktop, RECT]), 1);
    assert_eq!(
        [
            process.mem.read_u32(RECT).unwrap(),
            process.mem.read_u32(RECT + 4).unwrap(),
            process.mem.read_u32(RECT + 8).unwrap(),
            process.mem.read_u32(RECT + 12).unwrap(),
        ],
        [0, 0, 1920, 1080]
    );

    assert_eq!(call("GetWindowRect", &mut process, &[desktop, RECT]), 1);
    assert_eq!(
        [
            process.mem.read_u32(RECT).unwrap(),
            process.mem.read_u32(RECT + 4).unwrap(),
            process.mem.read_u32(RECT + 8).unwrap(),
            process.mem.read_u32(RECT + 12).unwrap(),
        ],
        [0, 0, 1920, 1080]
    );
}
