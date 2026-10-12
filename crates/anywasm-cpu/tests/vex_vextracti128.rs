use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_extracti128_and_inserti128_use_their_distinct_opcodes() {
    const CODE: u64 = 0x2f000;
    const DATA: u64 = 0x30000;
    let extract_reg = [0xc4, 0xe3, 0x7d, 0x39, 0xc2, 0x01]; // vextracti128 xmm2, ymm0, 1
    let extract_mem = [0xc4, 0xe3, 0x7d, 0x39, 0x08, 0x01]; // vextracti128 [rax], ymm1, 1
    let insert_reg = [0xc4, 0xe3, 0x7d, 0x38, 0xda, 0x01]; // vinserti128 ymm3, ymm0, xmm2, 1

    let decoded = decode(&extract_reg, CODE).unwrap();
    assert_eq!(decoded.op, Op::Vextractf128);
    assert_eq!(decoded.ops[0], Opnd::Xmm(2));
    assert_eq!(decoded.ops[1], Opnd::Xmm(0));
    assert_eq!(decoded.imm, 1);

    let inserted = decode(&insert_reg, CODE).unwrap();
    assert_eq!(inserted.op, Op::Vinserti128);
    assert_eq!(inserted.ops[0], Opnd::Xmm(3));
    assert_eq!(inserted.ops[1], Opnd::Xmm(0));
    assert_eq!(inserted.ops[2], Opnd::Xmm(2));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &extract_reg).unwrap();
    mem.write_force(CODE + 6, &extract_mem).unwrap();
    mem.write_force(CODE + 12, &insert_reg).unwrap();

    let source_low = 0x1111_2222_3333_4444_5555_6666_7777_8888u128;
    let source_high = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdefu128;
    let second_low = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
    let second_high = 0x8877_6655_4433_2211_ffeeddccbbaa0099u128;
    let inserted_lane = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100u128;
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = source_low;
    cpu.ymm_hi[0] = source_high;
    cpu.xmm[1] = second_low;
    cpu.ymm_hi[1] = second_high;
    cpu.xmm[2] = inserted_lane;
    cpu.ymm_hi[2] = 0xffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888;
    cpu.gpr[RAX] = DATA;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::OF | anywasm_cpu::cpu::ZF;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[2], source_high);
    assert_eq!(cpu.ymm_hi[2], 0);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 6;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(mem.read_vec(DATA, 16).unwrap(), second_high.to_le_bytes());
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 12;
    cpu.xmm[2] = inserted_lane;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[3], source_low);
    assert_eq!(cpu.ymm_hi[3], inserted_lane);
    assert_eq!(cpu.rflags, flags);
}
