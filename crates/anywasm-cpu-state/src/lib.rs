#![no_std]

//! Allocation-free architectural state and the versioned shared-worker ABI.
//!
//! The context has a fixed C layout so a coordinator and independently instantiated worker
//! modules can exchange it through a shared memory region. A worker must own its context for the
//! duration of a dispatch; the slot's release/acquire transitions define that ownership boundary.

#[cfg(feature = "std")]
extern crate std;

pub mod exec_core;
pub mod guest_memory;
pub mod memory;
pub mod shared;
pub mod exec {
    pub use crate::exec_core::*;
}
#[cfg(feature = "std")]
#[path = "../../anywasm-cpu/src/sse.rs"]
pub mod sse;
#[cfg(feature = "std")]
#[path = "../../anywasm-cpu/src/x87.rs"]
pub mod x87;

pub use memory::{Access, Fault, GuestMemory, MemResult};

// Keep the decoder's existing import path while it is shared with the std interpreter crate.
pub mod cpu {
    pub use crate::{
        Access, Cpu, CpuMode, AF, ARITH_FLAGS, CF, DF, IF, OF, PF, R10, R11, R8, R9, RAX, RBP, RBX,
        RCX, RDI, RDX, RSI, RSP, SF, TF, USER_FLAGS, ZF,
    };
}

#[path = "../../anywasm-cpu/src/decode.rs"]
pub mod decode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CpuMode {
    X86_32 = 0,
    #[default]
    X86_64 = 1,
}

pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RBX: usize = 3;
pub const RSP: usize = 4;
pub const RBP: usize = 5;
pub const RSI: usize = 6;
pub const RDI: usize = 7;
pub const R8: usize = 8;
pub const R9: usize = 9;
pub const R10: usize = 10;
pub const R11: usize = 11;

pub const CF: u64 = 1 << 0;
pub const PF: u64 = 1 << 2;
pub const AF: u64 = 1 << 4;
pub const ZF: u64 = 1 << 6;
pub const SF: u64 = 1 << 7;
pub const TF: u64 = 1 << 8;
pub const IF: u64 = 1 << 9;
pub const DF: u64 = 1 << 10;
pub const OF: u64 = 1 << 11;
/// Bits software can modify with POPF/SAHF etc. in user mode.
pub const USER_FLAGS: u64 = CF | PF | AF | ZF | SF | TF | IF | DF | OF | (1 << 18) | (1 << 14);
pub const ARITH_FLAGS: u64 = CF | PF | AF | ZF | SF | OF;

/// Architectural state for one x86 user-mode thread.
#[derive(Clone, Debug)]
#[repr(C, align(16))]
pub struct Cpu {
    pub mode: CpuMode,
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    /// XMM0..15 (low 128 bits of YMM).
    pub xmm: [u128; 16],
    /// YMM0..15 upper 128 bits; legacy SSE instructions leave these unchanged.
    pub ymm_hi: [u128; 16],
    pub mxcsr: u32,
    /// x87 state. Values are held as f64 (documented precision limitation: not 80-bit).
    pub fpu_st: [f64; 8],
    pub fpu_top: u8,
    pub fpu_cw: u16,
    pub fpu_sw: u16,
    pub fpu_tw: u16,
    /// Time stamp counter, advanced by one per retired instruction.
    pub tsc: u64,
    /// Retired instruction count.
    pub icount: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            mode: CpuMode::X86_64,
            gpr: [0; 16],
            rip: 0,
            rflags: 0x202,
            fs_base: 0,
            gs_base: 0,
            xmm: [0; 16],
            ymm_hi: [0; 16],
            mxcsr: 0x1f80,
            fpu_st: [0.0; 8],
            fpu_top: 0,
            fpu_cw: 0x037f,
            fpu_sw: 0,
            fpu_tw: 0xffff,
            tsc: 0,
            icount: 0,
        }
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }
    #[inline]
    pub fn flag(&self, f: u64) -> bool {
        self.rflags & f != 0
    }
    #[inline]
    pub fn set_flag(&mut self, f: u64, v: bool) {
        if v {
            self.rflags |= f
        } else {
            self.rflags &= !f
        }
    }
}

/// Parity flag value for the low byte of `v` (set when the number of 1 bits is even).
#[inline]
pub fn parity(v: u64) -> bool {
    (v as u8).count_ones() % 2 == 0
}

/// Evaluate x86 condition code `cc` (0..15, the low nibble of Jcc/SETcc/CMOVcc opcodes).
#[inline]
pub fn cond(rflags: u64, cc: u8) -> bool {
    let cf = rflags & CF != 0;
    let zf = rflags & ZF != 0;
    let sf = rflags & SF != 0;
    let of = rflags & OF != 0;
    let pf = rflags & PF != 0;
    let r = match cc >> 1 {
        0 => of,
        1 => cf,
        2 => zf,
        3 => cf || zf,
        4 => sf,
        5 => pf,
        6 => sf != of,
        _ => zf || (sf != of),
    };
    r ^ (cc & 1 != 0)
}

const _: [(); 784] = [(); core::mem::size_of::<Cpu>()];
const _: [(); 16] = [(); core::mem::align_of::<Cpu>()];
const _: [(); 8] = [(); core::mem::offset_of!(Cpu, gpr)];
const _: [(); 176] = [(); core::mem::offset_of!(Cpu, xmm)];
const _: [(); 432] = [(); core::mem::offset_of!(Cpu, ymm_hi)];
const _: [(); 768] = [(); core::mem::offset_of!(Cpu, tsc)];
