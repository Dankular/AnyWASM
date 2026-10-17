//! Win32/Win64 process runtime for AnyWASM.
//!
//! The technique mirrors AnyPS5: guest machine code is executed as-is (here on the interpreter in
//! `anywasm-cpu`), while every *import* is bound by name to a reimplemented system library that
//! lives outside the guest. Imports that are not implemented are bound to a trap that reports the
//! exact `dll!function` (instead of guessing), so compatibility gaps are explicit.

pub mod bundle;
mod exception;
mod exception32;
pub mod hle;
mod msvc_eh;
mod msvc_eh32;
pub mod process;
pub mod registry;
pub mod vfs;
mod x86_abi_generated;

pub use anywasm_cpu::SharedSnapshotError;
pub use bundle::Bundle;
pub use process::{x86_import_cleanup_bytes, Process, RunResult, Stop};
pub use vfs::Vfs;
