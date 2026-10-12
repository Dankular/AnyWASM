//! x86-64 user-mode CPU core for AnyWASM: paged memory, decoder, interpreter.
pub mod cpu;
pub use anywasm_cpu_state::decode;
pub mod exec;
pub mod jit;
pub mod sse;
pub mod x87;
pub mod mem;

pub use cpu::{Cpu, CpuMode};
pub use decode::{decode, decode_mode, DecodeError, Inst, Op, Opnd};
pub use exec::{exec, step, BlockCache, DecodedBlock, DecodeCache, Exit};
pub use mem::{Access, Fault, Memory};
