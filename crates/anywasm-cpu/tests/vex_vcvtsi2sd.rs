use anywasm_cpu::cpu::{Cpu, RCX};
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_vcvtsi2sd_converts_signed_i64_and_merges_from_vvvv() {
    const CODE: u64 = 0x1d000;
    const MERGE_SOURCE_HIGH: u64 = 0x0123_4567_89ab_cdef;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xe1, 0xfb, 0x2a, 0xc1])
        .unwrap(); // vcvtsi2sd xmm0, xmm0, rcx
    let instruction = decode(&[0xc4, 0xe1, 0xfb, 0x2a, 0xc1], CODE).unwrap();
    assert_eq!(instruction.op, Op::VcvtSi2Sd);
    assert_eq!(instruction.ssz, 8);
    assert_eq!(instruction.ops[0], Opnd::Xmm(0));
    assert_eq!(instruction.ops[1], Opnd::Xmm(0));
    assert_eq!(instruction.ops[2], Opnd::Gpr(RCX as u8));

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RCX] = (-9i64) as u64;
    cpu.xmm[0] = ((MERGE_SOURCE_HIGH as u128) << 64) | 0xfeed_face_cafe_beef;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    let result = step(&mut cpu, &mut mem);
    assert!(result.is_ok(), "step failed at rip {:#x}: {result:?}", cpu.rip);
    assert_eq!(cpu.xmm[0] as u64, (-9.0f64).to_bits());
    assert_eq!((cpu.xmm[0] >> 64) as u64, MERGE_SOURCE_HIGH);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, flags);
}
