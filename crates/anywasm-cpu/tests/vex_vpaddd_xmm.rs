use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

fn dwords(lane0: u32, lane1: u32, lane2: u32, lane3: u32) -> u128 {
    lane0 as u128 | ((lane1 as u128) << 32) | ((lane2 as u128) << 64) | ((lane3 as u128) << 96)
}

#[test]
fn vex_xmm_broadcast_and_packed_add_keep_128_bit_width() {
    const CODE: u64 = 0x25000;
    let code = [
        0xc4, 0xe2, 0x79, 0x58, 0xc0, // vpbroadcastd xmm0, xmm0
        0xc5, 0xf9, 0xfe, 0xc6, // vpaddd xmm0, xmm0, xmm6
        0xc5, 0xf9, 0xfe, 0xcd, // vpaddd xmm1, xmm0, xmm5
    ];
    let broadcast = decode(&code[..5], CODE).unwrap();
    assert_eq!(broadcast.op, Op::Vpbroadcastd);
    assert_eq!(broadcast.osz, 16);
    assert_eq!(broadcast.ops[0], Opnd::Xmm(0));
    assert_eq!(broadcast.ops[2], Opnd::Xmm(0));
    let add = decode(&code[5..9], CODE + 5).unwrap();
    assert_eq!(add.op, Op::Vpaddd);
    assert_eq!(add.osz, 16);

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = dwords(1, 2, 3, 4);
    cpu.xmm[6] = dwords(10, 20, 30, 40);
    cpu.xmm[5] = dwords(100, 200, 300, 400);
    cpu.ymm_hi[0] = u128::MAX;
    cpu.ymm_hi[1] = u128::MAX;
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    for _ in 0..3 {
        step(&mut cpu, &mut mem).unwrap();
    }

    assert_eq!(cpu.xmm[0], dwords(11, 21, 31, 41));
    assert_eq!(cpu.xmm[1], dwords(111, 221, 331, 441));
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.ymm_hi[1], 0);
    assert_eq!(cpu.rflags, flags);
}
