use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vbroadcastf128_duplicates_a_memory_lane_across_ymm() {
    const CODE: u64 = 0x2f000;
    const DATA: u64 = 0x30000;
    let instruction = [0xc4, 0xe2, 0x7d, 0x1a, 0x08]; // vbroadcastf128 ymm1, [rax]

    let decoded = decode(&instruction, CODE).unwrap();
    assert_eq!(format!("{:?}", decoded.op), "Vbroadcastf128");
    assert_eq!(decoded.osz, 32);
    assert_eq!(decoded.ops[0], Opnd::Xmm(1));
    assert!(matches!(decoded.ops[2], Opnd::Mem(_)));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &instruction).unwrap();
    let source = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdefu128;
    mem.write_force(DATA, &source.to_le_bytes()).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[1] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    cpu.ymm_hi[1] = 0x9999_aaaa_bbbb_cccc_ddddeeeeffff0000;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::OF | anywasm_cpu::cpu::ZF;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1], source);
    assert_eq!(cpu.ymm_hi[1], source);
    assert_eq!(cpu.rflags, flags);
}
