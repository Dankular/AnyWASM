//! x87 FPU. Registers are modelled as `f64` (not 80-bit extended): a documented precision
//! deviation that does not affect typical x86-64 user code, where SSE2 is the FP workhorse.

use crate::cpu::*;
use crate::decode::*;
use crate::exec::*;
use crate::mem::Memory;

pub fn f64_to_f80(x: f64) -> [u8; 10] {
    let bits = x.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (e, m): (u16, u64) = if exp == 0 {
        if frac == 0 {
            (0, 0)
        } else {
            // subnormal f64 -> normalised f80
            let lz = frac.leading_zeros() as i32 - 11; // shift to put leading 1 at bit 52
            let m = frac << (lz + 11); // leading one to bit 63
            let e = 1 - 1023 - lz + 16383;
            (e as u16, m)
        }
    } else if exp == 0x7ff {
        (0x7fff, (1u64 << 63) | (frac << 11))
    } else {
        ((exp - 1023 + 16383) as u16, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&m.to_le_bytes());
    out[8..].copy_from_slice(&(e | sign << 15).to_le_bytes());
    out
}

pub fn f80_to_f64(b: &[u8; 10]) -> f64 {
    let m = u64::from_le_bytes(b[..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = (se >> 15) as u64;
    let e = (se & 0x7fff) as i32;
    if e == 0 && m == 0 {
        return f64::from_bits(sign << 63);
    }
    if e == 0x7fff {
        let frac = (m << 1) >> 12;
        let nan = (m << 1) != 0;
        return f64::from_bits(sign << 63 | 0x7ff << 52 | if nan { frac | 1 << 51 } else { 0 });
    }
    // value = m * 2^(e - 16383 - 63)
    let v = (m as f64) * 2f64.powi(e - 16383 - 63);
    if sign != 0 {
        -v
    } else {
        v
    }
}

const C0: u16 = 1 << 8;
const C2: u16 = 1 << 10;
const C3: u16 = 1 << 14;

fn st(cpu: &Cpu, i: usize) -> f64 {
    cpu.fpu_st[(cpu.fpu_top as usize + i) & 7]
}
fn set_st(cpu: &mut Cpu, i: usize, v: f64) {
    let idx = (cpu.fpu_top as usize + i) & 7;
    cpu.fpu_st[idx] = v;
    cpu.fpu_tw &= !(3 << (2 * idx));
}
fn push(cpu: &mut Cpu, v: f64) {
    cpu.fpu_top = (cpu.fpu_top.wrapping_sub(1)) & 7;
    set_st(cpu, 0, v);
}
fn pop(cpu: &mut Cpu) {
    let idx = cpu.fpu_top as usize & 7;
    cpu.fpu_tw |= 3 << (2 * idx);
    cpu.fpu_top = (cpu.fpu_top + 1) & 7;
}
fn status(cpu: &Cpu) -> u16 {
    (cpu.fpu_sw & !(7 << 11)) | (cpu.fpu_top as u16) << 11
}
fn set_cc(cpu: &mut Cpu, c0: bool, c2: bool, c3: bool) {
    cpu.fpu_sw &= !(C0 | C2 | C3);
    if c0 {
        cpu.fpu_sw |= C0
    }
    if c2 {
        cpu.fpu_sw |= C2
    }
    if c3 {
        cpu.fpu_sw |= C3
    }
}
fn compare(cpu: &mut Cpu, a: f64, b: f64) {
    if a.is_nan() || b.is_nan() {
        set_cc(cpu, true, true, true)
    } else if a > b {
        set_cc(cpu, false, false, false)
    } else if a < b {
        set_cc(cpu, true, false, false)
    } else {
        set_cc(cpu, false, false, true)
    }
}
fn compare_flags(cpu: &mut Cpu, a: f64, b: f64) {
    cpu.rflags &= !(ZF | PF | CF);
    if a.is_nan() || b.is_nan() {
        cpu.rflags |= ZF | PF | CF;
    } else if a < b {
        cpu.rflags |= CF;
    } else if a == b {
        cpu.rflags |= ZF;
    }
}

fn arith(sel: u8, a: f64, b: f64) -> f64 {
    // sel: 0 add, 1 mul, 4 sub (a-b), 5 subr (b-a), 6 div (a/b), 7 divr (b/a)
    match sel {
        0 => a + b,
        1 => a * b,
        4 => a - b,
        5 => b - a,
        6 => a / b,
        _ => b / a,
    }
}

fn round_cw(cpu: &Cpu, x: f64) -> f64 {
    match (cpu.fpu_cw >> 10) & 3 {
        0 => x.round_ties_even(),
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

fn to_int(x: f64, bits: u32, trunc: bool, cpu: &Cpu) -> u64 {
    let r = if trunc { x.trunc() } else { round_cw(cpu, x) };
    let lim = (1u64 << (bits - 1)) as f64;
    if x.is_nan() || r >= lim || r < -lim {
        1u64 << (bits - 1) // integer indefinite
    } else {
        (r as i64 as u64) & if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 }
    }
}

pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Inst, next: u64) -> ExecResult<()> {
    let opc = i.cc; // low 3 bits of D8..DF
    let md = (i.imm >> 6) as u8 & 3;
    let reg = (i.imm >> 3) as u8 & 7;
    let rm = i.imm as u8 & 7;
    if md != 3 {
        let Opnd::Mem(m) = i.ops[0] else { return Err(Exit::Undefined) };
        let a = ea(cpu, &m);
        match opc {
            0 | 2 | 4 | 6 => {
                // D8/DA/DC/DE memory arithmetic
                let v = match opc {
                    0 => f32::from_bits(mem.read_u32(a)?) as f64,
                    2 => mem.read_u32(a)? as i32 as f64,
                    4 => f64::from_bits(mem.read_u64(a)?),
                    _ => mem.read_u16(a)? as i16 as f64,
                };
                if reg == 2 || reg == 3 {
                    compare(cpu, st(cpu, 0), v);
                    if reg == 3 {
                        pop(cpu);
                    }
                } else {
                    let r = arith(reg, st(cpu, 0), v);
                    set_st(cpu, 0, r);
                }
            }
            1 => match reg {
                0 => push(cpu, f32::from_bits(mem.read_u32(a)?) as f64),
                2 | 3 => {
                    mem.write_u32(a, (st(cpu, 0) as f32).to_bits())?;
                    if reg == 3 {
                        pop(cpu);
                    }
                }
                5 => cpu.fpu_cw = mem.read_u16(a)?,
                7 => mem.write_u16(a, cpu.fpu_cw)?,
                6 => {
                    // fnstenv: 28-byte protected-mode layout
                    let mut b = [0u8; 28];
                    b[0..2].copy_from_slice(&cpu.fpu_cw.to_le_bytes());
                    b[4..6].copy_from_slice(&status(cpu).to_le_bytes());
                    b[8..10].copy_from_slice(&cpu.fpu_tw.to_le_bytes());
                    mem.write_from(a, &b)?;
                }
                4 => {
                    let b = mem.read_vec(a, 28)?;
                    cpu.fpu_cw = u16::from_le_bytes([b[0], b[1]]);
                    cpu.fpu_sw = u16::from_le_bytes([b[4], b[5]]);
                    cpu.fpu_top = ((cpu.fpu_sw >> 11) & 7) as u8;
                    cpu.fpu_tw = u16::from_le_bytes([b[8], b[9]]);
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            3 => match reg {
                0 => push(cpu, mem.read_u32(a)? as i32 as f64),
                1 | 2 | 3 => {
                    let v = st(cpu, 0);
                    mem.write_u32(a, to_int(v, 32, reg == 1, cpu) as u32)?;
                    if reg != 2 {
                        pop(cpu);
                    }
                }
                5 => {
                    let mut raw = [0u8; 10];
                    mem.read_into(a, &mut raw, crate::mem::Access::Read)?;
                    push(cpu, f80_to_f64(&raw));
                }
                7 => {
                    mem.write_from(a, &f64_to_f80(st(cpu, 0)))?;
                    pop(cpu);
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            5 => match reg {
                0 => push(cpu, f64::from_bits(mem.read_u64(a)?)),
                1 => {
                    mem.write_u64(a, to_int(st(cpu, 0), 64, true, cpu))?;
                    pop(cpu);
                }
                2 | 3 => {
                    mem.write_u64(a, st(cpu, 0).to_bits())?;
                    if reg == 3 {
                        pop(cpu);
                    }
                }
                7 => mem.write_u16(a, status(cpu))?,
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            7 => match reg {
                0 => push(cpu, mem.read_u16(a)? as i16 as f64),
                1 | 2 | 3 => {
                    let v = st(cpu, 0);
                    mem.write_u16(a, to_int(v, 16, reg == 1, cpu) as u16)?;
                    if reg != 2 {
                        pop(cpu);
                    }
                }
                5 => push(cpu, mem.read_u64(a)? as i64 as f64),
                7 => {
                    let v = st(cpu, 0);
                    mem.write_u64(a, to_int(v, 64, false, cpu))?;
                    pop(cpu);
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            _ => return Err(Exit::Unimplemented(Op::X87)),
        }
    } else {
        let r = rm as usize;
        match opc {
            0 => {
                // D8: ST0 = ST0 op STi
                if reg == 2 || reg == 3 {
                    compare(cpu, st(cpu, 0), st(cpu, r));
                    if reg == 3 {
                        pop(cpu);
                    }
                } else {
                    let v = arith(reg, st(cpu, 0), st(cpu, r));
                    set_st(cpu, 0, v);
                }
            }
            1 => match reg {
                0 => push(cpu, st(cpu, r)),
                1 => {
                    let (a, b) = (st(cpu, 0), st(cpu, r));
                    set_st(cpu, 0, b);
                    set_st(cpu, r, a);
                }
                2 => {}
                4 => match rm {
                    0 => {
                        let v = -st(cpu, 0);
                        set_st(cpu, 0, v)
                    }
                    1 => {
                        let v = st(cpu, 0).abs();
                        set_st(cpu, 0, v)
                    }
                    4 => compare(cpu, st(cpu, 0), 0.0),
                    5 => {
                        let v = st(cpu, 0);
                        let c1 = v.is_sign_negative();
                        let (c0, c2, c3) = if v.is_nan() {
                            (true, false, false)
                        } else if v.is_infinite() {
                            (true, true, false)
                        } else if v == 0.0 {
                            (false, false, true)
                        } else if v.is_subnormal() {
                            (false, true, true)
                        } else {
                            (false, true, false)
                        };
                        set_cc(cpu, c0, c2, c3);
                        cpu.fpu_sw = (cpu.fpu_sw & !(1 << 9)) | (c1 as u16) << 9;
                    }
                    _ => return Err(Exit::Unimplemented(Op::X87)),
                },
                5 => {
                    let v = match rm {
                        0 => 1.0,
                        1 => 10f64.log2(),
                        2 => std::f64::consts::LOG2_E,
                        3 => std::f64::consts::PI,
                        4 => 2f64.log10(),
                        5 => std::f64::consts::LN_2,
                        6 => 0.0,
                        _ => return Err(Exit::Undefined),
                    };
                    push(cpu, v);
                }
                6 => match rm {
                    0 => {
                        let v = 2f64.powf(st(cpu, 0)) - 1.0;
                        set_st(cpu, 0, v)
                    }
                    1 => {
                        let v = st(cpu, 1) * st(cpu, 0).log2();
                        set_st(cpu, 1, v);
                        pop(cpu);
                    }
                    2 => {
                        let v = st(cpu, 0).tan();
                        set_st(cpu, 0, v);
                        push(cpu, 1.0);
                        set_cc(cpu, false, false, false);
                    }
                    3 => {
                        let v = st(cpu, 1).atan2(st(cpu, 0));
                        set_st(cpu, 1, v);
                        pop(cpu);
                    }
                    4 => {
                        let x = st(cpu, 0);
                        let e = x.abs().log2().floor();
                        let sig = x / 2f64.powf(e);
                        set_st(cpu, 0, e);
                        push(cpu, sig);
                    }
                    5 => {
                        let (a, b) = (st(cpu, 0), st(cpu, 1));
                        let v = a - (a / b).round_ties_even() * b;
                        set_st(cpu, 0, v);
                        set_cc(cpu, false, false, false);
                    }
                    6 => cpu.fpu_top = (cpu.fpu_top.wrapping_sub(1)) & 7,
                    _ => cpu.fpu_top = (cpu.fpu_top + 1) & 7,
                },
                7 => match rm {
                    0 => {
                        let (a, b) = (st(cpu, 0), st(cpu, 1));
                        set_st(cpu, 0, a % b);
                        set_cc(cpu, false, false, false);
                    }
                    1 => {
                        let v = st(cpu, 1) * (1.0 + st(cpu, 0)).log2();
                        set_st(cpu, 1, v);
                        pop(cpu);
                    }
                    2 => {
                        let v = st(cpu, 0).sqrt();
                        set_st(cpu, 0, v)
                    }
                    3 => {
                        let x = st(cpu, 0);
                        set_st(cpu, 0, x.sin());
                        push(cpu, x.cos());
                        set_cc(cpu, false, false, false);
                    }
                    4 => {
                        let v = round_cw(cpu, st(cpu, 0));
                        set_st(cpu, 0, v)
                    }
                    5 => {
                        let v = st(cpu, 0) * 2f64.powf(st(cpu, 1).trunc());
                        set_st(cpu, 0, v)
                    }
                    6 => {
                        let v = st(cpu, 0).sin();
                        set_st(cpu, 0, v);
                        set_cc(cpu, false, false, false);
                    }
                    _ => {
                        let v = st(cpu, 0).cos();
                        set_st(cpu, 0, v);
                        set_cc(cpu, false, false, false);
                    }
                },
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            2 => match (reg, rm) {
                (0..=3, _) => {
                    let t = match reg {
                        0 => cpu.flag(CF),
                        1 => cpu.flag(ZF),
                        2 => cpu.flag(CF) || cpu.flag(ZF),
                        _ => cpu.flag(PF),
                    };
                    if t {
                        let v = st(cpu, r);
                        set_st(cpu, 0, v);
                    }
                }
                (5, 1) => {
                    compare(cpu, st(cpu, 0), st(cpu, 1));
                    pop(cpu);
                    pop(cpu);
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            3 => match reg {
                0..=3 => {
                    let t = match reg {
                        0 => !cpu.flag(CF),
                        1 => !cpu.flag(ZF),
                        2 => !(cpu.flag(CF) || cpu.flag(ZF)),
                        _ => !cpu.flag(PF),
                    };
                    if t {
                        let v = st(cpu, r);
                        set_st(cpu, 0, v);
                    }
                }
                4 => match rm {
                    2 => cpu.fpu_sw &= 0x7f00,
                    3 => {
                        cpu.fpu_cw = 0x037f;
                        cpu.fpu_sw = 0;
                        cpu.fpu_tw = 0xffff;
                        cpu.fpu_top = 0;
                    }
                    _ => {}
                },
                5 | 6 => {
                    compare_flags(cpu, st(cpu, 0), st(cpu, r));
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            4 => {
                // DC: STi = STi op ST0 (sub/div direction swapped relative to D8)
                let sel = match reg {
                    4 => 5,
                    5 => 4,
                    6 => 7,
                    7 => 6,
                    s => s,
                };
                if reg == 2 || reg == 3 {
                    return Err(Exit::Undefined);
                }
                let v = arith(sel, st(cpu, r), st(cpu, 0));
                set_st(cpu, r, v);
            }
            5 => match reg {
                0 => {
                    let idx = (cpu.fpu_top as usize + r) & 7;
                    cpu.fpu_tw |= 3 << (2 * idx);
                }
                2 | 3 => {
                    let v = st(cpu, 0);
                    set_st(cpu, r, v);
                    if reg == 3 {
                        pop(cpu);
                    }
                }
                4 | 5 => {
                    compare(cpu, st(cpu, 0), st(cpu, r));
                    if reg == 5 {
                        pop(cpu);
                    }
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
            6 => {
                if reg == 3 && rm == 1 {
                    compare(cpu, st(cpu, 0), st(cpu, 1));
                    pop(cpu);
                    pop(cpu);
                } else {
                    let sel = match reg {
                        4 => 5,
                        5 => 4,
                        6 => 7,
                        7 => 6,
                        s => s,
                    };
                    if reg == 2 || reg == 3 {
                        return Err(Exit::Undefined);
                    }
                    let v = arith(sel, st(cpu, r), st(cpu, 0));
                    set_st(cpu, r, v);
                    pop(cpu);
                }
            }
            _ => match (reg, rm) {
                (4, 0) => {
                    let s = status(cpu) as u64;
                    cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xffff) | s;
                }
                (5, _) | (6, _) => {
                    compare_flags(cpu, st(cpu, 0), st(cpu, r));
                    pop(cpu);
                }
                _ => return Err(Exit::Unimplemented(Op::X87)),
            },
        }
    }
    cpu.rip = next;
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f80_roundtrip() {
        for x in [0.0, -0.0, 1.0, -1.5, 3.141592653589793, 1e300, -1e-300, 5e-324, f64::INFINITY, f64::NEG_INFINITY] {
            let b = f64_to_f80(x);
            let y = f80_to_f64(&b);
            assert_eq!(x.to_bits(), y.to_bits(), "{x}");
        }
        assert!(f80_to_f64(&f64_to_f80(f64::NAN)).is_nan());
    }
}
