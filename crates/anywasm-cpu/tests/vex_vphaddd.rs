use anywasm_cpu::cpu::{Cpu, RAX};
use anywasm_cpu::decode::{decode, Op, Opnd, PiK};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn vex_vphaddd_obeys_128_bit_lane_boundaries_for_xmm_ymm_and_memory() {
    const CODE: u64 = 0x2f000;
    const DATA: u64 = 0x30000;
    let code = [
        0xc4, 0xe2, 0x7d, 0x02, 0xc8, // vphaddd ymm1, ymm0, ymm0
        0xc4, 0xe2, 0x69, 0x02, 0xc8, // vphaddd xmm1, xmm2, xmm0
        0xc4, 0xe2, 0x6d, 0x02, 0x08, // vphaddd ymm1, ymm2, [rax]
    ];
    let decoded = decode(&code[..5], CODE).unwrap();
    assert_eq!(decoded.op, Op::Vpi(PiK::Hadd, 4, true));
    assert_eq!(decoded.osz, 32);
    assert_eq!(decoded.ops[0], Opnd::Xmm(1));
    assert_eq!(decoded.ops[1], Opnd::Xmm(0));
    assert_eq!(decoded.ops[2], Opnd::Xmm(0));

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &code).unwrap();
    let memory_values = [100u32, 200, 300, 400, 500, 600, 700, 800];
    let memory_bytes = memory_values
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    mem.write_force(DATA, &memory_bytes).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[0] = 4u128 << 96 | 3u128 << 64 | 2u128 << 32 | 1;
    cpu.ymm_hi[0] = 8u128 << 96 | 7u128 << 64 | 6u128 << 32 | 5;
    cpu.xmm[2] = 12u128 << 96 | 11u128 << 64 | 10u128 << 32 | 9;
    cpu.ymm_hi[2] = 16u128 << 96 | 15u128 << 64 | 14u128 << 32 | 13;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[1], 7u128 << 96 | 3u128 << 64 | 7u128 << 32 | 3);
    assert_eq!(cpu.ymm_hi[1], 15u128 << 96 | 11u128 << 64 | 15u128 << 32 | 11);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[1], 7u128 << 96 | 3u128 << 64 | 23u128 << 32 | 19);
    assert_eq!(cpu.ymm_hi[1], 0);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[1], 700u128 << 96 | 300u128 << 64 | 23u128 << 32 | 19);
    assert_eq!(cpu.ymm_hi[1], 1500u128 << 96 | 1100u128 << 64 | 31u128 << 32 | 27);
    assert_eq!(cpu.rflags, flags);
}
