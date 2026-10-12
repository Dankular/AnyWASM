use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn target_vmovq_decodes_extended_gpr_source() {
    let instruction = decode(&[0xc4, 0xc1, 0xf9, 0x6e, 0xe1], 0x1000).unwrap();

    assert_eq!(format!("{:?}", instruction.op), "Vmovq");
    assert_eq!(instruction.len, 5);
    assert_eq!(instruction.osz, 16);
    assert_eq!(instruction.ops[0], Opnd::Xmm(4));
    assert_eq!(instruction.ops[1], Opnd::Gpr(9));
}

#[test]
fn vmovq_gpr_and_memory_forms_zero_upper_lanes() {
    const CODE: u64 = 0x1000;
    const DATA: u64 = 0x2000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(
        CODE,
        &[
            0xc4, 0xc1, 0xf9, 0x6e, 0xe1, // vmovq xmm4, r9
            0xc4, 0xe1, 0xf9, 0x6e, 0x08, // vmovq xmm1, [rax]
        ],
    )
    .unwrap();
    mem.write_force(DATA, &0x0123_4567_89ab_cdefu64.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[9] = 0xfedc_ba98_7654_3210;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[4] = u128::MAX;
    cpu.xmm[1] = u128::MAX;
    cpu.ymm_hi[4] = u128::MAX;
    cpu.ymm_hi[1] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[4], 0xfedc_ba98_7654_3210);
    assert_eq!(cpu.xmm[1], 0x0123_4567_89ab_cdef);
    assert_eq!(cpu.ymm_hi[4], 0);
    assert_eq!(cpu.ymm_hi[1], 0);
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn vmovq_store_writes_only_the_low_qword() {
    const CODE: u64 = 0x3000;
    const DATA: u64 = 0x4000;
    let code = [0xc5, 0xf9, 0xd6, 0x42, 0xe0]; // vmovq [rdx-0x20], xmm0
    let decoded = decode(&code, CODE).unwrap();
    assert_eq!(format!("{:?}", decoded.op), "MovqStore");
    assert_eq!(decoded.osz, 8);

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &code).unwrap();
    mem.write_force(DATA, &[0xaa; 16]).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[2] = DATA + 0x20;
    cpu.xmm[0] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    cpu.ymm_hi[0] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(mem.read_u64(DATA).unwrap(), 0x99aa_bbcc_ddee_ff00);
    assert_eq!(mem.read_u64(DATA + 8).unwrap(), 0xaaaa_aaaa_aaaa_aaaa);
}
