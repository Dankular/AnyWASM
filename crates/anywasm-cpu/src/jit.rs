//! Small WebAssembly block emitter used by the wasm32 runtime.
//!
//! The generated module imports the runtime's memory and receives a pointer to a copied CPU
//! snapshot. This keeps a trapped or rejected module from partially changing the live CPU.

use crate::cpu::Cpu;
use crate::decode::{Inst, MemOp, Op, Opnd};
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
const CHAIN_HANDOFF_OFFSET: u32 = 176;
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
    chain_handoff: u32,
    _reserved: u32,
}

const _: [(); 184] = [(); std::mem::size_of::<JitState>()];
const _: [(); GPR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, gpr)];
const _: [(); RIP_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rip)];
const _: [(); RFLAGS_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rflags)];
const _: [(); ICOUNT_OFFSET as usize] = [(); std::mem::offset_of!(JitState, icount)];
const _: [(); TSC_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tsc)];
const _: [(); FRAMES_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, frames)];
const _: [(); FRAMES_LEN_OFFSET as usize] = [(); std::mem::offset_of!(JitState, frames_len)];
const _: [(); TLB_READ_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tlb_read)];
const _: [(); TLB_WRITE_PTR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tlb_write)];
const _: [(); CHAIN_HANDOFF_OFFSET as usize] = [(); std::mem::offset_of!(JitState, chain_handoff)];

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
            chain_handoff: 0,
            _reserved: 0,
        }
    }
}

impl JitState {
    #[cfg(target_arch = "wasm32")]
    fn copy_to(self, cpu: &mut Cpu) {
        cpu.gpr = self.gpr;
        cpu.rip = self.rip;
        cpu.rflags = self.rflags;
        cpu.icount = self.icount;
        cpu.tsc = self.tsc;
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
    Jump {
        target: u64,
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
                self.start,
                mem.code_epoch,
                &mut state,
                budget,
                self.instructions as u32,
                self.first_register_register.unwrap_or(u32::MAX),
            )
        };
        let expected = (budget as usize).min(self.instructions);
        if executed <= 0 || (executed as usize) < expected || executed as u64 > budget {
            return None;
        }
        let chain_handoff = state.chain_handoff != 0;
        state.copy_to(cpu);
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
    for inst in instructions.iter().copied() {
        let Some(instruction) = translate_instruction(inst) else {
            break;
        };
        let is_store = matches!(instruction, JitInstruction::Store(_));
        let is_exit = matches!(
            instruction,
            JitInstruction::Jump { .. } | JitInstruction::Branch { .. }
        );
        emitted.push(instruction);
        // A direct store is observable before Rust commits the copied CPU state. Keep it last so
        // no later TLB miss can discard the CPU snapshot and replay this memory side effect.
        if is_store || is_exit {
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
    })
}

fn translate_instruction(inst: Inst) -> Option<JitInstruction> {
    if inst.lock || inst.len == 0 {
        return None;
    }
    if inst.op == Op::Nop {
        return Some(JitInstruction::Nop { len: inst.len });
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
    // Locals: executed count (i32), effective address (i64), guest-frame offset (i32).
    code.extend_from_slice(&[0x03, 0x01, 0x7f, 0x01, 0x7e, 0x01, 0x7f]);
    let mut rip = start;
    for instruction in instructions {
        let len = match instruction {
            JitInstruction::Nop { len } => *len,
            JitInstruction::Mov(mov) => mov.len,
            JitInstruction::Load { len, .. } => *len,
            JitInstruction::Store(store) => store.len,
            JitInstruction::Jump { len, .. } | JitInstruction::Branch { len, .. } => *len,
        };
        rip = rip.wrapping_add(len as u64);
        let mut rip_after = rip;
        // if (executed >= budget) return executed;
        code.extend_from_slice(&[
            0x20, 0x02, 0x20, 0x01, 0x4f, 0x04, 0x40, 0x20, 0x02, 0x0f, 0x0b,
        ]);

        match instruction {
            JitInstruction::Nop { .. } => {}
            JitInstruction::Mov(mov) => emit_register_move(*mov, &mut code),
            JitInstruction::Load {
                dst, address, size, ..
            } => {
                emit_effective_address(*address, &mut code);
                emit_tlb_guard(false, *size, &mut code);
                code.push(0x20); // destination address in the JitState GPR array
                uleb(0, &mut code);
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
                code.push(0x37); // i64.store GPR
                uleb(3, &mut code);
                uleb(GPR_OFFSET + *dst as u32 * 8, &mut code);
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
                        uleb(0, &mut code);
                        code.push(0x29); // i64.load source GPR
                        uleb(3, &mut code);
                        uleb(GPR_OFFSET + index as u32 * 8, &mut code);
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
            JitInstruction::Jump { target, .. } => rip_after = *target,
            JitInstruction::Branch { cc, target, .. } => {
                emit_condition(*cc, &mut code);
                code.extend_from_slice(&[0x04, 0x7e]); // if (result i64)
                code.push(0x42);
                sleb(*target as i64, &mut code);
                code.push(0x05); // else
                code.push(0x42);
                sleb(rip as i64, &mut code);
                code.push(0x0b); // end
                code.push(0x21); // local.set effective-address scratch (no longer needed)
                uleb(3, &mut code);
            }
        }

        // RIP points at the next guest instruction after each emitted instruction.
        code.push(0x20);
        uleb(0, &mut code);
        if matches!(instruction, JitInstruction::Branch { .. }) {
            code.push(0x20);
            uleb(3, &mut code);
        } else {
            code.push(0x42);
            sleb(rip_after as i64, &mut code);
        }
        code.push(0x37);
        uleb(3, &mut code);
        uleb(RIP_OFFSET, &mut code);

        emit_increment_i64(ICOUNT_OFFSET, &mut code);
        emit_increment_i64(TSC_OFFSET, &mut code);

        // executed += 1
        code.extend_from_slice(&[0x20, 0x02, 0x41, 0x01, 0x6a, 0x21, 0x02]);
    }
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
    // Store an immediate or load the source GPR. MOV r32 zero-extends the low dword.
    code.push(0x20);
    uleb(0, code);
    match mov.source {
        MovSource::Immediate(value) => {
            code.push(0x42);
            sleb(value as i64, code);
        }
        MovSource::Register { index, zero_extend } => {
            code.push(0x20);
            uleb(0, code);
            code.push(0x29); // i64.load source GPR
            uleb(3, code);
            uleb(GPR_OFFSET + index as u32 * 8, code);
            if zero_extend {
                code.push(0xa7); // i32.wrap_i64
                code.push(0xad); // i64.extend_i32_u
            }
        }
    }
    code.push(0x37); // i64.store
    uleb(3, code);
    uleb(GPR_OFFSET + mov.dst as u32 * 8, code);
}

fn emit_effective_address(address: MemOp, code: &mut Vec<u8>) {
    code.push(0x42); // i64.const displacement (RIP-relative displacements are pre-resolved)
    sleb(address.disp, code);
    if address.base != 0xff {
        code.push(0x20);
        uleb(0, code);
        code.push(0x29); // i64.load base GPR
        uleb(3, code);
        uleb(GPR_OFFSET + address.base as u32 * 8, code);
        code.push(0x7c); // i64.add
    }
    if address.index != 0xff {
        code.push(0x20);
        uleb(0, code);
        code.push(0x29); // i64.load index GPR
        uleb(3, code);
        uleb(GPR_OFFSET + address.index as u32 * 8, code);
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

fn emit_increment_i64(offset: u32, code: &mut Vec<u8>) {
    code.push(0x20);
    uleb(0, code);
    code.push(0x20);
    uleb(0, code);
    code.push(0x29); // i64.load
    uleb(3, code);
    uleb(offset, code);
    code.push(0x42);
    sleb(1, code);
    code.push(0x7c); // i64.add
    code.push(0x37); // i64.store
    uleb(3, code);
    uleb(offset, code);
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
        start: u64,
        code_epoch: u64,
        state: *mut JitState,
        budget: u32,
        emitted_instructions: u32,
        first_register_register: u32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

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
        assert!(emitted.wasm.windows(3).any(|w| w == [0x29, 0x03, 24])); // load RBX
        assert!(emitted
            .wasm
            .windows(6)
            .any(|w| w == [0x29, 0x03, 24, 0xa7, 0xad, 0x37]));
        assert!(emitted
            .wasm
            .windows(6)
            .any(|w| w == [0x29, 0x03, 8, 0x37, 0x03, 0]));
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
