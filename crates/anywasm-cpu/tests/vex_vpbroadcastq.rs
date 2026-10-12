use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::decode;
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_vpbroadcastq_repeats_qwords_in_xmm_and_ymm_forms() {
    const CODE: u64 = 0x23000;
    const DATA: u64 = 0x24000;
    let register_ymm = [0xc4, 0xe2, 0x7d, 0x59, 0xe0]; // vpbroadcastq ymm4, xmm0
    let register_xmm = [0xc4, 0xe2, 0x79, 0x59, 0xf0]; // vpbroadcastq xmm6, xmm0
    let memory_ymm = [0xc4, 0xe2, 0x7d, 0x59, 0x28]; // vpbroadcastq ymm5, [rax]
    let memory_xmm = [0xc4, 0xe2, 0x79, 0x59, 0x38]; // vpbroadcastq xmm7, [rax]
    for (bytes, offset, width) in [
        (&register_ymm[..], 0, 32),
        (&register_xmm[..], 5, 16),
        (&memory_ymm[..], 10, 32),
        (&memory_xmm[..], 15, 16),
    ] {
        let decoded = decode(bytes, CODE + offset).unwrap();
        assert_eq!(format!("{:?}", decoded.op), "Vpbroadcastq");
        assert_eq!(decoded.osz, width);
    }

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &register_ymm).unwrap();
    mem.write_force(CODE + 5, &register_xmm).unwrap();
    mem.write_force(CODE + 10, &memory_ymm).unwrap();
    mem.write_force(CODE + 15, &memory_xmm).unwrap();
    mem.write_force(DATA, &0xfedc_ba98_7654_3210u64.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.rflags = 0x8d7;
    cpu.xmm[0] = 0xdead_beef_cafe_babe_0123_4567_89ab_cdef;
    cpu.ymm_hi[4] = u128::MAX;
    cpu.ymm_hi[5] = u128::MAX;
    cpu.ymm_hi[6] = u128::MAX;
    cpu.ymm_hi[7] = u128::MAX;

    for _ in 0..4 {
        step(&mut cpu, &mut mem).unwrap();
    }

    let register_half = 0x0123_4567_89ab_cdefu128;
    let memory_half = 0xfedc_ba98_7654_3210u128;
    let register_pair = register_half | (register_half << 64);
    let memory_pair = memory_half | (memory_half << 64);
    assert_eq!(cpu.xmm[4], register_pair);
    assert_eq!(cpu.ymm_hi[4], register_pair);
    assert_eq!(cpu.xmm[6], register_pair);
    assert_eq!(cpu.ymm_hi[6], 0);
    assert_eq!(cpu.xmm[5], memory_pair);
    assert_eq!(cpu.ymm_hi[5], memory_pair);
    assert_eq!(cpu.xmm[7], memory_pair);
    assert_eq!(cpu.ymm_hi[7], 0);
    assert_eq!(cpu.rflags, 0x8d7);
}
