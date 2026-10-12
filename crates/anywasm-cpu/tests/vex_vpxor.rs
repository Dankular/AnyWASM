use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn target_vxorpd_decodes_extended_registers() {
    let instruction = decode(&[0xc4, 0x41, 0x29, 0x57, 0xd2], 0x1000).unwrap();

    assert_eq!(instruction.op, Op::Vfp(FpK::Xor, Prec::Pd));
    assert_eq!(instruction.osz, 16);
    assert_eq!(instruction.len, 5);
    assert_eq!(instruction.ops[0], Opnd::Xmm(10));
    assert_eq!(instruction.ops[1], Opnd::Xmm(10));
    assert_eq!(instruction.ops[2], Opnd::Xmm(10));
}

#[test]
fn vxorpd_clears_the_vector_and_preserves_flags() {
    const CODE: u64 = 0x1000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0x41, 0x29, 0x57, 0xd2]).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[10] = u128::MAX;
    cpu.ymm_hi[10] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[10], 0);
    assert_eq!(cpu.ymm_hi[10], 0);
    assert_eq!(cpu.rflags, flags);
}
