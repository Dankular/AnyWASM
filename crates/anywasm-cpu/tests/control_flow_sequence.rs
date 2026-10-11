use anywasm_cpu::cpu::{RAX, RBX, RCX, ZF};
use anywasm_cpu::mem::PROT_RWX;
use anywasm_cpu::{step, Cpu, Memory};

const CODE: u64 = 0x1000_0000;

fn run_corpus_sequence(assembly: &str) -> Cpu {
    let line = include_str!("corpus.txt")
        .lines()
        .find(|line| line.ends_with(assembly))
        .expect("the differential control-flow sequence is present in the corpus");
    let hex = line.split_once('|').unwrap().0;
    let code: Vec<u8> = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();

    let mut mem = Memory::new();
    mem.map(CODE, 0x1000, PROT_RWX);
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    let end = CODE + code.len() as u64;
    let mut steps = 0;
    while cpu.rip != end {
        step(&mut cpu, &mut mem).unwrap();
        steps += 1;
        assert!(steps <= 32, "control-flow sequence did not reach its end");
    }

    cpu
}

#[test]
fn loop_and_conditional_branch_sequence_matches_its_expected_path() {
    let cpu = run_corpus_sequence(
        "|mov ecx, 3; xor eax, eax; add eax, 1; loop -5; cmp eax, 3; jne +2; inc eax",
    );

    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.gpr[RAX], 4);
    assert!(!cpu.flag(ZF));
}

#[test]
fn conditional_branch_sequence_takes_jne_for_unequal_values() {
    let cpu = run_corpus_sequence(
        "|mov ecx, 3; xor eax, eax; add eax, 1; loop -5; cmp eax, 4; jne +5; mov ebx, 4",
    );

    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.gpr[RAX], 3);
    assert_eq!(cpu.gpr[RBX], 0, "taken JNE skips mov ebx, 4");
    assert!(!cpu.flag(ZF));
}
