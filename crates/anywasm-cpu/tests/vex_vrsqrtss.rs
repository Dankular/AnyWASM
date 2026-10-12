use anywasm_cpu::cpu::{Cpu, RDX};
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

const CODE: u64 = 0x2f000;
const DATA: u64 = 0x30000;

#[test]
fn vex_vrsqrtss_uses_scalar_source_and_preserves_merge_lanes() {
    let register = [0xc5, 0xf2, 0x52, 0xc2]; // vrsqrtss xmm0, xmm1, xmm2
    let memory = [0xc5, 0xf2, 0x52, 0x02]; // vrsqrtss xmm0, xmm1, dword ptr [rdx]
    let decoded = decode(&register, CODE).unwrap();
    assert_eq!(decoded.op, Op::Vfp(FpK::Rsqrt, Prec::Ss));
    assert_eq!(decoded.ops[0], Opnd::Xmm(0));
    assert_eq!(decoded.ops[1], Opnd::Xmm(1));
    assert_eq!(decoded.ops[2], Opnd::Xmm(2));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &register).unwrap();
    mem.write_force(CODE + register.len() as u64, &memory).unwrap();
    mem.write_u32(DATA, 9.0f32.to_bits()).unwrap();

    let merge = 0x1122_3344_5566_7788_99aa_bbcc_0000_0000u128 | 0x3f80_0000;
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.rflags = 0x8d7;
    cpu.gpr[RDX] = DATA;
    cpu.xmm[1] = merge;
    cpu.xmm[2] = 4.0f32.to_bits() as u128;
    cpu.ymm_hi[0] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0] as u32, 0.5f32.to_bits());
    assert_eq!(cpu.xmm[0] >> 32, merge >> 32);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, 0x8d7);

    step(&mut cpu, &mut mem).unwrap();
    let result = f32::from_bits(cpu.xmm[0] as u32);
    assert!((result - (1.0 / 3.0)).abs() < 1e-6, "{result}");
    assert_eq!(cpu.xmm[0] >> 32, merge >> 32);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, 0x8d7);
}
