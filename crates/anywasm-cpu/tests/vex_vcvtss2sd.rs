use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_vcvtss2sd_converts_low_f32_and_merges_high_xmm_lane() {
    const CODE: u64 = 0x20000;
    const MERGE_HIGH: u64 = 0x0123_4567_89ab_cdef;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc5, 0xfa, 0x5a, 0xc0])
        .unwrap(); // vcvtss2sd xmm0, xmm0, xmm0

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = ((MERGE_HIGH as u128) << 64) | 1.5f32.to_bits() as u128;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    let result = step(&mut cpu, &mut mem);
    assert!(result.is_ok(), "step failed at rip {:#x}: {result:?}", cpu.rip);
    assert_eq!(cpu.xmm[0] as u64, 1.5f64.to_bits());
    assert_eq!((cpu.xmm[0] >> 64) as u64, MERGE_HIGH);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, flags);
}
