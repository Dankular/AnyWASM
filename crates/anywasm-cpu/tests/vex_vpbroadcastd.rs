use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_vpbroadcastd_xmm_register_and_memory_forms_clear_upper_ymm() {
    const CODE: u64 = 0x25000;
    const DATA: u64 = 0x26000;
    let register_form = [0xc4, 0xe2, 0x79, 0x58, 0xc0]; // vpbroadcastd xmm0, xmm0
    let memory_form = [0xc4, 0xe2, 0x79, 0x58, 0x08]; // vpbroadcastd xmm1, [rax]

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &register_form).unwrap();
    mem.write_force(CODE + 5, &memory_form).unwrap();
    mem.write_force(DATA, &0xfedc_ba98u32.to_le_bytes()).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.rflags = 0x8d7;
    cpu.xmm[0] = 0xdead_beef_cafe_babe_0123_4567_89ab_cdef;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.ymm_hi[1] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();
    step(&mut cpu, &mut mem).unwrap();

    let register_lane = 0x89ab_cdefu128;
    let memory_lane = 0xfedc_ba98u128;
    let register_value = register_lane
        | (register_lane << 32)
        | (register_lane << 64)
        | (register_lane << 96);
    let memory_value =
        memory_lane | (memory_lane << 32) | (memory_lane << 64) | (memory_lane << 96);
    assert_eq!(cpu.xmm[0], register_value);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.xmm[1], memory_value);
    assert_eq!(cpu.ymm_hi[1], 0);
    assert_eq!(cpu.rflags, 0x8d7);
}
