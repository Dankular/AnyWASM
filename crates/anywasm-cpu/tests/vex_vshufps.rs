use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

#[test]
fn vex_vshufps_uses_vvvv_and_modrm_sources() {
    const CODE: u64 = 0x21000;
    let code = [0xc5, 0xd0, 0xc6, 0xd5, 0x55]; // vshufps xmm2, xmm5, xmm5, 0x55
    let instruction = decode(&code, CODE).unwrap();
    assert_eq!(instruction.op, Op::Vshufps);
    assert_eq!(instruction.osz, 16);
    assert_eq!(instruction.ops[0], Opnd::Xmm(2));
    assert_eq!(instruction.ops[1], Opnd::Xmm(5));
    assert_eq!(instruction.ops[2], Opnd::Xmm(5));
    assert_eq!(instruction.imm, 0x55);

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[2] = u128::MAX;
    cpu.xmm[5] = (4u128 << 96) | (3u128 << 64) | (2u128 << 32) | 1;
    cpu.ymm_hi[2] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[2], (2u128 << 96) | (2u128 << 64) | (2u128 << 32) | 2);
    assert_eq!(cpu.ymm_hi[2], 0);
}
