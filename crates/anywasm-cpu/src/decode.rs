//! x86-64 instruction decoder (legacy + REX encodings and selected VEX forms; no EVEX yet).
//!
//! `decode` turns up to 15 bytes into an [`Inst`]. Operand convention: `ops[0]` is the
//! destination (and first source for two-operand forms), `ops[1]` the source. SSE store forms
//! (e.g. `0F 11`) are swapped by the decoder so `ops[0]` is always the destination.

use std::fmt;
use crate::cpu::CpuMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prec {
    Ps,
    Pd,
    Ss,
    Sd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpK {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Sqrt,
    Rcp,
    Rsqrt,
    And,
    Andn,
    Or,
    Xor,
    Hadd,
    Hsub,
    Addsub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiK {
    Add,
    Sub,
    /// Saturating add/sub (signedness in `Op::Pi.2`).
    Adds,
    Subs,
    Mullo,
    Mulhi,
    Mulhrsw,
    Muludq,
    Muldq,
    Madd,
    Maddubs,
    Avg,
    Min,
    Max,
    Cmpeq,
    Cmpgt,
    And,
    Andn,
    Or,
    Xor,
    Sad,
    Abs,
    Sign,
    Hadd,
    Hsub,
    Hadds,
    Hsubs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShK {
    Sll,
    Srl,
    Sra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CvtK {
    Si2Ss,
    Si2Sd,
    Ss2Si,
    Sd2Si,
    Tss2Si,
    Tsd2Si,
    Ss2Sd,
    Sd2Ss,
    Ps2Pd,
    Pd2Ps,
    Dq2Ps,
    Ps2Dq,
    Tps2Dq,
    Dq2Pd,
    Pd2Dq,
    Tpd2Dq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    // data movement
    Mov,
    Movzx,
    Movsx,
    Movsxd,
    Lea,
    Push,
    Pop,
    Xchg,
    Movbe,
    Bswap,
    Cbw,
    Cwd,
    Lahf,
    Sahf,
    Pushf,
    Popf,
    Xlat,
    // ALU
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Cmp,
    Test,
    Inc,
    Dec,
    Neg,
    Not,
    Mul,
    Imul1,
    Imul2,
    Imul3,
    Div,
    Idiv,
    Rol,
    Ror,
    Rcl,
    Rcr,
    Shl,
    Shr,
    Sar,
    Shld,
    Shrd,
    Bt,
    Bts,
    Btr,
    Btc,
    Bsf,
    Bsr,
    Tzcnt,
    Lzcnt,
    Popcnt,
    Cmpxchg,
    Cmpxchg8b,
    Cmpxchg16b,
    Xadd,
    Crc32,
    Adcx,
    Adox,
    Andn,
    Blsi,
    Bzhi,
    Shlx,
    Shrx,
    // control flow
    Jmp,
    JmpInd,
    Jcc,
    Call,
    CallInd,
    Ret,
    RetImm,
    Loop,
    Loope,
    Loopne,
    Jrcxz,
    Setcc,
    Cmov,
    Leave,
    Enter,
    // strings
    Movs,
    Cmps,
    Stos,
    Lods,
    Scas,
    // flags & misc
    Clc,
    Stc,
    Cmc,
    Cld,
    Std,
    Cli,
    Sti,
    Nop,
    Pause,
    Hlt,
    Int3,
    Int,
    Ud2,
    Syscall,
    Cpuid,
    Rdtsc,
    Rdtscp,
    Xgetbv,
    Fence,
    Prefetch,
    Ldmxcsr,
    Stmxcsr,
    Fxsave,
    Fxrstor,
    Rdrand,
    X87,
    // SSE family
    MovVec,
    Vmovups,
    Vmovss,
    Vmovsd,
    Vmovd,
    Vmovq,
    Vmovdqu,
    Vmovdqa,
    Vextractf128,
    Vinserti128,
    Vpxor,
    Vpbroadcastd,
    Vpbroadcastq,
    VShiftQ { left: bool, immediate: bool },
    Vpmovsxdq,
    Vpaddd,
    Vpsllq,
    Vpaddq,
    Vpsrldq,
    Vpslldq,
    Vfp(FpK, Prec),
    Vblendvps,
    Vroundss,
    Vcmpeqss,
    VcvtSi2Ss,
    VcvtSi2Sd,
    VcvtSs2Sd,
    VcvtSd2Ss,
    Vzeroupper,
    Movss,
    Movsd,
    MovdToX,
    MovdFromX,
    MovqLoad,
    MovqStore,
    Movlp,
    Movhp,
    Movhlps,
    Movlhps,
    Movddup,
    Movshdup,
    Movsldup,
    Movmsk(Prec),
    Pmovmskb,
    Unpck(Prec, bool),
    Shufps,
    Shufpd,
    Pshufd,
    Pshufhw,
    Pshuflw,
    Pshufb,
    Palignr,
    Fp(FpK, Prec),
    Pi(PiK, u8, bool),
    PShift(ShK, u8),
    PShiftBytes(bool),
    Punpck(u8, bool),
    Pack(u8, bool),
    Cvt(CvtK),
    CmpFp(Prec),
    Comi(Prec, bool),
    Round(Prec),
    Dpp(Prec),
    Blend(Prec),
    Pblendw,
    Pblendv,
    Blendv(Prec),
    Insertps,
    Vinsertps,
    Vshufps,
    Extractps,
    Pinsr(u8),
    Pextr(u8),
    Pmovx(u8, u8, bool),
    Ptest,
    Phminposuw,
    Pcmpstr(bool, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opnd {
    None,
    /// General-purpose register (full index 0..15; width from the instruction).
    Gpr(u8),
    /// AH/CH/DH/BH (index 0..3).
    GprHi(u8),
    Xmm(u8),
    Mem(MemOp),
    Imm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemOp {
    /// 0xff = none.
    pub base: u8,
    pub index: u8,
    /// log2 of the scale.
    pub scale: u8,
    /// Displacement; for RIP-relative operands this is already the absolute address.
    pub disp: i64,
    /// 0 none, 1 FS, 2 GS.
    pub seg: u8,
    pub addr32: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Inst {
    pub op: Op,
    pub len: u8,
    /// Operand size in bytes (1, 2, 4, 8) for GPRs; 16 or 32 for VEX vector moves.
    pub osz: u8,
    /// Source operand size (movzx/movsx/crc32/cvt forms), otherwise equals `osz`.
    pub ssz: u8,
    /// 0 none, 1 = F3 (rep/repe), 2 = F2 (repne).
    pub rep: u8,
    pub lock: bool,
    pub rexw: bool,
    pub cc: u8,
    pub ops: [Opnd; 3],
    pub imm: i64,
    pub imm2: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    Undefined(u8),
    Unsupported(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "truncated instruction"),
            DecodeError::Undefined(b) => write!(f, "undefined opcode {b:#04x}"),
            DecodeError::Unsupported(s) => write!(f, "unsupported instruction: {s}"),
        }
    }
}
impl std::error::Error for DecodeError {}

type R<T> = Result<T, DecodeError>;

struct Dec<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Dec<'a> {
    fn u8(&mut self) -> R<u8> {
        let v = *self.b.get(self.p).ok_or(DecodeError::Truncated)?;
        self.p += 1;
        Ok(v)
    }
    fn i8(&mut self) -> R<i64> {
        Ok(self.u8()? as i8 as i64)
    }
    fn i16(&mut self) -> R<i64> {
        let lo = self.u8()? as u16;
        let hi = self.u8()? as u16;
        Ok((lo | hi << 8) as i16 as i64)
    }
    fn i32(&mut self) -> R<i64> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.u8()? as u32) << (8 * i);
        }
        Ok(v as i32 as i64)
    }
    fn i64(&mut self) -> R<i64> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.u8()? as u64) << (8 * i);
        }
        Ok(v as i64)
    }
}

#[derive(Clone, Copy, Default)]
struct Pfx {
    rex: u8,
    mode32: bool,
    opsz: bool, // 0x66
    adsz: bool, // 0x67
    rep: u8,    // 1=F3, 2=F2
    lock: bool,
    seg: u8,
}

impl Pfx {
    fn w(&self) -> bool {
        self.rex & 8 != 0
    }
    fn r(&self) -> u8 {
        (self.rex >> 2 & 1) << 3
    }
    fn x(&self) -> u8 {
        (self.rex >> 1 & 1) << 3
    }
    fn b(&self) -> u8 {
        (self.rex & 1) << 3
    }
}

struct ModRm {
    md: u8,
    reg: u8, // includes REX.R
    rm: u8,  // includes REX.B when register form
    mem: Option<MemOp>,
}

fn parse_modrm(d: &mut Dec, pfx: &Pfx) -> R<ModRm> {
    let m = d.u8()?;
    let md = m >> 6;
    let reg = (m >> 3 & 7) | pfx.r();
    let rm_low = m & 7;
    if md == 3 {
        return Ok(ModRm {
            md,
            reg,
            rm: rm_low | pfx.b(),
            mem: None,
        });
    }
    let mut mem = MemOp {
        base: 0xff,
        index: 0xff,
        scale: 0,
        disp: 0,
        seg: pfx.seg,
        addr32: pfx.mode32 != pfx.adsz,
    };
    let mut rip_rel = false;
    if rm_low == 4 {
        let sib = d.u8()?;
        let ss = sib >> 6;
        let idx = (sib >> 3 & 7) | pfx.x();
        let base = sib & 7;
        mem.scale = ss;
        if idx != 4 {
            mem.index = idx;
        }
        if base == 5 && md == 0 {
            mem.disp = d.i32()?;
        } else {
            mem.base = base | pfx.b();
        }
    } else if rm_low == 5 && md == 0 {
        rip_rel = !pfx.mode32 && !pfx.adsz;
        mem.disp = d.i32()?;
    } else {
        mem.base = rm_low | pfx.b();
    }
    match md {
        1 => mem.disp = d.i8()?,
        2 => mem.disp = d.i32()?,
        _ => {}
    }
    // Stash the RIP-relative flag via base = 0xfe; fixed up by `finish`.
    if rip_rel {
        mem.base = 0xfe;
    }
    Ok(ModRm {
        md,
        reg,
        rm: rm_low | pfx.b(),
        mem: Some(mem),
    })
}

impl ModRm {
    fn rm_opnd(&self, kind: RK, pfx: &Pfx) -> Opnd {
        match self.mem {
            Some(m) => Opnd::Mem(m),
            None => match kind {
                RK::G => Opnd::Gpr(self.rm),
                RK::G8 => gpr8(self.rm, pfx),
                RK::X => Opnd::Xmm(self.rm),
            },
        }
    }
    fn reg_opnd(&self, kind: RK, pfx: &Pfx) -> Opnd {
        match kind {
            RK::G => Opnd::Gpr(self.reg),
            RK::G8 => gpr8(self.reg, pfx),
            RK::X => Opnd::Xmm(self.reg),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RK {
    G,
    G8,
    X,
}

fn gpr8(idx: u8, pfx: &Pfx) -> Opnd {
    if pfx.rex == 0 && (4..8).contains(&idx) {
        Opnd::GprHi(idx - 4)
    } else {
        Opnd::Gpr(idx)
    }
}

fn blank(op: Op) -> Inst {
    Inst {
        op,
        len: 0,
        osz: 4,
        ssz: 4,
        rep: 0,
        lock: false,
        rexw: false,
        cc: 0,
        ops: [Opnd::None; 3],
        imm: 0,
        imm2: 0,
    }
}

const ALU: [Op; 8] = [
    Op::Add,
    Op::Or,
    Op::Adc,
    Op::Sbb,
    Op::And,
    Op::Sub,
    Op::Xor,
    Op::Cmp,
];
const SHIFT: [Op; 8] = [
    Op::Rol,
    Op::Ror,
    Op::Rcl,
    Op::Rcr,
    Op::Shl,
    Op::Shr,
    Op::Shl,
    Op::Sar,
];

/// Decode one instruction located at `rip`.
pub fn decode(bytes: &[u8], rip: u64) -> Result<Inst, DecodeError> {
    decode_mode(bytes, rip, CpuMode::X86_64)
}

/// Decode one instruction using the selected x86 execution mode.
pub fn decode_mode(bytes: &[u8], rip: u64, mode: CpuMode) -> Result<Inst, DecodeError> {
    let mode32 = mode == CpuMode::X86_32;
    let mut d = Dec { b: bytes, p: 0 };
    let mut pfx = Pfx { mode32, ..Pfx::default() };
    // ── prefixes ──
    let mut opc;
    loop {
        opc = d.u8()?;
        match opc {
            0xF0 => pfx.lock = true,
            0xF2 => pfx.rep = 2,
            0xF3 => pfx.rep = 1,
            0x2E | 0x36 | 0x3E | 0x26 => {}
            0x64 => pfx.seg = 1,
            0x65 => pfx.seg = 2,
            0x66 => pfx.opsz = true,
            0x67 => pfx.adsz = true,
            _ => break,
        }
        if d.p > 14 {
            return Err(DecodeError::Unsupported("prefix run too long"));
        }
    }
    if !mode32 && (0x40..=0x4f).contains(&opc) {
        pfx.rex = opc;
        opc = d.u8()?;
    }
    if mode32 && pfx.adsz {
        return Err(DecodeError::Unsupported(
            "16-bit addressing in 32-bit mode",
        ));
    }
    if opc == 0xc5 || opc == 0xc4 {
        if pfx.rex != 0 || pfx.lock || pfx.opsz || pfx.adsz || pfx.rep != 0 || pfx.seg != 0 {
            return Err(DecodeError::Unsupported("legacy prefix before VEX"));
        }
        let (map, vex, vex_pfx) = if opc == 0xc5 {
            let vex = d.u8()?;
            (
                1,
                vex,
                Pfx {
                    rex: if vex & 0x80 == 0 { 0x04 } else { 0 },
                    mode32,
                    ..Pfx::default()
                },
            )
        } else {
            let vex2 = d.u8()?;
            let vex = d.u8()?;
            let map = vex2 & 0x1f;
            if !matches!(map, 1 | 2 | 3) {
                return Err(DecodeError::Unsupported("VEX3 opcode map"));
            }
            let rex = (((!vex2 >> 7) & 1) << 2)
                | (((!vex2 >> 6) & 1) << 1)
                | ((!vex2 >> 5) & 1)
                | ((vex >> 4) & 8);
            (
                map,
                vex,
                Pfx {
                    rex,
                    mode32,
                    ..Pfx::default()
                },
            )
        };
        let opcode = d.u8()?;
        let mut i = decode_vex(&mut d, &vex_pfx, map, opcode, vex)?;
        i.len = d.p as u8;
        let next = next_ip(rip, i.len, mode);
        for o in i.ops.iter_mut() {
            if let Opnd::Mem(m) = o {
                if m.base == 0xfe {
                    m.base = 0xff;
                    m.disp = next.wrapping_add(m.disp as u64) as i64;
                }
            }
        }
        return Ok(i);
    }
    let osz: u8 = if pfx.w() {
        8
    } else if pfx.opsz {
        2
    } else {
        4
    };
    let mut i = if opc == 0x0f {
        decode_0f(&mut d, &pfx, osz)?
    } else {
        decode_1byte(&mut d, &pfx, opc, osz, rip, mode)?
    };
    i.lock = pfx.lock;
    i.rexw = pfx.w();
    // Only string instructions interpret F2/F3 as repeat prefixes; elsewhere they are mandatory
    // prefixes (already consumed during decoding) or ignored.
    i.rep = if matches!(i.op, Op::Movs | Op::Cmps | Op::Stos | Op::Lods | Op::Scas) {
        pfx.rep
    } else {
        0
    };
    i.len = d.p as u8;
    // Resolve RIP-relative memory operands now that the length is known.
    let next = next_ip(rip, i.len, mode);
    for o in i.ops.iter_mut() {
        if let Opnd::Mem(m) = o {
            if m.base == 0xfe {
                m.base = 0xff;
                m.disp = next.wrapping_add(m.disp as u64) as i64;
            }
        }
    }
    // Relative branch targets were decoded as displacements; make them absolute.
    if matches!(
        i.op,
        Op::Jmp | Op::Jcc | Op::Call | Op::Loop | Op::Loope | Op::Loopne | Op::Jrcxz
    ) {
        let target = next.wrapping_add(i.imm as u64);
        i.imm = if mode32 {
            (target as u32 as u64) as i64
        } else {
            target as i64
        };
    }
    Ok(i)
}

#[inline]
fn next_ip(rip: u64, len: u8, mode: CpuMode) -> u64 {
    let next = rip.wrapping_add(len as u64);
    if mode == CpuMode::X86_32 {
        next as u32 as u64
    } else {
        next
    }
}

fn decode_vex(d: &mut Dec, pfx: &Pfx, map: u8, opcode: u8, vex: u8) -> R<Inst> {
    let pp = vex & 3;
    let vvvv_encoded = vex >> 3 & 0xf;
    let vvvv = (!vvvv_encoded) & 0xf;
    let l = vex & 4 != 0;
    if map == 1 && opcode == 0x73 && pp == 1 {
        let m = parse_modrm(d, pfx)?;
        if m.mem.is_some() {
            return Err(DecodeError::Undefined(opcode));
        }
        let op = match m.reg & 7 {
            2 => Op::VShiftQ {
                left: false,
                immediate: true,
            },
            3 => Op::Vpsrldq,
            6 => Op::Vpsllq,
            7 => Op::Vpslldq,
            _ => return Err(DecodeError::Undefined(opcode)),
        };
        let mut i = blank(op);
        i.osz = if l { 32 } else { 16 };
        i.ops = [m.rm_opnd(RK::X, pfx), Opnd::Xmm(vvvv), Opnd::None];
        i.imm = d.u8()? as i64;
        return Ok(i);
    }
    if map == 1 && opcode == 0xd4 && pp == 1 {
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(Op::Vpaddq);
        i.osz = if l { 32 } else { 16 };
        i.ops = [
            m.reg_opnd(RK::X, pfx),
            Opnd::Xmm(vvvv),
            m.rm_opnd(RK::X, pfx),
        ];
        return Ok(i);
    }
    if map == 2 && opcode == 0xf3 && pp == 0 && !l {
        let m = parse_modrm(d, pfx)?;
        if m.reg != 3 {
            return Err(DecodeError::Unsupported("VEX BLSI opcode extension"));
        }
        let mut i = blank(Op::Blsi);
        i.osz = if pfx.w() { 8 } else { 4 };
        i.ops = [Opnd::Gpr(vvvv), m.rm_opnd(RK::G, pfx), Opnd::None];
        return Ok(i);
    }
    if map == 3 {
        if opcode == 0x39 && pp == 1 && l && !pfx.w() {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vinserti128);
            i.osz = 32;
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            i.imm = d.u8()? as i64;
            return Ok(i);
        }
        if opcode == 0x21 && pp == 1 && !l {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vinsertps);
            i.osz = 16;
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            i.imm = d.u8()? as i64;
            return Ok(i);
        }
        if opcode == 0x4a && pp == 1 && !l && !pfx.w() {
            let m = parse_modrm(d, pfx)?;
            let mask = d.u8()?;
            let mut i = blank(Op::Vblendvps);
            i.osz = 16;
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            i.imm = (mask >> 4) as i64;
            return Ok(i);
        }
        if opcode == 0x19 && pp == 1 && l && !pfx.w() && vvvv_encoded == 0xf {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vextractf128);
            i.osz = 32;
            i.ops = [m.rm_opnd(RK::X, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
            i.imm = d.u8()? as i64;
            return Ok(i);
        }
        if matches!(opcode, 0x14 | 0x15 | 0x16) && pp == 1 && !l && vvvv_encoded == 0xf {
            let m = parse_modrm(d, pfx)?;
            let bytes = match opcode {
                0x14 => 1,
                0x15 => 2,
                _ if pfx.w() => 8,
                _ => 4,
            };
            let mut i = blank(Op::Pextr(bytes));
            i.osz = bytes;
            i.ops = [m.rm_opnd(RK::G, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
            i.imm = d.u8()? as i64;
            return Ok(i);
        }
        if opcode != 0x0a || pp != 1 || l {
            return Err(DecodeError::Unsupported("VEX3 0F3A instruction"));
        }
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(Op::Vroundss);
        i.ops = [
            m.reg_opnd(RK::X, pfx),
            Opnd::Xmm(vvvv),
            m.rm_opnd(RK::X, pfx),
        ];
        i.imm = d.u8()? as i64;
        return Ok(i);
    }
    if map == 2 {
        if opcode == 0x25 && pp == 1 && vvvv_encoded == 0xf {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vpmovsxdq);
            i.osz = if l { 32 } else { 16 };
            i.ops = [m.reg_opnd(RK::X, pfx), m.rm_opnd(RK::X, pfx), Opnd::None];
            return Ok(i);
        }
        if matches!(opcode, 0x58 | 0x59) && pp == 1 && !pfx.w() && vvvv_encoded == 0xf {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if opcode == 0x58 {
                Op::Vpbroadcastd
            } else {
                Op::Vpbroadcastq
            });
            i.osz = if l { 32 } else { 16 };
            i.ops = [m.reg_opnd(RK::X, pfx), Opnd::None, m.rm_opnd(RK::X, pfx)];
            return Ok(i);
        }
        if l {
            return Err(DecodeError::Unsupported("VEX3 0F38 instruction"));
        }
        let mut i = match (opcode, pp) {
            (0xf2, 0) => blank(Op::Andn),
            (0xf5, 0) if !pfx.w() => blank(Op::Bzhi),
            (0xf7, 1) => blank(Op::Shlx),
            (0xf7, 3) => blank(Op::Shrx),
            _ => return Err(DecodeError::Unsupported("VEX3 0F38 instruction")),
        };
        let m = parse_modrm(d, pfx)?;
        i.osz = if pfx.w() { 8 } else { 4 };
        i.ops = if opcode == 0xf2 {
            [
                m.reg_opnd(RK::G, pfx),
                Opnd::Gpr(vvvv),
                m.rm_opnd(RK::G, pfx),
            ]
        } else {
            [
                m.reg_opnd(RK::G, pfx),
                m.rm_opnd(RK::G, pfx),
                Opnd::Gpr(vvvv),
            ]
        };
        return Ok(i);
    }
    if opcode == 0xd6 && map == 1 && pp == 1 && !l && vvvv_encoded == 0xf {
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(Op::MovqStore);
        i.osz = 8;
        i.ops = [m.rm_opnd(RK::X, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
        return Ok(i);
    }
    if opcode == 0x6e && map == 1 && pp == 1 && !l && vvvv_encoded == 0xf {
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(if pfx.w() { Op::Vmovq } else { Op::Vmovd });
        i.osz = 16;
        i.ops = [m.reg_opnd(RK::X, pfx), m.rm_opnd(RK::G, pfx), Opnd::None];
        return Ok(i);
    }
    if opcode == 0x7e && map == 1 && pp == 1 && !l && vvvv_encoded == 0xf {
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(Op::MovdFromX);
        i.osz = if pfx.w() { 8 } else { 4 };
        i.rexw = pfx.w();
        i.ops = [m.rm_opnd(RK::G, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
        return Ok(i);
    }
    if map == 1 && pp == 1 && matches!(opcode, 0xd3 | 0xf3) {
        let m = parse_modrm(d, pfx)?;
        let mut i = blank(Op::VShiftQ {
            left: opcode == 0xf3,
            immediate: false,
        });
        i.osz = if l { 32 } else { 16 };
        i.ops = [
            m.reg_opnd(RK::X, pfx),
            Opnd::Xmm(vvvv),
            m.rm_opnd(RK::X, pfx),
        ];
        return Ok(i);
    }
    match opcode {
        0x2e | 0x2f if pp == 0 && vvvv_encoded == 0xf && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Comi(Prec::Ss, opcode == 0x2f));
            i.ops = [m.reg_opnd(RK::X, pfx), m.rm_opnd(RK::X, pfx), Opnd::None];
            Ok(i)
        }
        0x2a if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::VcvtSi2Ss);
            i.ssz = if pfx.w() { 8 } else { 4 };
            i.rexw = pfx.w();
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::G, pfx),
            ];
            Ok(i)
        }
        0x2a if pp == 3 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::VcvtSi2Sd);
            i.ssz = if pfx.w() { 8 } else { 4 };
            i.rexw = pfx.w();
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::G, pfx),
            ];
            Ok(i)
        }
        0xc2 if pp == 2 && !l && !pfx.w() => {
            let m = parse_modrm(d, pfx)?;
            let predicate = d.u8()?;
            if predicate != 0 {
                return Err(DecodeError::Unsupported("VCMPSS predicate"));
            }
            let mut i = blank(Op::Vcmpeqss);
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            i.imm = predicate as i64;
            Ok(i)
        }
        0x2c if pp == 2 && !l && vvvv_encoded == 0xf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Cvt(CvtK::Tss2Si));
            i.rexw = pfx.w();
            i.osz = if pfx.w() { 8 } else { 4 };
            i.ops = [m.reg_opnd(RK::G, pfx), m.rm_opnd(RK::X, pfx), Opnd::None];
            Ok(i)
        }
        0x5a if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::VcvtSs2Sd);
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x5a if pp == 3 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::VcvtSd2Ss);
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x10 if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vmovss);
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x11 if pp == 2 && !l && vvvv_encoded == 0xf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vmovss);
            i.ops = [m.rm_opnd(RK::X, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
            Ok(i)
        }
        0x10 if pp == 3 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vmovsd);
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x11 if pp == 3 && !l && vvvv_encoded == 0xf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vmovsd);
            i.ops = [m.rm_opnd(RK::X, pfx), m.reg_opnd(RK::X, pfx), Opnd::None];
            Ok(i)
        }
        0x51 | 0x58 | 0x59 | 0x5c | 0x5d | 0x5e | 0x5f if pp == 3 && !l => {
            let m = parse_modrm(d, pfx)?;
            let k = match opcode {
                0x51 => FpK::Sqrt,
                0x58 => FpK::Add,
                0x59 => FpK::Mul,
                0x5c => FpK::Sub,
                0x5d => FpK::Min,
                0x5e => FpK::Div,
                0x5f => FpK::Max,
                _ => unreachable!(),
            };
            let mut i = blank(Op::Vfp(k, Prec::Sd));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x58 if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vfp(FpK::Add, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x51 if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vfp(FpK::Sqrt, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x59 if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vfp(FpK::Mul, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x5c if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vfp(FpK::Sub, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x5d | 0x5f if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let kind = if opcode == 0x5d { FpK::Min } else { FpK::Max };
            let mut i = blank(Op::Vfp(kind, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x5e if pp == 2 && !l => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vfp(FpK::Div, Prec::Ss));
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x10 | 0x11 | 0x28 | 0x29 if pp == 0 && vvvv_encoded == 0xf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vmovups);
            i.osz = if l { 32 } else { 16 };
            let (r, rm) = (m.reg_opnd(RK::X, pfx), m.rm_opnd(RK::X, pfx));
            i.ops = if matches!(opcode, 0x10 | 0x28) {
                [r, rm, Opnd::None]
            } else {
                [rm, r, Opnd::None]
            };
            Ok(i)
        }
        0x6f | 0x7f if matches!(pp, 1 | 2) && vvvv_encoded == 0xf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if pp == 1 { Op::Vmovdqa } else { Op::Vmovdqu });
            i.osz = if l { 32 } else { 16 };
            let (r, rm) = (m.reg_opnd(RK::X, pfx), m.rm_opnd(RK::X, pfx));
            i.ops = if opcode == 0x6f {
                [r, rm, Opnd::None]
            } else {
                [rm, r, Opnd::None]
            };
            Ok(i)
        }
        0xc6 if pp == 0 => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vshufps);
            i.osz = if l { 32 } else { 16 };
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            i.imm = d.u8()? as i64;
            Ok(i)
        }
        0xef | 0x57
            if (opcode == 0xef && pp == 1) || (opcode == 0x57 && matches!(pp, 0 | 1)) =>
        {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vpxor);
            i.osz = if l { 32 } else { 16 };
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        0x77 if pp == 0 && vvvv_encoded == 0xf && !l => Ok(blank(Op::Vzeroupper)),
        0x77 if pp == 0 && vvvv_encoded == 0xf => Err(DecodeError::Unsupported("VZEROALL")),
        0xfe if map == 1 && pp == 1 => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Op::Vpaddd);
            i.osz = if l { 32 } else { 16 };
            i.ops = [
                m.reg_opnd(RK::X, pfx),
                Opnd::Xmm(vvvv),
                m.rm_opnd(RK::X, pfx),
            ];
            Ok(i)
        }
        _ => Err(DecodeError::Unsupported("VEX2 instruction")),
    }
}

fn imm_z(d: &mut Dec, osz: u8) -> R<i64> {
    match osz {
        2 => d.i16(),
        _ => d.i32(),
    }
}

fn decode_1byte(
    d: &mut Dec,
    pfx: &Pfx,
    opc: u8,
    osz: u8,
    _rip: u64,
    mode: CpuMode,
) -> R<Inst> {
    let g = RK::G;
    let stack_osz = if pfx.opsz {
        2
    } else if mode == CpuMode::X86_32 {
        4
    } else {
        8
    };
    // ALU block 00..3F
    if opc < 0x40 && (opc & 7) < 6 {
        let op = ALU[(opc >> 3) as usize];
        let mut i = blank(op);
        match opc & 7 {
            0 | 1 | 2 | 3 => {
                let byte = opc & 1 == 0;
                let m = parse_modrm(d, pfx)?;
                i.osz = if byte { 1 } else { osz };
                let k = if byte { RK::G8 } else { g };
                let (r, rm) = (m.reg_opnd(k, pfx), m.rm_opnd(k, pfx));
                if opc & 2 == 0 {
                    i.ops = [rm, r, Opnd::None];
                } else {
                    i.ops = [r, rm, Opnd::None];
                }
            }
            4 => {
                i.osz = 1;
                i.ops = [Opnd::Gpr(0), Opnd::Imm, Opnd::None];
                i.imm = d.i8()?;
            }
            _ => {
                i.osz = osz;
                i.ops = [Opnd::Gpr(0), Opnd::Imm, Opnd::None];
                i.imm = imm_z(d, osz)?;
            }
        }
        i.ssz = i.osz;
        return Ok(i);
    }
    let mut i = blank(Op::Nop);
    i.osz = osz;
    i.ssz = osz;
    match opc {
        0x40..=0x47 if mode == CpuMode::X86_32 => {
            i.op = Op::Inc;
            i.ops[0] = Opnd::Gpr(opc & 7);
        }
        0x48..=0x4f if mode == CpuMode::X86_32 => {
            i.op = Op::Dec;
            i.ops[0] = Opnd::Gpr(opc & 7);
        }
        0x50..=0x57 => {
            i.op = Op::Push;
            i.osz = stack_osz;
            i.ops[0] = Opnd::Gpr((opc & 7) | pfx.b());
        }
        0x58..=0x5f => {
            i.op = Op::Pop;
            i.osz = stack_osz;
            i.ops[0] = Opnd::Gpr((opc & 7) | pfx.b());
        }
        0x63 => {
            if mode == CpuMode::X86_32 {
                return Err(DecodeError::Unsupported("ARPL"));
            }
            let m = parse_modrm(d, pfx)?;
            i.op = Op::Movsxd;
            i.ssz = 4;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
        }
        0x68 => {
            i.op = Op::Push;
            i.osz = stack_osz;
            i.ops[0] = Opnd::Imm;
            i.imm = imm_z(d, i.osz.min(4))?;
        }
        0x6a => {
            i.op = Op::Push;
            i.osz = stack_osz;
            i.ops[0] = Opnd::Imm;
            i.imm = d.i8()?;
        }
        0x69 | 0x6b => {
            let m = parse_modrm(d, pfx)?;
            i.op = Op::Imul3;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::Imm];
            i.imm = if opc == 0x6b { d.i8()? } else { imm_z(d, osz)? };
        }
        0x70..=0x7f => {
            i.op = Op::Jcc;
            i.cc = opc & 15;
            i.imm = d.i8()?;
        }
        0x80 | 0x81 | 0x83 => {
            let m = parse_modrm(d, pfx)?;
            i.op = ALU[(m.reg & 7) as usize];
            let byte = opc == 0x80;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            i.ops = [m.rm_opnd(k, pfx), Opnd::Imm, Opnd::None];
            i.imm = if opc == 0x81 { imm_z(d, osz)? } else { d.i8()? };
        }
        0x84 | 0x85 | 0x86 | 0x87 | 0x88 | 0x89 | 0x8a | 0x8b => {
            let byte = opc & 1 == 0;
            let m = parse_modrm(d, pfx)?;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            let (r, rm) = (m.reg_opnd(k, pfx), m.rm_opnd(k, pfx));
            match opc {
                0x84 | 0x85 => {
                    i.op = Op::Test;
                    i.ops = [rm, r, Opnd::None];
                }
                0x86 | 0x87 => {
                    i.op = Op::Xchg;
                    i.ops = [rm, r, Opnd::None];
                }
                0x88 | 0x89 => {
                    i.op = Op::Mov;
                    i.ops = [rm, r, Opnd::None];
                }
                _ => {
                    i.op = Op::Mov;
                    i.ops = [r, rm, Opnd::None];
                }
            }
        }
        0x8d => {
            let m = parse_modrm(d, pfx)?;
            if m.mem.is_none() {
                return Err(DecodeError::Undefined(opc));
            }
            i.op = Op::Lea;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
        }
        0x8f => {
            let m = parse_modrm(d, pfx)?;
            if m.reg & 7 != 0 {
                return Err(DecodeError::Undefined(opc));
            }
            i.op = Op::Pop;
            i.osz = stack_osz;
            i.ops[0] = m.rm_opnd(g, pfx);
        }
        0x90 => {
            if pfx.b() != 0 {
                i.op = Op::Xchg;
                i.ops = [Opnd::Gpr(8), Opnd::Gpr(0), Opnd::None];
            } else if pfx.rep == 1 {
                i.op = Op::Pause;
            } else {
                i.op = Op::Nop;
            }
        }
        0x91..=0x97 => {
            i.op = Op::Xchg;
            i.ops = [Opnd::Gpr((opc & 7) | pfx.b()), Opnd::Gpr(0), Opnd::None];
        }
        0x98 => i.op = Op::Cbw,
        0x99 => i.op = Op::Cwd,
        0x9b => i.op = Op::Nop,
        0x9c => {
            i.op = Op::Pushf;
            i.osz = stack_osz;
        }
        0x9d => {
            i.op = Op::Popf;
            i.osz = stack_osz;
        }
        0x9e => i.op = Op::Sahf,
        0x9f => i.op = Op::Lahf,
        0xa0..=0xa3 => {
            let (addr, addr32) = if pfx.mode32 || pfx.adsz {
                (d.i32()?, true)
            } else {
                (d.i64()?, false)
            };
            let byte = opc & 1 == 0;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            i.op = Op::Mov;
            let mem = Opnd::Mem(MemOp {
                base: 0xff,
                index: 0xff,
                scale: 0,
                disp: addr,
                seg: pfx.seg,
                addr32,
            });
            i.ops = if opc < 0xa2 {
                [Opnd::Gpr(0), mem, Opnd::None]
            } else {
                [mem, Opnd::Gpr(0), Opnd::None]
            };
        }
        0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xaa | 0xab | 0xac | 0xad | 0xae | 0xaf => {
            i.op = match opc {
                0xa4 | 0xa5 => Op::Movs,
                0xa6 | 0xa7 => Op::Cmps,
                0xaa | 0xab => Op::Stos,
                0xac | 0xad => Op::Lods,
                _ => Op::Scas,
            };
            i.osz = if opc & 1 == 0 { 1 } else { osz };
            i.ssz = i.osz;
            // Address-size and segment info travel in ops[0] as a dummy memory operand.
            i.ops[0] = Opnd::Mem(MemOp {
                base: 0xff,
                index: 0xff,
                scale: 0,
                disp: 0,
                seg: pfx.seg,
                addr32: pfx.mode32 != pfx.adsz,
            });
        }
        0xa8 => {
            i.op = Op::Test;
            i.osz = 1;
            i.ssz = 1;
            i.ops = [Opnd::Gpr(0), Opnd::Imm, Opnd::None];
            i.imm = d.i8()?;
        }
        0xa9 => {
            i.op = Op::Test;
            i.ops = [Opnd::Gpr(0), Opnd::Imm, Opnd::None];
            i.imm = imm_z(d, osz)?;
        }
        0xb0..=0xb7 => {
            i.op = Op::Mov;
            i.osz = 1;
            i.ssz = 1;
            i.ops = [gpr8((opc & 7) | pfx.b(), pfx), Opnd::Imm, Opnd::None];
            i.imm = d.i8()?;
        }
        0xb8..=0xbf => {
            i.op = Op::Mov;
            i.ops = [Opnd::Gpr((opc & 7) | pfx.b()), Opnd::Imm, Opnd::None];
            i.imm = match osz {
                8 => d.i64()?,
                2 => d.i16()?,
                _ => d.i32()?,
            };
        }
        0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
            let m = parse_modrm(d, pfx)?;
            i.op = SHIFT[(m.reg & 7) as usize];
            let byte = opc & 1 == 0;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            i.ops[0] = m.rm_opnd(k, pfx);
            match opc {
                0xc0 | 0xc1 => {
                    i.ops[1] = Opnd::Imm;
                    i.imm = d.i8()?;
                }
                0xd0 | 0xd1 => {
                    i.ops[1] = Opnd::Imm;
                    i.imm = 1;
                }
                _ => i.ops[1] = Opnd::Gpr(1),
            }
        }
        0xc2 => {
            i.op = Op::RetImm;
            i.imm = d.i16()? & 0xffff;
        }
        0xc3 => i.op = Op::Ret,
        0xc6 | 0xc7 => {
            let m = parse_modrm(d, pfx)?;
            if m.reg & 7 != 0 {
                return Err(DecodeError::Undefined(opc));
            }
            let byte = opc == 0xc6;
            i.op = Op::Mov;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            i.ops = [
                m.rm_opnd(if byte { RK::G8 } else { g }, pfx),
                Opnd::Imm,
                Opnd::None,
            ];
            i.imm = if byte { d.i8()? } else { imm_z(d, osz)? };
        }
        0xc8 => {
            i.op = Op::Enter;
            i.imm = d.i16()? & 0xffff;
            i.imm2 = d.u8()? as i64;
        }
        0xc9 => i.op = Op::Leave,
        0xcc => i.op = Op::Int3,
        0xcd => {
            i.op = Op::Int;
            i.imm = d.u8()? as i64;
        }
        0xd7 => i.op = Op::Xlat,
        0xd8..=0xdf => {
            let m = parse_modrm(d, pfx)?;
            i.op = Op::X87;
            i.cc = opc & 7;
            i.imm = ((m.md as i64) << 6) | ((m.reg & 7) as i64) << 3 | (m.rm & 7) as i64;
            if let Some(mem) = m.mem {
                i.ops[0] = Opnd::Mem(mem);
            }
        }
        0xe0 | 0xe1 | 0xe2 | 0xe3 => {
            i.op = [Op::Loopne, Op::Loope, Op::Loop, Op::Jrcxz][(opc & 3) as usize];
            i.imm = d.i8()?;
            i.ssz = if pfx.mode32 || pfx.adsz { 4 } else { 8 };
        }
        0xe8 => {
            i.op = Op::Call;
            i.imm = d.i32()?;
        }
        0xe9 => {
            i.op = Op::Jmp;
            i.imm = d.i32()?;
        }
        0xeb => {
            i.op = Op::Jmp;
            i.imm = d.i8()?;
        }
        0xf4 => i.op = Op::Hlt,
        0xf5 => i.op = Op::Cmc,
        0xf6 | 0xf7 => {
            let m = parse_modrm(d, pfx)?;
            let byte = opc == 0xf6;
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            i.ops[0] = m.rm_opnd(k, pfx);
            match m.reg & 7 {
                0 | 1 => {
                    i.op = Op::Test;
                    i.ops[1] = Opnd::Imm;
                    i.imm = if byte { d.i8()? } else { imm_z(d, osz)? };
                }
                2 => i.op = Op::Not,
                3 => i.op = Op::Neg,
                4 => i.op = Op::Mul,
                5 => i.op = Op::Imul1,
                6 => i.op = Op::Div,
                _ => i.op = Op::Idiv,
            }
        }
        0xf8 => i.op = Op::Clc,
        0xf9 => i.op = Op::Stc,
        0xfa => i.op = Op::Cli,
        0xfb => i.op = Op::Sti,
        0xfc => i.op = Op::Cld,
        0xfd => i.op = Op::Std,
        0xfe => {
            let m = parse_modrm(d, pfx)?;
            i.osz = 1;
            i.ssz = 1;
            i.ops[0] = m.rm_opnd(RK::G8, pfx);
            i.op = match m.reg & 7 {
                0 => Op::Inc,
                1 => Op::Dec,
                _ => return Err(DecodeError::Undefined(opc)),
            };
        }
        0xff => {
            let m = parse_modrm(d, pfx)?;
            i.ops[0] = m.rm_opnd(g, pfx);
            match m.reg & 7 {
                0 => i.op = Op::Inc,
                1 => i.op = Op::Dec,
                2 => {
                    i.op = Op::CallInd;
                    i.osz = stack_osz;
                }
                4 => {
                    i.op = Op::JmpInd;
                    i.osz = stack_osz;
                }
                6 => {
                    i.op = Op::Push;
                    i.osz = stack_osz;
                }
                _ => return Err(DecodeError::Unsupported("far call/jmp")),
            }
        }
        _ => return Err(DecodeError::Undefined(opc)),
    }
    Ok(i)
}

// ── 0F two-byte map ─────────────────────────────────────────────────────────

/// Selects the SSE flavour from the mandatory prefix.
fn pidx(pfx: &Pfx) -> usize {
    match (pfx.rep, pfx.opsz) {
        (1, _) => 2,
        (2, _) => 3,
        (_, true) => 1,
        _ => 0,
    }
}

fn prec(p: usize) -> Prec {
    [Prec::Ps, Prec::Pd, Prec::Ss, Prec::Sd][p]
}

/// Build an SSE instruction: ops[0] = rm-or-reg per `swap`.
fn sse(d: &mut Dec, pfx: &Pfx, op: Op, reg: RK, rm: RK, swap: bool, imm8: bool) -> R<Inst> {
    let m = parse_modrm(d, pfx)?;
    let mut i = blank(op);
    i.osz = if pfx.w() { 8 } else { 4 };
    i.ssz = i.osz;
    let (r, x) = (m.reg_opnd(reg, pfx), m.rm_opnd(rm, pfx));
    i.ops = if swap {
        [x, r, Opnd::None]
    } else {
        [r, x, Opnd::None]
    };
    if imm8 {
        i.imm = d.u8()? as i64;
    }
    // The mandatory prefix has been consumed.
    i.rep = 0;
    Ok(i)
}

fn decode_0f(d: &mut Dec, pfx: &Pfx, osz: u8) -> R<Inst> {
    let opc = d.u8()?;
    let p = pidx(pfx);
    let x = RK::X;
    let g = RK::G;
    use Op::*;
    let fp = |k: FpK, pr: Prec| Fp(k, pr);
    // 66-prefixed integer SSE: the legacy MMX forms (no prefix) are unsupported.
    macro_rules! need66 {
        () => {
            if p != 1 {
                return Err(DecodeError::Unsupported(
                    "MMX (no-prefix) packed-integer instruction",
                ));
            }
        };
    }
    macro_rules! pint {
        ($op:expr) => {{
            need66!();
            return sse(d, pfx, $op, x, x, false, false);
        }};
    }
    match opc {
        0x05 => {
            let mut i = blank(Syscall);
            i.osz = 8;
            Ok(i)
        }
        0x01 => match d.u8()? {
            0xd0 => Ok(blank(Xgetbv)),
            0xf9 => Ok(blank(Rdtscp)),
            _ => Err(DecodeError::Undefined(opc)),
        },
        0x0b => Ok(blank(Ud2)),
        0x0d | 0x18 | 0x19..=0x1e => {
            // prefetch / reserved-nop group: consume modrm
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if opc == 0x18 || opc == 0x0d {
                Prefetch
            } else {
                Nop
            });
            if let Some(mem) = m.mem {
                i.ops[0] = Opnd::Mem(mem);
            }
            Ok(i)
        }
        0x1f => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Nop);
            if let Some(mem) = m.mem {
                i.ops[0] = Opnd::Mem(mem);
            }
            Ok(i)
        }
        0x10 | 0x11 => {
            let swap = opc == 0x11;
            let op = match p {
                0 | 1 => MovVec,
                2 => Movss,
                _ => Movsd,
            };
            sse(d, pfx, op, x, x, swap, false)
        }
        0x12 | 0x13 => {
            let swap = opc == 0x13;
            match p {
                0 | 1 => {
                    let m = parse_modrm(d, pfx)?;
                    let mut i = blank(if m.mem.is_none() && opc == 0x12 {
                        Movhlps
                    } else {
                        Movlp
                    });
                    let (r, rm) = (m.reg_opnd(x, pfx), m.rm_opnd(x, pfx));
                    i.ops = if swap {
                        [rm, r, Opnd::None]
                    } else {
                        [r, rm, Opnd::None]
                    };
                    Ok(i)
                }
                2 => sse(d, pfx, Movsldup, x, x, false, false),
                _ => sse(d, pfx, Movddup, x, x, false, false),
            }
        }
        0x14 => sse(d, pfx, Unpck(prec(p & 1), false), x, x, false, false),
        0x15 => sse(d, pfx, Unpck(prec(p & 1), true), x, x, false, false),
        0x16 | 0x17 => {
            let swap = opc == 0x17;
            match p {
                0 | 1 => {
                    let m = parse_modrm(d, pfx)?;
                    let mut i = blank(if m.mem.is_none() && opc == 0x16 {
                        Movlhps
                    } else {
                        Movhp
                    });
                    let (r, rm) = (m.reg_opnd(x, pfx), m.rm_opnd(x, pfx));
                    i.ops = if swap {
                        [rm, r, Opnd::None]
                    } else {
                        [r, rm, Opnd::None]
                    };
                    Ok(i)
                }
                2 => sse(d, pfx, Movshdup, x, x, false, false),
                _ => Err(DecodeError::Undefined(opc)),
            }
        }
        0x28 | 0x29 => sse(d, pfx, MovVec, x, x, opc == 0x29, false),
        0x2a => {
            // cvtsi2ss/sd xmm, r/m32/64   (cvtpi2ps = MMX, unsupported)
            match p {
                2 => sse(d, pfx, Cvt(CvtK::Si2Ss), x, g, false, false),
                3 => sse(d, pfx, Cvt(CvtK::Si2Sd), x, g, false, false),
                _ => Err(DecodeError::Unsupported("cvtpi2ps (MMX)")),
            }
        }
        0x2b => sse(d, pfx, MovVec, x, x, true, false),
        0x2c | 0x2d => match p {
            2 => sse(
                d,
                pfx,
                Cvt(if opc == 0x2c {
                    CvtK::Tss2Si
                } else {
                    CvtK::Ss2Si
                }),
                g,
                x,
                false,
                false,
            ),
            3 => sse(
                d,
                pfx,
                Cvt(if opc == 0x2c {
                    CvtK::Tsd2Si
                } else {
                    CvtK::Sd2Si
                }),
                g,
                x,
                false,
                false,
            ),
            _ => Err(DecodeError::Unsupported("cvt(t)ps2pi (MMX)")),
        },
        0x2e | 0x2f => {
            let pr = if p == 1 { Prec::Sd } else { Prec::Ss };
            sse(d, pfx, Comi(pr, opc == 0x2f), x, x, false, false)
        }
        0x31 => Ok(blank(Rdtsc)),
        0x38 => decode_0f38(d, pfx, osz),
        0x3a => decode_0f3a(d, pfx),
        0x40..=0x4f => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Cmov);
            i.osz = osz;
            i.ssz = osz;
            i.cc = opc & 15;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
            Ok(i)
        }
        0x50 => {
            if p > 1 {
                return Err(DecodeError::Undefined(opc));
            }
            sse(d, pfx, Movmsk(prec(p)), g, x, false, false)
        }
        0x51 => sse(d, pfx, fp(FpK::Sqrt, prec(p)), x, x, false, false),
        0x52 => sse(d, pfx, fp(FpK::Rsqrt, prec(p)), x, x, false, false),
        0x53 => sse(d, pfx, fp(FpK::Rcp, prec(p)), x, x, false, false),
        0x54 => sse(d, pfx, fp(FpK::And, prec(p & 1)), x, x, false, false),
        0x55 => sse(d, pfx, fp(FpK::Andn, prec(p & 1)), x, x, false, false),
        0x56 => sse(d, pfx, fp(FpK::Or, prec(p & 1)), x, x, false, false),
        0x57 => sse(d, pfx, fp(FpK::Xor, prec(p & 1)), x, x, false, false),
        0x58 => sse(d, pfx, fp(FpK::Add, prec(p)), x, x, false, false),
        0x59 => sse(d, pfx, fp(FpK::Mul, prec(p)), x, x, false, false),
        0x5a => {
            let k = match p {
                0 => CvtK::Ps2Pd,
                1 => CvtK::Pd2Ps,
                2 => CvtK::Ss2Sd,
                _ => CvtK::Sd2Ss,
            };
            sse(d, pfx, Cvt(k), x, x, false, false)
        }
        0x5b => {
            let k = match p {
                0 => CvtK::Dq2Ps,
                1 => CvtK::Ps2Dq,
                2 => CvtK::Tps2Dq,
                _ => return Err(DecodeError::Undefined(opc)),
            };
            sse(d, pfx, Cvt(k), x, x, false, false)
        }
        0x5c => sse(d, pfx, fp(FpK::Sub, prec(p)), x, x, false, false),
        0x5d => sse(d, pfx, fp(FpK::Min, prec(p)), x, x, false, false),
        0x5e => sse(d, pfx, fp(FpK::Div, prec(p)), x, x, false, false),
        0x5f => sse(d, pfx, fp(FpK::Max, prec(p)), x, x, false, false),
        0x60 => pint!(Punpck(1, false)),
        0x61 => pint!(Punpck(2, false)),
        0x62 => pint!(Punpck(4, false)),
        0x63 => pint!(Pack(2, true)),
        0x64 => pint!(Pi(PiK::Cmpgt, 1, true)),
        0x65 => pint!(Pi(PiK::Cmpgt, 2, true)),
        0x66 => pint!(Pi(PiK::Cmpgt, 4, true)),
        0x67 => pint!(Pack(2, false)),
        0x68 => pint!(Punpck(1, true)),
        0x69 => pint!(Punpck(2, true)),
        0x6a => pint!(Punpck(4, true)),
        0x6b => pint!(Pack(4, true)),
        0x6c => pint!(Punpck(8, false)),
        0x6d => pint!(Punpck(8, true)),
        0x6e => {
            need66!();
            sse(d, pfx, MovdToX, x, g, false, false)
        }
        0x6f => match p {
            1 | 2 => sse(d, pfx, MovVec, x, x, false, false),
            _ => Err(DecodeError::Unsupported("movq mm (MMX)")),
        },
        0x70 => {
            let op = match p {
                1 => Pshufd,
                2 => Pshufhw,
                3 => Pshuflw,
                _ => return Err(DecodeError::Unsupported("pshufw (MMX)")),
            };
            sse(d, pfx, op, x, x, false, true)
        }
        0x71 | 0x72 | 0x73 => {
            need66!();
            let m = parse_modrm(d, pfx)?;
            if m.mem.is_some() {
                return Err(DecodeError::Undefined(opc));
            }
            let sub = m.reg & 7;
            let op = match (opc, sub) {
                (0x71, 2) => PShift(ShK::Srl, 2),
                (0x71, 4) => PShift(ShK::Sra, 2),
                (0x71, 6) => PShift(ShK::Sll, 2),
                (0x72, 2) => PShift(ShK::Srl, 4),
                (0x72, 4) => PShift(ShK::Sra, 4),
                (0x72, 6) => PShift(ShK::Sll, 4),
                (0x73, 2) => PShift(ShK::Srl, 8),
                (0x73, 6) => PShift(ShK::Sll, 8),
                (0x73, 3) => PShiftBytes(true),
                (0x73, 7) => PShiftBytes(false),
                _ => return Err(DecodeError::Undefined(opc)),
            };
            let mut i = blank(op);
            i.ops[0] = Opnd::Xmm(m.rm);
            i.ops[1] = Opnd::Imm;
            i.imm = d.u8()? as i64;
            Ok(i)
        }
        0x74 => pint!(Pi(PiK::Cmpeq, 1, true)),
        0x75 => pint!(Pi(PiK::Cmpeq, 2, true)),
        0x76 => pint!(Pi(PiK::Cmpeq, 4, true)),
        0x77 => Err(DecodeError::Unsupported("emms")),
        0x7c => match p {
            1 => sse(d, pfx, fp(FpK::Hadd, Prec::Pd), x, x, false, false),
            3 => sse(d, pfx, fp(FpK::Hadd, Prec::Ps), x, x, false, false),
            _ => Err(DecodeError::Undefined(opc)),
        },
        0x7d => match p {
            1 => sse(d, pfx, fp(FpK::Hsub, Prec::Pd), x, x, false, false),
            3 => sse(d, pfx, fp(FpK::Hsub, Prec::Ps), x, x, false, false),
            _ => Err(DecodeError::Undefined(opc)),
        },
        0x7e => match p {
            1 => sse(d, pfx, MovdFromX, x, g, true, false),
            2 => sse(d, pfx, MovqLoad, x, x, false, false),
            _ => Err(DecodeError::Unsupported("movd mm (MMX)")),
        },
        0x7f => match p {
            1 | 2 => sse(d, pfx, MovVec, x, x, true, false),
            _ => Err(DecodeError::Unsupported("movq mm (MMX)")),
        },
        0x80..=0x8f => {
            let mut i = blank(Jcc);
            i.cc = opc & 15;
            i.imm = d.i32()?;
            Ok(i)
        }
        0x90..=0x9f => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Setcc);
            i.osz = 1;
            i.ssz = 1;
            i.cc = opc & 15;
            i.ops[0] = m.rm_opnd(RK::G8, pfx);
            Ok(i)
        }
        0xa2 => Ok(blank(Cpuid)),
        0xa3 | 0xab | 0xb3 | 0xbb => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(match opc {
                0xa3 => Bt,
                0xab => Bts,
                0xb3 => Btr,
                _ => Btc,
            });
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.rm_opnd(g, pfx), m.reg_opnd(g, pfx), Opnd::None];
            Ok(i)
        }
        0xa4 | 0xa5 | 0xac | 0xad => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if opc < 0xa8 { Shld } else { Shrd });
            i.osz = osz;
            i.ssz = osz;
            i.ops = [
                m.rm_opnd(g, pfx),
                m.reg_opnd(g, pfx),
                if opc & 1 == 0 {
                    Opnd::Imm
                } else {
                    Opnd::Gpr(1)
                },
            ];
            if opc & 1 == 0 {
                i.imm = d.u8()? as i64;
            }
            Ok(i)
        }
        0xae => {
            let m = parse_modrm(d, pfx)?;
            let sub = m.reg & 7;
            let mut i = blank(Nop);
            if let Some(mem) = m.mem {
                i.ops[0] = Opnd::Mem(mem);
                i.op = match sub {
                    0 => Fxsave,
                    1 => Fxrstor,
                    2 => Ldmxcsr,
                    3 => Stmxcsr,
                    7 => Prefetch, // clflush
                    _ => return Err(DecodeError::Unsupported("xsave family")),
                };
            } else {
                i.op = match sub {
                    5 | 6 | 7 => Fence,
                    _ => return Err(DecodeError::Unsupported("rd/wr fs/gs base")),
                };
            }
            Ok(i)
        }
        0xaf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Imul2);
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
            Ok(i)
        }
        0xb0 | 0xb1 => {
            let byte = opc == 0xb0;
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Cmpxchg);
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            i.ops = [m.rm_opnd(k, pfx), m.reg_opnd(k, pfx), Opnd::None];
            Ok(i)
        }
        0xb6 | 0xb7 | 0xbe | 0xbf => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if opc < 0xb8 { Movzx } else { Movsx });
            i.osz = osz;
            i.ssz = if opc & 1 == 0 { 1 } else { 2 };
            let sk = if i.ssz == 1 { RK::G8 } else { g };
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(sk, pfx), Opnd::None];
            Ok(i)
        }
        0xb8 => {
            if pfx.rep != 1 {
                return Err(DecodeError::Undefined(opc));
            }
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Popcnt);
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
            i.rep = 0;
            Ok(i)
        }
        0xba => {
            let m = parse_modrm(d, pfx)?;
            let op = match m.reg & 7 {
                4 => Bt,
                5 => Bts,
                6 => Btr,
                7 => Btc,
                _ => return Err(DecodeError::Undefined(opc)),
            };
            let mut i = blank(op);
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.rm_opnd(g, pfx), Opnd::Imm, Opnd::None];
            i.imm = d.u8()? as i64;
            Ok(i)
        }
        0xbc | 0xbd => {
            let m = parse_modrm(d, pfx)?;
            let tz = opc == 0xbc;
            let op = match (pfx.rep == 1, tz) {
                (true, true) => Tzcnt,
                (true, false) => Lzcnt,
                (false, true) => Bsf,
                (false, false) => Bsr,
            };
            let mut i = blank(op);
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(g, pfx), Opnd::None];
            i.rep = 0;
            Ok(i)
        }
        0xc0 | 0xc1 => {
            let byte = opc == 0xc0;
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Xadd);
            i.osz = if byte { 1 } else { osz };
            i.ssz = i.osz;
            let k = if byte { RK::G8 } else { g };
            i.ops = [m.rm_opnd(k, pfx), m.reg_opnd(k, pfx), Opnd::None];
            Ok(i)
        }
        0xc2 => sse(d, pfx, CmpFp(prec(p)), x, x, false, true),
        0xc3 => {
            let m = parse_modrm(d, pfx)?;
            if m.mem.is_none() {
                return Err(DecodeError::Undefined(opc));
            }
            let mut i = blank(Mov); // movnti m, r
            i.osz = osz;
            i.ssz = osz;
            i.ops = [m.rm_opnd(g, pfx), m.reg_opnd(g, pfx), Opnd::None];
            Ok(i)
        }
        0xc4 => {
            need66!();
            sse(d, pfx, Pinsr(2), x, g, false, true)
        }
        0xc5 => {
            need66!();
            sse(d, pfx, Pextr(2), g, x, false, true)
        }
        0xc6 => sse(
            d,
            pfx,
            if p == 1 { Shufpd } else { Shufps },
            x,
            x,
            false,
            true,
        ),
        0xc7 => {
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(Nop);
            match (m.reg & 7, m.mem) {
                (1, Some(mem)) => {
                    i.op = if pfx.w() { Cmpxchg16b } else { Cmpxchg8b };
                    i.ops[0] = Opnd::Mem(mem);
                }
                (6, None) => {
                    i.op = Rdrand;
                    i.osz = osz;
                    i.ops[0] = Opnd::Gpr(m.rm);
                }
                _ => return Err(DecodeError::Unsupported("0F C7 group member")),
            }
            Ok(i)
        }
        0xc8..=0xcf => {
            let mut i = blank(Bswap);
            i.osz = osz;
            i.ops[0] = Opnd::Gpr((opc & 7) | pfx.b());
            Ok(i)
        }
        0xd0 => match p {
            1 => sse(d, pfx, fp(FpK::Addsub, Prec::Pd), x, x, false, false),
            3 => sse(d, pfx, fp(FpK::Addsub, Prec::Ps), x, x, false, false),
            _ => Err(DecodeError::Undefined(opc)),
        },
        0xd1 => pint!(PShift(ShK::Srl, 2)),
        0xd2 => pint!(PShift(ShK::Srl, 4)),
        0xd3 => pint!(PShift(ShK::Srl, 8)),
        0xd4 => pint!(Pi(PiK::Add, 8, false)),
        0xd5 => pint!(Pi(PiK::Mullo, 2, true)),
        0xd6 => {
            need66!();
            sse(d, pfx, MovqStore, x, x, true, false)
        }
        0xd7 => {
            need66!();
            sse(d, pfx, Pmovmskb, g, x, false, false)
        }
        0xd8 => pint!(Pi(PiK::Subs, 1, false)),
        0xd9 => pint!(Pi(PiK::Subs, 2, false)),
        0xda => pint!(Pi(PiK::Min, 1, false)),
        0xdb => pint!(Pi(PiK::And, 8, false)),
        0xdc => pint!(Pi(PiK::Adds, 1, false)),
        0xdd => pint!(Pi(PiK::Adds, 2, false)),
        0xde => pint!(Pi(PiK::Max, 1, false)),
        0xdf => pint!(Pi(PiK::Andn, 8, false)),
        0xe0 => pint!(Pi(PiK::Avg, 1, false)),
        0xe1 => pint!(PShift(ShK::Sra, 2)),
        0xe2 => pint!(PShift(ShK::Sra, 4)),
        0xe3 => pint!(Pi(PiK::Avg, 2, false)),
        0xe4 => pint!(Pi(PiK::Mulhi, 2, false)),
        0xe5 => pint!(Pi(PiK::Mulhi, 2, true)),
        0xe6 => match p {
            1 => sse(d, pfx, Cvt(CvtK::Tpd2Dq), x, x, false, false),
            2 => sse(d, pfx, Cvt(CvtK::Dq2Pd), x, x, false, false),
            3 => sse(d, pfx, Cvt(CvtK::Pd2Dq), x, x, false, false),
            _ => Err(DecodeError::Undefined(opc)),
        },
        0xe7 => sse(d, pfx, MovVec, x, x, true, false),
        0xe8 => pint!(Pi(PiK::Subs, 1, true)),
        0xe9 => pint!(Pi(PiK::Subs, 2, true)),
        0xea => pint!(Pi(PiK::Min, 2, true)),
        0xeb => pint!(Pi(PiK::Or, 8, false)),
        0xec => pint!(Pi(PiK::Adds, 1, true)),
        0xed => pint!(Pi(PiK::Adds, 2, true)),
        0xee => pint!(Pi(PiK::Max, 2, true)),
        0xef => pint!(Pi(PiK::Xor, 8, false)),
        0xf0 => {
            if p != 3 {
                return Err(DecodeError::Undefined(opc));
            }
            sse(d, pfx, MovVec, x, x, false, false) // lddqu
        }
        0xf1 => pint!(PShift(ShK::Sll, 2)),
        0xf2 => pint!(PShift(ShK::Sll, 4)),
        0xf3 => pint!(PShift(ShK::Sll, 8)),
        0xf4 => pint!(Pi(PiK::Muludq, 8, false)),
        0xf5 => pint!(Pi(PiK::Madd, 2, true)),
        0xf6 => pint!(Pi(PiK::Sad, 1, false)),
        0xf8 => pint!(Pi(PiK::Sub, 1, false)),
        0xf9 => pint!(Pi(PiK::Sub, 2, false)),
        0xfa => pint!(Pi(PiK::Sub, 4, false)),
        0xfb => pint!(Pi(PiK::Sub, 8, false)),
        0xfc => pint!(Pi(PiK::Add, 1, false)),
        0xfd => pint!(Pi(PiK::Add, 2, false)),
        0xfe => pint!(Pi(PiK::Add, 4, false)),
        _ => Err(DecodeError::Undefined(opc)),
    }
}

fn decode_0f38(d: &mut Dec, pfx: &Pfx, osz: u8) -> R<Inst> {
    let opc = d.u8()?;
    let p = pidx(pfx);
    let x = RK::X;
    use Op::*;
    macro_rules! s66 {
        ($op:expr) => {{
            if p != 1 {
                return Err(DecodeError::Undefined(opc));
            }
            sse(d, pfx, $op, x, x, false, false)
        }};
    }
    match opc {
        0x00 => s66!(Pshufb),
        0x01 => s66!(Pi(PiK::Hadd, 2, true)),
        0x02 => s66!(Pi(PiK::Hadd, 4, true)),
        0x03 => s66!(Pi(PiK::Hadds, 2, true)),
        0x04 => s66!(Pi(PiK::Maddubs, 1, true)),
        0x05 => s66!(Pi(PiK::Hsub, 2, true)),
        0x06 => s66!(Pi(PiK::Hsub, 4, true)),
        0x07 => s66!(Pi(PiK::Hsubs, 2, true)),
        0x08 => s66!(Pi(PiK::Sign, 1, true)),
        0x09 => s66!(Pi(PiK::Sign, 2, true)),
        0x0a => s66!(Pi(PiK::Sign, 4, true)),
        0x0b => s66!(Pi(PiK::Mulhrsw, 2, true)),
        0x10 => s66!(Pblendv),
        0x14 => s66!(Blendv(Prec::Ps)),
        0x15 => s66!(Blendv(Prec::Pd)),
        0x17 => s66!(Ptest),
        0x1c => s66!(Pi(PiK::Abs, 1, true)),
        0x1d => s66!(Pi(PiK::Abs, 2, true)),
        0x1e => s66!(Pi(PiK::Abs, 4, true)),
        0x20 => s66!(Pmovx(1, 2, true)),
        0x21 => s66!(Pmovx(1, 4, true)),
        0x22 => s66!(Pmovx(1, 8, true)),
        0x23 => s66!(Pmovx(2, 4, true)),
        0x24 => s66!(Pmovx(2, 8, true)),
        0x25 => s66!(Pmovx(4, 8, true)),
        0x28 => s66!(Pi(PiK::Muldq, 4, true)),
        0x29 => s66!(Pi(PiK::Cmpeq, 8, true)),
        0x2a => sse(d, pfx, MovVec, x, x, false, false),
        0x2b => s66!(Pack(4, false)),
        0x30 => s66!(Pmovx(1, 2, false)),
        0x31 => s66!(Pmovx(1, 4, false)),
        0x32 => s66!(Pmovx(1, 8, false)),
        0x33 => s66!(Pmovx(2, 4, false)),
        0x34 => s66!(Pmovx(2, 8, false)),
        0x35 => s66!(Pmovx(4, 8, false)),
        0x37 => s66!(Pi(PiK::Cmpgt, 8, true)),
        0x38 => s66!(Pi(PiK::Min, 1, true)),
        0x39 => s66!(Pi(PiK::Min, 4, true)),
        0x3a => s66!(Pi(PiK::Min, 2, false)),
        0x3b => s66!(Pi(PiK::Min, 4, false)),
        0x3c => s66!(Pi(PiK::Max, 1, true)),
        0x3d => s66!(Pi(PiK::Max, 4, true)),
        0x3e => s66!(Pi(PiK::Max, 2, false)),
        0x3f => s66!(Pi(PiK::Max, 4, false)),
        0x40 => s66!(Pi(PiK::Mullo, 4, true)),
        0x41 => s66!(Phminposuw),
        0xf0 | 0xf1 => {
            let m = parse_modrm(d, pfx)?;
            let g = RK::G;
            if pfx.rep == 2 {
                // crc32 r32/64, r/m8|16|32|64
                let mut i = blank(Crc32);
                i.osz = if pfx.w() { 8 } else { 4 };
                i.ssz = if opc == 0xf0 {
                    1
                } else if pfx.w() {
                    8
                } else if pfx.opsz {
                    2
                } else {
                    4
                };
                let sk = if opc == 0xf0 { RK::G8 } else { g };
                i.ops = [m.reg_opnd(g, pfx), m.rm_opnd(sk, pfx), Opnd::None];
                i.rep = 0;
                Ok(i)
            } else {
                // movbe
                let mem = m.mem.ok_or(DecodeError::Undefined(opc))?;
                let mut i = blank(Movbe);
                i.osz = osz;
                i.ssz = osz;
                let r = m.reg_opnd(g, pfx);
                i.ops = if opc == 0xf0 {
                    [r, Opnd::Mem(mem), Opnd::None]
                } else {
                    [Opnd::Mem(mem), r, Opnd::None]
                };
                Ok(i)
            }
        }
        0xf6 => {
            // adcx (66) / adox (F3)
            let m = parse_modrm(d, pfx)?;
            let mut i = blank(if pfx.rep == 1 { Adox } else { Adcx });
            i.osz = if pfx.w() { 8 } else { 4 };
            i.ssz = i.osz;
            i.ops = [m.reg_opnd(RK::G, pfx), m.rm_opnd(RK::G, pfx), Opnd::None];
            i.rep = 0;
            Ok(i)
        }
        _ => Err(DecodeError::Undefined(opc)),
    }
}

fn decode_0f3a(d: &mut Dec, pfx: &Pfx) -> R<Inst> {
    let opc = d.u8()?;
    let p = pidx(pfx);
    let (x, g) = (RK::X, RK::G);
    use Op::*;
    if p != 1 {
        return Err(DecodeError::Undefined(opc));
    }
    match opc {
        0x08 => sse(d, pfx, Round(Prec::Ps), x, x, false, true),
        0x09 => sse(d, pfx, Round(Prec::Pd), x, x, false, true),
        0x0a => sse(d, pfx, Round(Prec::Ss), x, x, false, true),
        0x0b => sse(d, pfx, Round(Prec::Sd), x, x, false, true),
        0x0c => sse(d, pfx, Blend(Prec::Ps), x, x, false, true),
        0x0d => sse(d, pfx, Blend(Prec::Pd), x, x, false, true),
        0x0e => sse(d, pfx, Pblendw, x, x, false, true),
        0x0f => sse(d, pfx, Palignr, x, x, false, true),
        0x14 => sse(d, pfx, Pextr(1), x, g, true, true),
        0x15 => sse(d, pfx, Pextr(2), x, g, true, true),
        0x16 => sse(d, pfx, Pextr(if pfx.w() { 8 } else { 4 }), x, g, true, true),
        0x17 => sse(d, pfx, Extractps, x, g, true, true),
        0x20 => sse(d, pfx, Pinsr(1), x, g, false, true),
        0x21 => sse(d, pfx, Insertps, x, x, false, true),
        0x22 => sse(
            d,
            pfx,
            Pinsr(if pfx.w() { 8 } else { 4 }),
            x,
            g,
            false,
            true,
        ),
        0x40 => sse(d, pfx, Dpp(Prec::Ps), x, x, false, true),
        0x41 => sse(d, pfx, Dpp(Prec::Pd), x, x, false, true),
        0x60 => sse(d, pfx, Pcmpstr(true, true), x, x, false, true),
        0x61 => sse(d, pfx, Pcmpstr(true, false), x, x, false, true),
        0x62 => sse(d, pfx, Pcmpstr(false, true), x, x, false, true),
        0x63 => sse(d, pfx, Pcmpstr(false, false), x, x, false, true),
        _ => Err(DecodeError::Undefined(opc)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuMode;

    fn dec(b: &[u8]) -> Inst {
        decode(b, 0x1000).unwrap()
    }

    #[test]
    fn mov_rax_imm64() {
        let i = dec(&[0x48, 0xb8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(i.op, Op::Mov);
        assert_eq!(i.len, 10);
        assert_eq!(i.osz, 8);
        assert_eq!(i.imm, 0x0807060504030201);
    }

    #[test]
    fn rip_relative_resolves_absolute() {
        // lea rax, [rip+0x10] at 0x1000, len 7 -> 0x1000+7+0x10
        let i = dec(&[0x48, 0x8d, 0x05, 0x10, 0, 0, 0]);
        assert_eq!(i.op, Op::Lea);
        match i.ops[1] {
            Opnd::Mem(m) => {
                assert_eq!(m.base, 0xff);
                assert_eq!(m.disp as u64, 0x1000 + 7 + 0x10);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn sib_and_rex() {
        // mov [r9+r10*4+8], ecx
        let i = dec(&[0x43, 0x89, 0x4c, 0x91, 0x08]);
        match i.ops[0] {
            Opnd::Mem(m) => {
                assert_eq!((m.base, m.index, m.scale, m.disp), (9, 10, 2, 8));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn byte_regs_ah_vs_spl() {
        let i = dec(&[0x88, 0xe0]); // mov al, ah
        assert_eq!(i.ops[1], Opnd::GprHi(0));
        let i = dec(&[0x40, 0x88, 0xe0]); // mov al, spl
        assert_eq!(i.ops[1], Opnd::Gpr(4));
    }

    #[test]
    fn branch_targets_absolute() {
        let i = dec(&[0xeb, 0x10]);
        assert_eq!((i.op, i.imm as u64), (Op::Jmp, 0x1000 + 2 + 0x10));
        let i = dec(&[0x0f, 0x84, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!((i.op, i.cc, i.imm as u64), (Op::Jcc, 4, 0x1000 + 6 + 0x100));
        let i = dec(&[0xe8, 0xfb, 0xff, 0xff, 0xff]);
        assert_eq!((i.op, i.imm as u64), (Op::Call, 0x1000));
    }

    #[test]
    fn sse_forms() {
        let i = dec(&[0xf2, 0x0f, 0x58, 0xc1]); // addsd xmm0, xmm1
        assert_eq!(i.op, Op::Fp(FpK::Add, Prec::Sd));
        assert_eq!((i.ops[0], i.ops[1]), (Opnd::Xmm(0), Opnd::Xmm(1)));
        let i = dec(&[0x0f, 0x11, 0x08]); // movups [rax], xmm1
        assert_eq!(i.op, Op::MovVec);
        assert!(matches!(i.ops[0], Opnd::Mem(_)));
        assert_eq!(i.ops[1], Opnd::Xmm(1));
        let i = dec(&[0x66, 0x0f, 0xef, 0xc0]); // pxor xmm0,xmm0
        assert_eq!(i.op, Op::Pi(PiK::Xor, 8, false));
    }

    #[test]
    fn truncated_and_undefined() {
        assert_eq!(decode(&[0x48], 0).unwrap_err(), DecodeError::Truncated);
        assert_eq!(decode(&[0x0f, 0x0b], 0).unwrap().op, Op::Ud2);
        assert!(decode(&[0x06], 0).is_err()); // push es: invalid in 64-bit mode
    }

    #[test]
    fn x86_mode_decodes_legacy_inc_and_32_bit_stack_operations() {
        let inc = decode_mode(&[0x40], 0x401000, CpuMode::X86_32).unwrap();
        assert_eq!(inc.op, Op::Inc);
        assert_eq!(inc.osz, 4);
        assert_eq!(inc.ops[0], Opnd::Gpr(0));

        let push = decode_mode(&[0x50], 0x401001, CpuMode::X86_32).unwrap();
        assert_eq!((push.op, push.osz), (Op::Push, 4));
        let ret = decode_mode(&[0xc3], 0x401002, CpuMode::X86_32).unwrap();
        assert_eq!((ret.op, ret.osz), (Op::Ret, 4));
    }

    #[test]
    fn x86_mode_uses_absolute_32_bit_modrm_addresses_and_wraps_branches() {
        let mov = decode_mode(
            &[0x8b, 0x05, 0x78, 0x56, 0x34, 0x12],
            0x401000,
            CpuMode::X86_32,
        )
        .unwrap();
        match mov.ops[1] {
            Opnd::Mem(mem) => {
                assert_eq!(mem.base, 0xff);
                assert_eq!(mem.disp, 0x1234_5678);
                assert!(mem.addr32);
            }
            other => panic!("expected memory operand, got {other:?}"),
        }

        let branch = decode_mode(&[0xeb, 0x02], 0xffff_fffe, CpuMode::X86_32).unwrap();
        assert_eq!(branch.imm as u32, 2);
    }

    #[test]
    fn lengths_match_known_encodings() {
        let cases: &[(&[u8], u8)] = &[
            (&[0x90], 1),
            (&[0xc3], 1),
            (&[0x48, 0x83, 0xec, 0x28], 4),
            (&[0x48, 0x89, 0x5c, 0x24, 0x08], 5),
            (&[0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00], 6),
            (&[0x0f, 0x1f, 0x84, 0x00, 0, 0, 0, 0], 8),
            (&[0xff, 0x15, 0x10, 0, 0, 0], 6),
            (&[0x48, 0xc7, 0xc0, 0xff, 0xff, 0xff, 0xff], 7),
            (&[0xf3, 0x0f, 0x10, 0x05, 0, 0, 0, 0], 8),
        ];
        for (b, n) in cases {
            assert_eq!(dec(b).len, *n, "{b:02x?}");
        }
    }
}
