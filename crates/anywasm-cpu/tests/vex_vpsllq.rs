use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_vpsllq_shifts_each_ymm_qword_and_preserves_flags() {
    const CODE: u64 = 0x27000;
    let code = [0xc5, 0xed, 0x73, 0xf3, 0x04]; // vpsllq ymm3, ymm2, 4
    let instruction = decode(&code, CODE).unwrap();
    assert_eq!(format!("{:?}", instruction.op), "Vpsllq");
    assert_eq!(instruction.ops[0], Opnd::Xmm(3));
    assert_eq!(instruction.ops[1], Opnd::Xmm(2));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[2] = (0x8000_0000_0000_0000u128 << 64) | 1;
    cpu.ymm_hi[2] = (0x0fu128 << 64) | 0x0123_4567_89ab_cdef;
    cpu.xmm[3] = u128::MAX;
    cpu.ymm_hi[3] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[3], 16u128 | (0u128 << 64));
    assert_eq!(cpu.ymm_hi[3], 0x1234_5678_9abc_def0u128 | (0xf0u128 << 64));
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn vex_vpsrlq_variable_register_and_memory_counts_shift_each_ymm_qword() {
    const CODE: u64 = 0x28000;
    const DATA: u64 = 0x29000;
    let register_count = [0xc5, 0xed, 0xd3, 0xd9]; // vpsrlq ymm3, ymm2, xmm1
    let memory_count = [0xc4, 0xe1, 0x6d, 0xd3, 0x18]; // vpsrlq ymm3, ymm2, [rax]

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &register_count).unwrap();
    mem.write_force(CODE + register_count.len() as u64, &memory_count)
        .unwrap();
    mem.write_force(DATA, &4u64.to_le_bytes()).unwrap();
    mem.write_force(DATA + 8, &0xfeed_face_cafe_beefu64.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[1] = 4;
    cpu.xmm[2] = 0x8000_0000_0000_0000_0123_4567_89ab_cdef;
    cpu.ymm_hi[2] = 0xffff_ffff_ffff_ffff_0000_0000_0000_000f;
    cpu.ymm_hi[3] = u128::MAX;
    cpu.rflags = 0x8d7;

    step(&mut cpu, &mut mem).unwrap();
    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[3], 0x0800_0000_0000_0000_0012_3456_789a_bcde);
    assert_eq!(cpu.ymm_hi[3], 0x0fff_ffff_ffff_ffff_0000_0000_0000_0000);
    assert_eq!(cpu.rflags, 0x8d7);
}
