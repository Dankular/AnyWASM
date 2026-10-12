use anywasm_cpu::cpu::{Cpu, RDX};
use anywasm_cpu::decode::{decode, FpK, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

const CODE: u64 = 0x31000;
const DATA: u64 = 0x32000;

#[test]
fn vex_vandps_handles_register_memory_and_ymm_sources() {
    let code = [
        0xc4, 0xc1, 0x78, 0x54, 0xc2, // vandps xmm0, xmm0, xmm10
        0xc4, 0xe1, 0x78, 0x54, 0x02, // vandps xmm0, xmm0, [rdx]
        0xc4, 0xe1, 0x7c, 0x54, 0xc1, // vandps ymm0, ymm0, ymm1
    ];
    let decoded = decode(&code[..5], CODE).unwrap();
    assert_eq!(decoded.op, Op::Vfp(FpK::And, Prec::Ps));
    assert_eq!(decoded.osz, 16);
    assert_eq!(decoded.ops[0], Opnd::Xmm(0));
    assert_eq!(decoded.ops[1], Opnd::Xmm(0));
    assert_eq!(decoded.ops[2], Opnd::Xmm(10));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &code).unwrap();
    mem.write_force(DATA, &0xf0f0_00ff_f00f_0ff0_aaaa_5555_cccc_3333u128.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.rflags = 0x8d7;
    cpu.gpr[RDX] = DATA;
    cpu.xmm[0] = 0xffff_ffff_ffff_ffff_ffff_0000_0f0f_f0f0;
    cpu.xmm[10] = 0x00ff_0f0f_f0f0_ffff_0f0f_ffff_3333_cccc;
    cpu.ymm_hi[0] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 0x00ff_0f0f_f0f0_ffff_0f0f_0000_0303_c0c0);
    assert_eq!(cpu.ymm_hi[0], 0);

    step(&mut cpu, &mut mem).unwrap();
    let memory_value = u128::from_le_bytes(
        mem.read_vec(DATA, 16).unwrap().try_into().unwrap(),
    );
    assert_eq!(cpu.xmm[0], 0x00ff_0f0f_f0f0_ffff_0f0f_0000_0303_c0c0 & memory_value);
    assert_eq!(cpu.ymm_hi[0], 0);

    cpu.xmm[0] = 0xaaaa_5555_ffff_0000_0f0f_f0f0_3333_cccc;
    cpu.ymm_hi[0] = 0xcccc_3333_aaaa_5555_ffff_ffff_0000_0f0f;
    cpu.xmm[1] = 0x5555_ffff_0f0f_ffff_3333_cccc_ffff_0000;
    cpu.ymm_hi[1] = 0x3333_cccc_ffff_0f0f_aaaa_5555_ffff_0000;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 0xaaaa_5555_ffff_0000_0f0f_f0f0_3333_cccc & cpu.xmm[1]);
    assert_eq!(cpu.ymm_hi[0], 0xcccc_3333_aaaa_5555_ffff_ffff_0000_0f0f & cpu.ymm_hi[1]);
    assert_eq!(cpu.rflags, 0x8d7);
}
