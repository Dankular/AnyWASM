use anywasm_cpu::cpu::Cpu;
use anywasm_cpu::decode::{decode, Op, Opnd, Prec};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RWX};

fn pack32(lanes: [u32; 4]) -> u128 {
    lanes
        .into_iter()
        .enumerate()
        .fold(0u128, |value, (index, lane)| value | ((lane as u128) << (index * 32)))
}

fn pack64(lanes: [u64; 2]) -> u128 {
    lanes[0] as u128 | ((lanes[1] as u128) << 64)
}

#[test]
fn vex_unpacks_low_and_high_lanes_per_128_bit_half() {
    const CODE: u64 = 0x2e000;
    let target = decode(&[0xc5, 0xd0, 0x14, 0xc5], CODE).unwrap();
    assert_eq!(target.op, Op::Vunpack(Prec::Ps, false));
    assert_eq!(target.osz, 16);
    assert_eq!(target.ops[0], Opnd::Xmm(0));
    assert_eq!(target.ops[1], Opnd::Xmm(5));
    assert_eq!(target.ops[2], Opnd::Xmm(5));

    let cases = [
        (
            [0xc5, 0xd0, 0x14, 0xc1],
            pack32([0x10, 0x20, 0x11, 0x21]),
            0,
        ),
        (
            [0xc5, 0xd0, 0x15, 0xc1],
            pack32([0x12, 0x22, 0x13, 0x23]),
            0,
        ),
        (
            [0xc5, 0xd1, 0x14, 0xc1],
            pack64([0x0000_0011_0000_0010, 0x0000_0021_0000_0020]),
            0,
        ),
        (
            [0xc5, 0xd1, 0x15, 0xc1],
            pack64([0x0000_0013_0000_0012, 0x0000_0023_0000_0022]),
            0,
        ),
        (
            [0xc5, 0xd4, 0x14, 0xc1],
            pack32([0x10, 0x20, 0x11, 0x21]),
            pack32([0x110, 0x120, 0x111, 0x121]),
        ),
        (
            [0xc5, 0xd4, 0x15, 0xc1],
            pack32([0x12, 0x22, 0x13, 0x23]),
            pack32([0x112, 0x122, 0x113, 0x123]),
        ),
        (
            [0xc5, 0xd5, 0x14, 0xc1],
            pack64([0x0000_0011_0000_0010, 0x0000_0021_0000_0020]),
            pack64([0x0000_0111_0000_0110, 0x0000_0121_0000_0120]),
        ),
        (
            [0xc5, 0xd5, 0x15, 0xc1],
            pack64([0x0000_0013_0000_0012, 0x0000_0023_0000_0022]),
            pack64([0x0000_0113_0000_0112, 0x0000_0123_0000_0122]),
        ),
    ];

    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[5] = pack32([0x10, 0x11, 0x12, 0x13]);
    cpu.xmm[1] = pack32([0x20, 0x21, 0x22, 0x23]);
    cpu.ymm_hi[5] = pack32([0x110, 0x111, 0x112, 0x113]);
    cpu.ymm_hi[1] = pack32([0x120, 0x121, 0x122, 0x123]);
    cpu.rflags = 0x8d7;
    let flags = cpu.rflags;

    for (instruction, expected_low, expected_high) in cases {
        mem.write_force(CODE, &instruction).unwrap();
        cpu.rip = CODE;
        cpu.xmm[0] = u128::MAX;
        cpu.ymm_hi[0] = u128::MAX;
        step(&mut cpu, &mut mem).unwrap();
        assert_eq!(cpu.xmm[0], expected_low);
        assert_eq!(cpu.ymm_hi[0], expected_high);
        assert_eq!(cpu.rflags, flags);
    }
}
