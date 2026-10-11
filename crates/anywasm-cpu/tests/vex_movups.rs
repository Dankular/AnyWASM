use anywasm_cpu::cpu::{Cpu, RAX, RSI, RSP};
use anywasm_cpu::decode::{decode, Op, Opnd};
use anywasm_cpu::exec::step;
use anywasm_cpu::mem::{Memory, PAGE_SIZE, PROT_RW, PROT_RWX};

#[test]
fn decodes_target_vmovups_and_vzeroupper_encodings() {
    let load = decode(&[0xc5, 0xfc, 0x10, 0x00], 0x1000).unwrap();
    assert_eq!((load.op, load.len, load.osz), (Op::Vmovups, 4, 32));
    assert_eq!(load.ops[0], Opnd::Xmm(0));
    assert!(matches!(load.ops[1], Opnd::Mem(mem) if mem.base == RAX as u8));

    let store = decode(&[0xc5, 0xfc, 0x11, 0x46, 0x10], 0x1000).unwrap();
    assert_eq!((store.op, store.len, store.osz), (Op::Vmovups, 5, 32));
    assert!(matches!(store.ops[0], Opnd::Mem(mem) if mem.base == RSI as u8 && mem.disp == 0x10));
    assert_eq!(store.ops[1], Opnd::Xmm(0));

    let zero = decode(&[0xc5, 0xf8, 0x77], 0x1000).unwrap();
    assert_eq!((zero.op, zero.len), (Op::Vzeroupper, 3));
}

#[test]
fn decodes_target_vex_vmovd_register_and_memory_forms() {
    let reg = decode(&[0xc5, 0xf9, 0x6e, 0xc0], 0x1000).unwrap();
    assert_eq!(
        (format!("{:?}", reg.op).as_str(), reg.len, reg.osz),
        ("Vmovd", 4, 16)
    );
    assert_eq!(reg.ops[0], Opnd::Xmm(0));
    assert_eq!(reg.ops[1], Opnd::Gpr(0));

    let memory = decode(&[0xc5, 0xf9, 0x6e, 0x00], 0x2000).unwrap();
    assert_eq!(
        (format!("{:?}", memory.op).as_str(), memory.len, memory.osz),
        ("Vmovd", 4, 16)
    );
    assert_eq!(memory.ops[0], Opnd::Xmm(0));
    assert!(matches!(memory.ops[1], Opnd::Mem(mem) if mem.base == RAX as u8));

    assert!(decode(&[0xc5, 0xfd, 0x6e, 0xc0], 0x3000).is_err()); // L=1 is not this form
    assert!(decode(&[0xc5, 0xf1, 0x6e, 0xc0], 0x3000).is_err()); // vvvv is reserved
    assert!(decode(&[0xc4, 0xe1, 0xf9, 0x6e, 0xc0], 0x3000).is_err()); // W=1 is not this form
}

#[test]
fn decodes_target_vpbroadcastd_vpaddd_and_ymm_store() {
    let broadcast = decode(&[0xc4, 0xe2, 0x7d, 0x58, 0xc0], 0x3000).unwrap();
    assert_eq!(
        (
            format!("{:?}", broadcast.op).as_str(),
            broadcast.len,
            broadcast.osz
        ),
        ("Vpbroadcastd", 5, 32)
    );
    assert_eq!(broadcast.ops[0], Opnd::Xmm(0));
    assert_eq!(broadcast.ops[2], Opnd::Xmm(0));

    let broadcast_mem = decode(&[0xc4, 0xe2, 0x7d, 0x58, 0x08], 0x3000).unwrap();
    assert!(matches!(broadcast_mem.ops[2], Opnd::Mem(mem) if mem.base == RAX as u8));

    assert!(decode(&[0xc4, 0xe2, 0x79, 0x58, 0xc0], 0x3000).is_err()); // L=0 not in this slice
    assert!(decode(&[0xc4, 0xe2, 0x75, 0x58, 0xc0], 0x3000).is_err()); // vvvv is reserved

    let add = decode(&[0xc5, 0xfd, 0xfe, 0xca], 0x4000).unwrap();
    assert_eq!(
        (format!("{:?}", add.op).as_str(), add.len, add.osz),
        ("Vpaddd", 4, 32)
    );
    assert_eq!(add.ops[0], Opnd::Xmm(1));
    assert_eq!(add.ops[1], Opnd::Xmm(0));
    assert_eq!(add.ops[2], Opnd::Xmm(2));

    let store = decode(&[0xc4, 0xc1, 0x7e, 0x7f, 0x49, 0xc0], 0x5000).unwrap();
    assert_eq!((store.op, store.len, store.osz), (Op::Vmovdqu, 6, 32));
    assert!(matches!(store.ops[0], Opnd::Mem(mem) if mem.base == 9 && mem.disp == -64));
    assert_eq!(store.ops[1], Opnd::Xmm(1));
    assert!(decode(&[0xc5, 0xf9, 0xfe, 0xca], 0x5000).is_err()); // 128-bit form not in this slice
}

#[test]
fn target_integer_vex_sequence_clears_broadcasts_adds_and_stores_all_ymm_lanes() {
    const CODE: u64 = 0x10_000;
    const DATA: u64 = 0x20_000;
    const DEST: u64 = 0x30_000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    assert!(mem.map(DEST, PAGE_SIZE, PROT_RW));
    mem.write_force(
        CODE,
        &[
            0xc5, 0xf9, 0x6e, 0xc0, // vmovd xmm0, eax
            0xc4, 0xe2, 0x7d, 0x58, 0xc0, // vpbroadcastd ymm0, xmm0
            0xc5, 0xfd, 0xfe, 0xca, // vpaddd ymm1, ymm0, ymm2
            0xc4, 0xc1, 0x7e, 0x7f, 0x49, 0xc0, // vmovdqu [r9-0x40], ymm1
        ],
    )
    .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[anywasm_cpu::cpu::RAX] = 0xffff_ffff;
    cpu.gpr[anywasm_cpu::cpu::R9] = DEST + 0x40;
    cpu.rflags = 0xa47;
    cpu.xmm[0] = u128::MAX;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.xmm[2] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
    cpu.ymm_hi[2] = 0x0000_0008_0000_0007_0000_0006_0000_0005;

    for _ in 0..4 {
        step(&mut cpu, &mut mem).unwrap();
    }

    let result_lanes = [0, 1, 2, 3, 4, 5, 6, 7];
    for (lane, value) in result_lanes.into_iter().enumerate() {
        let address = DEST + (lane as u64) * 4;
        assert_eq!(mem.read_u32(address).unwrap(), value, "lane {lane}");
    }
    assert_eq!(cpu.xmm[0], u128::MAX);
    assert_eq!(cpu.ymm_hi[0], u128::MAX);
    assert_eq!(cpu.xmm[1], 0x0000_0003_0000_0002_0000_0001_0000_0000);
    assert_eq!(cpu.ymm_hi[1], 0x0000_0007_0000_0006_0000_0005_0000_0004);
    assert_eq!(cpu.rflags, 0xa47);
}

#[test]
fn vex_vmovd_zeroes_upper_lanes_and_vpbroadcastd_reads_one_dword() {
    const CODE: u64 = 0x40_000;
    const DATA: u64 = 0x50_000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(
        CODE,
        &[
            0xc5, 0xf9, 0x6e, 0x18, // vmovd xmm3, [rax]
            0xc4, 0xe2, 0x7d, 0x58, 0x08, // vpbroadcastd ymm1, [rax]
        ],
    )
    .unwrap();
    let scalar_source = DATA + PAGE_SIZE - 4;
    mem.write_u32(scalar_source, 0x89ab_cdef).unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = scalar_source;
    cpu.xmm[3] = u128::MAX;
    cpu.ymm_hi[3] = u128::MAX;
    cpu.xmm[1] = 0;
    cpu.ymm_hi[1] = 0;
    cpu.rflags = 0x8d7;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[3], 0x89ab_cdef);
    assert_eq!(cpu.ymm_hi[3], 0);
    step(&mut cpu, &mut mem).unwrap();
    let repeated = 0x89ab_cdef_89ab_cdef_89ab_cdef_89ab_cdefu128;
    assert_eq!(cpu.xmm[1], repeated);
    assert_eq!(cpu.ymm_hi[1], repeated);
    assert_eq!(cpu.rflags, 0x8d7);
}

#[test]
fn vex_vmovd_gpr_source_clears_xmm_and_ymm_high_bits() {
    const CODE: u64 = 0x60_000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc5, 0xf9, 0x6e, 0xc0]).unwrap(); // vmovd xmm0, eax
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[anywasm_cpu::cpu::RAX] = 0xfeed_cafe_89ab_cdef;
    cpu.xmm[0] = u128::MAX;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], 0x89ab_cdef);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, 0x8d7);
}

#[test]
fn target_vpaddd_memory_source_adds_all_eight_dword_lanes() {
    const CODE: u64 = 0x70_000;
    const DATA: u64 = 0x80_000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc5, 0xfd, 0xfe, 0x08]).unwrap(); // vpaddd ymm1, ymm0, [rax]
    for lane in 0..8 {
        mem.write_u32(DATA + lane * 4, lane as u32 + 1).unwrap();
    }
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.xmm[0] = u128::MAX;
    cpu.ymm_hi[0] = u128::MAX;
    cpu.rflags = 0x8d7;

    step(&mut cpu, &mut mem).unwrap();
    for lane in 0..8 {
        let result = if lane == 0 { 0 } else { lane as u32 };
        let actual = if lane < 4 {
            ((cpu.xmm[1] >> (lane * 32)) & 0xffff_ffff) as u32
        } else {
            ((cpu.ymm_hi[1] >> ((lane - 4) * 32)) & 0xffff_ffff) as u32
        };
        assert_eq!(actual, result, "lane {lane}");
    }
    assert_eq!(cpu.rflags, 0x8d7);
}

#[test]
fn decodes_vex2_vmovdqu_load_and_store_encodings() {
    let load = decode(
        &[0xc5, 0xfa, 0x6f, 0x05, 0x12, 0xcc, 0x10, 0x00],
        0x1800_1016,
    )
    .unwrap();
    assert_eq!((load.op, load.len, load.osz), (Op::Vmovdqu, 8, 16));
    assert_eq!(load.ops[0], Opnd::Xmm(0));
    assert!(matches!(load.ops[1], Opnd::Mem(mem) if mem.base == 0xff && mem.disp == 0x1810_dc30));

    let store = decode(
        &[0xc5, 0xfa, 0x7f, 0x05, 0x78, 0x53, 0x12, 0x00],
        0x1800_1028,
    )
    .unwrap();
    assert_eq!((store.op, store.len, store.osz), (Op::Vmovdqu, 8, 16));
    assert!(matches!(store.ops[0], Opnd::Mem(mem) if mem.base == 0xff && mem.disp == 0x1812_63a8));
    assert_eq!(store.ops[1], Opnd::Xmm(0));

    let xor = decode(&[0xc5, 0xf9, 0xef, 0xc0], 0x1800_46e0).unwrap();
    assert_eq!((xor.op, xor.len, xor.osz), (Op::Vpxor, 4, 16));
    assert_eq!(xor.ops[0], Opnd::Xmm(0));
    assert_eq!(xor.ops[1], Opnd::Xmm(0));
    assert_eq!(xor.ops[2], Opnd::Xmm(0));

    let xorps = decode(&[0xc5, 0xf0, 0x57, 0xc9], 0x1800_b3b3a).unwrap();
    assert_eq!((xorps.op, xorps.len, xorps.osz), (Op::Vpxor, 4, 16));
    assert_eq!(xorps.ops[0], Opnd::Xmm(1));
    assert_eq!(xorps.ops[1], Opnd::Xmm(1));
    assert_eq!(xorps.ops[2], Opnd::Xmm(1));

    let cvtsi = decode(&[0xc4, 0xe1, 0xf2, 0x2a, 0xc9], 0x1800_b3b40).unwrap();
    assert_eq!(
        (cvtsi.op, cvtsi.len, cvtsi.ssz, cvtsi.rexw),
        (Op::VcvtSi2Ss, 5, 8, true)
    );
    assert_eq!(cvtsi.ops[0], Opnd::Xmm(1));
    assert_eq!(cvtsi.ops[1], Opnd::Xmm(1));
    assert_eq!(cvtsi.ops[2], Opnd::Gpr(1));

    let addss = decode(&[0xc5, 0xfa, 0x58, 0xc0], 0x1800_b3b81).unwrap();
    assert_eq!(
        (addss.op, addss.len),
        (
            Op::Vfp(anywasm_cpu::decode::FpK::Add, anywasm_cpu::decode::Prec::Ss),
            4
        )
    );
    assert_eq!(addss.ops[0], Opnd::Xmm(0));
    assert_eq!(addss.ops[1], Opnd::Xmm(0));
    assert_eq!(addss.ops[2], Opnd::Xmm(0));

    let divss = decode(&[0xc5, 0xf2, 0x5e, 0xc0], 0x1800_b3b85).unwrap();
    assert_eq!(
        (divss.op, divss.len),
        (
            Op::Vfp(anywasm_cpu::decode::FpK::Div, anywasm_cpu::decode::Prec::Ss),
            4
        )
    );
    assert_eq!(divss.ops[0], Opnd::Xmm(0));
    assert_eq!(divss.ops[1], Opnd::Xmm(1));
    assert_eq!(divss.ops[2], Opnd::Xmm(0));

    let comiss = decode(&[0xc5, 0xf8, 0x2f, 0x03], 0x1800_b3b89).unwrap();
    assert_eq!(
        (comiss.op, comiss.len),
        (Op::Comi(anywasm_cpu::decode::Prec::Ss, true), 4)
    );
    assert_eq!(comiss.ops[0], Opnd::Xmm(0));
    assert!(matches!(comiss.ops[1], Opnd::Mem(mem) if mem.base == 3));

    let vmovss = decode(&[0xc5, 0xfa, 0x10, 0x5b, 0x78], 0x1800_b3058).unwrap();
    assert_eq!((vmovss.op, vmovss.len), (Op::Vmovss, 5));
    assert_eq!(vmovss.ops[0], Opnd::Xmm(3));
    assert!(matches!(vmovss.ops[2], Opnd::Mem(mem) if mem.base == 3 && mem.disp == 0x78));

    let roundss = decode(&[0xc4, 0xe3, 0x71, 0x0a, 0xc8, 0x02], 0x1800_b309a).unwrap();
    assert_eq!((roundss.op, roundss.len, roundss.imm), (Op::Vroundss, 6, 2));
    assert_eq!(roundss.ops[0], Opnd::Xmm(1));
    assert_eq!(roundss.ops[1], Opnd::Xmm(1));
    assert_eq!(roundss.ops[2], Opnd::Xmm(0));

    let vmovaps = decode(
        &[0xc5, 0xf8, 0x29, 0xb4, 0x24, 0xe0, 0x01, 0x00, 0x00],
        0x1800_b4b0a,
    )
    .unwrap();
    assert_eq!((vmovaps.op, vmovaps.len, vmovaps.osz), (Op::Vmovups, 9, 16));
    assert!(matches!(vmovaps.ops[0], Opnd::Mem(mem) if mem.base == RSP as u8 && mem.disp == 0x1e0));
    assert_eq!(vmovaps.ops[1], Opnd::Xmm(6));

    let cvttss = decode(&[0xc4, 0x61, 0xfa, 0x2c, 0xc1], 0x1800_b30c6).unwrap();
    assert_eq!(
        (cvttss.op, cvttss.len, cvttss.osz, cvttss.rexw),
        (Op::Cvt(anywasm_cpu::decode::CvtK::Tss2Si), 5, 8, true)
    );
    assert_eq!(cvttss.ops[0], Opnd::Gpr(8));
    assert_eq!(cvttss.ops[1], Opnd::Xmm(1));

    let shlx = decode(&[0xc4, 0xc2, 0xf9, 0xf7, 0xc1], 0x1800_b3f4d).unwrap();
    assert_eq!((shlx.op, shlx.len, shlx.osz), (Op::Shlx, 5, 8));
    assert_eq!(shlx.ops[0], Opnd::Gpr(0));
    assert_eq!(shlx.ops[1], Opnd::Gpr(9));
    assert_eq!(shlx.ops[2], Opnd::Gpr(0));

    let aligned_store = decode(&[0xc5, 0xf9, 0x7f, 0x44, 0x24, 0x40], 0x1800_7b57).unwrap();
    assert_eq!(
        (aligned_store.op, aligned_store.len, aligned_store.osz),
        (Op::Vmovdqa, 6, 16)
    );
    assert!(
        matches!(aligned_store.ops[0], Opnd::Mem(mem) if mem.base == RSP as u8 && mem.disp == 0x40)
    );
    assert_eq!(aligned_store.ops[1], Opnd::Xmm(0));

    let vex3_load = decode(&[0xc4, 0xc1, 0x78, 0x10, 0x00], 0x1800_9671).unwrap();
    assert_eq!(
        (vex3_load.op, vex3_load.len, vex3_load.osz),
        (Op::Vmovups, 5, 16)
    );
    assert!(matches!(vex3_load.ops[1], Opnd::Mem(mem) if mem.base == 8));

    let andn = decode(&[0xc4, 0xe2, 0xf8, 0xf2, 0xc9], 0x1800_b812a).unwrap();
    assert_eq!(andn.len, 5);
    assert_eq!(andn.osz, 8);
    assert_eq!(andn.op, Op::Andn);
    assert_eq!(andn.ops[0], Opnd::Gpr(1));
    assert_eq!(andn.ops[1], Opnd::Gpr(0));
    assert_eq!(andn.ops[2], Opnd::Gpr(1));
}

#[test]
fn vex_vmovsd_load_store_and_register_form_match_target_encodings() {
    let load = decode(&[0xc5, 0xfb, 0x10, 0x4d, 0xd8], 0x1800_526a).unwrap();
    assert_eq!((load.op, load.len), (Op::Vmovsd, 5));
    assert_eq!(load.ops[0], Opnd::Xmm(1));
    assert!(matches!(load.ops[2], Opnd::Mem(mem) if mem.base == 5 && mem.disp == -40));

    let store = decode(&[0xc4, 0xc1, 0x7b, 0x11, 0x48, 0x20], 0x1800_526f).unwrap();
    assert_eq!((store.op, store.len), (Op::Vmovsd, 6));
    assert!(matches!(store.ops[0], Opnd::Mem(mem) if mem.base == 8 && mem.disp == 0x20));
    assert_eq!(store.ops[1], Opnd::Xmm(1));

    let reg = decode(&[0xc5, 0xf3, 0x10, 0xca], 0x1000).unwrap();
    assert_eq!((reg.op, reg.len), (Op::Vmovsd, 4));
    assert_eq!(reg.ops[0], Opnd::Xmm(1));
    assert_eq!(reg.ops[1], Opnd::Xmm(1));
    assert_eq!(reg.ops[2], Opnd::Xmm(2));
}

#[test]
fn vex_vmovsd_obeys_scalar_upper_lane_rules() {
    const CODE: u64 = 0x5000;
    const DATA: u64 = 0x6000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(
        CODE,
        &[
            0xc5, 0xfb, 0x10, 0x08, // vmovsd xmm1, [rax]
            0xc5, 0xeb, 0x10, 0xd9, // vmovsd xmm3, xmm2, xmm1
            0xc4, 0xc1, 0x7b, 0x11, 0x18, // vmovsd [r8], xmm3
        ],
    )
    .unwrap();
    mem.write_u64(DATA, 0x1122_3344_5566_7788).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.gpr[8] = DATA + 0x100;
    cpu.xmm[1] = 0xffff_eeee_dddd_cccc_aaaa_bbbb_cccc_dddd;
    cpu.xmm[2] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.ymm_hi[1] = u128::MAX;
    cpu.ymm_hi[2] = u128::MAX;

    for _ in 0..3 {
        let result = step(&mut cpu, &mut mem);
        assert!(
            result.is_ok(),
            "step failed at rip {:#x}: {result:?}",
            cpu.rip
        );
    }
    assert_eq!(cpu.xmm[3], 0x0123_4567_89ab_cdef_1122_3344_5566_7788);
    assert_eq!(cpu.ymm_hi[3], 0);
    assert_eq!(mem.read_u64(DATA + 0x100).unwrap(), 0x1122_3344_5566_7788);
}

#[test]
fn vex_vmovss_load_store_and_register_form_follow_scalar_lane_rules() {
    const CODE: u64 = 0x9000;
    const DATA: u64 = 0xa000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(
        CODE,
        &[
            0xc5, 0xfa, 0x10, 0x08, // vmovss xmm1, [rax]
            0xc5, 0xea, 0x10, 0xd9, // vmovss xmm3, xmm2, xmm1
            0xc4, 0xc1, 0x7a, 0x11, 0x18, // vmovss [r8], xmm3
        ],
    )
    .unwrap();
    mem.write_u32(DATA, 3.5f32.to_bits()).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.gpr[8] = DATA + 0x100;
    cpu.xmm[2] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.ymm_hi[1] = u128::MAX;
    cpu.ymm_hi[3] = u128::MAX;

    for _ in 0..3 {
        step(&mut cpu, &mut mem).unwrap();
    }

    assert_eq!(cpu.xmm[1], 3.5f32.to_bits() as u128);
    assert_eq!(cpu.xmm[3] as u32, 3.5f32.to_bits());
    assert_eq!(cpu.xmm[3] >> 32, cpu.xmm[2] >> 32);
    assert_eq!(cpu.ymm_hi[1], 0);
    assert_eq!(cpu.ymm_hi[3], 0);
    assert_eq!(mem.read_u32(DATA + 0x100).unwrap(), 3.5f32.to_bits());
}

#[test]
fn vex_vcvtsi2ss_converts_signed_64_bit_integer_and_preserves_upper_xmm_lanes() {
    const CODE: u64 = 0x7000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xe1, 0xf2, 0x2a, 0xc9])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[1] = (-7i64) as u64;
    cpu.xmm[1] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.ymm_hi[1] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1] as u32, (-7.0f32).to_bits());
    assert_eq!(cpu.xmm[1] >> 32, 0x0123_4567_89ab_cdef_fedc_ba98);
    assert_eq!(cpu.ymm_hi[1], 0);
}

#[test]
fn vex_scalar_float_arithmetic_uses_three_sources_and_comiss_sets_flags() {
    const CODE: u64 = 0x8000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(
        CODE,
        &[
            0xc5, 0xf2, 0x58, 0xc2, // vaddss xmm0, xmm1, xmm2
            0xc5, 0xea, 0x5e, 0xd9, // vdivss xmm3, xmm2, xmm1
            0xc5, 0xf8, 0x2f, 0xda, // vcomiss xmm3, xmm2
        ],
    )
    .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_0000;
    cpu.xmm[1] = 0x0123_4567_89ab_cdef_fedc_ba98_4120_0000; // 10.0f
    cpu.xmm[2] = 0xfedc_ba98_7654_3210_0000_0000_4000_0000; // 2.0f
    cpu.ymm_hi[0] = u128::MAX;
    cpu.ymm_hi[3] = u128::MAX;

    for _ in 0..3 {
        let result = step(&mut cpu, &mut mem);
        assert!(
            result.is_ok(),
            "step failed at rip {:#x}: {result:?}",
            cpu.rip
        );
    }

    assert_eq!(cpu.xmm[0] as u32, 12.0f32.to_bits());
    assert_eq!(cpu.xmm[0] >> 32, cpu.xmm[1] >> 32);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert!((f32::from_bits(cpu.xmm[3] as u32) - 0.2).abs() < 0.000001);
    assert_eq!(cpu.xmm[3] >> 32, cpu.xmm[2] >> 32);
    assert_eq!(cpu.ymm_hi[3], 0);
    assert!(cpu.flag(anywasm_cpu::cpu::CF));
    assert!(!cpu.flag(anywasm_cpu::cpu::ZF));
    assert!(!cpu.flag(anywasm_cpu::cpu::PF));
}

#[test]
fn vex_vroundss_uses_immediate_rounding_mode_and_merge_source() {
    const CODE: u64 = 0xb000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xe3, 0x71, 0x0a, 0xc8, 0x02])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[1] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.xmm[0] = 0xbfd8_0000; // -1.6875f
    cpu.ymm_hi[1] = u128::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.xmm[1] as u32, (-1.0f32).to_bits());
    assert_eq!(cpu.xmm[1] >> 32, 0x0123_4567_89ab_cdef_fedc_ba98);
    assert_eq!(cpu.ymm_hi[1], 0);
}

#[test]
fn vex_vcvttss2si_truncates_to_signed_64_bit_gpr() {
    const CODE: u64 = 0xc000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0x61, 0xfa, 0x2c, 0xc1])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[1] = (-2.75f32).to_bits() as u128;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[8], (-2i64) as u64);
}

#[test]
fn vex_shlx_uses_three_gpr_operands_without_changing_flags() {
    const CODE: u64 = 0xd000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xc2, 0xf9, 0xf7, 0xc1])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[0] = 7;
    cpu.gpr[9] = 3;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::ZF;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[0], 384);
    assert!(cpu.flag(anywasm_cpu::cpu::CF));
    assert!(cpu.flag(anywasm_cpu::cpu::ZF));
}

#[test]
fn target_shrx_decodes_only_the_supported_32_bit_form() {
    let shrx = decode(&[0xc4, 0xe2, 0x7b, 0xf7, 0xc5], 0x1400_3ffa5).unwrap();
    assert_eq!((shrx.op, shrx.len, shrx.osz), (Op::Shrx, 5, 4));
    assert_eq!(shrx.ops[0], Opnd::Gpr(RAX as u8));
    assert_eq!(shrx.ops[1], Opnd::Gpr(5));
    assert_eq!(shrx.ops[2], Opnd::Gpr(RAX as u8));

    assert!(decode(&[0xc4, 0xe2, 0x7f, 0xf7, 0xc5], 0x1000).is_err()); // VEX.L=1
    assert_eq!(
        decode(&[0xc4, 0xe2, 0x79, 0xf7, 0xc5], 0x1000).unwrap().op,
        Op::Shlx
    ); // pp=66 is SHLX
    assert!(decode(&[0xc4, 0xe2, 0x7a, 0xf7, 0xc5], 0x1000).is_err()); // pp=F3 is SARX
    assert!(decode(&[0xc4, 0xe2, 0xfb, 0xf7, 0xc5], 0x1000).is_err()); // W=1
}

#[test]
fn vex_shrx_uses_register_or_memory_source_and_preserves_flags() {
    const CODE: u64 = 0xe000;
    const DATA: u64 = 0xf000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xe2, 0x7b, 0xf7, 0xc5])
        .unwrap(); // shrx eax, ebp, eax
    mem.write_force(CODE + 5, &[0xc4, 0xe2, 0x7b, 0xf7, 0x45, 0x00])
        .unwrap(); // shrx eax, [rbp], eax
    mem.write_force(DATA, &0x8000_0000u32.to_le_bytes())
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = 4;
    cpu.gpr[5] = 0xf000_0000;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::OF | anywasm_cpu::cpu::ZF;
    let flags = cpu.rflags;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x0f00_0000);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 5;
    cpu.gpr[RAX] = 1;
    cpu.gpr[5] = DATA;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x4000_0000);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE;
    cpu.gpr[RAX] = 32;
    cpu.gpr[5] = 0x8000_0001;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(
        cpu.gpr[RAX], 0x8000_0001,
        "32-bit shifts mask the count to five bits"
    );
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn target_vex_movq_and_pextrq_decode_with_target_operands() {
    let movq = decode(&[0xc4, 0xc1, 0xf9, 0x7e, 0xc0], 0x1400_440c6).unwrap();
    assert_eq!(
        (movq.op, movq.len, movq.osz, movq.rexw),
        (Op::MovdFromX, 5, 8, true)
    );
    assert_eq!(movq.ops[0], Opnd::Gpr(8));
    assert_eq!(movq.ops[1], Opnd::Xmm(0));

    let pextrq = decode(&[0xc4, 0xe3, 0xf9, 0x16, 0xc0, 0x01], 0x1400_440d6).unwrap();
    assert_eq!((pextrq.op, pextrq.len, pextrq.osz), (Op::Pextr(8), 6, 8));
    assert_eq!(pextrq.ops[0], Opnd::Gpr(RAX as u8));
    assert_eq!(pextrq.ops[1], Opnd::Xmm(0));
    assert_eq!(pextrq.imm, 1);

    assert!(decode(&[0xc4, 0xc1, 0xfd, 0x7e, 0xc0], 0x1000).is_err()); // VEX.L=1
    assert!(decode(&[0xc4, 0xc1, 0xf1, 0x7e, 0xc0], 0x1000).is_err()); // VEX.vvvv is not reserved
    assert!(decode(&[0xc4, 0xc1, 0xf9, 0x6e, 0xc0], 0x1000).is_err()); // other VMOVQ direction
    assert!(decode(&[0xc4, 0xe3, 0xfd, 0x16, 0xc0, 0x01], 0x1000).is_err()); // VEX.L=1
}

#[test]
fn vex_movq_and_pextrq_copy_qwords_to_registers_and_memory_without_flags() {
    const CODE: u64 = 0x12000;
    const DATA: u64 = 0x13000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xc1, 0xf9, 0x7e, 0xc0])
        .unwrap(); // vmovq r8, xmm0
    mem.write_force(CODE + 5, &[0xc4, 0xc1, 0xf9, 0x7e, 0x00])
        .unwrap(); // vmovq [r8], xmm0
    mem.write_force(CODE + 10, &[0xc4, 0xe3, 0xf9, 0x16, 0xc0, 0x00])
        .unwrap(); // vpextrq rax, xmm0, 0
    mem.write_force(CODE + 16, &[0xc4, 0xe3, 0xf9, 0x16, 0x00, 0x01])
        .unwrap(); // vpextrq [rax], xmm0, 1

    let expected_xmm0 = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdefu128;
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = expected_xmm0;
    cpu.gpr[8] = DATA;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::OF | anywasm_cpu::cpu::ZF;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[8], expected_xmm0 as u64);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 5;
    cpu.gpr[8] = DATA;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(mem.read_u64(DATA).unwrap(), expected_xmm0 as u64);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 10;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], expected_xmm0 as u64);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 16;
    cpu.gpr[RAX] = DATA;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(mem.read_u64(DATA).unwrap(), (expected_xmm0 >> 64) as u64);
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn target_vextractf128_decodes_exact_lane_extract_forms() {
    let extract = decode(&[0xc4, 0xe3, 0x7d, 0x19, 0xc0, 0x01], 0x1400_036b3).unwrap();
    assert_eq!(format!("{:?}", extract.op), "Vextractf128");
    assert_eq!(extract.len, 6);
    assert_eq!(extract.osz, 32);
    assert_eq!(extract.ops[0], Opnd::Xmm(0));
    assert_eq!(extract.ops[1], Opnd::Xmm(0));
    assert_eq!(extract.imm, 1);

    let store = decode(&[0xc4, 0xe3, 0x7d, 0x19, 0x08, 0x01], 0x2000).unwrap();
    assert_eq!(format!("{:?}", store.op), "Vextractf128");
    assert_eq!(store.ops[0], Opnd::Mem(anywasm_cpu::decode::MemOp {
        base: RAX as u8, index: 0xff, scale: 0, disp: 0, seg: 0, addr32: false,
    }));
    assert_eq!(store.ops[1], Opnd::Xmm(1));

    assert!(decode(&[0xc4, 0xe3, 0x79, 0x19, 0xc0, 0x01], 0).is_err()); // L=0
    assert!(decode(&[0xc4, 0xe3, 0x7c, 0x19, 0xc0, 0x01], 0).is_err()); // pp != 66
    assert!(decode(&[0xc4, 0xe3, 0x75, 0x19, 0xc0, 0x01], 0).is_err()); // vvvv is not reserved
    assert!(decode(&[0xc4, 0xe3, 0xfd, 0x19, 0xc0, 0x01], 0).is_err()); // W=1
}

#[test]
fn vex_vextractf128_selects_the_upper_lane_and_preserves_flags() {
    const CODE: u64 = 0x14000;
    const DATA: u64 = 0x15000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xe3, 0x7d, 0x19, 0xc0, 0x01])
        .unwrap(); // vextractf128 xmm0, ymm0, 1
    mem.write_force(CODE + 6, &[0xc4, 0xe3, 0x7d, 0x19, 0x08, 0x01])
        .unwrap(); // vextractf128 [rax], ymm1, 1

    let high0 = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdefu128;
    let high1 = 0x8877_6655_4433_2211_ffeeddccbbaa0099u128;
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    cpu.ymm_hi[0] = high0;
    cpu.ymm_hi[1] = high1;
    cpu.gpr[RAX] = DATA;
    cpu.rflags |= anywasm_cpu::cpu::CF | anywasm_cpu::cpu::OF | anywasm_cpu::cpu::ZF;
    let flags = cpu.rflags;

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.xmm[0], high0);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert_eq!(cpu.rflags, flags);

    cpu.rip = CODE + 6;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(mem.read_vec(DATA, 16).unwrap(), high1.to_le_bytes());
    assert_eq!(cpu.rflags, flags);
}

#[test]
fn target_bzhi_decodes_modrm_sib_memory_and_vvvv_index_operands() {
    let bzhi = decode(&[0xc4, 0xc2, 0x70, 0xf5, 0x44, 0x91, 0xfc], 0x1400_4421c).unwrap();
    assert_eq!((bzhi.op, bzhi.len, bzhi.osz), (Op::Bzhi, 7, 4));
    assert_eq!(bzhi.ops[0], Opnd::Gpr(RAX as u8));
    assert!(
        matches!(bzhi.ops[1], Opnd::Mem(m) if m.base == 9 && m.index == 2 && m.scale == 2 && m.disp == -4),
        "memory operand: {:?}",
        bzhi.ops[1]
    );
    assert_eq!(bzhi.ops[2], Opnd::Gpr(1));

    assert!(decode(&[0xc4, 0xc2, 0x74, 0xf5, 0x44, 0x91, 0xfc], 0x1000).is_err()); // VEX.L=1
    assert!(decode(&[0xc4, 0xc2, 0xf0, 0xf5, 0x44, 0x91, 0xfc], 0x1000).is_err()); // W=1
    assert!(decode(&[0xc4, 0xc2, 0x71, 0xf5, 0x44, 0x91, 0xfc], 0x1000).is_err());
    // pp=66
}

#[test]
fn vex_bzhi_uses_eight_bit_index_and_sets_documented_flags() {
    const CODE: u64 = 0x14000;
    const DATA: u64 = 0x15000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));
    mem.write_force(CODE, &[0xc4, 0xc2, 0x70, 0xf5, 0x44, 0x91, 0xfc])
        .unwrap();

    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[9] = DATA + 0x20;
    cpu.gpr[2] = 3; // SIB index: [r9 + rdx*4 - 4] = DATA + 0x28
    cpu.gpr[1] = 4; // VEX.vvvv bit index
    mem.write_force(DATA + 0x28, &0xf0f0_000fu32.to_le_bytes())
        .unwrap();
    cpu.rflags |= anywasm_cpu::cpu::CF
        | anywasm_cpu::cpu::OF
        | anywasm_cpu::cpu::SF
        | anywasm_cpu::cpu::ZF
        | anywasm_cpu::cpu::AF
        | anywasm_cpu::cpu::PF;
    let undefined_flags = cpu.rflags & (anywasm_cpu::cpu::AF | anywasm_cpu::cpu::PF);

    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x0f);
    assert_eq!(
        cpu.rflags
            & (anywasm_cpu::cpu::CF
                | anywasm_cpu::cpu::OF
                | anywasm_cpu::cpu::SF
                | anywasm_cpu::cpu::ZF),
        0
    );
    assert_eq!(
        cpu.rflags & (anywasm_cpu::cpu::AF | anywasm_cpu::cpu::PF),
        undefined_flags
    );

    cpu.rip = CODE;
    cpu.gpr[1] = 0;
    mem.write_force(DATA + 0x28, &u32::MAX.to_le_bytes())
        .unwrap();
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0);
    assert!(cpu.flag(anywasm_cpu::cpu::ZF));
    assert!(!cpu.flag(anywasm_cpu::cpu::CF));

    cpu.rip = CODE;
    cpu.gpr[1] = 31;
    mem.write_force(DATA + 0x28, &u32::MAX.to_le_bytes())
        .unwrap();
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x7fff_ffff);
    assert!(!cpu.flag(anywasm_cpu::cpu::CF));
    assert!(!cpu.flag(anywasm_cpu::cpu::SF));

    cpu.rip = CODE;
    cpu.gpr[1] = 32;
    mem.write_force(DATA + 0x28, &0x8000_0000u32.to_le_bytes())
        .unwrap();
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x8000_0000);
    assert!(cpu.flag(anywasm_cpu::cpu::CF));
    assert!(cpu.flag(anywasm_cpu::cpu::SF));
    assert!(!cpu.flag(anywasm_cpu::cpu::OF));

    cpu.rip = CODE;
    cpu.gpr[1] = 0x101; // only the low byte controls the index
    mem.write_force(DATA + 0x28, &0xffff_ffffu32.to_le_bytes())
        .unwrap();
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 1);
    assert!(!cpu.flag(anywasm_cpu::cpu::CF));
}

#[test]
fn ymm_unaligned_load_store_and_vzeroupper_preserve_expected_bytes() {
    const CODE: u64 = 0x1000;
    const DATA: u64 = 0x2000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    assert!(mem.map(DATA, PAGE_SIZE, PROT_RW));

    // vmovups ymm0, [rax]; vmovups [rsi+0x10], ymm0; vzeroupper;
    // vmovups [rsi+0x40], ymm0
    let code = [
        0xc5, 0xfc, 0x10, 0x00, 0xc5, 0xfc, 0x11, 0x46, 0x10, 0xc5, 0xf8, 0x77, 0xc5, 0xfc, 0x11,
        0x46, 0x40,
    ];
    mem.write_force(CODE, &code).unwrap();

    let source: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
    mem.write_from(DATA, &source).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[RAX] = DATA;
    cpu.gpr[RSI] = DATA + 0x100;

    for _ in 0..4 {
        let result = step(&mut cpu, &mut mem);
        assert!(result.is_ok(), "VEX instruction failed: {result:?}");
    }

    assert_eq!(mem.read_vec(DATA + 0x110, 32).unwrap(), source);
    let mut after_zero_upper = source[..16].to_vec();
    after_zero_upper.extend([0; 16]);
    assert_eq!(mem.read_vec(DATA + 0x140, 32).unwrap(), after_zero_upper);
    assert_eq!(cpu.icount, 4);
}

#[test]
fn vex_xor_supports_128_and_256_bit_register_forms() {
    const CODE: u64 = 0x3000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc5, 0xfd, 0xef, 0xc1, 0xc5, 0xf1, 0xef, 0xd0])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.xmm[0] = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;
    cpu.xmm[1] = 0xffee_ddcc_bbaa_9988_7766_5544_3322_1100;
    cpu.ymm_hi[0] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    cpu.ymm_hi[1] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

    assert!(step(&mut cpu, &mut mem).is_ok());
    assert_eq!(cpu.xmm[0], u128::MAX);
    assert_eq!(cpu.ymm_hi[0], 0);
    assert!(step(&mut cpu, &mut mem).is_ok());
    assert_eq!(cpu.xmm[2], 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    assert_eq!(cpu.ymm_hi[2], 0);
}

#[test]
fn vex3_andn_uses_vvvv_and_modrm_sources() {
    const CODE: u64 = 0x4000;
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, &[0xc4, 0xe2, 0xf8, 0xf2, 0xc9])
        .unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    cpu.gpr[0] = 0x00ff_00ff_00ff_00ff;
    cpu.gpr[1] = u64::MAX;
    assert!(step(&mut cpu, &mut mem).is_ok());
    assert_eq!(cpu.gpr[1], 0xff00_ff00_ff00_ff00);
    assert!(!cpu.flag(anywasm_cpu::cpu::CF));
    assert!(!cpu.flag(anywasm_cpu::cpu::OF));
    assert!(!cpu.flag(anywasm_cpu::cpu::ZF));
}
