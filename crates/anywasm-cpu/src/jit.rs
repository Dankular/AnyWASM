//! Small WebAssembly block emitter used by the wasm32 runtime.
//!
//! The generated module imports the runtime's memory and receives a pointer to a copied CPU
//! snapshot. This keeps a trapped or rejected module from partially changing the live CPU.

use crate::cpu::Cpu;
use crate::decode::{Inst, Op, Opnd};

const GPR_OFFSET: u32 = 0;
const RIP_OFFSET: u32 = 128;
const RFLAGS_OFFSET: u32 = 136;
const ICOUNT_OFFSET: u32 = 144;
const TSC_OFFSET: u32 = 152;

/// ABI shared by the generated module and the host trampoline. Keep fields in this order.
#[repr(C)]
#[derive(Clone, Copy)]
struct JitState {
    gpr: [u64; 16],
    rip: u64,
    rflags: u64,
    icount: u64,
    tsc: u64,
}

const _: [(); 160] = [(); std::mem::size_of::<JitState>()];
const _: [(); GPR_OFFSET as usize] = [(); std::mem::offset_of!(JitState, gpr)];
const _: [(); RIP_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rip)];
const _: [(); RFLAGS_OFFSET as usize] = [(); std::mem::offset_of!(JitState, rflags)];
const _: [(); ICOUNT_OFFSET as usize] = [(); std::mem::offset_of!(JitState, icount)];
const _: [(); TSC_OFFSET as usize] = [(); std::mem::offset_of!(JitState, tsc)];

impl From<&Cpu> for JitState {
    fn from(cpu: &Cpu) -> Self {
        Self {
            gpr: cpu.gpr,
            rip: cpu.rip,
            rflags: cpu.rflags,
            icount: cpu.icount,
            tsc: cpu.tsc,
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

/// A generated straight-line prefix and its process-local cache key.
#[allow(dead_code)]
pub struct EmittedBlock {
    id: u32,
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
    pub(crate) fn execute(&self, cpu: &mut Cpu, budget: u64) -> Option<usize> {
        if budget == 0 {
            return Some(0);
        }
        let mut state = JitState::from(&*cpu);
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
            )
        };
        let expected = (budget as usize).min(self.instructions);
        if executed <= 0 || executed as usize != expected {
            return None;
        }
        state.copy_to(cpu);
        Some(executed as usize)
    }
}

/// Emit a module for the leading immediate and register-register MOV instructions in a block.
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
    let prefix_len = instructions
        .iter()
        .take_while(|inst| translate_mov(**inst).is_some())
        .count();
    if prefix_len < min_prefix {
        return None;
    }
    let moves: Vec<_> = instructions[..prefix_len]
        .iter()
        .copied()
        .map(|inst| translate_mov(inst).unwrap())
        .collect();
    let first_register_register = moves
        .iter()
        .position(|mov| matches!(mov.source, MovSource::Register { .. }))
        .map(|index| index as u32);
    Some(EmittedBlock {
        id,
        wasm: emit_module(start, &moves),
        instructions: moves.len(),
        first_register_register,
    })
}

fn translate_mov(inst: Inst) -> Option<MovInstruction> {
    if inst.lock || inst.len == 0 {
        return None;
    }
    let (dst, source) = match (inst.op, inst.ops[0], inst.ops[1], inst.osz) {
        (Op::Mov, Opnd::Gpr(dst), Opnd::Imm, 4) => {
            (dst, MovSource::Immediate((inst.imm as u32) as u64))
        }
        (Op::Mov, Opnd::Gpr(dst), Opnd::Imm, 8) => (dst, MovSource::Immediate(inst.imm as u64)),
        (Op::Mov, Opnd::Gpr(dst), Opnd::Gpr(src), 4) => (
            dst,
            MovSource::Register {
                index: src,
                zero_extend: true,
            },
        ),
        (Op::Mov, Opnd::Gpr(dst), Opnd::Gpr(src), 8) => (
            dst,
            MovSource::Register {
                index: src,
                zero_extend: false,
            },
        ),
        _ => return None,
    };
    if dst >= 16 || matches!(source, MovSource::Register { index, .. } if index >= 16) {
        return None;
    }
    Some(MovInstruction {
        dst,
        source,
        len: inst.len,
    })
}

fn emit_module(start: u64, moves: &[MovInstruction]) -> Vec<u8> {
    let mut code = Vec::new();
    // One i32 local: the number of retired guest instructions.
    code.extend_from_slice(&[0x01, 0x01, 0x7f]);
    let mut rip = start;
    for mov in moves {
        rip = rip.wrapping_add(mov.len as u64);
        // if (executed >= budget) return executed;
        code.extend_from_slice(&[
            0x20, 0x02, 0x20, 0x01, 0x4f, 0x04, 0x40, 0x20, 0x02, 0x0f, 0x0b,
        ]);

        // Store an immediate or load the source GPR. MOV r32 zero-extends the low dword.
        code.push(0x20);
        uleb(0, &mut code);
        match mov.source {
            MovSource::Immediate(value) => {
                code.push(0x42);
                sleb(value as i64, &mut code);
            }
            MovSource::Register { index, zero_extend } => {
                code.push(0x20);
                uleb(0, &mut code);
                code.push(0x29); // i64.load source GPR
                uleb(3, &mut code);
                uleb(GPR_OFFSET + index as u32 * 8, &mut code);
                if zero_extend {
                    code.push(0xa7); // i32.wrap_i64
                    code.push(0xad); // i64.extend_i32_u
                }
            }
        }
        code.push(0x37); // i64.store
        uleb(3, &mut code);
        uleb(GPR_OFFSET + mov.dst as u32 * 8, &mut code);

        // RIP points at the next guest instruction after each emitted MOV.
        code.push(0x20);
        uleb(0, &mut code);
        code.push(0x42);
        sleb(rip as i64, &mut code);
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
    fn emits_only_a_leading_register_immediate_mov_prefix() {
        let mov_rax = decode(&[0x48, 0xb8, 1, 0, 0, 0, 0, 0, 0, 0], 0x1000).unwrap();
        let nop = decode(&[0x90], 0x100a).unwrap();
        let emitted = emit_mov_block(7, 0x1000, &[mov_rax, nop], 1).unwrap();
        assert_eq!(emitted.instruction_count(), 1);
        assert!(emitted.wasm.starts_with(b"\0asm\x01\0\0\0"));
        assert!(emitted.wasm.windows(7).any(|window| window == b"anywasm"));
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
