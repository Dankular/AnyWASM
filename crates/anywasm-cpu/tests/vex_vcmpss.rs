use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

const CODE: u64 = 0x2b000;

#[test]
fn target_vcmpless_predicate_compares_source_registers_in_order() {
    let code = [0xc5, 0xea, 0xc2, 0xc8, 0x02]; // vcmpless xmm1, xmm2, xmm0, 2
    let instruction = decode(&code, CODE).unwrap();

    assert_eq!(instruction.ops[0], Opnd::Xmm(1));
    assert_eq!(instruction.ops[1], Opnd::Xmm(2));
    assert_eq!(instruction.ops[2], Opnd::Xmm(0));
    assert_eq!(instruction.imm, 2);

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[2] = 0xfeed_face_cafe_babe_0123_4567_3f80_0000;
    cpu.xmm[0] = 0xaaaa_bbbb_cccc_dddd_eeee_ffff_4000_0000;
    cpu.ymm_hi[1] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1] as u32, u32::MAX);
    assert_eq!(cpu.xmm[1] >> 32, cpu.xmm[2] >> 32);
    assert_eq!(cpu.ymm_hi[1], 0);
}

#[test]
fn vcmpss_implements_all_32_predicates() {
    const EXPECTED: [[bool; 4]; 32] = [
        [true, false, false, false],
        [false, true, false, false],
        [true, true, false, false],
        [false, false, false, true],
        [false, true, true, true],
        [true, false, true, true],
        [false, false, true, true],
        [true, true, true, false],
        [true, false, false, true],
        [false, true, false, true],
        [true, true, false, true],
        [false, false, false, false],
        [false, true, true, false],
        [true, false, true, false],
        [false, false, true, false],
        [true, true, true, true],
        [true, false, false, false],
        [false, true, false, false],
        [true, true, false, false],
        [false, false, false, true],
        [false, true, true, true],
        [true, false, true, true],
        [false, false, true, true],
        [true, true, true, false],
        [true, false, false, true],
        [false, true, false, true],
        [true, true, false, true],
        [false, false, false, false],
        [false, true, true, false],
        [true, false, true, false],
        [false, false, true, false],
        [true, true, true, true],
    ];
    let inputs = [
        (0x3f80_0000u32, 0x3f80_0000u32), // equal
        (0x3f80_0000, 0x4000_0000),       // less
        (0x4000_0000, 0x3f80_0000),       // greater
        (0x7fc0_0000, 0x3f80_0000),       // unordered
    ];
    let mut code = Vec::with_capacity(32 * 5);
    for predicate in 0u8..32 {
        code.extend_from_slice(&[0xc5, 0xea, 0xc2, 0xc8, predicate | 0xe0]);
    }

    for (relation, (first, second)) in inputs.into_iter().enumerate() {
        let mut mem = Memory::new();
        assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
        mem.write_force(CODE, &code).unwrap();
        let mut cpu = Cpu::new();
        cpu.rip = CODE;
        let source_one = 0xfeed_face_cafe_babe_0123_4567_0000_0000u128 | first as u128;
        cpu.xmm[2] = source_one;
        cpu.xmm[0] = second as u128;
        cpu.ymm_hi[1] = u128::MAX;
        let flags = cpu.rflags;

        for predicate in 0..32 {
            step(&mut cpu, &mut mem).unwrap();
            let expected = if EXPECTED[predicate][relation] {
                u32::MAX
            } else {
                0
            };
            assert_eq!(cpu.xmm[1] as u32, expected, "predicate {predicate}");
            assert_eq!(cpu.xmm[1] >> 32, source_one >> 32, "predicate {predicate}");
            assert_eq!(cpu.ymm_hi[1], 0, "predicate {predicate}");
        }
        assert_eq!(cpu.rflags, flags);
    }
}
