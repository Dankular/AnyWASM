use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn target_vdivsd_decodes_extended_destination_and_source_registers() {
    let instruction = decode(&[0xc4, 0x41, 0x7b, 0x5e, 0xc1], 0x1000).unwrap();

    assert_eq!(instruction.op, Op::Vfp(FpK::Div, Prec::Sd));
    assert_eq!(instruction.len, 5);
    assert_eq!(instruction.ops[0], Opnd::Xmm(8));
    assert_eq!(instruction.ops[1], Opnd::Xmm(0));
    assert_eq!(instruction.ops[2], Opnd::Xmm(9));
}

#[test]
fn vdivsd_register_form_divides_and_merges_high_qword_from_vvvv() {
    const CODE: u64 = 0x1000;
    const MERGE_SOURCE_HIGH: u64 = 0x0123_4567_89ab_cdef;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0x41, 0x7b, 0x5e, 0xc1]).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = ((MERGE_SOURCE_HIGH as u128) << 64) | 0x4022_0000_0000_0000; // 9.0
    cpu.xmm[8] = u128::MAX;
    cpu.xmm[9] = 0xfedc_ba98_7654_3210_4008_0000_0000_0000; // 3.0
    cpu.ymm_hi[8] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[8] as u64, 3.0f64.to_bits());
    assert_eq!((cpu.xmm[8] >> 64) as u64, MERGE_SOURCE_HIGH);
    assert_eq!(cpu.ymm_hi[8], 0);
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn vdivsd_memory_source_reads_all_eight_bytes() {
    const CODE: u64 = 0x2000;
    const DATA: u64 = 0x3000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc5, 0xfb, 0x5e, 0x00]).unwrap(); // vdivsd xmm0, xmm0, [rax]
    mem.write_force(DATA, &3.0f64.to_bits().to_le_bytes()).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[0] = 0x0123_4567_89ab_cdef_4022_0000_0000_0000; // upper qword + 9.0
    cpu.ymm_hi[0] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[0] as u64, 3.0f64.to_bits());
    assert_eq!(cpu.xmm[0] >> 64, 0x0123_4567_89ab_cdef);
    assert_eq!(cpu.ymm_hi[0], 0);
}

#[test]
fn vmaxss_uses_vvvv_source_and_preserves_flags() {
    const CODE: u64 = 0x4000;
    const MERGE_SOURCE: u128 = 0x1122_3344_5566_7788_99aa_bbcc_4100_0000; // 8.0f
    let code = [0xc5, 0xc2, 0x5f, 0xc0]; // vmaxss xmm0, xmm7, xmm0
    assert_eq!(decode(&code, CODE).unwrap().op, Op::Vfp(FpK::Max, Prec::Ss));
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 3.0f32.to_bits() as u128;
    cpu.xmm[7] = MERGE_SOURCE;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[0] as u32, 8.0f32.to_bits());
    assert_eq!(cpu.xmm[0] >> 32, MERGE_SOURCE >> 32);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn vminss_uses_the_second_source_when_values_match() {
    const CODE: u64 = 0x5000;
    const MERGE_SOURCE: u128 = 0x1122_3344_5566_7788_99aa_bbcc_4040_0000; // 3.0f
    let code = [0xc5, 0xc2, 0x5d, 0xc0]; // vminss xmm0, xmm7, xmm0
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 3.0f32.to_bits() as u128;
    cpu.xmm[7] = MERGE_SOURCE;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[0] as u32, 3.0f32.to_bits());
    assert_eq!(cpu.xmm[0] >> 32, MERGE_SOURCE >> 32);
}
