use anywasm_cpu::cpu::{Cpu, RSP};
use anywasm_cpu::decode::decode;
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn target_vinsertps_encoding_decodes() {
    let instruction = decode(&[0xc4, 0xe3, 0x71, 0x21, 0xcb, 0x30], 0x1000)
        .expect("target VINSERTPS encoding must decode");
    assert_eq!(instruction.len, 6);
}

#[test]
fn vinsertps_register_source_merges_vvvv_and_applies_zero_mask() {
    const CODE: u64 = 0x1000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xe3, 0x69, 0x21, 0xcb, 0xb5])
        .unwrap(); // vinsertps xmm1, xmm2, xmm3, 0xb5

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[1] = u128::MAX;
    cpu.xmm[2] = 0x4444_4444_3333_3333_2222_2222_1111_1111;
    cpu.xmm[3] = 0xdddd_dddd_cccc_cccc_bbbb_bbbb_aaaa_aaaa;
    cpu.ymm_hi[1] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1], 0xcccc_cccc_0000_0000_2222_2222_0000_0000);
    assert_eq!(cpu.ymm_hi[1], 0);
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn vinsertps_memory_source_reads_one_dword_and_merges_vvvv() {
    const CODE: u64 = 0x2000;
    const DATA: u64 = 0x3000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xe3, 0x69, 0x21, 0x4c, 0x24, 0x10, 0xb5])
        .unwrap(); // vinsertps xmm1, xmm2, [rsp+0x10], 0xb5
    mem.write_force(DATA + 0x10, &0xaabb_ccddu32.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RSP] = DATA;
    cpu.xmm[1] = u128::MAX;
    cpu.xmm[2] = 0x4444_4444_3333_3333_2222_2222_1111_1111;
    cpu.ymm_hi[1] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1], 0xaabb_ccdd_0000_0000_2222_2222_0000_0000);
    assert_eq!(cpu.ymm_hi[1], 0);
}
