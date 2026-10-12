use anywasm_cpu::cpu::{Cpu, CF, OF, PF, RAX, SF, ZF};
use anywasm_cpu::decode::{decode, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

const CODE: u64 = 0x2d000;
const DATA: u64 = 0x30000;

fn compare(first: f64, second: f64, memory_form: bool, expected: u64) {
    let instruction = if memory_form {
        [0xc5, 0xf9, 0x2f, 0x08] // vcomisd xmm1, qword ptr [rax]
    } else {
        [0xc5, 0xf9, 0x2f, 0xc2] // vcomisd xmm0, xmm2
    };
    let decoded = decode(&instruction, CODE).unwrap();
    assert!(format!("{:?}", decoded.op).starts_with("Comi(Sd, true)"));
    assert_eq!(decoded.ops[0], Opnd::Xmm(if memory_form { 1 } else { 0 }));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &instruction).unwrap();
    mem.write_u64(DATA, second.to_bits()).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[if memory_form { 1 } else { 0 }] = first.to_bits() as u128;
    cpu.xmm[2] = second.to_bits() as u128;
    cpu.rflags |= CF | PF | ZF | OF | SF | (1 << 4);
    let preserved = cpu.rflags & !(CF | PF | ZF | OF | SF | (1 << 4));

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.rflags & (CF | PF | ZF), expected);
    assert_eq!(cpu.rflags & (OF | SF | (1 << 4)), 0);
    assert_eq!(cpu.rflags & !(CF | PF | ZF | OF | SF | (1 << 4)), preserved);
}

#[test]
fn vcomisd_sets_flags_for_ordered_and_unordered_values() {
    for memory_form in [false, true] {
        compare(2.0, 1.0, memory_form, 0);
        compare(1.0, 2.0, memory_form, CF);
        compare(1.0, 1.0, memory_form, ZF);
        compare(f64::NAN, 1.0, memory_form, ZF | PF | CF);
    }
}
