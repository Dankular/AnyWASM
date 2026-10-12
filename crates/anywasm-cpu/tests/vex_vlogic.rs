use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

const CODE: u64 = 0x31000;

fn logic(kind: FpK, first: u128, second: u128) -> u128 {
    match kind {
        FpK::And => first & second,
        FpK::Andn => !first & second,
        FpK::Or => first | second,
        FpK::Xor => first ^ second,
        _ => unreachable!(),
    }
}

#[test]
fn vex_vector_logical_forms_support_xmm_ymm_and_extended_registers() {
    let target_encoding = [0xc4, 0xc1, 0x78, 0x54, 0xc2]; // vandps xmm0, xmm0, xmm10
    let target = decode(&target_encoding, CODE).unwrap();
    assert_eq!(target.op, Op::Vfp(FpK::And, Prec::Ps));
    assert_eq!(target.ops[0], Opnd::Xmm(0));
    assert_eq!(target.ops[1], Opnd::Xmm(0));
    assert_eq!(target.ops[2], Opnd::Xmm(10));

    let operations = [
        (0x54, FpK::And),
        (0x55, FpK::Andn),
        (0x56, FpK::Or),
        (0x57, FpK::Xor),
    ];
    let mut code = target_encoding.to_vec();
    for (pp, precision) in [(0, Prec::Ps), (1, Prec::Pd)] {
        for (opcode, kind) in operations {
            for l in [false, true] {
                let vex3 = 0x68 | ((l as u8) << 2) | pp;
                code.extend_from_slice(&[0xc4, 0xe1, vex3, opcode, 0xc1]);
                let decoded = decode(&code[code.len() - 5..], CODE).unwrap();
                assert_eq!(decoded.op, Op::Vfp(kind, precision));
                assert_eq!(decoded.osz, if l { 32 } else { 16 });
                assert_eq!(decoded.ops[0], Opnd::Xmm(0));
                assert_eq!(decoded.ops[1], Opnd::Xmm(2));
                assert_eq!(decoded.ops[2], Opnd::Xmm(1));
            }
        }
    }

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 0xf0f0_0ff0_aaaa_5555_1234_5678_9abc_def0;
    cpu.xmm[10] = 0x0ff0_f0ff_5555_aaaa_fedc_ba98_7654_3210;
    cpu.xmm[2] = 0x8877_6655_4433_2211_0123_4567_89ab_cdef;
    cpu.ymm_hi[2] = 0x1020_3040_5060_7080_90a0_b0c0_d0e0_f000;
    cpu.xmm[1] = 0xff00_ffff_0f0f_f0f0_ffff_0000_aaaa_5555;
    cpu.ymm_hi[1] = 0x0f0f_0f0f_f0f0_f0f0_ffff_ffff_1111_1111;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;
    let source_first = cpu.xmm[2];
    let source_first_hi = cpu.ymm_hi[2];
    let source_second = cpu.xmm[1];
    let source_second_hi = cpu.ymm_hi[1];
    let target_first = cpu.xmm[0];
    let target_second = cpu.xmm[10];

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], target_first & target_second);
    assert_eq!(cpu.ymm_hi[0], 0);
    cpu.xmm[0] = 0;

    for pp in [0, 1] {
        for (opcode, kind) in operations {
            for l in [false, true] {
                let first = if l { source_first_hi } else { source_first };
                let second = if l { source_second_hi } else { source_second };
                step(&mut cpu, &mut mem).unwrap();
                assert_eq!(cpu.xmm[0], logic(kind, source_first, source_second));
                assert_eq!(
                    cpu.ymm_hi[0],
                    if l {
                        logic(kind, source_first_hi, source_second_hi)
                    } else {
                        0
                    },
                    "opcode={opcode:#x}, pp={pp}, l={l}, first={first:#x}, second={second:#x}"
                );
                cpu.xmm[0] = 0;
                cpu.ymm_hi[0] = u128::MAX;
            }
        }
    }
    assert_eq!(cpu.rflags, flags);
}
