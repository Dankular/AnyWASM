use anywasm_cpu::cpu::{AF, CF, OF, PF, RAX, RCX, RDX, SF, ZF};
use anywasm_cpu::mem::{PAGE_SIZE, PROT_RWX};
use anywasm_cpu::{step, Cpu, Memory};

const CODE: u64 = 0x1000_0000;
const FLAGS: u64 = CF | PF | AF | ZF | SF | OF | 2;

fn machine(code: &[u8]) -> (Cpu, Memory) {
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    (cpu, mem)
}

#[test]
fn bextr_32_extracts_the_requested_field_and_handles_out_of_range_control() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0x68, 0xf7, 0xc1]); // bextr eax, ecx, edx
    cpu.gpr[RCX] = 0x1234_5678;
    cpu.gpr[RDX] = (12 << 8) | 4;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0x567);
    assert!(!cpu.flag(CF));
    assert!(!cpu.flag(OF));
    assert!(!cpu.flag(ZF));

    cpu.rip = CODE;
    cpu.gpr[RDX] = (8 << 8) | 32;
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0);
    assert!(cpu.flag(ZF));
}

#[test]
fn bextr_64_extracts_across_the_low_dword_boundary() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0xf8, 0xf7, 0xd1]); // bextr rdx, rcx, rax
    cpu.gpr[RCX] = 0x0123_4567_89ab_cdef;
    cpu.gpr[RAX] = (16 << 8) | 32;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RDX], 0x4567);
}

#[test]
fn blsr_clears_the_lowest_set_bit_and_sets_zero_input_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0x68, 0xf3, 0xc8]); // blsr edx, eax
    cpu.gpr[RAX] = 0x58;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RDX], 0x50);
    assert!(!cpu.flag(CF));
    assert!(!cpu.flag(OF));
    assert!(!cpu.flag(SF));
    assert!(!cpu.flag(ZF));

    cpu.rip = CODE;
    cpu.gpr[RAX] = 0;
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RDX], 0);
    assert!(cpu.flag(CF));
    assert!(!cpu.flag(OF));
    assert!(!cpu.flag(SF));
    assert!(cpu.flag(ZF));
}

#[test]
fn blsmsk_sets_bits_through_the_lowest_set_bit_and_handles_zero() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0x68, 0xf3, 0xd0]); // blsmsk edx, eax
    cpu.gpr[RAX] = 0x58;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RDX], 0x0f);
    assert!(!cpu.flag(CF));
    assert!(!cpu.flag(OF));
    assert!(!cpu.flag(SF));
    assert!(!cpu.flag(ZF));

    cpu.rip = CODE;
    cpu.gpr[RAX] = 0;
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RDX], u32::MAX as u64);
    assert!(cpu.flag(CF));
    assert!(!cpu.flag(OF));
    assert!(cpu.flag(SF));
    assert!(!cpu.flag(ZF));
}

#[test]
fn tzcnt_counts_low_dword_trailing_zeros_and_reports_zero_input() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0x0f, 0xbc, 0xc1]); // tzcnt eax, ecx
    cpu.gpr[RCX] = 0x0000_0001_0000_1000;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 12);
    assert!(!cpu.flag(CF));
    assert!(!cpu.flag(ZF));

    cpu.rip = CODE;
    cpu.gpr[RCX] = 0x1_0000_0000;
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 32);
    assert!(cpu.flag(CF));
    assert!(!cpu.flag(ZF));
}

#[test]
fn pdep_deposits_source_bits_into_the_selected_mask_positions_without_changing_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0x6b, 0xf5, 0xc1]); // pdep eax, edx, ecx
    cpu.gpr[RDX] = 0x5;
    cpu.gpr[RCX] = 0x94;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0x84);
    assert_eq!(cpu.rflags, FLAGS);
}

#[test]
fn pext_collects_source_bits_selected_by_the_mask_without_changing_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0x6a, 0xf5, 0xc1]); // pext eax, edx, ecx
    cpu.gpr[RDX] = 0xd6;
    cpu.gpr[RCX] = 0x94;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0x7);
    assert_eq!(cpu.rflags, FLAGS);
}

#[test]
fn mulx_64_returns_both_halves_and_preserves_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0xeb, 0xf6, 0xc1]); // mulx rax, rdx, rcx
    cpu.gpr[RDX] = u64::MAX;
    cpu.gpr[RCX] = 2;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 1);
    assert_eq!(cpu.gpr[RDX], 0xffff_ffff_ffff_fffe);
    assert_eq!(cpu.rflags, FLAGS);
}

#[test]
fn rorx_rotates_64_bits_without_changing_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe3, 0xfb, 0xf0, 0xc1, 8]); // rorx rax, rcx, 8
    cpu.gpr[RCX] = 0x1234_5678_9abc_def0;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0xf012_3456_789a_bcde);
    assert_eq!(cpu.rflags, FLAGS);
}

#[test]
fn sarx_64_sign_extends_the_source_and_preserves_flags() {
    let (mut cpu, mut mem) = machine(&[0xc4, 0xe2, 0xea, 0xf7, 0xc1]); // sarx rax, rcx, rdx
    cpu.gpr[RCX] = 0x8000_0000_0000_0080;
    cpu.gpr[RDX] = 4;
    cpu.rflags = FLAGS;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 0xf800_0000_0000_0008);
    assert_eq!(cpu.rflags, FLAGS);
}
