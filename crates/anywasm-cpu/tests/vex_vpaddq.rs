use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_vpaddq_adds_qword_lanes_and_zeros_upper_ymm_on_xmm_form() {
    const CODE: u64 = 0x28000;
    let code = [
        0xc5, 0xe5, 0xd4, 0xc4, // vpaddq ymm0, ymm3, ymm4
        0xc5, 0xe1, 0xd4, 0xc4, // vpaddq xmm0, xmm3, xmm4
    ];
    let decoded = decode(&code, CODE);
    assert_eq!(decoded.as_ref().map(|i| (i.op, i.osz)), Ok((Op::Vpaddq, 32)));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[3] = u64::MAX as u128 | ((5u128) << 64);
    cpu.ymm_hi[3] = 0x1234_5678_9abc_def0 | ((8u128) << 64);
    cpu.xmm[4] = 1 | ((7u128) << 64);
    cpu.ymm_hi[4] = u64::MAX as u128 | ((9u128) << 64);
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 12u128 << 64);
    assert_eq!(cpu.ymm_hi[0], 0x1234_5678_9abc_deefu128 | (17u128 << 64));
    assert_eq!(cpu.rflags, flags);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 12u128 << 64);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, flags);
}
