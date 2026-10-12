use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_vinserti128_replaces_selected_ymm_half() {
    const CODE: u64 = 0x29000;
    let code = [
        0xc4, 0xe3, 0x7d, 0x39, 0xc2, 0x01, // vinserti128 ymm0, ymm0, xmm2, 1
        0xc4, 0xe3, 0x7d, 0x39, 0xc2, 0x00, // vinserti128 ymm0, ymm0, xmm2, 0
    ];
    let decoded = decode(&code, CODE);
    assert_eq!(decoded.as_ref().map(|i| (i.op, i.osz)), Ok((Op::Vinserti128, 32)));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    cpu.ymm_hi[0] = 0x9999_aaaa_bbbb_cccc_ddddeeeeffff0000;
    cpu.xmm[2] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 0x1111_2222_3333_4444_5555_6666_7777_8888);
    assert_eq!(cpu.ymm_hi[0], cpu.xmm[2]);
    assert_eq!(cpu.rflags, flags);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], cpu.xmm[2]);
    assert_eq!(cpu.ymm_hi[0], cpu.xmm[2]);
    assert_eq!(cpu.rflags, flags);
}
