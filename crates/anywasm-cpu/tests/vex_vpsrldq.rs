use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_byte_shifts_apply_per_128_bit_lane_and_clear_upper_for_xmm() {
    const CODE: u64 = 0x2a000;
    const XMM: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    const YMM_HI: u128 = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
    let code = [
        0xc5, 0xf9, 0x73, 0xd8, 0x08, // vpsrldq xmm0, xmm0, 8
        0xc5, 0xf9, 0x73, 0xf8, 0x08, // vpslldq xmm0, xmm0, 8
        0xc5, 0xfd, 0x73, 0xd8, 0x04, // vpsrldq ymm0, ymm0, 4
        0xc5, 0xfd, 0x73, 0xf8, 0x04, // vpslldq ymm0, ymm0, 4
    ];
    let distinct_source = [
        0xc5, 0xe1, 0x73, 0xd8, 0x08, // vpsrldq xmm3, xmm0, 8
    ];
    let decoded = decode(&distinct_source, CODE).unwrap();
    assert_eq!(decoded.ops[0], Opnd::Xmm(3));
    assert_eq!(decoded.ops[1], Opnd::Xmm(0));
    let decoded = decode(&code, CODE).unwrap();
    assert_eq!((decoded.op, decoded.osz), (Op::Vpsrldq, 16));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = XMM;
    cpu.ymm_hi[0] = YMM_HI;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], XMM >> 64);
    assert_eq!(cpu.ymm_hi[0], 0);

    cpu.xmm[0] = XMM;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], XMM << 64);
    assert_eq!(cpu.ymm_hi[0], 0);

    cpu.xmm[0] = XMM;
    cpu.ymm_hi[0] = YMM_HI;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], XMM >> 32);
    assert_eq!(cpu.ymm_hi[0], YMM_HI >> 32);

    cpu.xmm[0] = XMM;
    cpu.ymm_hi[0] = YMM_HI;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], XMM << 32);
    assert_eq!(cpu.ymm_hi[0], YMM_HI << 32);
    assert_eq!(cpu.rflags, flags);
}
