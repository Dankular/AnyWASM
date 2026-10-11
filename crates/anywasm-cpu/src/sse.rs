//! SSE / SSE2 / SSE3 / SSSE3 / SSE4.1 / SSE4.2 interpreter.
//!
//! Semantics follow the Intel SDM. Known deliberate deviations (documented in docs/STATUS.md):
//! `rcpps`/`rsqrtps` return exact reciprocals instead of the hardware's 12-bit approximations,
//! and MXCSR FTZ/DAZ and exception masks are not modelled.

use crate::cpu::*;
use crate::decode::*;
use crate::exec::*;
use crate::mem::Memory;

// ── lane helpers ─────────────────────────────────────────────────────────────

#[inline]
fn lane_mask(l: u8) -> u128 {
    if l >= 16 {
        u128::MAX
    } else {
        (1u128 << (8 * l as u32)) - 1
    }
}
#[inline]
fn get(v: u128, l: u8, i: usize) -> u64 {
    ((v >> (8 * l as usize * i)) & lane_mask(l)) as u64
}
#[inline]
fn put(v: &mut u128, l: u8, i: usize, x: u64) {
    let sh = 8 * l as usize * i;
    *v = (*v & !(lane_mask(l) << sh)) | (((x as u128) & lane_mask(l)) << sh);
}
#[inline]
fn lsx(x: u64, l: u8) -> i64 {
    sext(x, l)
}

fn map2(a: u128, b: u128, l: u8, f: impl Fn(u64, u64) -> u64) -> u128 {
    let mut r = 0u128;
    for i in 0..(16 / l as usize) {
        put(&mut r, l, i, f(get(a, l, i), get(b, l, i)));
    }
    r
}

fn f32x4(v: u128) -> [f32; 4] {
    [0, 1, 2, 3].map(|i| f32::from_bits(get(v, 4, i) as u32))
}
fn from_f32x4(a: [f32; 4]) -> u128 {
    let mut r = 0;
    for (i, x) in a.iter().enumerate() {
        put(&mut r, 4, i, x.to_bits() as u64);
    }
    r
}
fn f64x2(v: u128) -> [f64; 2] {
    [0, 1].map(|i| f64::from_bits(get(v, 8, i)))
}
fn from_f64x2(a: [f64; 2]) -> u128 {
    a[0].to_bits() as u128 | (a[1].to_bits() as u128) << 64
}

/// Read an XMM-or-memory source of `bytes` bytes, zero-extended to 128 bits.
fn src_x(cpu: &Cpu, mem: &Memory, o: &Opnd, bytes: u8) -> ExecResult<u128> {
    Ok(match o {
        Opnd::Xmm(n) => {
            let v = cpu.xmm[*n as usize];
            if bytes >= 16 {
                v
            } else {
                v & lane_mask(bytes)
            }
        }
        Opnd::Mem(m) => {
            let a = ea(cpu, m);
            match bytes {
                1 => mem.read_u8(a)? as u128,
                2 => mem.read_u16(a)? as u128,
                4 => mem.read_u32(a)? as u128,
                8 => mem.read_u64(a)? as u128,
                _ => mem.read_u128(a)?,
            }
        }
        _ => return Err(Exit::Undefined),
    })
}

fn src_ymm(cpu: &Cpu, mem: &Memory, o: &Opnd, bytes: u8) -> ExecResult<(u128, u128)> {
    Ok(match o {
        Opnd::Xmm(n) => (
            cpu.xmm[*n as usize],
            if bytes == 32 { cpu.ymm_hi[*n as usize] } else { 0 },
        ),
        Opnd::Mem(m) => {
            let raw = mem.read_vec(ea(cpu, m), bytes as usize)?;
            let mut low = [0; 16];
            low.copy_from_slice(&raw[..16]);
            let low = u128::from_le_bytes(low);
            let high = if bytes == 32 {
                let mut high = [0; 16];
                high.copy_from_slice(&raw[16..32]);
                u128::from_le_bytes(high)
            } else {
                0
            };
            (low, high)
        }
        _ => return Err(Exit::Undefined),
    })
}

fn dst_n(o: &Opnd) -> ExecResult<usize> {
    match o {
        Opnd::Xmm(n) => Ok(*n as usize),
        _ => Err(Exit::Undefined),
    }
}

/// Store `bytes` low bytes of `v` into an XMM (full replace when 16, else merge) or memory.
fn store_x(cpu: &mut Cpu, mem: &mut Memory, o: &Opnd, v: u128, bytes: u8) -> ExecResult<()> {
    match o {
        Opnd::Xmm(n) => cpu.xmm[*n as usize] = v,
        Opnd::Mem(m) => {
            let a = ea(cpu, m);
            match bytes {
                4 => mem.write_u32(a, v as u32)?,
                8 => mem.write_u64(a, v as u64)?,
                _ => mem.write_u128(a, v)?,
            }
        }
        _ => return Err(Exit::Undefined),
    }
    Ok(())
}

// ── rounding / conversion ────────────────────────────────────────────────────

fn rnd(x: f64, mode: u32) -> f64 {
    match mode & 3 {
        0 => x.round_ties_even(),
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}
fn rc(cpu: &Cpu) -> u32 {
    (cpu.mxcsr >> 13) & 3
}
fn f2i32(x: f64, mode: u32) -> u32 {
    if x.is_nan() {
        return 0x8000_0000;
    }
    let r = rnd(x, mode);
    if !(-2147483648.0..2147483648.0).contains(&r) {
        0x8000_0000
    } else {
        r as i32 as u32
    }
}
fn f2i64(x: f64, mode: u32) -> u64 {
    if x.is_nan() {
        return 1 << 63;
    }
    let r = rnd(x, mode);
    if !(-9223372036854775808.0..9223372036854775808.0).contains(&r) {
        1 << 63
    } else {
        r as i64 as u64
    }
}

fn fp32(k: FpK, a: f32, b: f32) -> f32 {
    match k {
        FpK::Add => a + b,
        FpK::Sub => a - b,
        FpK::Mul => a * b,
        FpK::Div => a / b,
        FpK::Min => {
            if a < b {
                a
            } else {
                b
            }
        }
        FpK::Max => {
            if a > b {
                a
            } else {
                b
            }
        }
        FpK::Sqrt => b.sqrt(),
        FpK::Rcp => 1.0 / b,
        FpK::Rsqrt => 1.0 / b.sqrt(),
        _ => unreachable!(),
    }
}
fn fp64(k: FpK, a: f64, b: f64) -> f64 {
    match k {
        FpK::Add => a + b,
        FpK::Sub => a - b,
        FpK::Mul => a * b,
        FpK::Div => a / b,
        FpK::Min => {
            if a < b {
                a
            } else {
                b
            }
        }
        FpK::Max => {
            if a > b {
                a
            } else {
                b
            }
        }
        FpK::Sqrt => b.sqrt(),
        FpK::Rcp => 1.0 / b,
        FpK::Rsqrt => 1.0 / b.sqrt(),
        _ => unreachable!(),
    }
}

fn fp_exec(k: FpK, pr: Prec, d: u128, s: u128) -> u128 {
    match k {
        FpK::And => return d & s,
        FpK::Andn => return !d & s,
        FpK::Or => return d | s,
        FpK::Xor => return d ^ s,
        FpK::Hadd | FpK::Hsub => {
            let sub = k == FpK::Hsub;
            return if pr == Prec::Ps {
                let (a, b) = (f32x4(d), f32x4(s));
                let f = |x: f32, y: f32| if sub { x - y } else { x + y };
                from_f32x4([f(a[0], a[1]), f(a[2], a[3]), f(b[0], b[1]), f(b[2], b[3])])
            } else {
                let (a, b) = (f64x2(d), f64x2(s));
                let f = |x: f64, y: f64| if sub { x - y } else { x + y };
                from_f64x2([f(a[0], a[1]), f(b[0], b[1])])
            };
        }
        FpK::Addsub => {
            return if pr == Prec::Ps {
                let (a, b) = (f32x4(d), f32x4(s));
                from_f32x4([a[0] - b[0], a[1] + b[1], a[2] - b[2], a[3] + b[3]])
            } else {
                let (a, b) = (f64x2(d), f64x2(s));
                from_f64x2([a[0] - b[0], a[1] + b[1]])
            };
        }
        _ => {}
    }
    match pr {
        Prec::Ps => {
            let (a, b) = (f32x4(d), f32x4(s));
            from_f32x4([0, 1, 2, 3].map(|i| fp32(k, a[i], b[i])))
        }
        Prec::Pd => {
            let (a, b) = (f64x2(d), f64x2(s));
            from_f64x2([0, 1].map(|i| fp64(k, a[i], b[i])))
        }
        Prec::Ss => {
            let r = fp32(k, f32::from_bits(d as u32), f32::from_bits(s as u32));
            (d & !0xffff_ffffu128) | r.to_bits() as u128
        }
        Prec::Sd => {
            let r = fp64(k, f64::from_bits(d as u64), f64::from_bits(s as u64));
            (d & !(u64::MAX as u128)) | r.to_bits() as u128
        }
    }
}

fn cmp_pred(pred: u8, a: f64, b: f64) -> bool {
    let un = a.is_nan() || b.is_nan();
    match pred & 7 {
        0 => a == b,
        1 => a < b,
        2 => a <= b,
        3 => un,
        4 => !(a == b),
        5 => !(a < b),
        6 => !(a <= b),
        _ => !un,
    }
}

// ── saturation helpers ───────────────────────────────────────────────────────

fn sat_s(v: i64, bits: u32) -> u64 {
    let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
    (v.clamp(lo, hi) as u64) & ((1u64 << bits) - 1)
}
fn sat_u(v: i64, bits: u32) -> u64 {
    v.clamp(0, (1i64 << bits) - 1) as u64
}

fn pi_exec(k: PiK, l: u8, signed: bool, d: u128, s: u128) -> u128 {
    let bits = 8 * l as u32;
    let m = lane_mask(l) as u64;
    match k {
        PiK::Add => map2(d, s, l, |a, b| a.wrapping_add(b) & m),
        PiK::Sub => map2(d, s, l, |a, b| a.wrapping_sub(b) & m),
        PiK::Adds => map2(d, s, l, |a, b| {
            if signed {
                sat_s(lsx(a, l) + lsx(b, l), bits)
            } else {
                (a + b).min(m)
            }
        }),
        PiK::Subs => map2(d, s, l, |a, b| {
            if signed {
                sat_s(lsx(a, l) - lsx(b, l), bits)
            } else {
                a.saturating_sub(b)
            }
        }),
        PiK::Mullo => map2(d, s, l, |a, b| a.wrapping_mul(b) & m),
        PiK::Mulhi => map2(d, s, l, |a, b| {
            if signed {
                ((lsx(a, l) * lsx(b, l)) >> bits) as u64 & m
            } else {
                (a * b) >> bits
            }
        }),
        PiK::Mulhrsw => map2(d, s, 2, |a, b| ((((lsx(a, 2) * lsx(b, 2)) >> 14) + 1) >> 1) as u64 & 0xffff),
        PiK::Muludq => {
            let mut r = 0u128;
            for i in 0..2 {
                put(&mut r, 8, i, (get(d, 4, 2 * i) as u64) * (get(s, 4, 2 * i) as u64));
            }
            r
        }
        PiK::Muldq => {
            let mut r = 0u128;
            for i in 0..2 {
                put(&mut r, 8, i, (lsx(get(d, 4, 2 * i), 4) * lsx(get(s, 4, 2 * i), 4)) as u64);
            }
            r
        }
        PiK::Madd => {
            let mut r = 0u128;
            for i in 0..4 {
                let v = lsx(get(d, 2, 2 * i), 2) * lsx(get(s, 2, 2 * i), 2) + lsx(get(d, 2, 2 * i + 1), 2) * lsx(get(s, 2, 2 * i + 1), 2);
                put(&mut r, 4, i, v as u64);
            }
            r
        }
        PiK::Maddubs => {
            let mut r = 0u128;
            for i in 0..8 {
                let v = get(d, 1, 2 * i) as i64 * lsx(get(s, 1, 2 * i), 1) + get(d, 1, 2 * i + 1) as i64 * lsx(get(s, 1, 2 * i + 1), 1);
                put(&mut r, 2, i, sat_s(v, 16));
            }
            r
        }
        PiK::Avg => map2(d, s, l, |a, b| (a + b + 1) >> 1),
        PiK::Min => map2(d, s, l, |a, b| {
            if signed {
                if lsx(a, l) < lsx(b, l) {
                    a
                } else {
                    b
                }
            } else {
                a.min(b)
            }
        }),
        PiK::Max => map2(d, s, l, |a, b| {
            if signed {
                if lsx(a, l) > lsx(b, l) {
                    a
                } else {
                    b
                }
            } else {
                a.max(b)
            }
        }),
        PiK::Cmpeq => map2(d, s, l, |a, b| if a == b { m } else { 0 }),
        PiK::Cmpgt => map2(d, s, l, |a, b| if lsx(a, l) > lsx(b, l) { m } else { 0 }),
        PiK::And => d & s,
        PiK::Andn => !d & s,
        PiK::Or => d | s,
        PiK::Xor => d ^ s,
        PiK::Sad => {
            let mut r = 0u128;
            for q in 0..2 {
                let mut sum = 0u64;
                for j in 0..8 {
                    let (a, b) = (get(d, 1, q * 8 + j), get(s, 1, q * 8 + j));
                    sum += a.abs_diff(b);
                }
                put(&mut r, 8, q, sum);
            }
            r
        }
        PiK::Abs => map2(s, s, l, |a, _| lsx(a, l).unsigned_abs() & m),
        PiK::Sign => map2(d, s, l, |a, b| {
            let sb = lsx(b, l);
            if sb < 0 {
                a.wrapping_neg() & m
            } else if sb == 0 {
                0
            } else {
                a
            }
        }),
        PiK::Hadd | PiK::Hsub | PiK::Hadds | PiK::Hsubs => {
            let n = 16 / l as usize;
            let mut r = 0u128;
            let sub = matches!(k, PiK::Hsub | PiK::Hsubs);
            let sat = matches!(k, PiK::Hadds | PiK::Hsubs);
            for i in 0..n / 2 {
                for (half, src) in [d, s].iter().enumerate() {
                    let (a, b) = (lsx(get(*src, l, 2 * i), l), lsx(get(*src, l, 2 * i + 1), l));
                    let v = if sub { a - b } else { a + b };
                    let out = if sat { sat_s(v, bits) } else { v as u64 & m };
                    put(&mut r, l, half * (n / 2) + i, out);
                }
            }
            r
        }
    }
}

fn shift_lanes(k: ShK, l: u8, d: u128, cnt: u64) -> u128 {
    let bits = 8 * l as u64;
    let m = lane_mask(l) as u64;
    map2(d, d, l, |a, _| match k {
        ShK::Sll => {
            if cnt >= bits {
                0
            } else {
                (a << cnt) & m
            }
        }
        ShK::Srl => {
            if cnt >= bits {
                0
            } else {
                a >> cnt
            }
        }
        ShK::Sra => (lsx(a, l) >> cnt.min(bits - 1)) as u64 & m,
    })
}

fn pack(src_l: u8, signed_out: bool, d: u128, s: u128) -> u128 {
    let dl = src_l / 2;
    let n = 16 / src_l as usize;
    let bits = 8 * dl as u32;
    let mut r = 0u128;
    for (half, src) in [d, s].iter().enumerate() {
        for i in 0..n {
            let v = lsx(get(*src, src_l, i), src_l);
            let o = if signed_out { sat_s(v, bits) } else { sat_u(v, bits) };
            put(&mut r, dl, half * n + i, o);
        }
    }
    r
}

fn punpck(l: u8, hi: bool, d: u128, s: u128) -> u128 {
    let n = 16 / l as usize;
    let mut r = 0u128;
    let base = if hi { n / 2 } else { 0 };
    for i in 0..n / 2 {
        put(&mut r, l, 2 * i, get(d, l, base + i));
        put(&mut r, l, 2 * i + 1, get(s, l, base + i));
    }
    r
}

fn pshufb(d: u128, s: u128) -> u128 {
    let mut r = 0u128;
    for i in 0..16 {
        let idx = get(s, 1, i);
        put(&mut r, 1, i, if idx & 0x80 != 0 { 0 } else { get(d, 1, (idx & 15) as usize) });
    }
    r
}

fn palignr(d: u128, s: u128, imm: u8) -> u128 {
    let n = imm as u32;
    let (hi, lo) = (d, s);
    if n >= 32 {
        0
    } else if n >= 16 {
        hi >> (8 * (n - 16))
    } else if n == 0 {
        lo
    } else {
        (lo >> (8 * n)) | (hi << (128 - 8 * n))
    }
}

// ── string compare (pcmp[ei]str[im]) ─────────────────────────────────────────

fn pcmpstr(cpu: &mut Cpu, d: u128, s: u128, imm: u8, explicit: bool, want_mask: bool, wide: bool) {
    let word = imm & 1 != 0;
    let signed = imm & 2 != 0;
    let agg = (imm >> 2) & 3;
    let pol = (imm >> 4) & 3;
    let l: u8 = if word { 2 } else { 1 };
    let n = 16 / l as usize;
    let len_of = |v: u128, reg: u64| -> usize {
        if explicit {
            // Lengths come from EAX/EDX (RAX/RDX with REX.W), as signed values.
            let r = if wide { reg as i64 } else { reg as i32 as i64 };
            (r.unsigned_abs() as usize).min(n)
        } else {
            (0..n).position(|i| get(v, l, i) == 0).unwrap_or(n)
        }
    };
    let (la, lb) = (len_of(d, cpu.gpr[RAX]), len_of(s, cpu.gpr[RDX]));
    let el = |v: u128, i: usize| -> i64 {
        if signed {
            lsx(get(v, l, i), l)
        } else {
            get(v, l, i) as i64
        }
    };
    // bool_res[j][i] = compare(a[i], b[j]) with validity rules
    let mut res = 0u32;
    match agg {
        0 => {
            // equal any
            for j in 0..n {
                let mut hit = false;
                for i in 0..n {
                    if i < la && j < lb && el(d, i) == el(s, j) {
                        hit = true;
                    }
                }
                if hit {
                    res |= 1 << j;
                }
            }
        }
        1 => {
            // ranges
            for j in 0..n {
                let mut hit = false;
                let mut i = 0;
                while i + 1 < n {
                    if i + 1 < la && j < lb && el(d, i) <= el(s, j) && el(s, j) <= el(d, i + 1) {
                        hit = true;
                    }
                    i += 2;
                }
                if hit {
                    res |= 1 << j;
                }
            }
        }
        2 => {
            // equal each
            for j in 0..n {
                let (va, vb) = (j < la, j < lb);
                let r = if va && vb {
                    el(d, j) == el(s, j)
                } else {
                    !va && !vb
                };
                if r {
                    res |= 1 << j;
                }
            }
        }
        _ => {
            // equal ordered
            for j in 0..n {
                let mut ok = true;
                for i in 0..(n - j) {
                    let va = i < la;
                    let vb = j + i < lb;
                    let eq = va && vb && el(d, i) == el(s, j + i);
                    if va {
                        if !eq {
                            ok = false;
                            break;
                        }
                    } else if vb {
                        // a exhausted, b valid: match so far
                        break;
                    } else {
                        break;
                    }
                }
                if ok {
                    res |= 1 << j;
                }
            }
        }
    }
    let full = if n == 16 { 0xffff } else { 0xff };
    let mut out = res & full;
    match pol {
        1 => out = !out & full,
        3 => {
            let valid = if lb >= n { full } else { (1u32 << lb) - 1 };
            out = (out ^ valid) & full;
        }
        _ => {}
    }
    cpu.rflags &= !ARITH_FLAGS;
    cpu.set_flag(CF, out != 0);
    cpu.set_flag(ZF, lb < n);
    cpu.set_flag(SF, la < n);
    cpu.set_flag(OF, out & 1 != 0);
    if want_mask {
        if imm & 0x40 != 0 {
            let mut r = 0u128;
            for i in 0..n {
                if out >> i & 1 != 0 {
                    put(&mut r, l, i, lane_mask(l) as u64);
                }
            }
            cpu.xmm[0] = r;
        } else {
            cpu.xmm[0] = out as u128;
        }
    } else {
        let idx = if out == 0 {
            n as u64
        } else if imm & 0x40 != 0 {
            (31 - out.leading_zeros()) as u64
        } else {
            out.trailing_zeros() as u64
        };
        cpu.gpr[RCX] = idx;
    }
}

// ── main dispatcher ──────────────────────────────────────────────────────────

pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Inst, next: u64) -> ExecResult<()> {
    let imm8 = i.imm as u8;
    match i.op {
        Op::Vmovups | Op::Vmovdqu | Op::Vmovdqa => match (&i.ops[0], &i.ops[1]) {
            (Opnd::Xmm(d), Opnd::Xmm(s)) => {
                let (d, s) = (*d as usize, *s as usize);
                let (lo, hi) = (cpu.xmm[s], if i.osz == 32 { cpu.ymm_hi[s] } else { 0 });
                cpu.xmm[d] = lo;
                cpu.ymm_hi[d] = hi;
            }
            (Opnd::Xmm(d), Opnd::Mem(m)) => {
                let bytes = mem.read_vec(ea(cpu, m), i.osz as usize)?;
                let mut lo = [0; 16];
                lo.copy_from_slice(&bytes[..16]);
                cpu.xmm[*d as usize] = u128::from_le_bytes(lo);
                cpu.ymm_hi[*d as usize] = if i.osz == 32 {
                    let mut hi = [0; 16];
                    hi.copy_from_slice(&bytes[16..32]);
                    u128::from_le_bytes(hi)
                } else {
                    0
                };
            }
            (Opnd::Mem(m), Opnd::Xmm(s)) => {
                let mut bytes = cpu.xmm[*s as usize].to_le_bytes().to_vec();
                if i.osz == 32 {
                    bytes.extend_from_slice(&cpu.ymm_hi[*s as usize].to_le_bytes());
                }
                mem.write_from(ea(cpu, m), &bytes)?;
            }
            _ => return Err(Exit::Undefined),
        },
        Op::Vmovss | Op::Vmovsd => {
            let bytes = if i.op == Op::Vmovss { 4 } else { 8 };
            let mask = lane_mask(bytes);
            match (&i.ops[0], &i.ops[1], &i.ops[2]) {
                (Opnd::Xmm(d), Opnd::Xmm(a), Opnd::Xmm(b)) => {
                    let (a, b) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                    cpu.xmm[*d as usize] = (a & !mask) | (b & mask);
                    cpu.ymm_hi[*d as usize] = 0;
                }
                (Opnd::Xmm(d), _, Opnd::Mem(m)) => {
                    cpu.xmm[*d as usize] = read_mem(mem, ea(cpu, m), bytes)? as u128;
                    cpu.ymm_hi[*d as usize] = 0;
                }
                (Opnd::Mem(m), Opnd::Xmm(s), _) => {
                    write_mem(mem, ea(cpu, m), bytes, cpu.xmm[*s as usize] as u64)?;
                }
                _ => return Err(Exit::Undefined),
            }
        }
        Op::Vpxor => {
            let d = dst_n(&i.ops[0])?;
            let (a_lo, a_hi) = src_ymm(cpu, mem, &i.ops[1], i.osz)?;
            let (b_lo, b_hi) = src_ymm(cpu, mem, &i.ops[2], i.osz)?;
            cpu.xmm[d] = a_lo ^ b_lo;
            cpu.ymm_hi[d] = if i.osz == 32 { a_hi ^ b_hi } else { 0 };
        }
        Op::VcvtSi2Ss => {
            let d = dst_n(&i.ops[0])?;
            let source = rd(cpu, mem, &i.ops[2], i.ssz, 0)?;
            let value = (sext(source, i.ssz) as f32).to_bits() as u128;
            cpu.xmm[d] = (cpu.xmm[match i.ops[1] { Opnd::Xmm(n) => n as usize, _ => return Err(Exit::Undefined) }] & !lane_mask(4)) | value;
            cpu.ymm_hi[d] = 0;
        }
        Op::Vfp(k, pr) => {
            let d = dst_n(&i.ops[0])?;
            let a = src_x(cpu, mem, &i.ops[1], 16)?;
            let b = src_x(cpu, mem, &i.ops[2], 4)?;
            cpu.xmm[d] = fp_exec(k, pr, a, b);
            cpu.ymm_hi[d] = 0;
        }
        Op::Vroundss => {
            let d = dst_n(&i.ops[0])?;
            let merge = src_x(cpu, mem, &i.ops[1], 16)?;
            let source = src_x(cpu, mem, &i.ops[2], 4)?;
            let mode = if imm8 & 4 != 0 { rc(cpu) } else { (imm8 & 3) as u32 };
            let result = (rnd(f32::from_bits(source as u32) as f64, mode) as f32).to_bits() as u128;
            cpu.xmm[d] = (merge & !lane_mask(4)) | result;
            cpu.ymm_hi[d] = 0;
        }
        Op::Vzeroupper => cpu.ymm_hi.fill(0),
        Op::MovVec => {
            let v = src_x(cpu, mem, &i.ops[1], 16)?;
            store_x(cpu, mem, &i.ops[0], v, 16)?;
        }
        Op::Movss | Op::Movsd => {
            let bytes = if i.op == Op::Movss { 4 } else { 8 };
            match (&i.ops[0], &i.ops[1]) {
                (Opnd::Xmm(d), Opnd::Xmm(s)) => {
                    let (dv, sv) = (cpu.xmm[*d as usize], cpu.xmm[*s as usize]);
                    let m = lane_mask(bytes);
                    cpu.xmm[*d as usize] = (dv & !m) | (sv & m);
                }
                (Opnd::Xmm(d), Opnd::Mem(_)) => cpu.xmm[*d as usize] = src_x(cpu, mem, &i.ops[1], bytes)?,
                (Opnd::Mem(_), Opnd::Xmm(s)) => {
                    let v = cpu.xmm[*s as usize];
                    store_x(cpu, mem, &i.ops[0], v, bytes)?;
                }
                _ => return Err(Exit::Undefined),
            }
        }
        Op::MovdToX => {
            let sz = if i.rexw { 8 } else { 4 };
            let v = rd(cpu, mem, &i.ops[1], sz, 0)?;
            cpu.xmm[dst_n(&i.ops[0])?] = v as u128;
        }
        Op::MovdFromX => {
            let sz = if i.rexw { 8 } else { 4 };
            let v = src_x(cpu, mem, &i.ops[1], 16)? as u64 & msk(sz);
            wr(cpu, mem, &i.ops[0], sz, v)?;
        }
        Op::MovqLoad => {
            let v = src_x(cpu, mem, &i.ops[1], 8)?;
            cpu.xmm[dst_n(&i.ops[0])?] = v;
        }
        Op::MovqStore => {
            let v = src_x(cpu, mem, &i.ops[1], 16)? & lane_mask(8);
            match i.ops[0] {
                Opnd::Xmm(n) => cpu.xmm[n as usize] = v,
                _ => store_x(cpu, mem, &i.ops[0], v, 8)?,
            }
        }
        Op::Movlp => match (&i.ops[0], &i.ops[1]) {
            (Opnd::Xmm(d), Opnd::Mem(_)) => {
                let v = src_x(cpu, mem, &i.ops[1], 8)?;
                let dv = cpu.xmm[*d as usize];
                cpu.xmm[*d as usize] = (dv & !lane_mask(8)) | v;
            }
            (Opnd::Mem(_), Opnd::Xmm(s)) => {
                let v = cpu.xmm[*s as usize];
                store_x(cpu, mem, &i.ops[0], v, 8)?;
            }
            _ => return Err(Exit::Undefined),
        },
        Op::Movhp => match (&i.ops[0], &i.ops[1]) {
            (Opnd::Xmm(d), Opnd::Mem(_)) => {
                let v = src_x(cpu, mem, &i.ops[1], 8)?;
                let dv = cpu.xmm[*d as usize];
                cpu.xmm[*d as usize] = (dv & lane_mask(8)) | v << 64;
            }
            (Opnd::Mem(_), Opnd::Xmm(s)) => {
                let v = cpu.xmm[*s as usize] >> 64;
                store_x(cpu, mem, &i.ops[0], v, 8)?;
            }
            _ => return Err(Exit::Undefined),
        },
        Op::Movhlps => {
            let (d, s) = (dst_n(&i.ops[0])?, dst_n(&i.ops[1])?);
            cpu.xmm[d] = (cpu.xmm[d] & !lane_mask(8)) | (cpu.xmm[s] >> 64);
        }
        Op::Movlhps => {
            let (d, s) = (dst_n(&i.ops[0])?, dst_n(&i.ops[1])?);
            cpu.xmm[d] = (cpu.xmm[d] & lane_mask(8)) | cpu.xmm[s] << 64;
        }
        Op::Movddup => {
            let v = src_x(cpu, mem, &i.ops[1], 8)? & lane_mask(8);
            cpu.xmm[dst_n(&i.ops[0])?] = v | v << 64;
        }
        Op::Movshdup | Op::Movsldup => {
            let v = src_x(cpu, mem, &i.ops[1], 16)?;
            let off = if i.op == Op::Movshdup { 1 } else { 0 };
            let mut r = 0u128;
            for q in 0..2 {
                put(&mut r, 4, 2 * q, get(v, 4, 2 * q + off));
                put(&mut r, 4, 2 * q + 1, get(v, 4, 2 * q + off));
            }
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Movmsk(pr) => {
            let v = src_x(cpu, mem, &i.ops[1], 16)?;
            let r = if pr == Prec::Ps {
                (0..4).fold(0u64, |a, k| a | (get(v, 4, k) >> 31) << k)
            } else {
                (0..2).fold(0u64, |a, k| a | (get(v, 8, k) >> 63) << k)
            };
            wr(cpu, mem, &i.ops[0], 4, r)?;
        }
        Op::Pmovmskb => {
            let v = src_x(cpu, mem, &i.ops[1], 16)?;
            let r = (0..16).fold(0u64, |a, k| a | (get(v, 1, k) >> 7) << k);
            wr(cpu, mem, &i.ops[0], 4, r)?;
        }
        Op::Unpck(pr, hi) => {
            let (d, s) = (cpu.xmm[dst_n(&i.ops[0])?], src_x(cpu, mem, &i.ops[1], 16)?);
            let l = if pr == Prec::Ps { 4 } else { 8 };
            cpu.xmm[dst_n(&i.ops[0])?] = punpck(l, hi, d, s);
        }
        Op::Shufps => {
            let (d, s) = (cpu.xmm[dst_n(&i.ops[0])?], src_x(cpu, mem, &i.ops[1], 16)?);
            let mut r = 0u128;
            put(&mut r, 4, 0, get(d, 4, (imm8 & 3) as usize));
            put(&mut r, 4, 1, get(d, 4, (imm8 >> 2 & 3) as usize));
            put(&mut r, 4, 2, get(s, 4, (imm8 >> 4 & 3) as usize));
            put(&mut r, 4, 3, get(s, 4, (imm8 >> 6 & 3) as usize));
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Shufpd => {
            let (d, s) = (cpu.xmm[dst_n(&i.ops[0])?], src_x(cpu, mem, &i.ops[1], 16)?);
            let mut r = 0u128;
            put(&mut r, 8, 0, get(d, 8, (imm8 & 1) as usize));
            put(&mut r, 8, 1, get(s, 8, (imm8 >> 1 & 1) as usize));
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Pshufd => {
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let mut r = 0u128;
            for k in 0..4 {
                put(&mut r, 4, k, get(s, 4, (imm8 >> (2 * k) & 3) as usize));
            }
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Pshufhw | Op::Pshuflw => {
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let base = if i.op == Op::Pshufhw { 4 } else { 0 };
            let mut r = s;
            for k in 0..4 {
                put(&mut r, 2, base + k, get(s, 2, base + (imm8 >> (2 * k) & 3) as usize));
            }
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Pshufb => {
            let (d, s) = (cpu.xmm[dst_n(&i.ops[0])?], src_x(cpu, mem, &i.ops[1], 16)?);
            cpu.xmm[dst_n(&i.ops[0])?] = pshufb(d, s);
        }
        Op::Palignr => {
            let (d, s) = (cpu.xmm[dst_n(&i.ops[0])?], src_x(cpu, mem, &i.ops[1], 16)?);
            cpu.xmm[dst_n(&i.ops[0])?] = palignr(d, s, imm8);
        }
        Op::Fp(k, pr) => {
            let bytes = match pr {
                Prec::Ss => 4,
                Prec::Sd => 8,
                _ => 16,
            };
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], bytes)?;
            cpu.xmm[n] = fp_exec(k, pr, cpu.xmm[n], s);
        }
        Op::Pi(k, l, sg) => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            cpu.xmm[n] = pi_exec(k, l, sg, cpu.xmm[n], s);
        }
        Op::PShift(k, l) => {
            let n = dst_n(&i.ops[0])?;
            let cnt = match i.ops[1] {
                Opnd::Imm => i.imm as u64 & 0xff,
                // The count is the low quadword of the source; the high quadword is ignored.
                _ => src_x(cpu, mem, &i.ops[1], 16)? as u64,
            };
            cpu.xmm[n] = shift_lanes(k, l, cpu.xmm[n], cnt);
        }
        Op::PShiftBytes(right) => {
            let n = dst_n(&i.ops[0])?;
            let c = (i.imm as u32).min(16);
            let v = cpu.xmm[n];
            cpu.xmm[n] = if c >= 16 {
                0
            } else if right {
                v >> (8 * c)
            } else {
                v << (8 * c)
            };
        }
        Op::Punpck(l, hi) => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            cpu.xmm[n] = punpck(l, hi, cpu.xmm[n], s);
        }
        Op::Pack(l, sg) => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            cpu.xmm[n] = pack(l, sg, cpu.xmm[n], s);
        }
        Op::Cvt(k) => exec_cvt(cpu, mem, i, k)?,
        Op::CmpFp(pr) => {
            let n = dst_n(&i.ops[0])?;
            let bytes = match pr {
                Prec::Ss => 4,
                Prec::Sd => 8,
                _ => 16,
            };
            let s = src_x(cpu, mem, &i.ops[1], bytes)?;
            let d = cpu.xmm[n];
            cpu.xmm[n] = match pr {
                Prec::Ps => {
                    let (a, b) = (f32x4(d), f32x4(s));
                    let mut r = 0;
                    for k in 0..4 {
                        put(&mut r, 4, k, if cmp_pred(imm8, a[k] as f64, b[k] as f64) { u32::MAX as u64 } else { 0 });
                    }
                    r
                }
                Prec::Pd => {
                    let (a, b) = (f64x2(d), f64x2(s));
                    let mut r = 0;
                    for k in 0..2 {
                        put(&mut r, 8, k, if cmp_pred(imm8, a[k], b[k]) { u64::MAX } else { 0 });
                    }
                    r
                }
                Prec::Ss => {
                    let t = cmp_pred(imm8, f32::from_bits(d as u32) as f64, f32::from_bits(s as u32) as f64);
                    (d & !lane_mask(4)) | if t { u32::MAX as u128 } else { 0 }
                }
                Prec::Sd => {
                    let t = cmp_pred(imm8, f64::from_bits(d as u64), f64::from_bits(s as u64));
                    (d & !lane_mask(8)) | if t { u64::MAX as u128 } else { 0 }
                }
            };
        }
        Op::Comi(pr, _ordered) => {
            let bytes = if pr == Prec::Sd { 8 } else { 4 };
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], bytes)?;
            let d = cpu.xmm[n];
            let (a, b) = if pr == Prec::Sd {
                (f64::from_bits(d as u64), f64::from_bits(s as u64))
            } else {
                (f32::from_bits(d as u32) as f64, f32::from_bits(s as u32) as f64)
            };
            cpu.rflags &= !ARITH_FLAGS;
            if a.is_nan() || b.is_nan() {
                cpu.rflags |= ZF | PF | CF;
            } else if a < b {
                cpu.rflags |= CF;
            } else if a == b {
                cpu.rflags |= ZF;
            }
        }
        Op::Round(pr) => {
            let n = dst_n(&i.ops[0])?;
            let mode = if imm8 & 4 != 0 { rc(cpu) } else { (imm8 & 3) as u32 };
            let bytes = match pr {
                Prec::Ss => 4,
                Prec::Sd => 8,
                _ => 16,
            };
            let s = src_x(cpu, mem, &i.ops[1], bytes)?;
            let d = cpu.xmm[n];
            cpu.xmm[n] = match pr {
                Prec::Ps => {
                    let a = f32x4(s);
                    from_f32x4(a.map(|x| rnd(x as f64, mode) as f32))
                }
                Prec::Pd => from_f64x2(f64x2(s).map(|x| rnd(x, mode))),
                Prec::Ss => (d & !lane_mask(4)) | (rnd(f32::from_bits(s as u32) as f64, mode) as f32).to_bits() as u128,
                Prec::Sd => (d & !lane_mask(8)) | rnd(f64::from_bits(s as u64), mode).to_bits() as u128,
            };
        }
        Op::Dpp(pr) => {
            // Per the SDM: masked-out products are +0.0, then summed pairwise ((p0+p1)+(p2+p3)).
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let d = cpu.xmm[n];
            cpu.xmm[n] = if pr == Prec::Ps {
                let (a, b) = (f32x4(d), f32x4(s));
                let p = [0, 1, 2, 3].map(|k| if imm8 >> (4 + k) & 1 != 0 { a[k] * b[k] } else { 0.0 });
                let sum = (p[0] + p[1]) + (p[2] + p[3]);
                from_f32x4([0, 1, 2, 3].map(|k| if imm8 >> k & 1 != 0 { sum } else { 0.0 }))
            } else {
                let (a, b) = (f64x2(d), f64x2(s));
                let p = [0, 1].map(|k| if imm8 >> (4 + k) & 1 != 0 { a[k] * b[k] } else { 0.0 });
                let sum = p[0] + p[1];
                from_f64x2([0, 1].map(|k| if imm8 >> k & 1 != 0 { sum } else { 0.0 }))
            };
        }
        Op::Blend(pr) => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let (l, cnt) = if pr == Prec::Ps { (4u8, 4) } else { (8u8, 2) };
            let mut r = cpu.xmm[n];
            for k in 0..cnt {
                if imm8 >> k & 1 != 0 {
                    put(&mut r, l, k, get(s, l, k));
                }
            }
            cpu.xmm[n] = r;
        }
        Op::Pblendw => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let mut r = cpu.xmm[n];
            for k in 0..8 {
                if imm8 >> k & 1 != 0 {
                    put(&mut r, 2, k, get(s, 2, k));
                }
            }
            cpu.xmm[n] = r;
        }
        Op::Pblendv | Op::Blendv(_) => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let l = match i.op {
                Op::Pblendv => 1,
                Op::Blendv(Prec::Ps) => 4,
                _ => 8,
            };
            let mask = cpu.xmm[0];
            let mut r = cpu.xmm[n];
            for k in 0..(16 / l as usize) {
                if get(mask, l, k) >> (8 * l as u32 - 1) & 1 != 0 {
                    put(&mut r, l, k, get(s, l, k));
                }
            }
            cpu.xmm[n] = r;
        }
        Op::Insertps => {
            let n = dst_n(&i.ops[0])?;
            let (cnt_s, cnt_d, zmask) = ((imm8 >> 6) as usize, (imm8 >> 4 & 3) as usize, imm8 & 15);
            let v = match i.ops[1] {
                Opnd::Xmm(s) => get(cpu.xmm[s as usize], 4, cnt_s),
                _ => src_x(cpu, mem, &i.ops[1], 4)? as u64,
            };
            let mut r = cpu.xmm[n];
            put(&mut r, 4, cnt_d, v);
            for k in 0..4 {
                if zmask >> k & 1 != 0 {
                    put(&mut r, 4, k, 0);
                }
            }
            cpu.xmm[n] = r;
        }
        Op::Extractps => {
            let v = get(src_x(cpu, mem, &i.ops[1], 16)?, 4, (imm8 & 3) as usize);
            wr(cpu, mem, &i.ops[0], 4, v)?;
        }
        Op::Pinsr(l) => {
            let n = dst_n(&i.ops[0])?;
            let v = rd(cpu, mem, &i.ops[1], if l >= 4 { l } else { l }, 0)?;
            let idx = (imm8 as usize) % (16 / l as usize);
            let mut r = cpu.xmm[n];
            put(&mut r, l, idx, v);
            cpu.xmm[n] = r;
        }
        Op::Pextr(l) => {
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let v = get(s, l, (imm8 as usize) % (16 / l as usize));
            let sz = if matches!(i.ops[0], Opnd::Mem(_)) { l } else if l == 8 { 8 } else { 4 };
            wr(cpu, mem, &i.ops[0], sz, v)?;
        }
        Op::Pmovx(sl, dl, sg) => {
            let cnt = 16 / dl as usize;
            let s = src_x(cpu, mem, &i.ops[1], cnt as u8 * sl)?;
            let mut r = 0u128;
            for k in 0..cnt {
                let x = get(s, sl, k);
                put(&mut r, dl, k, if sg { lsx(x, sl) as u64 } else { x });
            }
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Op::Ptest => {
            let d = cpu.xmm[dst_n(&i.ops[0])?];
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            cpu.rflags &= !ARITH_FLAGS;
            cpu.set_flag(ZF, d & s == 0);
            cpu.set_flag(CF, !d & s == 0);
        }
        Op::Phminposuw => {
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            let (mut best, mut idx) = (get(s, 2, 0), 0u64);
            for k in 1..8 {
                if get(s, 2, k) < best {
                    best = get(s, 2, k);
                    idx = k as u64;
                }
            }
            cpu.xmm[dst_n(&i.ops[0])?] = best as u128 | (idx as u128) << 16;
        }
        Op::Pcmpstr(explicit, mask) => {
            let d = cpu.xmm[dst_n(&i.ops[0])?];
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            pcmpstr(cpu, d, s, imm8, explicit, mask, i.rexw);
        }
        other => return Err(Exit::Unimplemented(other)),
    }
    cpu.rip = next;
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

fn exec_cvt(cpu: &mut Cpu, mem: &mut Memory, i: &Inst, k: CvtK) -> ExecResult<()> {
    use CvtK::*;
    let mode = rc(cpu);
    let osz = if i.rexw { 8 } else { 4 };
    match k {
        Si2Ss | Si2Sd => {
            let n = dst_n(&i.ops[0])?;
            let v = sext(rd(cpu, mem, &i.ops[1], osz, 0)?, osz);
            let d = cpu.xmm[n];
            cpu.xmm[n] = if k == Si2Ss {
                (d & !lane_mask(4)) | (v as f32).to_bits() as u128
            } else {
                (d & !lane_mask(8)) | (v as f64).to_bits() as u128
            };
        }
        Ss2Si | Sd2Si | Tss2Si | Tsd2Si => {
            let single = matches!(k, Ss2Si | Tss2Si);
            let trunc = matches!(k, Tss2Si | Tsd2Si);
            let s = src_x(cpu, mem, &i.ops[1], if single { 4 } else { 8 })?;
            let x = if single { f32::from_bits(s as u32) as f64 } else { f64::from_bits(s as u64) };
            let m = if trunc { 3 } else { mode };
            let r = if osz == 8 { f2i64(x, m) } else { f2i32(x, m) as u64 };
            wr(cpu, mem, &i.ops[0], osz, r)?;
        }
        Ss2Sd => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 4)?;
            let v = f32::from_bits(s as u32) as f64;
            cpu.xmm[n] = (cpu.xmm[n] & !lane_mask(8)) | v.to_bits() as u128;
        }
        Sd2Ss => {
            let n = dst_n(&i.ops[0])?;
            let s = src_x(cpu, mem, &i.ops[1], 8)?;
            let v = f64::from_bits(s as u64) as f32;
            cpu.xmm[n] = (cpu.xmm[n] & !lane_mask(4)) | v.to_bits() as u128;
        }
        Ps2Pd => {
            let s = src_x(cpu, mem, &i.ops[1], 8)?;
            let r = from_f64x2([f32::from_bits(get(s, 4, 0) as u32) as f64, f32::from_bits(get(s, 4, 1) as u32) as f64]);
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Pd2Ps => {
            let s = f64x2(src_x(cpu, mem, &i.ops[1], 16)?);
            let r = from_f32x4([s[0] as f32, s[1] as f32, 0.0, 0.0]);
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Dq2Ps => {
            let s = src_x(cpu, mem, &i.ops[1], 16)?;
            cpu.xmm[dst_n(&i.ops[0])?] = from_f32x4([0, 1, 2, 3].map(|k| lsx(get(s, 4, k), 4) as f32));
        }
        Ps2Dq | Tps2Dq => {
            let s = f32x4(src_x(cpu, mem, &i.ops[1], 16)?);
            let m = if k == Tps2Dq { 3 } else { mode };
            let mut r = 0u128;
            for (k2, x) in s.iter().enumerate() {
                put(&mut r, 4, k2, f2i32(*x as f64, m) as u64);
            }
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
        Dq2Pd => {
            let s = src_x(cpu, mem, &i.ops[1], 8)?;
            cpu.xmm[dst_n(&i.ops[0])?] = from_f64x2([lsx(get(s, 4, 0), 4) as f64, lsx(get(s, 4, 1), 4) as f64]);
        }
        Pd2Dq | Tpd2Dq => {
            let s = f64x2(src_x(cpu, mem, &i.ops[1], 16)?);
            let m = if k == Tpd2Dq { 3 } else { mode };
            let mut r = 0u128;
            put(&mut r, 4, 0, f2i32(s[0], m) as u64);
            put(&mut r, 4, 1, f2i32(s[1], m) as u64);
            cpu.xmm[dst_n(&i.ops[0])?] = r;
        }
    }
    Ok(())
}

/// FXSAVE / FXRSTOR (512-byte legacy layout; x87 registers go through f64 <-> f80 conversion).
pub fn fxsave_restore(cpu: &mut Cpu, mem: &mut Memory, i: &Inst, next: u64) -> ExecResult<()> {
    let Opnd::Mem(m) = i.ops[0] else { return Err(Exit::Undefined) };
    let a = ea(cpu, &m);
    if i.op == Op::Fxsave {
        let mut buf = [0u8; 512];
        buf[0..2].copy_from_slice(&cpu.fpu_cw.to_le_bytes());
        buf[2..4].copy_from_slice(&cpu.fpu_sw.to_le_bytes());
        let ftw: u8 = (0..8).fold(0, |acc, k| acc | if (cpu.fpu_tw >> (2 * k)) & 3 != 3 { 1 << k } else { 0 });
        buf[4] = ftw;
        buf[24..28].copy_from_slice(&cpu.mxcsr.to_le_bytes());
        buf[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
        for k in 0..8 {
            let st = crate::x87::f64_to_f80(cpu.fpu_st[(cpu.fpu_top as usize + k) & 7]);
            buf[32 + 16 * k..32 + 16 * k + 10].copy_from_slice(&st);
        }
        for k in 0..16 {
            buf[160 + 16 * k..176 + 16 * k].copy_from_slice(&cpu.xmm[k].to_le_bytes());
        }
        mem.write_from(a, &buf)?;
    } else {
        let buf = mem.read_vec(a, 512)?;
        cpu.fpu_cw = u16::from_le_bytes([buf[0], buf[1]]);
        cpu.fpu_sw = u16::from_le_bytes([buf[2], buf[3]]);
        cpu.fpu_top = ((cpu.fpu_sw >> 11) & 7) as u8;
        cpu.mxcsr = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        for k in 0..8 {
            let mut raw = [0u8; 10];
            raw.copy_from_slice(&buf[32 + 16 * k..32 + 16 * k + 10]);
            cpu.fpu_st[(cpu.fpu_top as usize + k) & 7] = crate::x87::f80_to_f64(&raw);
        }
        for k in 0..16 {
            cpu.xmm[k] = u128::from_le_bytes(buf[160 + 16 * k..176 + 16 * k].try_into().unwrap());
        }
    }
    cpu.rip = next;
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}
