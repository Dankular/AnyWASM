use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};

#[test]
fn vex_scalar_single_arithmetic_opcodes_decode() {
    for (opcode, kind) in [
        (0x51, FpK::Sqrt),
        (0x58, FpK::Add),
        (0x59, FpK::Mul),
        (0x5c, FpK::Sub),
        (0x5d, FpK::Min),
        (0x5e, FpK::Div),
        (0x5f, FpK::Max),
    ] {
        let instruction = decode(&[0xc5, 0xfa, opcode, 0xc1], 0x1000)
            .unwrap_or_else(|error| panic!("opcode {opcode:#04x} failed to decode: {error}"));

        assert_eq!(instruction.op, Op::Vfp(kind, Prec::Ss));
        assert_eq!(instruction.ops[0], Opnd::Xmm(0));
        assert_eq!(instruction.ops[1], Opnd::Xmm(0));
        assert_eq!(instruction.ops[2], Opnd::Xmm(1));
    }
}
