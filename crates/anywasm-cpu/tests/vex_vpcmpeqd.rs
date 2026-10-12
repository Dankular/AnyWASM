use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_vpcmpeqd_compares_register_and_memory_lanes_for_xmm_and_ymm() {
    const CODE: u64 = 0x2c000;
    const DATA: u64 = 0x2d000;
    let code = [
        0xc5, 0xf9, 0x76, 0xd1, // vpcmpeqd xmm2, xmm0, xmm1
        0xc5, 0xfd, 0x76, 0xd1, // vpcmpeqd ymm2, ymm0, ymm1
        0xc5, 0xf9, 0x76, 0x10, // vpcmpeqd xmm2, xmm0, [rax]
    ];
    let decoded = decode(&code, CODE).unwrap();
    assert_eq!(decoded.op, Op::Vpi(anywasm_cpu::decode::PiK::Cmpeq, 4, true));
    assert_eq!(decoded.osz, 16);
    assert_eq!(decoded.ops[0], Opnd::Xmm(2));
    assert_eq!(decoded.ops[1], Opnd::Xmm(0));
    assert_eq!(decoded.ops[2], Opnd::Xmm(1));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &code).unwrap();
    mem.write_force(DATA, &[7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[0] = 7 | (9u128 << 32) | (11u128 << 64) | (13u128 << 96);
    cpu.xmm[1] = 7 | (10u128 << 32) | (11u128 << 64) | (12u128 << 96);
    cpu.ymm_hi[0] = 1 | (2u128 << 32) | (3u128 << 64) | (4u128 << 96);
    cpu.ymm_hi[1] = 1 | (0u128 << 32) | (3u128 << 64) | (0u128 << 96);
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;
    let equal_lanes = 0xffff_ffff_0000_0000_ffff_ffffu128;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[2], equal_lanes);
    assert_eq!(cpu.ymm_hi[2], 0);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[2], equal_lanes);
    assert_eq!(cpu.ymm_hi[2], equal_lanes);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[2], equal_lanes);
    assert_eq!(cpu.ymm_hi[2], 0);
    assert_eq!(cpu.rflags, flags);
}
