//! Integer / control-flow interpreter. SSE and x87 live in `sse.rs` / `x87.rs`.

use std::collections::HashMap;

use crate::cpu::*;
use crate::decode::{decode, DecodeError, Inst, MemOp, Op, Opnd};
use crate::mem::{Fault, MemResult, Memory};

/// Why execution of an instruction did not complete normally.
#[derive(Debug, Clone, PartialEq)]
pub enum Exit {
    Fault(Fault),
    DivideError,
    Breakpoint,
    Syscall,
    Int(u8),
    Halt,
    Undefined,
    Decode(DecodeError),
    Unimplemented(Op),
}

impl From<Fault> for Exit {
    fn from(f: Fault) -> Self {
        Exit::Fault(f)
    }
}

pub type ExecResult<T> = Result<T, Exit>;

#[inline]
pub fn msk(sz: u8) -> u64 {
    if sz >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * sz as u32)) - 1
    }
}
#[inline]
pub fn sgn(sz: u8) -> u64 {
    1u64 << (8 * sz as u32 - 1)
}
#[inline]
pub fn sext(v: u64, sz: u8) -> i64 {
    match sz {
        1 => v as i8 as i64,
        2 => v as i16 as i64,
        4 => v as i32 as i64,
        _ => v as i64,
    }
}

/// Linear address of a memory operand (segment base applied, 32-bit wrap if addr32).
#[inline]
pub fn ea(cpu: &Cpu, m: &MemOp) -> u64 {
    let mut a = m.disp as u64;
    if m.base != 0xff {
        a = a.wrapping_add(cpu.gpr[m.base as usize]);
    }
    if m.index != 0xff {
        a = a.wrapping_add(cpu.gpr[m.index as usize] << m.scale);
    }
    if m.addr32 {
        a &= 0xffff_ffff;
    }
    match m.seg {
        1 => a.wrapping_add(cpu.fs_base),
        2 => a.wrapping_add(cpu.gs_base),
        _ => a,
    }
}

pub fn read_mem(mem: &Memory, a: u64, sz: u8) -> MemResult<u64> {
    Ok(match sz {
        1 => mem.read_u8(a)? as u64,
        2 => mem.read_u16(a)? as u64,
        4 => mem.read_u32(a)? as u64,
        _ => mem.read_u64(a)?,
    })
}

pub fn write_mem(mem: &mut Memory, a: u64, sz: u8, v: u64) -> MemResult<()> {
    match sz {
        1 => mem.write_u8(a, v as u8),
        2 => mem.write_u16(a, v as u16),
        4 => mem.write_u32(a, v as u32),
        _ => mem.write_u64(a, v),
    }
}

pub fn rd(cpu: &Cpu, mem: &Memory, o: &Opnd, sz: u8, imm: i64) -> ExecResult<u64> {
    Ok(match o {
        Opnd::Gpr(i) => cpu.gpr[*i as usize] & msk(sz),
        Opnd::GprHi(i) => (cpu.gpr[*i as usize] >> 8) & 0xff,
        Opnd::Imm => (imm as u64) & msk(sz),
        Opnd::Mem(m) => read_mem(mem, ea(cpu, m), sz)?,
        _ => return Err(Exit::Undefined),
    })
}

pub fn wr(cpu: &mut Cpu, mem: &mut Memory, o: &Opnd, sz: u8, v: u64) -> ExecResult<()> {
    match o {
        Opnd::Gpr(i) => {
            let r = &mut cpu.gpr[*i as usize];
            *r = match sz {
                8 => v,
                4 => v & 0xffff_ffff,
                2 => (*r & !0xffff) | (v & 0xffff),
                _ => (*r & !0xff) | (v & 0xff),
            };
        }
        Opnd::GprHi(i) => {
            let r = &mut cpu.gpr[*i as usize];
            *r = (*r & !0xff00) | ((v & 0xff) << 8);
        }
        Opnd::Mem(m) => {
            let a = ea(cpu, m);
            write_mem(mem, a, sz, v)?;
        }
        _ => return Err(Exit::Undefined),
    }
    Ok(())
}

// ── flag helpers ─────────────────────────────────────────────────────────────

#[inline]
fn set_szp(cpu: &mut Cpu, res: u64, sz: u8) {
    let r = res & msk(sz);
    cpu.set_flag(ZF, r == 0);
    cpu.set_flag(SF, r & sgn(sz) != 0);
    cpu.set_flag(PF, parity(r));
}

fn logic_flags(cpu: &mut Cpu, res: u64, sz: u8) {
    set_szp(cpu, res, sz);
    cpu.set_flag(CF, false);
    cpu.set_flag(OF, false);
    cpu.set_flag(AF, false);
}

fn add_op(cpu: &mut Cpu, a: u64, b: u64, c: u64, sz: u8) -> u64 {
    let m = msk(sz);
    let (a, b) = (a & m, b & m);
    let full = a as u128 + b as u128 + c as u128;
    let res = (full as u64) & m;
    cpu.set_flag(CF, full >> (8 * sz as u32) != 0);
    cpu.set_flag(OF, (a ^ res) & (b ^ res) & sgn(sz) != 0);
    cpu.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
    set_szp(cpu, res, sz);
    res
}

fn sub_op(cpu: &mut Cpu, a: u64, b: u64, c: u64, sz: u8) -> u64 {
    let m = msk(sz);
    let (a, b) = (a & m, b & m);
    let res = a.wrapping_sub(b).wrapping_sub(c) & m;
    cpu.set_flag(CF, (a as u128) < b as u128 + c as u128);
    cpu.set_flag(OF, (a ^ b) & (a ^ res) & sgn(sz) != 0);
    cpu.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
    set_szp(cpu, res, sz);
    res
}

fn shift_op(cpu: &mut Cpu, op: Op, v: u64, count: u64, sz: u8) -> u64 {
    let bits = 8 * sz as u32;
    let m = msk(sz);
    let v = v & m;
    let cnt = (count & if sz == 8 { 0x3f } else { 0x1f }) as u32;
    if cnt == 0 {
        return v;
    }
    match op {
        Op::Shl => {
            let res = if cnt >= bits { 0 } else { (v << cnt) & m };
            let cf = if cnt > bits { false } else { (v >> (bits - cnt)) & 1 != 0 };
            cpu.set_flag(CF, cf);
            cpu.set_flag(OF, (res & sgn(sz) != 0) != cf);
            cpu.set_flag(AF, false);
            set_szp(cpu, res, sz);
            res
        }
        Op::Shr => {
            let res = if cnt >= bits { 0 } else { v >> cnt };
            let cf = if cnt > bits { false } else { (v >> (cnt - 1)) & 1 != 0 };
            cpu.set_flag(CF, cf);
            cpu.set_flag(OF, v & sgn(sz) != 0);
            cpu.set_flag(AF, false);
            set_szp(cpu, res, sz);
            res
        }
        Op::Sar => {
            let s = sext(v, sz);
            let sh = cnt.min(63);
            let res = ((s >> sh) as u64) & m;
            let cf = if cnt >= bits { s < 0 } else { (s >> (cnt - 1)) & 1 != 0 };
            cpu.set_flag(CF, cf);
            cpu.set_flag(OF, false);
            cpu.set_flag(AF, false);
            set_szp(cpu, res, sz);
            res
        }
        Op::Rol => {
            let r = cnt % bits;
            let res = if r == 0 { v } else { ((v << r) | (v >> (bits - r))) & m };
            cpu.set_flag(CF, res & 1 != 0);
            cpu.set_flag(OF, ((res >> (bits - 1)) & 1 != 0) != (res & 1 != 0));
            res
        }
        Op::Ror => {
            let r = cnt % bits;
            let res = if r == 0 { v } else { ((v >> r) | (v << (bits - r))) & m };
            cpu.set_flag(CF, res >> (bits - 1) & 1 != 0);
            cpu.set_flag(OF, ((res >> (bits - 1)) & 1) != ((res >> (bits - 2)) & 1));
            res
        }
        Op::Rcl | Op::Rcr => {
            // Rotate through carry over (bits+1)-bit quantity.
            let r = cnt % (bits + 1);
            let mut val = v as u128 | ((cpu.flag(CF) as u128) << bits);
            let w = bits + 1;
            let full_mask = (1u128 << w) - 1;
            if r != 0 {
                val = if op == Op::Rcl {
                    ((val << r) | (val >> (w - r))) & full_mask
                } else {
                    ((val >> r) | (val << (w - r))) & full_mask
                };
            }
            let res = (val as u64) & m;
            cpu.set_flag(CF, (val >> bits) & 1 != 0);
            let msb = res >> (bits - 1) & 1 != 0;
            if op == Op::Rcl {
                cpu.set_flag(OF, msb != cpu.flag(CF));
            } else {
                cpu.set_flag(OF, msb != ((res >> (bits - 2)) & 1 != 0));
            }
            res
        }
        _ => unreachable!(),
    }
}

fn double_shift(cpu: &mut Cpu, left: bool, dst: u64, src: u64, count: u64, sz: u8) -> u64 {
    let bits = 8 * sz as u32;
    let cnt = (count & if sz == 8 { 0x3f } else { 0x1f }) as u32;
    let m = msk(sz);
    let (dst, src) = (dst & m, src & m);
    if cnt == 0 {
        return dst;
    }
    if cnt > bits {
        // Only reachable for 16-bit operands with count 17..=31. The SDM calls the result
        // undefined and CPU vendors differ (an Intel Xeon rotated the 32-bit dst:src / src:dst
        // value, 80/80 probes; a different CI machine disagreed). This follows the Intel
        // behaviour; it is deliberately NOT asserted by the differential test.
        let res = if left {
            let w = ((dst as u32) << 16) | src as u32;
            (w.rotate_left(cnt) >> 16) as u64
        } else {
            let w = ((src as u32) << 16) | dst as u32;
            (w.rotate_right(cnt) & 0xffff) as u64
        };
        let cf = if left {
            (((dst as u32) << 16 | src as u32) >> (32 - cnt)) & 1 != 0
        } else {
            (((src as u32) << 16 | dst as u32) >> (cnt - 1)) & 1 != 0
        };
        cpu.set_flag(CF, cf);
        cpu.set_flag(OF, false);
        set_szp(cpu, res, sz);
        cpu.set_flag(AF, false);
        return res;
    }
    let res = if left {
        let wide = ((dst as u128) << bits) | src as u128;
        ((wide << cnt) >> bits) as u64 & m
    } else {
        let wide = ((src as u128) << bits) | dst as u128;
        ((wide >> cnt) as u64) & m
    };
    let cf = if left { (dst >> (bits - cnt)) & 1 != 0 } else { (dst >> (cnt - 1)) & 1 != 0 };
    cpu.set_flag(CF, cf);
    cpu.set_flag(OF, (res ^ dst) & sgn(sz) != 0);
    cpu.set_flag(AF, false);
    set_szp(cpu, res, sz);
    res
}

fn push(cpu: &mut Cpu, mem: &mut Memory, v: u64, sz: u8) -> ExecResult<()> {
    let sp = cpu.gpr[RSP].wrapping_sub(sz as u64);
    write_mem(mem, sp, sz, v)?;
    cpu.gpr[RSP] = sp;
    Ok(())
}

fn pop(cpu: &mut Cpu, mem: &Memory, sz: u8) -> ExecResult<u64> {
    let v = read_mem(mem, cpu.gpr[RSP], sz)?;
    cpu.gpr[RSP] = cpu.gpr[RSP].wrapping_add(sz as u64);
    Ok(v)
}

fn crc32c(crc: u32, data: u64, sz: u8) -> u32 {
    let mut c = crc;
    for i in 0..sz {
        c ^= ((data >> (8 * i)) & 0xff) as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0x82F6_3B78 } else { c >> 1 };
        }
    }
    c
}

/// CPUID leaves reported to the guest. Features that are not implemented (AVX, BMI, AES, ...)
/// are deliberately hidden so well-behaved software selects its SSE2 paths.
pub fn cpuid(leaf: u32, sub: u32) -> [u32; 4] {
    let _ = sub;
    match leaf {
        0 => [0x16, u32::from_le_bytes(*b"Genu"), u32::from_le_bytes(*b"ntel"), u32::from_le_bytes(*b"ineI")],
        1 => {
            let edx = 1 | 1 << 4 | 1 << 8 | 1 << 15 | 1 << 23 & 0 | 1 << 24 | 1 << 25 | 1 << 26 | 1 << 13 | 1 << 11 & 0 | 1 << 5 & 0;
            // FPU, TSC, CX8, CMOV, FXSR, SSE, SSE2 (MMX deliberately absent)
            let edx = edx | 1 << 5 & 0;
            let ecx = 1 | 1 << 9 | 1 << 19 | 1 << 20 | 1 << 23 | 1 << 13 | 1 << 22;
            // SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, CX16, MOVBE
            [0x000306c3, 0x0001_0800, ecx, edx]
        }
        7 => [0, 0, 0, 0],
        0x8000_0000 => [0x8000_0008, 0, 0, 0],
        0x8000_0001 => [0, 0, 1 | 1 << 5, 1 << 11 | 1 << 20 | 1 << 29],
        // 0x8000_0002..4 brand string
        0x8000_0002 => str4(b"AnyWASM x86-64 vCPU "),
        0x8000_0003 => str4(b"                    "),
        0x8000_0004 => str4(b"                    "),
        0x8000_0008 => [0x3030, 0, 0, 0],
        _ => [0, 0, 0, 0],
    }
}

fn str4(s: &[u8; 20]) -> [u32; 4] {
    let w = |i: usize| u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]);
    [w(0), w(4), w(8), w(12)]
}

/// Fetch, decode and execute one instruction.
pub fn step(cpu: &mut Cpu, mem: &mut Memory) -> ExecResult<()> {
    let mut buf = [0u8; 15];
    let n = mem.fetch(cpu.rip, &mut buf)?;
    let inst = decode(&buf[..n], cpu.rip).map_err(Exit::Decode)?;
    exec(cpu, mem, &inst)
}

/// Execute a decoded instruction. On `Err`, architectural state is unchanged except for the
/// partial effects documented per instruction (REP string ops are restartable).
pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Inst) -> ExecResult<()> {
    let next = cpu.rip.wrapping_add(i.len as u64);
    let sz = i.osz;
    macro_rules! r {
        ($n:expr) => {
            rd(cpu, mem, &i.ops[$n], sz, i.imm)?
        };
    }
    macro_rules! w {
        ($n:expr, $v:expr) => {{
            let v = $v;
            wr(cpu, mem, &i.ops[$n], sz, v)?
        }};
    }
    match i.op {
        Op::Nop | Op::Pause | Op::Fence | Op::Prefetch => {}
        Op::Mov => {
            let v = r!(1);
            w!(0, v);
        }
        Op::Movzx => {
            let v = rd(cpu, mem, &i.ops[1], i.ssz, 0)?;
            w!(0, v);
        }
        Op::Movsx | Op::Movsxd => {
            let v = rd(cpu, mem, &i.ops[1], i.ssz, 0)?;
            w!(0, sext(v, i.ssz) as u64);
        }
        Op::Lea => {
            let Opnd::Mem(m) = i.ops[1] else { return Err(Exit::Undefined) };
            let mut m2 = m;
            m2.seg = 0; // LEA ignores segment bases
            w!(0, ea(cpu, &m2));
        }
        Op::Xchg => {
            let (a, b) = (r!(0), r!(1));
            w!(0, b);
            w!(1, a);
        }
        Op::Push => {
            let v = if let Opnd::Imm = i.ops[0] { i.imm as u64 } else { r!(0) };
            push(cpu, mem, v, i.osz)?;
        }
        Op::Pop => {
            let v = pop(cpu, mem, i.osz)?;
            w!(0, v);
        }
        Op::Pushf => push(cpu, mem, cpu.rflags & !(1 << 16) | 0x2, 8)?,
        Op::Popf => {
            let v = pop(cpu, mem, 8)?;
            cpu.rflags = (cpu.rflags & !USER_FLAGS) | (v & USER_FLAGS) | 0x2;
        }
        Op::Lahf => {
            let f = (cpu.rflags & (SF | ZF | AF | PF | CF)) | 0x2;
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xff00) | f << 8;
        }
        Op::Sahf => {
            let ah = (cpu.gpr[RAX] >> 8) & (SF | ZF | AF | PF | CF);
            cpu.rflags = (cpu.rflags & !(SF | ZF | AF | PF | CF)) | ah;
        }
        Op::Cbw => match sz {
            2 => cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xffff) | (cpu.gpr[RAX] as i8 as i16 as u16 as u64),
            4 => cpu.gpr[RAX] = cpu.gpr[RAX] as i16 as i32 as u32 as u64,
            _ => cpu.gpr[RAX] = cpu.gpr[RAX] as i32 as i64 as u64,
        },
        Op::Cwd => match sz {
            2 => {
                let s = if cpu.gpr[RAX] & 0x8000 != 0 { 0xffff } else { 0 };
                cpu.gpr[RDX] = (cpu.gpr[RDX] & !0xffff) | s;
            }
            4 => cpu.gpr[RDX] = if cpu.gpr[RAX] & 0x8000_0000 != 0 { 0xffff_ffff } else { 0 },
            _ => cpu.gpr[RDX] = if cpu.gpr[RAX] >> 63 != 0 { u64::MAX } else { 0 },
        },
        Op::Xlat => {
            let a = cpu.gpr[RBX].wrapping_add(cpu.gpr[RAX] & 0xff);
            let v = mem.read_u8(a)?;
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xff) | v as u64;
        }
        Op::Bswap => {
            let v = r!(0);
            let res = if sz == 8 { v.swap_bytes() } else { (v as u32).swap_bytes() as u64 };
            w!(0, res);
        }
        Op::Movbe => {
            let v = r!(1);
            let res = match sz {
                2 => (v as u16).swap_bytes() as u64,
                4 => (v as u32).swap_bytes() as u64,
                _ => v.swap_bytes(),
            };
            w!(0, res);
        }

        // ── ALU ──
        Op::Add | Op::Adc => {
            let (a, b) = (r!(0), r!(1));
            let c = if i.op == Op::Adc { cpu.flag(CF) as u64 } else { 0 };
            let res = add_op(cpu, a, b, c, sz);
            w!(0, res);
        }
        Op::Sub | Op::Sbb | Op::Cmp => {
            let (a, b) = (r!(0), r!(1));
            let c = if i.op == Op::Sbb { cpu.flag(CF) as u64 } else { 0 };
            let res = sub_op(cpu, a, b, c, sz);
            if i.op != Op::Cmp {
                w!(0, res);
            }
        }
        Op::And | Op::Or | Op::Xor | Op::Test => {
            let (a, b) = (r!(0), r!(1));
            let res = match i.op {
                Op::And | Op::Test => a & b,
                Op::Or => a | b,
                _ => a ^ b,
            };
            logic_flags(cpu, res, sz);
            if i.op != Op::Test {
                w!(0, res);
            }
        }
        Op::Inc | Op::Dec => {
            let a = r!(0);
            let cf = cpu.flag(CF);
            let res = if i.op == Op::Inc { add_op(cpu, a, 1, 0, sz) } else { sub_op(cpu, a, 1, 0, sz) };
            cpu.set_flag(CF, cf);
            w!(0, res);
        }
        Op::Neg => {
            let a = r!(0);
            let res = sub_op(cpu, 0, a, 0, sz);
            w!(0, res);
        }
        Op::Not => {
            let a = r!(0);
            w!(0, !a);
        }
        Op::Mul => {
            let a = r!(0) as u128;
            let (lo, hi) = match sz {
                1 => {
                    let p = (cpu.gpr[RAX] & 0xff) as u128 * a;
                    cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xffff) | p as u64 & 0xffff;
                    (p as u64 & 0xff, (p >> 8) as u64 & 0xff)
                }
                _ => {
                    let p = (cpu.gpr[RAX] & msk(sz)) as u128 * a;
                    let lo = p as u64 & msk(sz);
                    let hi = (p >> (8 * sz as u32)) as u64 & msk(sz);
                    wr(cpu, mem, &Opnd::Gpr(0), sz, lo)?;
                    wr(cpu, mem, &Opnd::Gpr(2), sz, hi)?;
                    (lo, hi)
                }
            };
            let _ = lo;
            cpu.set_flag(CF, hi != 0);
            cpu.set_flag(OF, hi != 0);
            cpu.set_flag(SF, false);
            cpu.set_flag(ZF, false);
            cpu.set_flag(AF, false);
            cpu.set_flag(PF, false);
        }
        Op::Imul1 => {
            let a = sext(r!(0), sz) as i128;
            let acc = sext(cpu.gpr[RAX] & msk(sz), sz) as i128;
            let p = a * acc;
            let lo = (p as u64) & msk(sz);
            let fits = sext(lo, sz) as i128 == p;
            if sz == 1 {
                cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xffff) | (p as u64 & 0xffff);
            } else {
                let hi = ((p >> (8 * sz as u32)) as u64) & msk(sz);
                wr(cpu, mem, &Opnd::Gpr(0), sz, lo)?;
                wr(cpu, mem, &Opnd::Gpr(2), sz, hi)?;
            }
            cpu.set_flag(CF, !fits);
            cpu.set_flag(OF, !fits);
            cpu.set_flag(SF, false);
            cpu.set_flag(ZF, false);
            cpu.set_flag(AF, false);
            cpu.set_flag(PF, false);
        }
        Op::Imul2 | Op::Imul3 => {
            let (a, b) = if i.op == Op::Imul2 {
                (sext(r!(0), sz), sext(r!(1), sz))
            } else {
                (sext(r!(1), sz), sext(i.imm as u64 & msk(sz), sz))
            };
            let p = a as i128 * b as i128;
            let res = (p as u64) & msk(sz);
            let fits = sext(res, sz) as i128 == p;
            w!(0, res);
            cpu.set_flag(CF, !fits);
            cpu.set_flag(OF, !fits);
            // SF/ZF/PF/AF undefined; real hardware sets SF/ZF/PF from the result.
            set_szp(cpu, res, sz);
            cpu.set_flag(AF, false);
        }
        Op::Div | Op::Idiv => {
            let d = r!(0);
            if d == 0 {
                return Err(Exit::DivideError);
            }
            let signed = i.op == Op::Idiv;
            let (q, rem) = if sz == 1 {
                let n = cpu.gpr[RAX] & 0xffff;
                if signed {
                    let (n, d) = (n as u16 as i16 as i32, d as u8 as i8 as i32);
                    let (q, r) = (n / d, n % d);
                    if q > 127 || q < -128 {
                        return Err(Exit::DivideError);
                    }
                    (q as u64 & 0xff, r as u64 & 0xff)
                } else {
                    let (q, r) = (n / d, n % d);
                    if q > 0xff {
                        return Err(Exit::DivideError);
                    }
                    (q, r)
                }
            } else {
                let bits = 8 * sz as u32;
                let lo = cpu.gpr[RAX] & msk(sz);
                let hi = cpu.gpr[RDX] & msk(sz);
                if signed {
                    let n = (((hi as u128) << bits) | lo as u128) as i128;
                    let n = (n << (128 - 2 * bits)) >> (128 - 2 * bits);
                    let dd = sext(d, sz) as i128;
                    let (q, r) = (n.wrapping_div(dd), n.wrapping_rem(dd));
                    let lim = 1i128 << (bits - 1);
                    if q >= lim || q < -lim {
                        return Err(Exit::DivideError);
                    }
                    (q as u64 & msk(sz), r as u64 & msk(sz))
                } else {
                    let n = ((hi as u128) << bits) | lo as u128;
                    let (q, r) = (n / d as u128, n % d as u128);
                    if q >> bits != 0 {
                        return Err(Exit::DivideError);
                    }
                    (q as u64, r as u64)
                }
            };
            if sz == 1 {
                cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xffff) | q | rem << 8;
            } else {
                wr(cpu, mem, &Opnd::Gpr(0), sz, q)?;
                wr(cpu, mem, &Opnd::Gpr(2), sz, rem)?;
            }
        }
        Op::Rol | Op::Ror | Op::Rcl | Op::Rcr | Op::Shl | Op::Shr | Op::Sar => {
            let v = r!(0);
            let count = match i.ops[1] {
                Opnd::Imm => i.imm as u64,
                _ => cpu.gpr[RCX] & 0xff,
            };
            let res = shift_op(cpu, i.op, v, count, sz);
            w!(0, res);
        }
        Op::Shld | Op::Shrd => {
            let count = match i.ops[2] {
                Opnd::Imm => i.imm as u64,
                _ => cpu.gpr[RCX] & 0xff,
            };
            let (d, s) = (r!(0), r!(1));
            let res = double_shift(cpu, i.op == Op::Shld, d, s, count, sz);
            w!(0, res);
        }
        Op::Bt | Op::Bts | Op::Btr | Op::Btc => {
            let bits = 8 * sz as i64;
            let (addr_adj, bit): (Option<u64>, u64);
            let off = match i.ops[1] {
                Opnd::Imm => (i.imm as u64 & (bits as u64 - 1)) as i64,
                _ => sext(rd(cpu, mem, &i.ops[1], sz, 0)?, sz),
            };
            match i.ops[0] {
                Opnd::Mem(m) if !matches!(i.ops[1], Opnd::Imm) => {
                    let word = off.div_euclid(bits);
                    bit = off.rem_euclid(bits) as u64;
                    addr_adj = Some(ea(cpu, &m).wrapping_add((word * sz as i64) as u64));
                }
                Opnd::Mem(m) => {
                    bit = off as u64;
                    addr_adj = Some(ea(cpu, &m));
                }
                _ => {
                    bit = off as u64 & (bits as u64 - 1);
                    addr_adj = None;
                }
            }
            let val = match addr_adj {
                Some(a) => read_mem(mem, a, sz)?,
                None => rd(cpu, mem, &i.ops[0], sz, 0)?,
            };
            cpu.set_flag(CF, val >> bit & 1 != 0);
            let newv = match i.op {
                Op::Bts => val | 1 << bit,
                Op::Btr => val & !(1 << bit),
                Op::Btc => val ^ 1 << bit,
                _ => val,
            };
            if i.op != Op::Bt {
                match addr_adj {
                    Some(a) => write_mem(mem, a, sz, newv)?,
                    None => w!(0, newv),
                }
            }
        }
        Op::Bsf | Op::Bsr => {
            let v = r!(1);
            if v == 0 {
                cpu.set_flag(ZF, true);
            } else {
                cpu.set_flag(ZF, false);
                let idx = if i.op == Op::Bsf { v.trailing_zeros() } else { 63 - v.leading_zeros() };
                w!(0, idx as u64);
            }
        }
        Op::Tzcnt | Op::Lzcnt => {
            let v = r!(1);
            let bits = 8 * sz as u32;
            let res = if v == 0 {
                bits as u64
            } else if i.op == Op::Tzcnt {
                v.trailing_zeros() as u64
            } else {
                (v.leading_zeros() - (64 - bits)) as u64
            };
            cpu.set_flag(CF, v == 0);
            cpu.set_flag(ZF, res == 0);
            w!(0, res);
        }
        Op::Popcnt => {
            let v = r!(1);
            let res = v.count_ones() as u64;
            cpu.rflags &= !(CF | OF | SF | AF | PF);
            cpu.set_flag(ZF, v == 0);
            w!(0, res);
        }
        Op::Crc32 => {
            let src = rd(cpu, mem, &i.ops[1], i.ssz, 0)?;
            let acc = rd(cpu, mem, &i.ops[0], 4, 0)? as u32;
            let res = crc32c(acc, src, i.ssz) as u64;
            wr(cpu, mem, &i.ops[0], i.osz.max(4), res)?;
        }
        Op::Adcx | Op::Adox => {
            let flag = if i.op == Op::Adcx { CF } else { OF };
            let (a, b) = (r!(0), r!(1));
            let c = cpu.flag(flag) as u128;
            let full = a as u128 + b as u128 + c;
            w!(0, full as u64);
            cpu.set_flag(flag, full >> (8 * sz as u32) != 0);
        }
        Op::Cmpxchg => {
            let dst = r!(0);
            let acc = cpu.gpr[RAX] & msk(sz);
            sub_op(cpu, acc, dst, 0, sz);
            if acc == dst {
                let s = r!(1);
                w!(0, s);
            } else {
                wr(cpu, mem, &Opnd::Gpr(0), sz, dst)?;
            }
        }
        Op::Cmpxchg8b | Op::Cmpxchg16b => {
            let Opnd::Mem(m) = i.ops[0] else { return Err(Exit::Undefined) };
            let a = ea(cpu, &m);
            if i.op == Op::Cmpxchg8b {
                let cur = mem.read_u64(a)?;
                let cmp = (cpu.gpr[RDX] & 0xffff_ffff) << 32 | (cpu.gpr[RAX] & 0xffff_ffff);
                if cur == cmp {
                    mem.write_u64(a, (cpu.gpr[RCX] & 0xffff_ffff) << 32 | (cpu.gpr[RBX] & 0xffff_ffff))?;
                    cpu.set_flag(ZF, true);
                } else {
                    cpu.gpr[RAX] = cur & 0xffff_ffff;
                    cpu.gpr[RDX] = cur >> 32;
                    cpu.set_flag(ZF, false);
                }
            } else {
                if a & 15 != 0 {
                    return Err(Exit::Fault(Fault { addr: a, access: crate::mem::Access::Write, protection: true }));
                }
                let cur = mem.read_u128(a)?;
                let cmp = (cpu.gpr[RDX] as u128) << 64 | cpu.gpr[RAX] as u128;
                if cur == cmp {
                    mem.write_u128(a, (cpu.gpr[RCX] as u128) << 64 | cpu.gpr[RBX] as u128)?;
                    cpu.set_flag(ZF, true);
                } else {
                    cpu.gpr[RAX] = cur as u64;
                    cpu.gpr[RDX] = (cur >> 64) as u64;
                    cpu.set_flag(ZF, false);
                }
            }
        }
        Op::Xadd => {
            let (d, s) = (r!(0), r!(1));
            let res = add_op(cpu, d, s, 0, sz);
            w!(1, d);
            w!(0, res);
        }

        // ── control flow ──
        Op::Jmp => {
            cpu.rip = i.imm as u64;
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::JmpInd => {
            let t = r!(0);
            cpu.rip = t;
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::Jcc => {
            cpu.rip = if cond(cpu.rflags, i.cc) { i.imm as u64 } else { next };
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::Call => {
            push(cpu, mem, next, 8)?;
            cpu.rip = i.imm as u64;
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::CallInd => {
            let t = r!(0);
            push(cpu, mem, next, 8)?;
            cpu.rip = t;
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::Ret | Op::RetImm => {
            let t = pop(cpu, mem, 8)?;
            if i.op == Op::RetImm {
                cpu.gpr[RSP] = cpu.gpr[RSP].wrapping_add(i.imm as u64);
            }
            cpu.rip = t;
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::Loop | Op::Loope | Op::Loopne | Op::Jrcxz => {
            let m = if i.ssz == 4 { 0xffff_ffff } else { u64::MAX };
            let mut take;
            if i.op == Op::Jrcxz {
                take = cpu.gpr[RCX] & m == 0;
            } else {
                let c = (cpu.gpr[RCX] & m).wrapping_sub(1) & m;
                cpu.gpr[RCX] = if i.ssz == 4 { c } else { c };
                take = c != 0;
                match i.op {
                    Op::Loope => take &= cpu.flag(ZF),
                    Op::Loopne => take &= !cpu.flag(ZF),
                    _ => {}
                }
            }
            cpu.rip = if take { i.imm as u64 } else { next };
            cpu.icount += 1;
            cpu.tsc += 1;
            return Ok(());
        }
        Op::Setcc => {
            let v = cond(cpu.rflags, i.cc) as u64;
            wr(cpu, mem, &i.ops[0], 1, v)?;
        }
        Op::Cmov => {
            let v = r!(1);
            if cond(cpu.rflags, i.cc) {
                w!(0, v);
            } else if sz == 4 {
                let cur = rd(cpu, mem, &i.ops[0], 4, 0)?;
                wr(cpu, mem, &i.ops[0], 4, cur)?;
            }
        }
        Op::Leave => {
            cpu.gpr[RSP] = cpu.gpr[RBP];
            let v = pop(cpu, mem, 8)?;
            cpu.gpr[RBP] = v;
        }
        Op::Enter => {
            push(cpu, mem, cpu.gpr[RBP], 8)?;
            cpu.gpr[RBP] = cpu.gpr[RSP];
            cpu.gpr[RSP] = cpu.gpr[RSP].wrapping_sub(i.imm as u64);
        }

        // ── string operations ──
        Op::Movs | Op::Cmps | Op::Stos | Op::Lods | Op::Scas => {
            return exec_string(cpu, mem, i, next);
        }

        // ── flags and system ──
        Op::Clc => cpu.set_flag(CF, false),
        Op::Stc => cpu.set_flag(CF, true),
        Op::Cmc => {
            let c = cpu.flag(CF);
            cpu.set_flag(CF, !c)
        }
        Op::Cld => cpu.set_flag(DF, false),
        Op::Std => cpu.set_flag(DF, true),
        Op::Cli | Op::Sti => return Err(Exit::Undefined),
        Op::Hlt => return Err(Exit::Halt),
        Op::Int3 => {
            cpu.rip = next;
            return Err(Exit::Breakpoint);
        }
        Op::Int => {
            cpu.rip = next;
            return Err(Exit::Int(i.imm as u8));
        }
        Op::Ud2 => return Err(Exit::Undefined),
        Op::Syscall => {
            cpu.rip = next;
            return Err(Exit::Syscall);
        }
        Op::Cpuid => {
            let [a, b, c, d] = cpuid(cpu.gpr[RAX] as u32, cpu.gpr[RCX] as u32);
            cpu.gpr[RAX] = a as u64;
            cpu.gpr[RBX] = b as u64;
            cpu.gpr[RCX] = c as u64;
            cpu.gpr[RDX] = d as u64;
        }
        Op::Rdtsc | Op::Rdtscp => {
            let t = cpu.tsc;
            cpu.gpr[RAX] = t & 0xffff_ffff;
            cpu.gpr[RDX] = t >> 32;
            if i.op == Op::Rdtscp {
                cpu.gpr[RCX] = 0;
            }
        }
        Op::Xgetbv => {
            cpu.gpr[RAX] = 3;
            cpu.gpr[RDX] = 0;
        }
        Op::Rdrand => {
            // Deterministic xorshift64*: reproducible runs matter more than entropy here.
            let mut x = cpu.tsc ^ 0x9E37_79B9_7F4A_7C15 ^ cpu.icount.rotate_left(17);
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
            w!(0, v);
            cpu.rflags = (cpu.rflags & !ARITH_FLAGS) | CF;
        }
        Op::Ldmxcsr => {
            let Opnd::Mem(m) = i.ops[0] else { return Err(Exit::Undefined) };
            cpu.mxcsr = mem.read_u32(ea(cpu, &m))?;
        }
        Op::Stmxcsr => {
            let Opnd::Mem(m) = i.ops[0] else { return Err(Exit::Undefined) };
            mem.write_u32(ea(cpu, &m), cpu.mxcsr)?;
        }
        Op::Fxsave | Op::Fxrstor => return crate::sse::fxsave_restore(cpu, mem, i, next),
        Op::X87 => return crate::x87::exec(cpu, mem, i, next),
        _ => return crate::sse::exec(cpu, mem, i, next),
    }
    cpu.rip = next;
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

fn exec_string(cpu: &mut Cpu, mem: &mut Memory, i: &Inst, next: u64) -> ExecResult<()> {
    let sz = i.osz;
    let Opnd::Mem(mo) = i.ops[0] else { return Err(Exit::Undefined) };
    let seg_base = match mo.seg {
        1 => cpu.fs_base,
        2 => cpu.gs_base,
        _ => 0,
    };
    let am: u64 = if mo.addr32 { 0xffff_ffff } else { u64::MAX };
    let step = if cpu.flag(DF) { (sz as u64).wrapping_neg() } else { sz as u64 };
    let rep = i.rep != 0;
    let cmpish = matches!(i.op, Op::Cmps | Op::Scas);
    loop {
        if rep && cpu.gpr[RCX] & am == 0 {
            break;
        }
        let si = seg_base.wrapping_add(cpu.gpr[RSI] & am);
        let di = cpu.gpr[RDI] & am;
        match i.op {
            Op::Movs => {
                let v = read_mem(mem, si, sz)?;
                write_mem(mem, di, sz, v)?;
                cpu.gpr[RSI] = adv(cpu.gpr[RSI], step, am);
                cpu.gpr[RDI] = adv(cpu.gpr[RDI], step, am);
            }
            Op::Stos => {
                write_mem(mem, di, sz, cpu.gpr[RAX] & msk(sz))?;
                cpu.gpr[RDI] = adv(cpu.gpr[RDI], step, am);
            }
            Op::Lods => {
                let v = read_mem(mem, si, sz)?;
                wr(cpu, mem, &Opnd::Gpr(0), sz, v)?;
                cpu.gpr[RSI] = adv(cpu.gpr[RSI], step, am);
            }
            Op::Scas => {
                let v = read_mem(mem, di, sz)?;
                let acc = cpu.gpr[RAX] & msk(sz);
                sub_op(cpu, acc, v, 0, sz);
                cpu.gpr[RDI] = adv(cpu.gpr[RDI], step, am);
            }
            _ => {
                let a = read_mem(mem, si, sz)?;
                let b = read_mem(mem, di, sz)?;
                sub_op(cpu, a, b, 0, sz);
                cpu.gpr[RSI] = adv(cpu.gpr[RSI], step, am);
                cpu.gpr[RDI] = adv(cpu.gpr[RDI], step, am);
            }
        }
        if !rep {
            break;
        }
        cpu.gpr[RCX] = adv(cpu.gpr[RCX], 1u64.wrapping_neg(), am);
        if cmpish {
            // F3 = repe (stop when ZF clear), F2 = repne (stop when ZF set)
            if (i.rep == 1 && !cpu.flag(ZF)) || (i.rep == 2 && cpu.flag(ZF)) {
                break;
            }
        }
    }
    cpu.rip = next;
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

#[inline]
fn adv(reg: u64, delta: u64, am: u64) -> u64 {
    if am == u64::MAX {
        reg.wrapping_add(delta)
    } else {
        (reg.wrapping_add(delta)) & am
    }
}


/// Direct-mapped cache of decoded instructions, valid while `Memory::code_epoch` is unchanged.
pub struct DecodeCache {
    entries: Vec<(u64, u64, Inst)>,
}

const DC_BITS: usize = 15;

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeCache {
    pub fn new() -> Self {
        let dummy = crate::decode::decode(&[0x90], 0).unwrap();
        DecodeCache { entries: vec![(u64::MAX, u64::MAX, dummy); 1 << DC_BITS] }
    }

    /// Fetch (cached), decode and execute one instruction.
    #[inline]
    pub fn step(&mut self, cpu: &mut Cpu, mem: &mut Memory) -> ExecResult<()> {
        let rip = cpu.rip;
        let idx = ((rip ^ (rip >> DC_BITS)) as usize) & ((1 << DC_BITS) - 1);
        let e = &mut self.entries[idx];
        if e.0 != rip || e.1 != mem.code_epoch {
            let mut buf = [0u8; 15];
            let n = mem.fetch(rip, &mut buf)?;
            let inst = decode(&buf[..n], rip).map_err(Exit::Decode)?;
            *e = (rip, mem.code_epoch, inst);
        }
        let inst = e.2;
        exec(cpu, mem, &inst)
    }
}

/// Maximum number of decoded instructions retained in one straight-line block.
pub const MAX_BLOCK_INSTRUCTIONS: usize = 64;

/// A decoded basic block. `fallthrough_rip` is the address after the final instruction,
/// including when that instruction is a branch; `terminated` distinguishes control-flow ends
/// from blocks split only because of [`MAX_BLOCK_INSTRUCTIONS`].
#[derive(Debug)]
pub struct DecodedBlock {
    pub start: u64,
    pub instructions: Vec<Inst>,
    pub fallthrough_rip: u64,
    pub terminated: bool,
    /// A decode or execute-fetch error discovered after this block's valid instructions.
    /// The executor surfaces it when execution reaches the recorded RIP.
    pub deferred_exit: Option<(u64, Exit)>,
}

/// Cache of decoded basic blocks, invalidated whenever executable memory changes.
pub struct BlockCache {
    code_epoch: Option<u64>,
    blocks: HashMap<u64, CachedBlock>,
    #[cfg(target_arch = "wasm32")]
    next_jit_id: u32,
}

const MAX_CACHED_BLOCKS: usize = 1024;

struct CachedBlock {
    decoded: DecodedBlock,
    instructions: Vec<CachedInstruction>,
    #[cfg(target_arch = "wasm32")]
    emitted: Option<crate::jit::EmittedBlock>,
}

#[derive(Clone, Copy)]
struct CachedInstruction {
    inst: Inst,
    op: CachedOp,
}

#[derive(Clone, Copy)]
enum CachedOp {
    NoOp,
    Mov {
        dst: Opnd,
        src: Opnd,
        size: u8,
        imm: i64,
    },
    Lea {
        dst: Opnd,
        address: MemOp,
        size: u8,
    },
    Interpreter,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockCache {
    pub fn new() -> Self {
        Self {
            code_epoch: None,
            blocks: HashMap::new(),
            #[cfg(target_arch = "wasm32")]
            next_jit_id: 1,
        }
    }

    /// Return a cached block or decode a straight-line block beginning at `start`.
    ///
    /// The block includes the first control-flow or terminal instruction. A linear block that
    /// reaches [`MAX_BLOCK_INSTRUCTIONS`] is returned with `terminated == false`.
    pub fn get_block(&mut self, start: u64, mem: &Memory) -> ExecResult<&DecodedBlock> {
        Ok(&self.get_cached_block(start, mem)?.decoded)
    }

    /// Execute a decoded block up to `max_instructions` retired guest instructions.
    ///
    /// NOP-like instructions, register/immediate MOV, register/register MOV, and register LEA
    /// execute through cached operations. Every other instruction uses the normal interpreter.
    /// Execution stops at block boundaries, the instruction budget, unexpected control flow, or
    /// any change to executable memory.
    pub fn execute(
        &mut self,
        cpu: &mut Cpu,
        mem: &mut Memory,
        max_instructions: u64,
    ) -> ExecResult<u64> {
        if max_instructions == 0 {
            return Ok(0);
        }

        let block = self.get_cached_block(cpu.rip, mem)?;
        #[cfg(target_arch = "wasm32")]
        let (mut executed, start_index, interpreter_only) = match block.emitted.as_ref() {
            Some(emitted) => match emitted.execute(cpu, max_instructions) {
                Some(executed) => (executed as u64, emitted.instruction_count(), true),
                None => (0, 0, true),
            },
            None => (0, 0, false),
        };
        #[cfg(not(target_arch = "wasm32"))]
        let (mut executed, start_index, interpreter_only) = (0, 0, false);

        for cached in block.instructions.iter().skip(start_index) {
            if executed == max_instructions {
                break;
            }
            let before_rip = cpu.rip;
            let before_epoch = mem.code_epoch;
            if interpreter_only {
                exec(cpu, mem, &cached.inst)?;
            } else {
                execute_cached(cpu, mem, cached)?;
            }
            executed += 1;

            let sequential_rip = before_rip.wrapping_add(cached.inst.len as u64);
            if mem.code_epoch != before_epoch
                || ends_basic_block(cached.inst.op)
                || cpu.rip != sequential_rip
            {
                break;
            }
        }
        Ok(executed)
    }

    fn get_cached_block(&mut self, start: u64, mem: &Memory) -> ExecResult<&CachedBlock> {
        if self.code_epoch != Some(mem.code_epoch) {
            self.blocks.clear();
            self.code_epoch = Some(mem.code_epoch);
        }
        if !self.blocks.contains_key(&start) {
            if self.blocks.len() >= MAX_CACHED_BLOCKS {
                self.blocks.clear();
            }
            let decoded = decode_block(start, mem)?;
            let instructions = decoded
                .instructions
                .iter()
                .copied()
                .map(translate_instruction)
                .collect();
            #[cfg(target_arch = "wasm32")]
            let emitted = crate::jit::emit_mov_block(
                self.next_jit_id,
                decoded.start,
                &decoded.instructions,
            );
            #[cfg(target_arch = "wasm32")]
            if emitted.is_some() {
                self.next_jit_id = self.next_jit_id.wrapping_add(1).max(1);
            }
            let block = CachedBlock {
                decoded,
                instructions,
                #[cfg(target_arch = "wasm32")]
                emitted,
            };
            self.blocks.insert(start, block);
        }
        Ok(self.blocks.get(&start).unwrap())
    }
}

fn translate_instruction(inst: Inst) -> CachedInstruction {
    let op = match inst.op {
        Op::Nop | Op::Pause | Op::Fence | Op::Prefetch => CachedOp::NoOp,
        Op::Mov
            if matches!(inst.ops[0], Opnd::Gpr(_) | Opnd::GprHi(_))
                && matches!(inst.ops[1], Opnd::Gpr(_) | Opnd::GprHi(_) | Opnd::Imm) =>
        {
            CachedOp::Mov {
                dst: inst.ops[0],
                src: inst.ops[1],
                size: inst.osz,
                imm: inst.imm,
            }
        }
        Op::Lea => match (inst.ops[0], inst.ops[1]) {
            (dst @ Opnd::Gpr(_), Opnd::Mem(address)) => CachedOp::Lea {
                dst,
                address,
                size: inst.osz,
            },
            _ => CachedOp::Interpreter,
        },
        _ => CachedOp::Interpreter,
    };
    CachedInstruction { inst, op }
}

fn execute_cached(cpu: &mut Cpu, mem: &mut Memory, cached: &CachedInstruction) -> ExecResult<()> {
    match cached.op {
        CachedOp::NoOp => {}
        CachedOp::Mov {
            dst,
            src,
            size,
            imm,
        } => {
            let value = rd(cpu, mem, &src, size, imm)?;
            wr(cpu, mem, &dst, size, value)?;
        }
        CachedOp::Lea {
            dst,
            mut address,
            size,
        } => {
            address.seg = 0;
            let value = ea(cpu, &address);
            wr(cpu, mem, &dst, size, value)?;
        }
        CachedOp::Interpreter => return exec(cpu, mem, &cached.inst),
    }

    cpu.rip = cpu.rip.wrapping_add(cached.inst.len as u64);
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

fn decode_block(start: u64, mem: &Memory) -> ExecResult<DecodedBlock> {
    let mut rip = start;
    let mut instructions = Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS);
    for _ in 0..MAX_BLOCK_INSTRUCTIONS {
        let mut buf = [0u8; 15];
        let n = match mem.fetch(rip, &mut buf) {
            Ok(n) => n,
            Err(fault) if !instructions.is_empty() => {
                return Ok(partial_block(start, instructions, rip, Exit::Fault(fault)));
            }
            Err(fault) => return Err(Exit::Fault(fault)),
        };
        let inst = match decode(&buf[..n], rip) {
            Ok(inst) => inst,
            Err(error) if !instructions.is_empty() => {
                return Ok(partial_block(start, instructions, rip, Exit::Decode(error)));
            }
            Err(error) => return Err(Exit::Decode(error)),
        };
        rip = rip.wrapping_add(inst.len as u64);
        let terminated = ends_basic_block(inst.op);
        instructions.push(inst);
        if terminated {
            return Ok(DecodedBlock {
                start,
                instructions,
                fallthrough_rip: rip,
                terminated,
                deferred_exit: None,
            });
        }
    }
    Ok(DecodedBlock {
        start,
        instructions,
        fallthrough_rip: rip,
        terminated: false,
        deferred_exit: None,
    })
}

fn partial_block(start: u64, instructions: Vec<Inst>, rip: u64, exit: Exit) -> DecodedBlock {
    DecodedBlock {
        start,
        instructions,
        fallthrough_rip: rip,
        terminated: false,
        deferred_exit: Some((rip, exit)),
    }
}

fn ends_basic_block(op: Op) -> bool {
    matches!(
        op,
        Op::Jmp
            | Op::JmpInd
            | Op::Jcc
            | Op::Call
            | Op::CallInd
            | Op::Ret
            | Op::RetImm
            | Op::Loop
            | Op::Loope
            | Op::Loopne
            | Op::Jrcxz
            | Op::Int3
            | Op::Int
            | Op::Hlt
            | Op::Ud2
            | Op::Syscall
    )
}
