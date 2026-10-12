use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vpmovsxdq_sign_extends_all_four_dwords_into_ymm() {
    const CODE: u64 = 0x26000;
    let code = [0xc4, 0xe2, 0x7d, 0x25, 0xd1]; // vpmovsxdq ymm2, xmm1
    let decoded = decode(&code, CODE).unwrap();
    assert_eq!(decoded.op, Op::Vpmovsxdq);
    assert_eq!(decoded.osz, 32);
    assert_eq!(decoded.ops[0], Opnd::Xmm(2));
    assert_eq!(decoded.ops[1], Opnd::Xmm(1));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[1] = (4u128 << 96) | (0xffff_fffd_u128 << 64) | (2u128 << 32) | 0xffff_ffff;
    cpu.xmm[2] = u128::MAX;
    cpu.ymm_hi[2] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[2], (-1i64 as u64 as u128) | ((2u128) << 64));
    assert_eq!(cpu.ymm_hi[2], (-3i64 as u64 as u128) | ((4u128) << 64));
}
