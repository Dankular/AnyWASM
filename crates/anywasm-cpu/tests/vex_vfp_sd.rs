use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_scalar_double_arithmetic_opcodes_decode_with_three_sources() {
    for (opcode, kind) in [
        (0x51, FpK::Sqrt),
        (0x58, FpK::Add),
        (0x59, FpK::Mul),
        (0x5c, FpK::Sub),
        (0x5d, FpK::Min),
        (0x5e, FpK::Div),
        (0x5f, FpK::Max),
    ] {
        let instruction = decode(&[0xc4, 0xe1, 0xfb, opcode, 0xc1], 0x1000)
            .unwrap_or_else(|error| panic!("opcode {opcode:#04x} failed to decode: {error}"));

        assert_eq!(instruction.op, Op::Vfp(kind, Prec::Sd));
        assert_eq!(instruction.ops[0], Opnd::Xmm(0));
        assert_eq!(instruction.ops[1], Opnd::Xmm(0));
        assert_eq!(instruction.ops[2], Opnd::Xmm(1));
    }
}

#[test]
fn vex_vdivsd_uses_eight_byte_source_and_merges_upper_xmm_lane() {
    const CODE: u64 = 0x1e000;
    const MERGE_HIGH: u64 = 0x0123_4567_89ab_cdef;
    let code = [0xc4, 0x41, 0x7b, 0x5e, 0xc1]; // vdivsd xmm8, xmm0, xmm9
    let instruction = decode(&code, CODE).unwrap();
    assert_eq!(instruction.op, Op::Vfp(FpK::Div, Prec::Sd));
    assert_eq!(instruction.ops[0], Opnd::Xmm(8));
    assert_eq!(instruction.ops[1], Opnd::Xmm(0));
    assert_eq!(instruction.ops[2], Opnd::Xmm(9));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = ((MERGE_HIGH as u128) << 64) | 10.0f64.to_bits() as u128;
    cpu.xmm[8] = u128::MAX;
    cpu.xmm[9] = 0xfedc_ba98_7654_3210_4000_0000_0000_0000;
    cpu.ymm_hi[8] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[8] as u64, 5.0f64.to_bits());
    assert_eq!((cpu.xmm[8] >> 64) as u64, MERGE_HIGH);
    assert_eq!(cpu.ymm_hi[8], 0);
    assert_eq!(cpu.rflags, flags);
}
