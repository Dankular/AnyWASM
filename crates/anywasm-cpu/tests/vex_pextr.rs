use anywasm_cpu::cpu::{Cpu, RAX, RCX};
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn target_vpextrb_decodes_extended_gpr_and_lane_13() {
    let instruction = decode(&[0xc4, 0xc3, 0x79, 0x14, 0xc1, 0x0d], 0x1000).unwrap();

    assert_eq!(instruction.op, Op::Pextr(1));
    assert_eq!(instruction.len, 6);
    assert_eq!(instruction.ops[0], Opnd::Gpr(9));
    assert_eq!(instruction.ops[1], Opnd::Xmm(0));
    assert_eq!(instruction.imm, 13);
}

#[test]
fn vpextrw_decodes_word_source_and_lane() {
    let instruction = decode(&[0xc4, 0xe3, 0x79, 0x15, 0xc1, 0x03], 0x1000).unwrap();

    assert_eq!(instruction.op, Op::Pextr(2));
    assert_eq!(instruction.ops[0], Opnd::Gpr(1));
    assert_eq!(instruction.ops[1], Opnd::Xmm(0));
    assert_eq!(instruction.imm, 3);
}

#[test]
fn vpextrb_writes_selected_byte_to_extended_gpr_with_zero_extension() {
    const CODE: u64 = 0x1000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xc3, 0x79, 0x14, 0xc1, 0x0d])
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = u128::from_le_bytes([
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ]);
    cpu.gpr[9] = u64::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[9], 0x0d);
}

#[test]
fn vpextrb_writes_selected_byte_to_memory() {
    const CODE: u64 = 0x2000;
    const DATA: u64 = 0x3000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xe3, 0x79, 0x14, 0x44, 0x08, 0x10, 0x0d])
        .unwrap(); // vpextrb [rax+rcx+0x10], xmm0, 13

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.gpr[RCX] = 0x20;
    cpu.xmm[0] = u128::from_le_bytes([
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ]);

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(mem.read_u8(DATA + 0x30).unwrap(), 0x0d);
}
