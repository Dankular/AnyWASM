use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vcvtss2sd_merges_the_upper_qword_from_vvvv() {
    const CODE: u64 = 0x1000;
    const MERGE_HIGH: u64 = 0x0123_4567_89ab_cdef;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc5, 0xf2, 0x5a, 0xc2]).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = u128::MAX;
    cpu.xmm[1] = (MERGE_HIGH as u128) << 64 | 0xdead_beef_cafe_babe;
    cpu.xmm[2] = 0xfedc_ba98_7654_3210_0123_4567_3fc0_0000; // 1.5f
    cpu.ymm_hi[0] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[0] as u64, 1.5f64.to_bits());
    assert_eq!((cpu.xmm[0] >> 64) as u64, MERGE_HIGH);
    assert_eq!(cpu.ymm_hi[0], 0);
}

#[test]
fn vcvtsd2ss_merges_the_upper_96_bits_from_vvvv() {
    const CODE: u64 = 0x2000;
    const MERGE_HIGH: u128 = 0x1122_3344_5566_7788_99aa_bbcc;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc5, 0x63, 0x5a, 0xc2]).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[8] = u128::MAX;
    cpu.xmm[2] = 0xfedc_ba98_7654_3210_c004_0000_0000_0000; // -2.5
    cpu.xmm[3] = MERGE_HIGH << 32 | 0x3f80_0000;
    cpu.ymm_hi[8] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[8] as u32, (-2.5f32).to_bits());
    assert_eq!(cpu.xmm[8] >> 32, MERGE_HIGH);
    assert_eq!(cpu.ymm_hi[8], 0);
}
