//! Small WebAssembly block emitter used by the wasm32 runtime.
//!
//! The generated module imports the runtime's memory and receives a pointer to a copied CPU
//! snapshot. This keeps a trapped or rejected module from partially changing the live CPU.

use crate::cpu::{Cpu, CpuMode};
use crate::decode::{FpK, Inst, MemOp, Op, Opnd, Prec};
#[cfg(target_arch = "wasm32")]
use crate::mem::Memory;
use crate::mem::TlbEntry;

const GPR_OFFSET: u32 = 0;
const RIP_OFFSET: u32 = 128;
const RFLAGS_OFFSET: u32 = 136;
const ICOUNT_OFFSET: u32 = 144;
const TSC_OFFSET: u32 = 152;
const FRAMES_PTR_OFFSET: u32 = 160;
const FRAMES_LEN_OFFSET: u32 = 164;
const TLB_READ_PTR_OFFSET: u32 = 168;
const TLB_WRITE_PTR_OFFSET: u32 = 172;
const CHAIN_FALLBACKS_OFFSET: u32 = 176;
const CHAIN_TRANSITIONS_OFFSET: u32 = 180;
const CHAIN_REGREG_HITS_OFFSET: u32 = 184;
const CHAIN_SCRATCH_OFFSET: u32 = 192;
const ADD_LHS_LOCAL: u32 = 5;
const ADD_RHS_LOCAL: u32 = 6;
const ADD_RESULT_LOCAL: u32 = 7;
const ADD_FLAGS_LOCAL: u32 = 8;
const ALU64_LHS_LOCAL: u32 = 60;
const ALU64_RHS_LOCAL: u32 = 61;
const ALU64_RESULT_LOCAL: u32 = 62;
const GPR_LOCAL_BASE: u32 = 9;
const XMM_OFFSET: u32 = 352;
const MXCSR_OFFSET: u32 = 608;
const CPU_MODE_OFFSET: u32 = 612;
const FS_BASE_OFFSET: u32 = 616;
const GS_BASE_OFFSET: u32 = 624;
const FPU_ST_OFFSET: u32 = 896;
const FPU_TOP_OFFSET: u32 = 960;
const FPU_TW_OFFSET: u32 = 972;
const X87_TOP_LOCAL: u32 = 63;
const X87_VALUE0_LOCAL: u32 = 64;
const X87_VALUE1_LOCAL: u32 = 65;
const X87_RESULT_LOCAL: u32 = 66;
const X87_TAG_LOCAL: u32 = 67;
const XMM_LOCAL_BASE: u32 = 25;
const RIP_LOCAL: u32 = 57;
const ICOUNT_LOCAL: u32 = 58;
const TSC_LOCAL: u32 = 59;
const TLB_PAGE_OFFSET: u32 = std::mem::offset_of!(TlbEntry, page) as u32;
const TLB_FRAME_OFFSET: u32 = std::mem::offset_of!(TlbEntry, offset) as u32;
const TLB_PROT_OFFSET: u32 = std::mem::offset_of!(TlbEntry, prot) as u32;

/// ABI shared by the generated module and the host trampoline. Keep fields in this order.
#[repr(C)]
#[derive(Clone, Copy)]
struct JitState {
    gpr: [u64; 16],
    rip: u64,
    rflags: u64,
    icount: u64,
    tsc: u64,
    frames: u32,
    frames_len: u32,
    tlb_read: u32,
    tlb_write: u32,
    chain_fallbacks: u32,
    chain_transitions: u32,
    chain_register_hits: u32,
    _reserved: u32,
    chain_scratch: [u8; 160],
    xmm: [u128; 16],
    mxcsr: u32,
    cpu_mode: u32,
    fs_base: u64,
    gs_base: u64,
    ymm_hi: [u128; 16],
    fpu_st: [u64; 8],
    fpu_top: u32,
    fpu_cw: u32,
    fpu_sw: u32,
    fpu_tw: u32,
}

const _: [(); 976] = [(); std::mem::size_of::<JitState>()];
const _: [(); GPR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, gpr)];
const _: [(); RIP_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rip)];
const _: [(); RFLAGS_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rflags)];
const _: [(); ICOUNT_OFFSET as usize] = [(); std::mem::offset_of!(JitState, icount)];
const _: [(); TSC_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tsc)];
const _: [(); FRAMES_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, frames)];
const _: [(); FRAMES_LEN_OFFSET as usize] = [(); std::mem::offset_of!(JitState, frames_len)];
const _: [(); TLB_READ_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tlb_read)];
const _: [(); TLB_WRITE_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tlb_write)];
const _: [(); CHAIN_FALLBACKS_OFFSET as usize] =
    [(); std::mem::offset_of!(JitState, chain_fallbacks)];
const _: [(); CHAIN_TRANSITIONS_OFFSET as usize] =
    [(); std::mem::offset_of!(JitState, chain_transitions)];
const _: [(); CHAIN_REGREG_HITS_OFFSET as usize] =
    [(); std::mem::offset_of!(JitState, chain_register_hits)];
const _: [(); CHAIN_SCRATCH_OFFSET as usize] = [(); std::mem::offset_of!(JitState, chain_scratch)];
const _: [(); XMM_OFFSET as usize] = [(); std::mem::offset_of!(JitState, xmm)];
const _: [(); MXCSR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, mxcsr)];
const _: [(); CPU_MODE_OFFSET as usize] = [(); std::mem::offset_of!(JitState, cpu_mode)];
const _: [(); FS_BASE_OFFSET as usize] = [(); std::mem::offset_of!(JitState, fs_base)];
const _: [(); GS_BASE_OFFSET as usize] = [(); std::mem::offset_of!(JitState, gs_base)];
const _: [(); FPU_ST_OFFSET as usize] = [(); std::mem::offset_of!(JitState, fpu_st)];
const _: [(); FPU_TOP_OFFSET as usize] = [(); std::mem::offset_of!(JitState, fpu_top)];
const _: [(); FPU_TW_OFFSET as usize] = [(); std::mem::offset_of!(JitState, fpu_tw)];

impl From<&Cpu> for JitState {
    fn from(cpu: &Cpu) -> Self {
        Self {
            gpr: cpu.gpr,
            rip: cpu.rip,
            rflags: cpu.rflags,
            icount: cpu.icount,
            tsc: cpu.tsc,
            frames: 0,
            frames_len: 0,
            tlb_read: 0,
            tlb_write: 0,
            chain_fallbacks: 0,
            chain_transitions: 0,
            chain_register_hits: 0,
            _reserved: 0,
            chain_scratch: [0; 160],
            xmm: cpu.xmm,
            mxcsr: cpu.mxcsr,
            cpu_mode: u32::from(cpu.mode == CpuMode::X86_32),
            fs_base: cpu.fs_base,
            gs_base: cpu.gs_base,
            ymm_hi: cpu.ymm_hi,
            fpu_st: cpu.fpu_st.map(f64::to_bits),
            fpu_top: cpu.fpu_top as u32,
            fpu_cw: cpu.fpu_cw as u32,
            fpu_sw: cpu.fpu_sw as u32,
            fpu_tw: cpu.fpu_tw as u32,
        }
    }
}

impl JitState {
    #[cfg(any(target_arch = "wasm32", test))]
    fn copy_to(self, cpu: &mut Cpu) -> bool {
        if self.cpu_mode > 1 {
            return false;
        }
        cpu.mode = if self.cpu_mode == 0 {
            CpuMode::X86_64
        } else {
            CpuMode::X86_32
        };
        cpu.gpr = self.gpr;
        cpu.rip = self.rip;
        cpu.rflags = self.rflags;
        cpu.fs_base = self.fs_base;
        cpu.gs_base = self.gs_base;
        cpu.icount = self.icount;
        cpu.tsc = self.tsc;
        cpu.xmm = self.xmm;
        cpu.ymm_hi = self.ymm_hi;
        cpu.mxcsr = self.mxcsr;
        cpu.fpu_st = self.fpu_st.map(f64::from_bits);
        cpu.fpu_top = self.fpu_top as u8;
        cpu.fpu_cw = self.fpu_cw as u16;
        cpu.fpu_sw = self.fpu_sw as u16;
        cpu.fpu_tw = self.fpu_tw as u16;
        true
    }
}

#[derive(Clone, Copy)]
enum MovSource {
    Immediate(u64),
    Register { index: u8, zero_extend: bool },
}

#[derive(Clone, Copy)]
struct MovInstruction {
    dst: u8,
    source: MovSource,
    len: u8,
}

#[derive(Clone, Copy)]
enum StoreSource {
    Immediate(u64),
    Register(u8),
}

#[derive(Clone, Copy)]
enum AddSource {
    Immediate(u64),
    Register(u8),
}

#[derive(Clone, Copy)]
enum XmmSource {
    Register(u8),
    Memory(MemOp),
}

#[derive(Clone, Copy)]
enum X87StackOp {
    Load(u8),
    Exchange(u8),
    StorePop(u8),
    Arithmetic { index: u8, operation: u8 },
}

#[derive(Clone, Copy)]
struct StoreInstruction {
    address: MemOp,
    source: StoreSource,
    size: u8,
    len: u8,
}

#[derive(Clone, Copy)]
enum JitInstruction {
    Nop {
        len: u8,
    },
    Mov(MovInstruction),
    Load {
        dst: u8,
        address: MemOp,
        size: u8,
        len: u8,
    },
    Store(StoreInstruction),
    Add {
        dst: u8,
        source: AddSource,
        width: u8,
        len: u8,
    },
    Xor {
        dst: u8,
        source: u8,
        width: u8,
        len: u8,
    },
    Cmp {
        lhs: u8,
        source: AddSource,
        width: u8,
        len: u8,
    },
    Lea {
        dst: u8,
        address: MemOp,
        len: u8,
    },
    MemoryAdd {
        address: MemOp,
        source: u8,
        len: u8,
    },
    XmmMove {
        dst: u8,
        source: XmmSource,
        len: u8,
    },
    XmmStore {
        address: MemOp,
        source: u8,
        len: u8,
    },
    XmmFp {
        dst: u8,
        source: u8,
        kind: FpK,
        len: u8,
    },
    X87Stack {
        op: X87StackOp,
        len: u8,
    },
    Jump {
        target: u64,
        len: u8,
    },
    JumpRegister {
        register: u8,
        len: u8,
    },
    Branch {
        cc: u8,
        target: u64,
        len: u8,
    },
}

/// A generated straight-line prefix and its process-local cache key.
#[allow(dead_code)]
pub struct EmittedBlock {
    id: u32,
    start: u64,
    wasm: Vec<u8>,
    instructions: usize,
    first_register_register: Option<u32>,
    ends_with_store: bool,
}

impl EmittedBlock {
    pub fn instruction_count(&self) -> usize {
        self.instructions
    }

    /// Invoke the host compile-and-run trampoline. Invalid results leave the live CPU untouched.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn execute(
        &self,
        cpu: &mut Cpu,
        mem: &mut Memory,
        budget: u64,
    ) -> Option<(usize, bool)> {
        if budget == 0 {
            return Some((0, false));
        }
        let mut state = JitState::from(&*cpu);
        let view = mem.jit_view();
        state.frames = view.frames;
        state.frames_len = view.frames_len;
        state.tlb_read = view.tlb_read;
        state.tlb_write = view.tlb_write;
        let budget = budget.min(u32::MAX as u64) as u32;
        // SAFETY: both buffers are live in the main module's linear memory for this synchronous
        // import call. The host reads the module bytes and passes the state pointer to the emitted
        // module, which uses this exact repr(C) layout.
        let executed = unsafe {
            host_run_emitted_block(
                self.id,
                self.wasm.as_ptr(),
                self.wasm.len() as u32,
                &mut state,
                budget,
                self.instructions as u32,
                self.first_register_register.unwrap_or(u32::MAX),
                self.start,
                mem.code_epoch,
            )
        };
        let expected = (budget as usize).min(self.instructions);
        if executed <= 0 || (executed as usize) < expected || executed as u64 > budget as u64 {
            return None;
        }
        let chain_handoff = state.chain_transitions != 0 || self.ends_with_store;
        if !state.copy_to(cpu) {
            return None;
        }
        Some((executed as usize, chain_handoff))
    }
}

/// Emit a module for a leading supported prefix in a decoded block.
///
/// Only 32-bit and 64-bit GPR operands are included. All other encodings and instructions stop
/// the emitted prefix and remain on the interpreter path.
pub fn emit_mov_block(
    id: u32,
    start: u64,
    instructions: &[Inst],
    min_prefix: usize,
) -> Option<EmittedBlock> {
    let min_prefix = min_prefix.max(1);
    if min_prefix > instructions.len() {
        return None;
    }
    let mut emitted = Vec::new();
    let mut ends_with_store = false;
    for inst in instructions.iter().copied() {
        let Some(instruction) = translate_instruction(inst) else {
            break;
        };
        let is_store = matches!(
            instruction,
            JitInstruction::Store(_)
                | JitInstruction::XmmStore { .. }
                | JitInstruction::MemoryAdd { .. }
        );
        let is_exit = matches!(
            instruction,
            JitInstruction::Jump { .. }
                | JitInstruction::JumpRegister { .. }
                | JitInstruction::Branch { .. }
        );
        emitted.push(instruction);
        // A direct store is observable before Rust commits the copied CPU state. Keep it last so
        // no later TLB miss can discard the CPU snapshot and replay this memory side effect.
        if is_store || is_exit {
            ends_with_store = is_store;
            break;
        }
    }
    if emitted.len() < min_prefix {
        return None;
    }
    let first_register_register = emitted
        .iter()
        .position(|instruction| {
            matches!(instruction, JitInstruction::Mov(mov) if matches!(mov.source, MovSource::Register { .. }))
        })
        .map(|index| index as u32);
    Some(EmittedBlock {
        id,
        start,
        wasm: emit_module(start, &emitted),
        instructions: emitted.len(),
        first_register_register,
        ends_with_store,
    })
}

fn translate_instruction(inst: Inst) -> Option<JitInstruction> {
    if inst.lock || inst.len == 0 {
        return None;
    }
    if inst.op == Op::Nop {
        return Some(JitInstruction::Nop { len: inst.len });
    }
    if inst.op == Op::JmpInd && inst.osz == 8 {
        let Opnd::Gpr(register) = inst.ops[0] else {
            return None;
        };
        return (register < 16).then_some(JitInstruction::JumpRegister {
            register,
            len: inst.len,
        });
    }
    if inst.op == Op::X87 {
        return translate_x87_stack(inst);
    }
    match inst.op {
        Op::Jmp => {
            return Some(JitInstruction::Jump {
                target: inst.imm as u64,
                len: inst.len,
            });
        }
        Op::Jcc if inst.cc < 16 => {
            return Some(JitInstruction::Branch {
                cc: inst.cc,
                target: inst.imm as u64,
                len: inst.len,
            });
        }
        _ => {}
    }
    translate_mov(inst)
        .or_else(|| translate_add(inst))
        .or_else(|| translate_xor(inst))
        .or_else(|| translate_cmp(inst))
        .or_else(|| translate_lea(inst))
        .or_else(|| translate_sse(inst))
}

fn translate_x87_stack(inst: Inst) -> Option<JitInstruction> {
    let modrm = inst.imm as u8;
    if modrm >> 6 != 3 {
        return None;
    }
    let reg = (modrm >> 3) & 7;
    let index = modrm & 7;
    let op = match (inst.cc, reg) {
        (1, 0) => X87StackOp::Load(index),
        (1, 1) => X87StackOp::Exchange(index),
        (1, 3) => X87StackOp::StorePop(index),
        (0, operation @ (0 | 1 | 4 | 5 | 6 | 7)) => {
            X87StackOp::Arithmetic { index, operation }
        }
        _ => return None,
    };
    Some(JitInstruction::X87Stack { op, len: inst.len })
}

fn translate_add(inst: Inst) -> Option<JitInstruction> {
    if inst.op != Op::Add || !matches!(inst.osz, 4 | 8) {
        return None;
    }
    match (inst.ops[0], inst.ops[1]) {
        (Opnd::Gpr(dst), Opnd::Imm) if dst < 16 => Some(JitInstruction::Add {
            dst,
            source: AddSource::Immediate(inst.imm as u64),
            width: inst.osz,
            len: inst.len,
        }),
        (Opnd::Gpr(dst), Opnd::Gpr(source)) if dst < 16 && source < 16 => {
            Some(JitInstruction::Add {
                dst,
                source: AddSource::Register(source),
                width: inst.osz,
                len: inst.len,
            })
        }
        (Opnd::Mem(address), Opnd::Gpr(source))
            if inst.osz == 8 && inline_address(address) && source < 16 =>
        {
            Some(JitInstruction::MemoryAdd {
                address,
                source,
                len: inst.len,
            })
        }
        _ => None,
    }
}

fn translate_xor(inst: Inst) -> Option<JitInstruction> {
    if inst.op != Op::Xor || !matches!(inst.osz, 4 | 8) {
        return None;
    }
    let (Opnd::Gpr(dst), Opnd::Gpr(source)) = (inst.ops[0], inst.ops[1]) else {
        return None;
    };
    (dst < 16 && source < 16).then_some(JitInstruction::Xor {
        dst,
        source,
        width: inst.osz,
        len: inst.len,
    })
}

fn translate_cmp(inst: Inst) -> Option<JitInstruction> {
    if inst.op != Op::Cmp || inst.osz != 8 {
        return None;
    }
    let Opnd::Gpr(lhs) = inst.ops[0] else {
        return None;
    };
    let source = match inst.ops[1] {
        Opnd::Imm => AddSource::Immediate(inst.imm as u64),
        Opnd::Gpr(index) if index < 16 => AddSource::Register(index),
        _ => return None,
    };
    (lhs < 16).then_some(JitInstruction::Cmp {
        lhs,
        source,
        width: inst.osz,
        len: inst.len,
    })
}

fn translate_lea(inst: Inst) -> Option<JitInstruction> {
    if inst.op != Op::Lea || inst.osz != 8 {
        return None;
    }
    let (Opnd::Gpr(dst), Opnd::Mem(address)) = (inst.ops[0], inst.ops[1]) else {
        return None;
    };
    (dst < 16 && inline_address(address)).then_some(JitInstruction::Lea {
        dst,
        address,
        len: inst.len,
    })
}

fn translate_sse(inst: Inst) -> Option<JitInstruction> {
    match inst.op {
        Op::MovVec => match (inst.ops[0], inst.ops[1]) {
            (Opnd::Xmm(dst), Opnd::Xmm(src)) if dst < 16 && src < 16 => {
                Some(JitInstruction::XmmMove {
                    dst,
                    source: XmmSource::Register(src),
                    len: inst.len,
                })
            }
            (Opnd::Xmm(dst), Opnd::Mem(address)) if dst < 16 && inline_address(address) => {
                Some(JitInstruction::XmmMove {
                    dst,
                    source: XmmSource::Memory(address),
                    len: inst.len,
                })
            }
            (Opnd::Mem(address), Opnd::Xmm(src)) if src < 16 && inline_address(address) => {
                Some(JitInstruction::XmmStore {
                    address,
                    source: src,
                    len: inst.len,
                })
            }
            _ => None,
        },
        Op::Fp(kind @ (FpK::Add | FpK::Mul | FpK::Sub | FpK::Div), Prec::Pd) => {
            let (Opnd::Xmm(dst), Opnd::Xmm(source)) = (inst.ops[0], inst.ops[1]) else {
                return None;
            };
            (dst < 16 && source < 16).then_some(JitInstruction::XmmFp {
                dst,
                source,
                kind,
                len: inst.len,
            })
        }
        _ => None,
    }
}

fn translate_mov(inst: Inst) -> Option<JitInstruction> {
    if inst.op != Op::Mov || !matches!(inst.osz, 4 | 8) {
        return None;
    }
    match (inst.ops[0], inst.ops[1]) {
        (Opnd::Gpr(dst), Opnd::Imm) if dst < 16 => Some(JitInstruction::Mov(MovInstruction {
            dst,
            source: MovSource::Immediate(if inst.osz == 4 {
                (inst.imm as u32) as u64
            } else {
                inst.imm as u64
            }),
            len: inst.len,
        })),
        (Opnd::Gpr(dst), Opnd::Gpr(src)) if dst < 16 && src < 16 => {
            Some(JitInstruction::Mov(MovInstruction {
                dst,
                source: MovSource::Register {
                    index: src,
                    zero_extend: inst.osz == 4,
                },
                len: inst.len,
            }))
        }
        (Opnd::Gpr(dst), Opnd::Mem(address)) if dst < 16 && inline_address(address) => {
            Some(JitInstruction::Load {
                dst,
                address,
                size: inst.osz,
                len: inst.len,
            })
        }
        (Opnd::Mem(address), Opnd::Gpr(src)) if src < 16 && inline_address(address) => {
            Some(JitInstruction::Store(StoreInstruction {
                address,
                source: StoreSource::Register(src),
                size: inst.osz,
                len: inst.len,
            }))
        }
        (Opnd::Mem(address), Opnd::Imm) if inline_address(address) => {
            Some(JitInstruction::Store(StoreInstruction {
                address,
                source: StoreSource::Immediate(if inst.osz == 4 {
                    (inst.imm as u32) as u64
                } else {
                    inst.imm as u64
                }),
                size: inst.osz,
                len: inst.len,
            }))
        }
        _ => None,
    }
}

fn inline_address(address: MemOp) -> bool {
    !address.addr32
        && address.seg == 0
        && address.scale <= 3
        && (address.base == 0xff || address.base < 16)
        && (address.index == 0xff || address.index < 16)
}

fn emit_module(start: u64, instructions: &[JitInstruction]) -> Vec<u8> {
    let mut code = Vec::new();
    // Locals: execution scratch, GPRs, both XMM lanes, RIP/count/TSC, and 64-bit ALU scratch.
    code.extend_from_slice(&[
        0x0b, 0x01, 0x7f, 0x01, 0x7e, 0x01, 0x7f, 0x04, 0x7f, 0x10, 0x7e, 0x20, 0x7e, 0x03,
        0x7e, 0x03, 0x7e, 0x01, 0x7f, 0x03, 0x7e, 0x01, 0x7f,
    ]);
    let (initial_gprs, written_gprs, initial_xmms, written_xmms) = register_masks(instructions);
    for register in 0..16 {
        if initial_gprs & (1 << register) != 0 {
            emit_state_gpr_load(register, &mut code);
        }
        if initial_xmms & (1 << register) != 0 {
            emit_state_xmm_load(register, &mut code);
        }
    }
    emit_state_local_load(ICOUNT_OFFSET, ICOUNT_LOCAL, &mut code);
    emit_state_local_load(TSC_OFFSET, TSC_LOCAL, &mut code);
    code.push(0x42); // local RIP begins at this block's entry address
    sleb(start as i64, &mut code);
    code.push(0x21);
    uleb(RIP_LOCAL, &mut code);
    code.extend_from_slice(&[0x02, 0x40]); // shared budget-exit block
    let mut rip = start;
    for instruction in instructions {
        let len = match instruction {
            JitInstruction::Nop { len } => *len,
            JitInstruction::Mov(mov) => mov.len,
            JitInstruction::Load { len, .. } => *len,
            JitInstruction::Store(store) => store.len,
            JitInstruction::Add { len, .. } => *len,
            JitInstruction::Xor { len, .. }
            | JitInstruction::Cmp { len, .. }
            | JitInstruction::Lea { len, .. }
            | JitInstruction::MemoryAdd { len, .. } => *len,
            JitInstruction::XmmMove { len, .. }
            | JitInstruction::XmmStore { len, .. }
            | JitInstruction::XmmFp { len, .. } => *len,
            JitInstruction::X87Stack { len, .. }
            | JitInstruction::Jump { len, .. }
            | JitInstruction::JumpRegister { len, .. }
            | JitInstruction::Branch { len, .. } => *len,
        };
        rip = rip.wrapping_add(len as u64);
        let mut rip_after = rip;
        // if (executed >= budget) branch to the shared state-flush epilogue.
        code.extend_from_slice(&[0x20, 0x02, 0x20, 0x01, 0x4f, 0x0d, 0x00]);

        match instruction {
            JitInstruction::Nop { .. } => {}
            JitInstruction::Mov(mov) => emit_register_move(*mov, &mut code),
            JitInstruction::Load {
                dst, address, size, ..
            } => {
                emit_effective_address(*address, &mut code);
                emit_tlb_guard(false, *size, &mut code);
                emit_frame_address(&mut code);
                if *size == 4 {
                    code.push(0x28); // i32.load
                    uleb(2, &mut code);
                    uleb(0, &mut code);
                    code.push(0xad); // i64.extend_i32_u
                } else {
                    code.push(0x29); // i64.load
                    uleb(3, &mut code);
                    uleb(0, &mut code);
                }
                code.push(0x21); // local.set destination GPR
                uleb(gpr_local(*dst), &mut code);
            }
            JitInstruction::Store(store) => {
                emit_effective_address(store.address, &mut code);
                emit_tlb_guard(true, store.size, &mut code);
                emit_frame_address(&mut code);
                match store.source {
                    StoreSource::Immediate(value) if store.size == 4 => {
                        code.push(0x41); // i32.const
                        sleb(value as u32 as i32 as i64, &mut code);
                    }
                    StoreSource::Immediate(value) => {
                        code.push(0x42); // i64.const
                        sleb(value as i64, &mut code);
                    }
                    StoreSource::Register(index) => {
                        code.push(0x20);
                        uleb(gpr_local(index), &mut code);
                        if store.size == 4 {
                            code.push(0xa7); // i32.wrap_i64
                        }
                    }
                }
                if store.size == 4 {
                    code.push(0x36); // i32.store
                    uleb(2, &mut code);
                } else {
                    code.push(0x37); // i64.store
                    uleb(3, &mut code);
                }
                uleb(0, &mut code);
            }
            JitInstruction::Add {
                dst, source, width, ..
            } => {
                if *width == 4 {
                    emit_add32(*dst, *source, &mut code);
                } else {
                    emit_add64_gpr(*dst, *source, &mut code);
                }
            }
            JitInstruction::Xor {
                dst, source, width, ..
            } => emit_xor_gpr(*dst, *source, *width, &mut code),
            JitInstruction::Cmp {
                lhs, source, width, ..
            } => emit_cmp_gpr(*lhs, *source, *width, &mut code),
            JitInstruction::Lea { dst, address, .. } => {
                emit_effective_address(*address, &mut code);
                code.push(0x20);
                uleb(3, &mut code);
                code.push(0x21);
                uleb(gpr_local(*dst), &mut code);
            }
            JitInstruction::MemoryAdd {
                address, source, ..
            } => {
                emit_effective_address(*address, &mut code);
                emit_tlb_guard(false, 8, &mut code);
                emit_effective_address(*address, &mut code);
                emit_tlb_guard(true, 8, &mut code);
                emit_frame_address(&mut code);
                code.push(0x29); // i64.load old destination
                uleb(3, &mut code);
                uleb(0, &mut code);
                code.push(0x21);
                uleb(ALU64_LHS_LOCAL, &mut code);
                code.push(0x20);
                uleb(gpr_local(*source), &mut code);
                code.push(0x21);
                uleb(ALU64_RHS_LOCAL, &mut code);
                emit_add64_values(
                    ALU64_LHS_LOCAL,
                    ALU64_RHS_LOCAL,
                    ALU64_RESULT_LOCAL,
                    &mut code,
                );
                emit_frame_address(&mut code);
                code.push(0x20);
                uleb(ALU64_RESULT_LOCAL, &mut code);
                code.push(0x37); // i64.store updated destination
                uleb(3, &mut code);
                uleb(0, &mut code);
            }
            JitInstruction::XmmMove { dst, source, .. } => match source {
                XmmSource::Register(src) => emit_xmm_register_move(*dst, *src, &mut code),
                XmmSource::Memory(address) => {
                    emit_effective_address(*address, &mut code);
                    emit_tlb_guard(false, 16, &mut code);
                    emit_xmm_memory_load(*dst, &mut code);
                }
            },
            JitInstruction::XmmStore {
                address, source, ..
            } => {
                emit_effective_address(*address, &mut code);
                emit_tlb_guard(true, 16, &mut code);
                emit_xmm_memory_store(*source, &mut code);
            }
            JitInstruction::XmmFp {
                dst, source, kind, ..
            } => emit_xmm_fp(*dst, *source, *kind, &mut code),
            JitInstruction::X87Stack { op, .. } => emit_x87_stack(*op, &mut code),
            JitInstruction::Jump { target, .. } => rip_after = *target,
            JitInstruction::JumpRegister { register, .. } => {
                code.push(0x20);
                uleb(gpr_local(*register), &mut code);
                code.push(0x21);
                uleb(RIP_LOCAL, &mut code);
            }
            JitInstruction::Branch { cc, target, .. } => {
                emit_condition(*cc, &mut code);
                code.extend_from_slice(&[0x04, 0x7e]); // if (result i64)
                code.push(0x42);
                sleb(*target as i64, &mut code);
                code.push(0x05); // else
                code.push(0x42);
                sleb(rip as i64, &mut code);
                code.push(0x0b); // end
                code.push(0x21); // local.set RIP
                uleb(RIP_LOCAL, &mut code);
            }
        }

        // RIP and counters stay in locals for the entire emitted block.
        if !matches!(
            instruction,
            JitInstruction::Branch { .. } | JitInstruction::JumpRegister { .. }
        ) {
            code.push(0x42);
            sleb(rip_after as i64, &mut code);
            code.push(0x21);
            uleb(RIP_LOCAL, &mut code);
        }
        emit_increment_local(ICOUNT_LOCAL, &mut code);
        emit_increment_local(TSC_LOCAL, &mut code);

        // executed += 1
        code.extend_from_slice(&[0x20, 0x02, 0x41, 0x01, 0x6a, 0x21, 0x02]);
    }
    code.push(0x0b); // end shared budget-exit block
    emit_flush_state(written_gprs, written_xmms, &mut code);
    code.extend_from_slice(&[0x20, 0x02, 0x0b]);

    let mut module = b"\0asm\x01\0\0\0".to_vec();
    // type: (i32 state_ptr, i32 budget) -> i32 executed
    section(1, &[0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f], &mut module);
    // import memory "anywasm"."memory" with minimum one page
    section(
        2,
        &[
            0x01, 0x07, b'a', b'n', b'y', b'w', b'a', b's', b'm', 0x06, b'm', b'e', b'm', b'o',
            b'r', b'y', 0x02, 0x00, 0x01,
        ],
        &mut module,
    );
    section(3, &[0x01, 0x00], &mut module); // one function, type 0
    section(7, &[0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00], &mut module);
    let mut body = Vec::with_capacity(code.len() + 8);
    body.extend_from_slice(&code);
    let mut code_section = vec![0x01];
    uleb(body.len() as u32, &mut code_section);
    code_section.extend_from_slice(&body);
    section(10, &code_section, &mut module);
    module
}

fn emit_condition(cc: u8, code: &mut Vec<u8>) {
    const CF: u64 = 1 << 0;
    const PF: u64 = 1 << 2;
    const ZF: u64 = 1 << 6;
    const SF: u64 = 1 << 7;
    const OF: u64 = 1 << 11;

    match cc >> 1 {
        0 => emit_flag(OF, code),
        1 => emit_flag(CF, code),
        2 => emit_flag(ZF, code),
        3 => {
            emit_flag(CF, code);
            emit_flag(ZF, code);
            code.push(0x72); // i32.or
        }
        4 => emit_flag(SF, code),
        5 => emit_flag(PF, code),
        6 => {
            emit_flag(SF, code);
            emit_flag(OF, code);
            code.push(0x47); // i32.ne
        }
        _ => {
            emit_flag(ZF, code);
            emit_flag(SF, code);
            emit_flag(OF, code);
            code.push(0x47); // i32.ne
            code.push(0x72); // i32.or
        }
    }
    if cc & 1 != 0 {
        code.push(0x45); // i32.eqz
    }
}

fn emit_flag(mask: u64, code: &mut Vec<u8>) {
    code.push(0x20); // state pointer
    uleb(0, code);
    code.push(0x29); // i64.load RFLAGS
    uleb(3, code);
    uleb(RFLAGS_OFFSET, code);
    code.push(0x42);
    sleb(mask as i64, code);
    code.push(0x83); // i64.and
    code.push(0x42);
    sleb(0, code);
    code.push(0x52); // i64.ne
}

fn emit_register_move(mov: MovInstruction, code: &mut Vec<u8>) {
    // Store an immediate or copy a block-local GPR. MOV r32 zero-extends the low dword.
    match mov.source {
        MovSource::Immediate(value) => {
            code.push(0x42);
            sleb(value as i64, code);
        }
        MovSource::Register { index, zero_extend } => {
            code.push(0x20);
            uleb(gpr_local(index), code);
            if zero_extend {
                code.push(0xa7); // i32.wrap_i64
                code.push(0xad); // i64.extend_i32_u
            }
        }
    }
    code.push(0x21); // local.set destination GPR
    uleb(gpr_local(mov.dst), code);
}

fn emit_xmm_register_move(dst: u8, source: u8, code: &mut Vec<u8>) {
    for lane in 0..2 {
        code.push(0x20);
        uleb(xmm_local(source, lane), code);
        code.push(0x21);
        uleb(xmm_local(dst, lane), code);
    }
}

fn emit_xmm_memory_load(dst: u8, code: &mut Vec<u8>) {
    for lane in 0..2 {
        emit_frame_address(code);
        code.push(0x29); // i64.load one packed-double lane
        uleb(3, code);
        uleb(lane as u32 * 8, code);
        code.push(0x21);
        uleb(xmm_local(dst, lane), code);
    }
}

fn emit_xmm_memory_store(source: u8, code: &mut Vec<u8>) {
    for lane in 0..2 {
        emit_frame_address(code);
        code.push(0x20);
        uleb(xmm_local(source, lane), code);
        code.push(0x37); // i64.store one packed-double lane
        uleb(3, code);
        uleb(lane as u32 * 8, code);
    }
}

fn emit_xmm_fp(dst: u8, source: u8, kind: FpK, code: &mut Vec<u8>) {
    let operation = match kind {
        FpK::Add => 0xa0, // f64.add
        FpK::Sub => 0xa1, // f64.sub
        FpK::Mul => 0xa2, // f64.mul
        FpK::Div => 0xa3, // f64.div
        _ => unreachable!("only packed add and multiply are translated"),
    };
    for lane in 0..2 {
        emit_xmm_fp_guard(xmm_local(dst, lane), code);
        emit_xmm_fp_guard(xmm_local(source, lane), code);
        code.push(0x20);
        uleb(xmm_local(dst, lane), code);
        code.push(0xbf); // f64.reinterpret_i64
        code.push(0x20);
        uleb(xmm_local(source, lane), code);
        code.push(0xbf);
        code.push(operation);
        code.push(0xbd); // i64.reinterpret_f64
        code.push(0x21);
        uleb(xmm_local(dst, lane), code);
    }
}

fn emit_xmm_fp_guard(local: u32, code: &mut Vec<u8>) {
    // Alternate MXCSR rounding modes, DAZ/FTZ, and exceptional operands use the interpreter.
    code.push(0x20);
    uleb(0, code);
    code.push(0x28); // i32.load copied MXCSR
    uleb(2, code);
    uleb(MXCSR_OFFSET, code);
    code.push(0x41);
    sleb(0xe040, code); // DAZ, FTZ, and rounding-control bits
    code.push(0x71); // i32.and
    emit_fallback_if_true(code);

    code.push(0x20);
    uleb(local, code);
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x83); // i64.and
    code.push(0x50); // i64.eqz: zero/subnormal exponent
    emit_fallback_if_true(code);

    code.push(0x20);
    uleb(local, code);
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x83); // i64.and
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x51); // i64.eq: infinity/NaN exponent
    emit_fallback_if_true(code);
}

fn emit_x87_stack(op: X87StackOp, code: &mut Vec<u8>) {
    emit_x87_top_guard(code);
    match op {
        X87StackOp::Load(index) => {
            emit_x87_stack_load(index, code);
            code.push(0x21);
            uleb(X87_VALUE0_LOCAL, code);
            code.push(0x20);
            uleb(X87_TOP_LOCAL, code);
            code.push(0x41);
            sleb(1, code);
            code.push(0x6b); // i32.sub: push decrements TOP modulo eight
            code.push(0x41);
            sleb(7, code);
            code.push(0x71); // i32.and
            code.push(0x21);
            uleb(X87_TOP_LOCAL, code);
            emit_x87_write_top(code);
            emit_x87_stack_store(0, X87_VALUE0_LOCAL, code);
            emit_x87_tag_update(false, code);
        }
        X87StackOp::Exchange(index) => {
            emit_x87_stack_load(0, code);
            code.push(0x21);
            uleb(X87_VALUE0_LOCAL, code);
            emit_x87_stack_load(index, code);
            code.push(0x21);
            uleb(X87_VALUE1_LOCAL, code);
            emit_x87_stack_store(0, X87_VALUE1_LOCAL, code);
            emit_x87_stack_store(index, X87_VALUE0_LOCAL, code);
        }
        X87StackOp::StorePop(index) => {
            emit_x87_stack_load(0, code);
            code.push(0x21);
            uleb(X87_VALUE0_LOCAL, code);
            emit_x87_stack_store(index, X87_VALUE0_LOCAL, code);
            emit_x87_tag_update(true, code);
            code.push(0x20);
            uleb(X87_TOP_LOCAL, code);
            code.push(0x41);
            sleb(1, code);
            code.push(0x6a); // i32.add: pop increments TOP modulo eight
            code.push(0x41);
            sleb(7, code);
            code.push(0x71); // i32.and
            code.push(0x21);
            uleb(X87_TOP_LOCAL, code);
            emit_x87_write_top(code);
        }
        X87StackOp::Arithmetic { index, operation } => {
            emit_x87_stack_load(0, code);
            code.push(0x21);
            uleb(X87_VALUE0_LOCAL, code);
            emit_x87_stack_load(index, code);
            code.push(0x21);
            uleb(X87_VALUE1_LOCAL, code);
            emit_x87_value_guard(X87_VALUE0_LOCAL, code);
            emit_x87_value_guard(X87_VALUE1_LOCAL, code);
            let (lhs, rhs) = if matches!(operation, 5 | 7) {
                (X87_VALUE1_LOCAL, X87_VALUE0_LOCAL)
            } else {
                (X87_VALUE0_LOCAL, X87_VALUE1_LOCAL)
            };
            code.push(0x20);
            uleb(lhs, code);
            code.push(0xbf); // i64.reinterpret_f64
            code.push(0x20);
            uleb(rhs, code);
            code.push(0xbf);
            code.push(match operation {
                0 => 0xa0, // f64.add
                1 => 0xa2, // f64.mul
                4 | 5 => 0xa1, // f64.sub
                6 | 7 => 0xa3, // f64.div
                _ => unreachable!("translation restricts x87 arithmetic selectors"),
            });
            code.push(0xbd); // i64.reinterpret_f64
            code.push(0x21);
            uleb(X87_RESULT_LOCAL, code);
            emit_x87_stack_store(0, X87_RESULT_LOCAL, code);
        }
    }
}

fn emit_x87_top_guard(code: &mut Vec<u8>) {
    emit_x87_field_address(FPU_TOP_OFFSET, code);
    code.push(0x28); // i32.load TOP
    uleb(2, code);
    uleb(0, code);
    code.push(0x22); // local.tee TOP
    uleb(X87_TOP_LOCAL, code);
    code.push(0x41);
    sleb(7, code);
    code.push(0x4b); // i32.gt_u
    emit_fallback_if_true(code);
}

fn emit_x87_field_address(offset: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x41);
    sleb(offset as i64, code);
    code.push(0x6a); // i32.add state pointer and field offset
}

fn emit_x87_stack_address(index: u8, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x41);
    sleb(FPU_ST_OFFSET as i64, code);
    code.push(0x20);
    uleb(X87_TOP_LOCAL, code);
    if index != 0 {
        code.push(0x41);
        sleb(index as i64, code);
        code.push(0x6a); // i32.add logical ST index
    }
    code.push(0x41);
    sleb(7, code);
    code.push(0x71); // i32.and, physical register = (TOP + index) & 7
    code.push(0x41);
    sleb(3, code);
    code.push(0x74); // i32.shl by three for the u64 slot offset
    code.push(0x6a); // add physical slot offset to the field offset
    code.push(0x6a); // add state pointer
}

fn emit_x87_stack_load(index: u8, code: &mut Vec<u8>) {
    emit_x87_stack_address(index, code);
    code.push(0x29); // i64.load x87 slot bits
    uleb(3, code);
    uleb(0, code);
}

fn emit_x87_stack_store(index: u8, value: u32, code: &mut Vec<u8>) {
    emit_x87_stack_address(index, code);
    code.push(0x20);
    uleb(value, code);
    code.push(0x37); // i64.store x87 slot bits
    uleb(3, code);
    uleb(0, code);
}

fn emit_x87_write_top(code: &mut Vec<u8>) {
    emit_x87_field_address(FPU_TOP_OFFSET, code);
    code.push(0x20);
    uleb(X87_TOP_LOCAL, code);
    code.push(0x36); // i32.store TOP
    uleb(2, code);
    uleb(0, code);
}

fn emit_x87_tag_update(set_empty: bool, code: &mut Vec<u8>) {
    emit_x87_field_address(FPU_TW_OFFSET, code);
    code.push(0x28); // i32.load tag word
    uleb(2, code);
    uleb(0, code);
    code.push(0x41);
    sleb(3, code);
    code.push(0x20);
    uleb(X87_TOP_LOCAL, code);
    code.push(0x41);
    sleb(1, code);
    code.push(0x74); // i32.shl TOP by one for the two-bit tag slot
    code.push(0x74); // i32.shl tag mask 3 by 2*TOP
    if set_empty {
        code.push(0x72); // i32.or: mark popped slot empty
    } else {
        code.push(0x41);
        sleb(-1, code);
        code.push(0x73); // i32.xor: invert mask
        code.push(0x71); // i32.and: mark pushed slot valid
    }
    code.push(0x21);
    uleb(X87_TAG_LOCAL, code);
    emit_x87_field_address(FPU_TW_OFFSET, code);
    code.push(0x20);
    uleb(X87_TAG_LOCAL, code);
    code.push(0x36); // i32.store tag word
    uleb(2, code);
    uleb(0, code);
}

fn emit_x87_value_guard(local: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(local, code);
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x83); // i64.and exponent bits
    code.push(0x50); // i64.eqz: zero or subnormal
    emit_fallback_if_true(code);

    code.push(0x20);
    uleb(local, code);
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x83);
    code.push(0x42);
    sleb(0x7ff0_0000_0000_0000u64 as i64, code);
    code.push(0x51); // i64.eq: infinity or NaN
    emit_fallback_if_true(code);
}

fn emit_add32(dst: u8, source: AddSource, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(gpr_local(dst), code);
    code.push(0xa7); // i32.wrap_i64
    code.push(0x21);
    uleb(ADD_LHS_LOCAL, code);
    match source {
        AddSource::Immediate(value) => {
            code.push(0x41);
            sleb(value as i32 as i64, code);
        }
        AddSource::Register(index) => {
            code.push(0x20);
            uleb(gpr_local(index), code);
            code.push(0xa7); // i32.wrap_i64
        }
    }
    code.push(0x21);
    uleb(ADD_RHS_LOCAL, code);
    code.push(0x20);
    uleb(ADD_LHS_LOCAL, code);
    code.push(0x20);
    uleb(ADD_RHS_LOCAL, code);
    code.push(0x6a); // i32.add
    code.push(0x21);
    uleb(ADD_RESULT_LOCAL, code);
    code.push(0x20);
    uleb(ADD_RESULT_LOCAL, code);
    code.push(0xad); // i64.extend_i32_u
    code.push(0x21);
    uleb(gpr_local(dst), code);

    code.push(0x41);
    sleb(0, code);
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);

    emit_add_flag(code, crate::cpu::CF as u32, |code| {
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x49); // i32.lt_u
    });
    emit_add_flag(code, crate::cpu::ZF as u32, |code| {
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x45); // i32.eqz
    });
    emit_add_flag(code, crate::cpu::SF as u32, |code| {
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x41);
        sleb(i32::MIN as i64, code);
        code.push(0x71); // i32.and
        code.push(0x45); // i32.eqz
        code.push(0x45); // invert: sign bit was set
    });
    emit_add_flag(code, crate::cpu::OF as u32, |code| {
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ADD_RHS_LOCAL, code);
        code.push(0x73); // i32.xor
        code.push(0x41);
        sleb(-1, code);
        code.push(0x73); // bitwise not of lhs ^ rhs
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x73); // lhs ^ result
        code.push(0x71); // i32.and
        code.push(0x41);
        sleb(i32::MIN as i64, code);
        code.push(0x71); // i32.and sign bit
        code.push(0x45);
        code.push(0x45); // nonzero -> overflow
    });
    emit_add_flag(code, crate::cpu::AF as u32, |code| {
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ADD_RHS_LOCAL, code);
        code.push(0x73);
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x73);
        code.push(0x41);
        sleb(0x10, code);
        code.push(0x71);
        code.push(0x45);
        code.push(0x45); // carry out of bit 3
    });
    emit_add_flag(code, crate::cpu::PF as u32, |code| {
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x41);
        sleb(0xff, code);
        code.push(0x71);
        code.push(0x21);
        uleb(ADD_LHS_LOCAL, code);
        for shift in [4, 2, 1] {
            code.push(0x20);
            uleb(ADD_LHS_LOCAL, code);
            code.push(0x20);
            uleb(ADD_LHS_LOCAL, code);
            code.push(0x41);
            sleb(shift, code);
            code.push(0x76); // i32.shr_u
            code.push(0x73); // i32.xor
            code.push(0x21);
            uleb(ADD_LHS_LOCAL, code);
        }
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x41);
        sleb(1, code);
        code.push(0x71); // low bit is odd parity
        code.push(0x45); // x86 PF is set for even parity
    });

    emit_commit_arithmetic_flags(code);
}

fn emit_add64_gpr(dst: u8, source: AddSource, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(gpr_local(dst), code);
    code.push(0x21);
    uleb(ALU64_LHS_LOCAL, code);
    match source {
        AddSource::Immediate(value) => {
            code.push(0x42);
            sleb(value as i64, code);
        }
        AddSource::Register(index) => {
            code.push(0x20);
            uleb(gpr_local(index), code);
        }
    }
    code.push(0x21);
    uleb(ALU64_RHS_LOCAL, code);
    emit_add64_values(ALU64_LHS_LOCAL, ALU64_RHS_LOCAL, ALU64_RESULT_LOCAL, code);
    code.push(0x20);
    uleb(ALU64_RESULT_LOCAL, code);
    code.push(0x21);
    uleb(gpr_local(dst), code);
}

fn emit_add64_values(lhs: u32, rhs: u32, result: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(lhs, code);
    code.push(0x20);
    uleb(rhs, code);
    code.push(0x7c); // i64.add
    code.push(0x21);
    uleb(result, code);
    code.push(0x41);
    sleb(0, code);
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);

    emit_add_flag(code, crate::cpu::CF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x20);
        uleb(lhs, code);
        code.push(0x54); // i64.lt_u: carry iff result wrapped below lhs
    });
    emit_add_flag(code, crate::cpu::ZF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x50); // i64.eqz
    });
    emit_add_flag(code, crate::cpu::SF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x42);
        sleb(0, code);
        code.push(0x53); // i64.lt_s
    });
    emit_add_flag(code, crate::cpu::OF as u32, |code| {
        code.push(0x20);
        uleb(lhs, code);
        code.push(0x20);
        uleb(rhs, code);
        code.push(0x85); // lhs ^ rhs
        code.push(0x42);
        sleb(-1, code);
        code.push(0x85); // ~(lhs ^ rhs)
        code.push(0x20);
        uleb(lhs, code);
        code.push(0x20);
        uleb(result, code);
        code.push(0x85); // lhs ^ result
        code.push(0x83); // i64.and
        code.push(0x42);
        sleb(i64::MIN, code);
        code.push(0x83);
        code.push(0x50);
        code.push(0x45); // nonzero sign bit means overflow
    });
    emit_add_flag(code, crate::cpu::AF as u32, |code| {
        code.push(0x20);
        uleb(lhs, code);
        code.push(0x20);
        uleb(rhs, code);
        code.push(0x85);
        code.push(0x20);
        uleb(result, code);
        code.push(0x85);
        code.push(0x42);
        sleb(0x10, code);
        code.push(0x83);
        code.push(0x50);
        code.push(0x45); // carry out of bit 3
    });
    emit_parity_flag64(result, code);
    emit_commit_arithmetic_flags(code);
}

fn emit_cmp_gpr(lhs: u8, source: AddSource, width: u8, code: &mut Vec<u8>) {
    debug_assert_eq!(width, 8);
    code.push(0x20);
    uleb(gpr_local(lhs), code);
    code.push(0x21);
    uleb(ALU64_LHS_LOCAL, code);
    match source {
        AddSource::Immediate(value) => {
            code.push(0x42);
            sleb(value as i64, code);
        }
        AddSource::Register(index) => {
            code.push(0x20);
            uleb(gpr_local(index), code);
        }
    }
    code.push(0x21);
    uleb(ALU64_RHS_LOCAL, code);
    code.push(0x20);
    uleb(ALU64_LHS_LOCAL, code);
    code.push(0x20);
    uleb(ALU64_RHS_LOCAL, code);
    code.push(0x7d); // i64.sub
    code.push(0x21);
    uleb(ALU64_RESULT_LOCAL, code);
    code.push(0x41);
    sleb(0, code);
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);

    emit_add_flag(code, crate::cpu::CF as u32, |code| {
        code.push(0x20);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RHS_LOCAL, code);
        code.push(0x54); // unsigned borrow
    });
    emit_add_flag(code, crate::cpu::ZF as u32, |code| {
        code.push(0x20);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x50);
    });
    emit_add_flag(code, crate::cpu::SF as u32, |code| {
        code.push(0x20);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x42);
        sleb(0, code);
        code.push(0x53);
    });
    emit_add_flag(code, crate::cpu::OF as u32, |code| {
        code.push(0x20);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RHS_LOCAL, code);
        code.push(0x85);
        code.push(0x20);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x85);
        code.push(0x83);
        code.push(0x42);
        sleb(i64::MIN, code);
        code.push(0x83);
        code.push(0x50);
        code.push(0x45);
    });
    emit_add_flag(code, crate::cpu::AF as u32, |code| {
        code.push(0x20);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RHS_LOCAL, code);
        code.push(0x85);
        code.push(0x20);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x85);
        code.push(0x42);
        sleb(0x10, code);
        code.push(0x83);
        code.push(0x50);
        code.push(0x45);
    });
    emit_parity_flag64(ALU64_RESULT_LOCAL, code);
    emit_commit_arithmetic_flags(code);
}

fn emit_xor_gpr(dst: u8, source: u8, width: u8, code: &mut Vec<u8>) {
    if width == 4 {
        code.push(0x20);
        uleb(gpr_local(dst), code);
        code.push(0xa7);
        code.push(0x21);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(gpr_local(source), code);
        code.push(0xa7);
        code.push(0x73); // i32.xor
        code.push(0x21);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0x20);
        uleb(ADD_RESULT_LOCAL, code);
        code.push(0xad);
        code.push(0x21);
        uleb(gpr_local(dst), code);
        emit_logic_flags32(ADD_RESULT_LOCAL, code);
    } else {
        code.push(0x20);
        uleb(gpr_local(dst), code);
        code.push(0x21);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(gpr_local(source), code);
        code.push(0x21);
        uleb(ALU64_RHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RHS_LOCAL, code);
        code.push(0x85); // i64.xor
        code.push(0x21);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x20);
        uleb(ALU64_RESULT_LOCAL, code);
        code.push(0x21);
        uleb(gpr_local(dst), code);
        emit_logic_flags64(ALU64_RESULT_LOCAL, code);
    }
}

fn emit_logic_flags32(result: u32, code: &mut Vec<u8>) {
    code.push(0x41);
    sleb(0, code);
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);
    emit_add_flag(code, crate::cpu::ZF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x45);
    });
    emit_add_flag(code, crate::cpu::SF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x41);
        sleb(0, code);
        code.push(0x48); // i32.lt_s
    });
    emit_parity_flag32(result, code);
    emit_commit_arithmetic_flags(code);
}

fn emit_logic_flags64(result: u32, code: &mut Vec<u8>) {
    code.push(0x41);
    sleb(0, code);
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);
    emit_add_flag(code, crate::cpu::ZF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x50);
    });
    emit_add_flag(code, crate::cpu::SF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x42);
        sleb(0, code);
        code.push(0x53);
    });
    emit_parity_flag64(result, code);
    emit_commit_arithmetic_flags(code);
}

fn emit_parity_flag32(result: u32, code: &mut Vec<u8>) {
    emit_add_flag(code, crate::cpu::PF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0x41);
        sleb(0xff, code);
        code.push(0x71);
        code.push(0x21);
        uleb(ADD_LHS_LOCAL, code);
        emit_parity_reduce32(code);
    });
}

fn emit_parity_flag64(result: u32, code: &mut Vec<u8>) {
    emit_add_flag(code, crate::cpu::PF as u32, |code| {
        code.push(0x20);
        uleb(result, code);
        code.push(0xa7); // low byte is enough for parity
        code.push(0x41);
        sleb(0xff, code);
        code.push(0x71);
        code.push(0x21);
        uleb(ADD_LHS_LOCAL, code);
        emit_parity_reduce32(code);
    });
}

fn emit_parity_reduce32(code: &mut Vec<u8>) {
    for shift in [4, 2, 1] {
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x20);
        uleb(ADD_LHS_LOCAL, code);
        code.push(0x41);
        sleb(shift, code);
        code.push(0x76); // i32.shr_u
        code.push(0x73); // i32.xor
        code.push(0x21);
        uleb(ADD_LHS_LOCAL, code);
    }
    code.push(0x20);
    uleb(ADD_LHS_LOCAL, code);
    code.push(0x41);
    sleb(1, code);
    code.push(0x71);
    code.push(0x45); // PF is set for even parity
}

fn emit_commit_arithmetic_flags(code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x20);
    uleb(0, code);
    code.push(0x29); // i64.load old RFLAGS
    uleb(3, code);
    uleb(RFLAGS_OFFSET, code);
    code.push(0x42);
    sleb((!crate::cpu::ARITH_FLAGS) as i64, code);
    code.push(0x83); // preserve non-arithmetic flags
    code.push(0x20);
    uleb(ADD_FLAGS_LOCAL, code);
    code.push(0xad); // i64.extend_i32_u new arithmetic flags
    code.push(0x84); // i64.or
    code.push(0x37); // i64.store updated RFLAGS
    uleb(3, code);
    uleb(RFLAGS_OFFSET, code);
}

fn emit_add_flag(code: &mut Vec<u8>, flag: u32, condition: impl FnOnce(&mut Vec<u8>)) {
    condition(code);
    code.push(0x41);
    sleb(flag.trailing_zeros() as i64, code);
    code.push(0x74); // i32.shl boolean into its RFLAGS position
    code.push(0x20);
    uleb(ADD_FLAGS_LOCAL, code);
    code.push(0x72); // i32.or
    code.push(0x21);
    uleb(ADD_FLAGS_LOCAL, code);
}

fn emit_effective_address(address: MemOp, code: &mut Vec<u8>) {
    code.push(0x42); // i64.const displacement (RIP-relative displacements are pre-resolved)
    sleb(address.disp, code);
    if address.base != 0xff {
        code.push(0x20);
        uleb(gpr_local(address.base), code);
        code.push(0x7c); // i64.add
    }
    if address.index != 0xff {
        code.push(0x20);
        uleb(gpr_local(address.index), code);
        if address.scale != 0 {
            code.push(0x42); // i64.const shift
            sleb(address.scale as i64, code);
            code.push(0x86); // i64.shl
        }
        code.push(0x7c); // i64.add
    }
    code.push(0x21); // local.set effective address
    uleb(3, code);
}

fn emit_tlb_guard(write: bool, size: u8, code: &mut Vec<u8>) {
    // A miss or unsupported boundary/permission condition asks Rust to replay the untouched CPU
    // snapshot through the interpreter, which performs the page-table walk and reports faults.
    code.push(0x20); // effective-address page != cached TLB page
    uleb(3, code);
    code.push(0x42);
    sleb(12, code);
    code.push(0x88); // i64.shr_u
    emit_tlb_pointer(write, code);
    code.push(0x29); // i64.load cached page number
    uleb(3, code);
    uleb(TLB_PAGE_OFFSET, code);
    code.push(0x52); // i64.ne
    emit_fallback_if_true(code);

    emit_tlb_pointer(write, code);
    code.push(0x2d); // i32.load8_u protection bits
    uleb(0, code);
    uleb(TLB_PROT_OFFSET, code);
    code.push(0x41);
    sleb(
        if write {
            (crate::mem::PROT_W | crate::mem::PROT_X) as i64
        } else {
            (crate::mem::PROT_R | crate::mem::PROT_X) as i64
        },
        code,
    );
    code.push(0x71); // i32.and
    if write {
        code.push(0x41);
        sleb(crate::mem::PROT_W as i64, code);
        code.push(0x47); // i32.ne: write allowed and execute permission absent
    } else {
        code.push(0x45); // i32.eqz: read allowed from R or X pages
    }
    emit_fallback_if_true(code);

    // Do not perform a partial direct access at a page boundary.
    code.push(0x20);
    uleb(3, code);
    code.push(0x42);
    sleb(crate::mem::PAGE_MASK as i64, code);
    code.push(0x83); // i64.and
    code.push(0x42);
    sleb((crate::mem::PAGE_SIZE - size as u64) as i64, code);
    code.push(0x56); // i64.gt_u
    emit_fallback_if_true(code);

    // Compute the frame-relative offset and check the backing vector bounds before the access.
    emit_tlb_pointer(write, code);
    code.push(0x29); // i64.load cached frame offset
    uleb(3, code);
    uleb(TLB_FRAME_OFFSET, code);
    code.push(0x20);
    uleb(3, code);
    code.push(0x42);
    sleb(crate::mem::PAGE_MASK as i64, code);
    code.push(0x83); // i64.and
    code.push(0x7c); // i64.add
    code.push(0x22); // local.tee effective address now holds frame-relative offset
    uleb(3, code);
    code.push(0x42);
    sleb(size as i64, code);
    code.push(0x7c); // i64.add
    code.push(0x20);
    uleb(0, code);
    code.push(0x28); // i32.load backing frame length
    uleb(2, code);
    uleb(FRAMES_LEN_OFFSET, code);
    code.push(0xad); // i64.extend_i32_u
    code.push(0x56); // i64.gt_u
    emit_fallback_if_true(code);
    code.push(0x20);
    uleb(3, code);
    code.push(0xa7); // i32.wrap_i64; bounds check proved it is in the frame vector
    code.push(0x21); // local.set frame offset
    uleb(4, code);
}

fn emit_tlb_pointer(write: bool, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x28); // i32.load pointer to the access-specific TLB entry
    uleb(2, code);
    uleb(
        if write {
            TLB_WRITE_PTR_OFFSET
        } else {
            TLB_READ_PTR_OFFSET
        },
        code,
    );
}

fn emit_frame_address(code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x28); // i32.load frame vector base
    uleb(2, code);
    uleb(FRAMES_PTR_OFFSET, code);
    code.push(0x20);
    uleb(4, code);
    code.push(0x6a); // i32.add
}

fn emit_fallback_if_true(code: &mut Vec<u8>) {
    code.extend_from_slice(&[0x04, 0x40, 0x41, 0x7f, 0x0f, 0x0b]); // if { return -1 }
}

fn emit_state_local_load(offset: u32, local: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x29); // i64.load
    uleb(3, code);
    uleb(offset, code);
    code.push(0x21);
    uleb(local, code);
}

fn emit_state_gpr_load(register: u8, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x29); // i64.load initial GPR value
    uleb(3, code);
    uleb(GPR_OFFSET + register as u32 * 8, code);
    code.push(0x21);
    uleb(gpr_local(register), code);
}

fn emit_state_xmm_load(register: u8, code: &mut Vec<u8>) {
    for lane in 0..2 {
        code.push(0x20);
        uleb(0, code);
        code.push(0x29); // i64.load one XMM lane
        uleb(3, code);
        uleb(xmm_offset(register, lane), code);
        code.push(0x21);
        uleb(xmm_local(register, lane), code);
    }
}

fn emit_state_local_store(offset: u32, local: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x20);
    uleb(local, code);
    code.push(0x37); // i64.store committed state field
    uleb(3, code);
    uleb(offset, code);
}

fn emit_flush_state(written_gprs: u16, written_xmms: u16, code: &mut Vec<u8>) {
    for register in 0..16 {
        if written_gprs & (1 << register) != 0 {
            emit_state_local_store(GPR_OFFSET + register as u32 * 8, gpr_local(register), code);
        }
        if written_xmms & (1 << register) != 0 {
            emit_state_local_store(xmm_offset(register, 0), xmm_local(register, 0), code);
            emit_state_local_store(xmm_offset(register, 1), xmm_local(register, 1), code);
        }
    }
    emit_state_local_store(RIP_OFFSET, RIP_LOCAL, code);
    emit_state_local_store(ICOUNT_OFFSET, ICOUNT_LOCAL, code);
    emit_state_local_store(TSC_OFFSET, TSC_LOCAL, code);
}

fn emit_increment_local(local: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(local, code);
    code.push(0x42);
    sleb(1, code);
    code.push(0x7c); // i64.add
    code.push(0x21);
    uleb(local, code);
}

fn register_masks(instructions: &[JitInstruction]) -> (u16, u16, u16, u16) {
    let mut initialized = 0u16;
    let mut initial_reads = 0u16;
    let mut written = 0u16;
    let mut initialized_xmm = 0u16;
    let mut initial_xmm_reads = 0u16;
    let mut written_xmm = 0u16;
    for instruction in instructions {
        match instruction {
            JitInstruction::Nop { .. }
            | JitInstruction::Jump { .. }
            | JitInstruction::Branch { .. } => {}
            JitInstruction::JumpRegister { register, .. } => {
                require_initial_register(*register, initialized, &mut initial_reads);
            }
            JitInstruction::X87Stack { .. } => {}
            JitInstruction::Mov(mov) => {
                if let MovSource::Register { index, .. } = mov.source {
                    require_initial_register(index, initialized, &mut initial_reads);
                }
                initialized |= 1 << mov.dst;
                written |= 1 << mov.dst;
            }
            JitInstruction::Add { dst, source, .. } => {
                require_initial_register(*dst, initialized, &mut initial_reads);
                if let AddSource::Register(index) = source {
                    require_initial_register(*index, initialized, &mut initial_reads);
                }
                initialized |= 1 << *dst;
                written |= 1 << *dst;
            }
            JitInstruction::Xor { dst, source, .. } => {
                require_initial_register(*dst, initialized, &mut initial_reads);
                require_initial_register(*source, initialized, &mut initial_reads);
                initialized |= 1 << *dst;
                written |= 1 << *dst;
            }
            JitInstruction::Cmp { lhs, source, .. } => {
                require_initial_register(*lhs, initialized, &mut initial_reads);
                if let AddSource::Register(index) = source {
                    require_initial_register(*index, initialized, &mut initial_reads);
                }
            }
            JitInstruction::Lea { dst, address, .. } => {
                address_initial_registers(*address, initialized, &mut initial_reads);
                initialized |= 1 << *dst;
                written |= 1 << *dst;
            }
            JitInstruction::MemoryAdd {
                address, source, ..
            } => {
                address_initial_registers(*address, initialized, &mut initial_reads);
                require_initial_register(*source, initialized, &mut initial_reads);
            }
            JitInstruction::Load { dst, address, .. } => {
                address_initial_registers(*address, initialized, &mut initial_reads);
                initialized |= 1 << dst;
                written |= 1 << dst;
            }
            JitInstruction::Store(store) => {
                address_initial_registers(store.address, initialized, &mut initial_reads);
                if let StoreSource::Register(index) = store.source {
                    require_initial_register(index, initialized, &mut initial_reads);
                }
            }
            JitInstruction::XmmMove { dst, source, .. } => {
                match source {
                    XmmSource::Register(index) => {
                        require_initial_xmm(*index, initialized_xmm, &mut initial_xmm_reads)
                    }
                    XmmSource::Memory(address) => {
                        address_initial_registers(*address, initialized, &mut initial_reads)
                    }
                }
                initialized_xmm |= 1 << dst;
                written_xmm |= 1 << dst;
            }
            JitInstruction::XmmStore {
                address, source, ..
            } => {
                address_initial_registers(*address, initialized, &mut initial_reads);
                require_initial_xmm(*source, initialized_xmm, &mut initial_xmm_reads);
            }
            JitInstruction::XmmFp { dst, source, .. } => {
                require_initial_xmm(*dst, initialized_xmm, &mut initial_xmm_reads);
                require_initial_xmm(*source, initialized_xmm, &mut initial_xmm_reads);
                initialized_xmm |= 1 << *dst;
                written_xmm |= 1 << *dst;
            }
        }
    }
    (initial_reads, written, initial_xmm_reads, written_xmm)
}

fn address_initial_registers(address: MemOp, initialized: u16, initial_reads: &mut u16) {
    if address.base != 0xff {
        require_initial_register(address.base, initialized, initial_reads);
    }
    if address.index != 0xff {
        require_initial_register(address.index, initialized, initial_reads);
    }
}

fn require_initial_register(register: u8, initialized: u16, initial_reads: &mut u16) {
    if initialized & (1 << register) == 0 {
        *initial_reads |= 1 << register;
    }
}

fn require_initial_xmm(register: u8, initialized: u16, initial_reads: &mut u16) {
    if initialized & (1 << register) == 0 {
        *initial_reads |= 1 << register;
    }
}

fn gpr_local(register: u8) -> u32 {
    GPR_LOCAL_BASE + register as u32
}

fn xmm_local(register: u8, lane: u8) -> u32 {
    XMM_LOCAL_BASE + register as u32 * 2 + lane as u32
}

fn xmm_offset(register: u8, lane: u8) -> u32 {
    XMM_OFFSET + register as u32 * 16 + lane as u32 * 8
}

fn section(id: u8, payload: &[u8], module: &mut Vec<u8>) {
    module.push(id);
    uleb(payload.len() as u32, module);
    module.extend_from_slice(payload);
}

fn uleb(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn sleb(mut value: i64, out: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    #[link_name = "run_emitted_block"]
    fn host_run_emitted_block(
        id: u32,
        wasm_ptr: *const u8,
        wasm_len: u32,
        state: *mut JitState,
        budget: u32,
        emitted_instructions: u32,
        first_register_register: u32,
        start: u64,
        code_epoch: u64,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuMode;
    use crate::decode::decode;

    #[test]
    fn jit_state_round_trips_every_architectural_cpu_field() {
        let mut cpu = Cpu::new();
        cpu.mode = CpuMode::X86_32;
        cpu.gpr = std::array::from_fn(|index| 0x1111_0000_0000_0000 | index as u64);
        cpu.rip = 0x1234_5678_9abc_def0;
        cpu.rflags = 0x0ed7;
        cpu.fs_base = 0x1111_2222_3333_4444;
        cpu.gs_base = 0x5555_6666_7777_8888;
        cpu.xmm = std::array::from_fn(|index| (index as u128) << 96 | 0x1234);
        cpu.ymm_hi = std::array::from_fn(|index| (index as u128) << 80 | 0x5678);
        cpu.mxcsr = 0x9fc0;
        cpu.fpu_st = std::array::from_fn(|index| (index as f64 + 0.25) * -3.5);
        cpu.fpu_top = 6;
        cpu.fpu_cw = 0x0b7f;
        cpu.fpu_sw = 0x4d20;
        cpu.fpu_tw = 0x1357;
        cpu.tsc = 0x0102_0304_0506_0708;
        cpu.icount = 0x1112_1314_1516_1718;

        let state = JitState::from(&cpu);
        assert_eq!(state.cpu_mode, 1);
        assert_eq!(state.fs_base, cpu.fs_base);
        assert_eq!(state.gs_base, cpu.gs_base);
        assert_eq!(state.ymm_hi, cpu.ymm_hi);
        assert_eq!(state.fpu_st, cpu.fpu_st.map(f64::to_bits));
        assert_eq!(state.fpu_top, cpu.fpu_top as u32);
        assert_eq!(state.fpu_cw, cpu.fpu_cw as u32);
        assert_eq!(state.fpu_sw, cpu.fpu_sw as u32);
        assert_eq!(state.fpu_tw, cpu.fpu_tw as u32);

        let mut restored = Cpu::new();
        assert!(state.copy_to(&mut restored));
        assert_eq!(restored.mode, cpu.mode);
        assert_eq!(restored.gpr, cpu.gpr);
        assert_eq!(restored.rip, cpu.rip);
        assert_eq!(restored.rflags, cpu.rflags);
        assert_eq!(restored.fs_base, cpu.fs_base);
        assert_eq!(restored.gs_base, cpu.gs_base);
        assert_eq!(restored.xmm, cpu.xmm);
        assert_eq!(restored.ymm_hi, cpu.ymm_hi);
        assert_eq!(restored.mxcsr, cpu.mxcsr);
        assert_eq!(
            restored.fpu_st.map(f64::to_bits),
            cpu.fpu_st.map(f64::to_bits)
        );
        assert_eq!(restored.fpu_top, cpu.fpu_top);
        assert_eq!(restored.fpu_cw, cpu.fpu_cw);
        assert_eq!(restored.fpu_sw, cpu.fpu_sw);
        assert_eq!(restored.fpu_tw, cpu.fpu_tw);
        assert_eq!(restored.tsc, cpu.tsc);
        assert_eq!(restored.icount, cpu.icount);
    }

    #[test]
    fn rejects_an_invalid_snapshotted_cpu_mode_without_partial_commit() {
        let mut state = JitState::from(&Cpu::new());
        state.cpu_mode = 2;
        let mut cpu = Cpu::new();
        cpu.gpr[0] = 0xfeed_face;
        cpu.rip = 0x1234;

        assert!(!state.copy_to(&mut cpu));
        assert_eq!(cpu.gpr[0], 0xfeed_face);
        assert_eq!(cpu.rip, 0x1234);
    }

    #[test]
    fn emits_supported_mov_and_nop_prefix_then_stops_at_partial_width_mov() {
        let mov_rax = decode(&[0x48, 0xb8, 1, 0, 0, 0, 0, 0, 0, 0], 0x1000).unwrap();
        let nop = decode(&[0x90], 0x100a).unwrap();
        let mov_ax = decode(&[0x66, 0xb8, 1, 0], 0x100b).unwrap();
        let emitted = emit_mov_block(7, 0x1000, &[mov_rax, nop, mov_ax], 1).unwrap();
        assert_eq!(emitted.instruction_count(), 2);
        assert!(emitted.wasm.starts_with(b"\0asm\x01\0\0\0"));
        assert!(emitted.wasm.windows(7).any(|window| window == b"anywasm"));
    }

    #[test]
    fn emits_a_leading_nop_and_continues_through_following_movs() {
        let nop = decode(&[0x90], 0x1800).unwrap();
        let mov_rax_rax = decode(&[0x48, 0x89, 0xc0], 0x1801).unwrap();
        let emitted = emit_mov_block(70, 0x1800, &[nop, mov_rax_rax], 1).unwrap();

        assert_eq!(emitted.instruction_count(), 2);
        assert_eq!(emitted.first_register_register, Some(1));
    }

    #[test]
    fn loads_a_reused_source_register_once_per_emitted_block() {
        let mov_rax_rbx = decode(&[0x48, 0x89, 0xd8], 0x1810).unwrap();
        let mov_rcx_rbx = decode(&[0x48, 0x89, 0xd9], 0x1813).unwrap();
        let mov_rdx_rbx = decode(&[0x48, 0x89, 0xda], 0x1816).unwrap();
        let emitted =
            emit_mov_block(72, 0x1810, &[mov_rax_rbx, mov_rcx_rbx, mov_rdx_rbx], 1).unwrap();

        // RBX is JitState byte offset 24. Each instruction currently emits its own i64.load;
        // block-local register caching should leave one entry load for all three MOVs.
        assert_eq!(
            emitted
                .wasm
                .windows(3)
                .filter(|window| *window == [0x29, 0x03, 24])
                .count(),
            1
        );
    }

    #[test]
    fn flushes_a_written_register_once_at_the_block_exit() {
        let mov_rax_rbx = decode(&[0x48, 0x89, 0xd8], 0x1820).unwrap();
        let emitted = emit_mov_block(73, 0x1820, &[mov_rax_rbx], 1).unwrap();

        assert_eq!(
            emitted
                .wasm
                .windows(3)
                .filter(|window| *window == [0x37, 0x03, 0])
                .count(),
            1
        );
    }

    #[test]
    fn emits_packed_double_multiply_and_add_instructions() {
        let mulpd = decode(&[0x66, 0x0f, 0x59, 0xc1], 0x1830).unwrap();
        let addpd = decode(&[0x66, 0x0f, 0x58, 0xc1], 0x1834).unwrap();
        let emitted = emit_mov_block(74, 0x1830, &[mulpd, addpd], 1).unwrap();

        assert_eq!(emitted.instruction_count(), 2);
        assert!(emitted.wasm.contains(&0xa2), "missing Wasm f64.mul");
        assert!(emitted.wasm.contains(&0xa0), "missing Wasm f64.add");
    }

    #[test]
    fn emits_a_32_bit_add_before_a_conditional_loop_exit() {
        let add_eax_minus_eight = decode(&[0x83, 0xc0, 0xf8], 0x1840).unwrap();
        let jne = decode(&[0x75, 0xfb], 0x1843).unwrap();
        assert_eq!(jne.imm as u64, 0x1840);

        let emitted = emit_mov_block(75, 0x1840, &[add_eax_minus_eight, jne], 2).unwrap();
        assert_eq!(emitted.instruction_count(), 2);
        assert!(emitted.wasm.contains(&0x6a), "missing Wasm i32.add");
    }

    #[test]
    fn emits_64_bit_loop_arithmetic_and_compare_before_jcc() {
        let add_rdx_two = decode(&[0x48, 0x83, 0xc2, 0x02], 0x1860).unwrap();
        let cmp_rdx_4096 = decode(&[0x48, 0x81, 0xfa, 0x00, 0x10, 0x00, 0x00], 0x1864).unwrap();
        let jne = decode(&[0x75, 0xf5], 0x186b).unwrap();
        let emitted = emit_mov_block(76, 0x1860, &[add_rdx_two, cmp_rdx_4096, jne], 3).unwrap();

        assert_eq!(emitted.instruction_count(), 3);
        assert!(emitted.wasm.contains(&0x7c), "missing Wasm i64.add");
        assert!(emitted.wasm.contains(&0x7d), "missing Wasm i64.sub");
    }

    #[test]
    fn emits_register_xor_and_memory_add_as_a_restartable_store_prefix() {
        let mov_r8_rdx = decode(&[0x49, 0x89, 0xd0], 0x1870).unwrap();
        let xor_r8_rax = decode(&[0x49, 0x31, 0xc0], 0x1873).unwrap();
        let add_mem_r8 = decode(&[0x4c, 0x01, 0x04, 0xd1], 0x1876).unwrap();
        let lea_r8 = decode(&[0x4c, 0x8d, 0x42, 0x01], 0x187a).unwrap();
        let emitted =
            emit_mov_block(77, 0x1870, &[mov_r8_rdx, xor_r8_rax, add_mem_r8, lea_r8], 3).unwrap();

        assert_eq!(emitted.instruction_count(), 3);
        assert!(emitted.ends_with_store);
        assert!(emitted.wasm.contains(&0x85), "missing Wasm i64.xor");
        assert!(emitted.wasm.contains(&0x29), "missing guarded i64.load");
        assert!(emitted.wasm.contains(&0x37), "missing guarded i64.store");
    }

    #[test]
    fn initializes_the_old_destination_for_a_leading_add() {
        let add_eax_minus_eight = decode(&[0x83, 0xc0, 0xf8], 0x1850).unwrap();
        let add = translate_instruction(add_eax_minus_eight).unwrap();
        let (initial_reads, written, _, _) = register_masks(&[add]);

        assert_eq!(initial_reads & 1, 1, "ADD reads the incoming EAX value");
        assert_eq!(written & 1, 1, "ADD writes EAX");
    }

    #[test]
    fn emits_tlb_backed_memory_moves_and_stops_after_a_store() {
        let load_rax = decode(&[0x48, 0x8b, 0x03], 0x1900).unwrap(); // mov rax, [rbx]
        let mov_rcx = decode(&[0x48, 0x89, 0xc1], 0x1903).unwrap(); // mov rcx, rax
        let store_rax = decode(&[0x48, 0x89, 0x03], 0x1906).unwrap(); // mov [rbx], rax
        let mov_rdx = decode(&[0x48, 0x89, 0xc2], 0x1909).unwrap(); // must remain interpreted
        let emitted =
            emit_mov_block(71, 0x1900, &[load_rax, mov_rcx, store_rax, mov_rdx], 1).unwrap();

        assert_eq!(emitted.instruction_count(), 3);
        assert_eq!(emitted.first_register_register, Some(1));
    }

    #[test]
    fn emits_a_direct_conditional_exit_as_the_end_of_a_block() {
        let nop = decode(&[0x90], 0x7000).unwrap();
        let mov = decode(&[0x4d, 0x89, 0xff], 0x7001).unwrap(); // mov r15, r15
        let je = decode(&[0x74, 0x02], 0x7004).unwrap(); // je 0x7008
        let trailing = decode(&[0x90], 0x7006).unwrap();
        let emitted = emit_mov_block(72, 0x7000, &[nop, mov, je, trailing], 3).unwrap();

        assert_eq!(emitted.instruction_count(), 3);
        assert_eq!(emitted.first_register_register, Some(1));
    }

    #[test]
    fn emits_a_register_indirect_jmp_exit() {
        let indirect_jmp_rax = decode(&[0xff, 0xe0], 0x7100).unwrap();
        assert_eq!(indirect_jmp_rax.op, Op::JmpInd);

        let emitted = emit_mov_block(73, 0x7100, &[indirect_jmp_rax], 1)
            .expect("register-indirect JMP should be emitted as a block exit");
        assert_eq!(emitted.instruction_count(), 1);
    }

    #[test]
    fn emits_packed_double_subtract_and_divide() {
        let subpd = decode(&[0x66, 0x0f, 0x5c, 0xc1], 0x7200).unwrap();
        let divpd = decode(&[0x66, 0x0f, 0x5e, 0xc1], 0x7204).unwrap();

        let emitted = emit_mov_block(74, 0x7200, &[subpd, divpd], 1)
            .expect("normal packed-double subtract and divide should be emitted");
        assert_eq!(emitted.instruction_count(), 2);
    }

    #[test]
    fn packed_double_memory_arithmetic_stays_on_the_interpreter_path() {
        let addpd_memory = decode(&[0x66, 0x0f, 0x58, 0x00], 0x7208).unwrap();
        assert!(emit_mov_block(77, 0x7208, &[addpd_memory], 1).is_none());
    }

    #[test]
    fn emits_supported_x87_register_stack_operations() {
        let fld_st1 = decode(&[0xd9, 0xc1], 0x7300).unwrap();
        let fxch_st1 = decode(&[0xd9, 0xc9], 0x7302).unwrap();
        let fstp_st1 = decode(&[0xd9, 0xd9], 0x7304).unwrap();
        let fadd_st0_st1 = decode(&[0xd8, 0xc1], 0x7306).unwrap();

        for (id, (address, instruction)) in [
            (0x7300, fld_st1),
            (0x7302, fxch_st1),
            (0x7304, fstp_st1),
            (0x7306, fadd_st0_st1),
        ]
        .into_iter()
        .enumerate()
        {
            let emitted = emit_mov_block(id as u32 + 75, address, &[instruction], 1)
                .expect("selected x87 register-stack instruction should be emitted");
            assert_eq!(emitted.instruction_count(), 1);
        }
    }

    #[test]
    fn emits_zero_extended_mov_r32_and_full_width_mov_r64_values() {
        let mov_eax = decode(&[0xb8, 0xff, 0xff, 0xff, 0xff], 0x2000).unwrap();
        let mov_rbx = decode(
            &[0x48, 0xbb, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80],
            0x2005,
        )
        .unwrap();
        let emitted = emit_mov_block(8, 0x2000, &[mov_eax, mov_rbx], 1).unwrap();
        assert_eq!(emitted.instruction_count(), 2);
        assert!(emitted
            .wasm
            .windows(6)
            .any(|window| window == [0x42, 0xff, 0xff, 0xff, 0xff, 0x0f]));
        assert!(emitted.wasm.windows(2).any(|window| window == [0x42, 0x80]));
    }

    #[test]
    fn does_not_emit_partial_width_mov() {
        let mov_ax_imm = decode(&[0x66, 0xb8, 1, 0], 0x3000).unwrap();
        assert!(emit_mov_block(10, 0x3000, &[mov_ax_imm], 1).is_none());
    }

    #[test]
    fn emits_32_bit_zero_extending_and_64_bit_register_moves() {
        let mov_eax_ebx = decode(&[0x89, 0xd8], 0x4000).unwrap();
        let mov_rax_rcx = decode(&[0x48, 0x89, 0xc8], 0x4002).unwrap();
        let emitted = emit_mov_block(11, 0x4000, &[mov_eax_ebx, mov_rax_rcx], 1).unwrap();

        assert_eq!(emitted.instruction_count(), 2);
        assert_eq!(emitted.first_register_register, Some(0));
        assert!(emitted
            .wasm
            .windows(7)
            .any(|w| w == [0x20, 0x00, 0x29, 0x03, 24, 0x21, (GPR_LOCAL_BASE + 3) as u8]));
        assert!(emitted.wasm.windows(6).any(|w| w
            == [
                0x20,
                (GPR_LOCAL_BASE + 3) as u8,
                0xa7,
                0xad,
                0x21,
                GPR_LOCAL_BASE as u8
            ]));
        assert!(emitted
            .wasm
            .windows(4)
            .any(|w| w == [0x20, (GPR_LOCAL_BASE + 1) as u8, 0x21, GPR_LOCAL_BASE as u8]));
        assert!(emitted
            .wasm
            .windows(7)
            .any(|w| w == [0x20, 0, 0x20, GPR_LOCAL_BASE as u8, 0x37, 0x03, 0]));
    }

    #[test]
    fn emits_self_moves_as_retired_register_writes() {
        let mov_rax_rax = decode(&[0x48, 0x89, 0xc0], 0x5000).unwrap();
        let emitted = emit_mov_block(12, 0x5000, &[mov_rax_rax], 1).unwrap();

        assert_eq!(emitted.instruction_count(), 1);
        assert_eq!(emitted.first_register_register, Some(0));
        assert!(emitted.wasm.windows(3).any(|w| w == [0x29, 0x03, 0])); // read RAX
        assert!(emitted.wasm.windows(3).any(|w| w == [0x37, 0x03, 0])); // write RAX
        assert!(emitted
            .wasm
            .windows(4)
            .any(|w| w == [0x37, 0x03, 0x80, 0x01])); // advance RIP
        assert!(emitted
            .wasm
            .windows(7)
            .any(|w| w == [0x20, 0x02, 0x41, 1, 0x6a, 0x21, 0x02]));
        assert!(emitted
            .wasm
            .windows(4)
            .any(|w| w == [0x37, 0x03, 0x90, 0x01])); // icount
        assert!(emitted
            .wasm
            .windows(4)
            .any(|w| w == [0x37, 0x03, 0x98, 0x01])); // TSC
    }

    #[test]
    fn prefix_gate_skips_short_blocks_before_module_generation() {
        let mov_rax = decode(&[0x48, 0xb8, 1, 0, 0, 0, 0, 0, 0, 0], 0x6000).unwrap();
        let mov_rbx = decode(&[0x48, 0xbb, 2, 0, 0, 0, 0, 0, 0, 0], 0x600a).unwrap();
        assert!(emit_mov_block(13, 0x6000, &[mov_rax, mov_rbx], 3).is_none());
        assert_eq!(
            emit_mov_block(14, 0x6000, &[mov_rax, mov_rbx], 2)
                .unwrap()
                .instruction_count(),
            2
        );
    }
}
