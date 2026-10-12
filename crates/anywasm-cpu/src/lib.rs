//! x86-64 user-mode CPU core for AnyWASM: paged memory, decoder, interpreter.
pub mod cpu;
pub use anywasm_cpu_state::decode;
pub mod exec;
pub(crate) mod memory {
    pub use anywasm_cpu_state::memory::*;
}
pub mod jit;
pub mod mem;
pub mod sse;
pub mod x87;

pub use cpu::{Cpu, CpuMode};
pub use decode::{decode, decode_mode, DecodeError, Inst, Op, Opnd};
pub use exec::{exec, step, BlockCache, DecodeCache, DecodedBlock, Exit};
pub use mem::{Access, Fault, Memory, SharedSnapshotError};
