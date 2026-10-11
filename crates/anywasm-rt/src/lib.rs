//! Win64 process runtime for AnyWASM.
//!
//! The technique mirrors AnyPS5: guest machine code is executed as-is (here on the interpreter in
//! `anywasm-cpu`), while every *import* is bound by name to a reimplemented system library that
//! lives outside the guest. Imports that are not implemented are bound to a trap that reports the
//! exact `dll!function` (instead of guessing), so compatibility gaps are explicit.

pub mod bundle;
mod exception;
pub mod hle;
pub mod process;
pub mod vfs;

pub use bundle::Bundle;
pub use process::{Process, RunResult, Stop};
pub use vfs::Vfs;
