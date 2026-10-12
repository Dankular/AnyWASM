use anywasm_cpu::cpu::{Cpu, RDX};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

const CODE: u64 = 0x2c000;
const DATA: u64 = 0x2d000;

#[test]
fn vex_vcvtpd2ps_converts_xmm_ymm_and_memory_sources() {
    let ignored_w_bit = decode(&[0xc4, 0xe1, 0xfd, 0x5a, 0xc1], CODE).unwrap();
    assert_eq!(ignored_w_bit.osz, 32);
    let code = [
        0xc5, 0xf9, 0x5a, 0xc0, // vcvtpd2ps xmm0, xmm0
        0xc5, 0xfd, 0x5a, 0xc1, // vcvtpd2ps xmm0, ymm1
        0xc5, 0xf9, 0x5a, 0x02, // vcvtpd2ps xmm0, [rdx]
        0xc5, 0xfd, 0x5a, 0x02, // vcvtpd2ps xmm0, [rdx] (256-bit source)
    ];
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &code).unwrap();
    let mut source = Vec::new();
    for value in [1.5f64, -2.25, 3.75, -4.5] {
        source.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    mem.write_force(DATA, &source).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RDX] = DATA;
    cpu.xmm[0] = ((-2.5f64).to_bits() as u128) << 64 | 1.25f64.to_bits() as u128;
    cpu.ymm_hi[0] = u128::MAX;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0] as u32, 1.25f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 32) as u32, (-2.5f32).to_bits());
    assert_eq!(cpu.xmm[0] >> 64, 0);
    assert_eq!(cpu.ymm_hi[0], 0);

    cpu.xmm[1] = ((-4.25f64).to_bits() as u128) << 64 | 3.5f64.to_bits() as u128;
    cpu.ymm_hi[1] = ((-123.75f64).to_bits() as u128) << 64 | 0.125f64.to_bits() as u128;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0] as u32, 3.5f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 32) as u32, (-4.25f32).to_bits());
    assert_eq!((cpu.xmm[0] >> 64) as u32, 0.125f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 96) as u32, (-123.75f32).to_bits());
    assert_eq!(cpu.ymm_hi[0], 0);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0] as u32, 1.5f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 32) as u32, (-2.25f32).to_bits());
    assert_eq!(cpu.xmm[0] >> 64, 0);
    assert_eq!(cpu.ymm_hi[0], 0);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0] as u32, 1.5f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 32) as u32, (-2.25f32).to_bits());
    assert_eq!((cpu.xmm[0] >> 64) as u32, 3.75f32.to_bits());
    assert_eq!((cpu.xmm[0] >> 96) as u32, (-4.5f32).to_bits());
    assert_eq!(cpu.ymm_hi[0], 0);
}
use anywasm_cpu::decode::decode;
