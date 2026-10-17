//! x86 instruction executor shared by the process interpreter and isolated CPU workers.

use std::collections::{hash_map::Entry, HashMap};

use crate::cpu::*;
use crate::decode::{decode_mode, Inst, MemOp, Op, Opnd};
use crate::mem::Memory;
pub use anywasm_cpu_state::exec_core::*;

/// Direct-mapped cache of decoded instructions, valid while `Memory::code_epoch` is unchanged.
pub struct DecodeCache {
    entries: Vec<(u64, u64, CpuMode, Inst)>,
}

const DC_BITS: usize = 15;

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeCache {
    pub fn new() -> Self {
        let dummy = decode_mode(&[0x90], 0, CpuMode::X86_64).unwrap();
        DecodeCache {
            entries: vec![(u64::MAX, u64::MAX, CpuMode::X86_64, dummy); 1 << DC_BITS],
        }
    }

    /// Fetch (cached), decode and execute one instruction.
    #[inline]
    pub fn step(&mut self, cpu: &mut Cpu, mem: &mut Memory) -> ExecResult<()> {
        let rip = cpu.rip;
        let idx = ((rip ^ (rip >> DC_BITS)) as usize) & ((1 << DC_BITS) - 1);
        let e = &mut self.entries[idx];
        if e.0 != rip || e.1 != mem.code_epoch || e.2 != cpu.mode {
            let mut buf = [0u8; 15];
            let n = mem.fetch(rip, &mut buf)?;
            let inst = decode_mode(&buf[..n], rip, cpu.mode).map_err(Exit::Decode)?;
            *e = (rip, mem.code_epoch, cpu.mode, inst);
        }
        let inst = e.3;
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
    mode: Option<CpuMode>,
    blocks: HashMap<u64, CachedBlock>,
    #[cfg(target_arch = "wasm32")]
    next_jit_id: u32,
    #[cfg(target_arch = "wasm32")]
    min_jit_prefix: usize,
}

const MAX_CACHED_BLOCKS: usize = 1024;
#[cfg(target_arch = "wasm32")]
const DEFAULT_JIT_MIN_PREFIX: usize = MAX_BLOCK_INSTRUCTIONS + 1;
#[cfg(target_arch = "wasm32")]
const HOT_BLOCK_EXECUTIONS: u8 = 1;

struct CachedBlock {
    decoded: DecodedBlock,
    instructions: Vec<CachedInstruction>,
    #[cfg(target_arch = "wasm32")]
    emitted: Option<crate::jit::EmittedBlock>,
    #[cfg(target_arch = "wasm32")]
    jit_hits: u8,
    #[cfg(target_arch = "wasm32")]
    jit_attempted: bool,
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
            mode: None,
            blocks: HashMap::new(),
            #[cfg(target_arch = "wasm32")]
            next_jit_id: 1,
            #[cfg(target_arch = "wasm32")]
            min_jit_prefix: DEFAULT_JIT_MIN_PREFIX,
        }
    }

    /// Set the minimum emitted MOV-prefix length. Values above the block limit disable emission.
    #[cfg(target_arch = "wasm32")]
    pub fn set_jit_min_prefix(&mut self, min_jit_prefix: usize) {
        self.min_jit_prefix = min_jit_prefix.max(1);
        self.blocks.clear();
    }

    #[cfg(target_arch = "wasm32")]
    fn maybe_emit_hot_block(&mut self, start: u64, mode: CpuMode) {
        let min_prefix = self.min_jit_prefix;
        let Some((block_start, instructions)) = (|| {
            let block = self.blocks.get_mut(&start)?;
            if block.jit_attempted || min_prefix > block.decoded.instructions.len() {
                return None;
            }
            block.jit_hits = block.jit_hits.saturating_add(1);
            if block.jit_hits < HOT_BLOCK_EXECUTIONS {
                return None;
            }
            block.jit_attempted = true;
            Some((block.decoded.start, block.decoded.instructions.clone()))
        })() else {
            return;
        };

        let Some(emitted) = crate::jit::emit_mov_block_mode(
            self.next_jit_id,
            block_start,
            &instructions,
            min_prefix,
            mode,
        ) else {
            return;
        };
        self.next_jit_id = self.next_jit_id.wrapping_add(1).max(1);
        self.blocks.get_mut(&start).unwrap().emitted = Some(emitted);
    }

    /// Return a cached block or decode a straight-line block beginning at `start`.
    ///
    /// The block includes the first control-flow or terminal instruction. A linear block that
    /// reaches [`MAX_BLOCK_INSTRUCTIONS`] is returned with `terminated == false`.
    pub fn get_block(&mut self, start: u64, mem: &Memory) -> ExecResult<&DecodedBlock> {
        Ok(&self.get_cached_block(start, mem, CpuMode::X86_64)?.decoded)
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
        let (executed, result) = self.execute_counted(cpu, mem, max_instructions);
        result.map(|()| executed)
    }

    /// Execute a block while preserving the count completed before a later instruction faults.
    pub fn execute_counted(
        &mut self,
        cpu: &mut Cpu,
        mem: &mut Memory,
        max_instructions: u64,
    ) -> (u64, ExecResult<()>) {
        self.execute_counted_with_hook(cpu, mem, max_instructions, |_, _| {})
    }

    /// Execute a block and call `before_instruction` with the live guest state immediately before
    /// each interpreted or cached instruction. This preserves normal block boundaries while
    /// allowing the runtime's fault trace to observe execution without single-stepping it.
    pub fn execute_counted_with_hook<F>(
        &mut self,
        cpu: &mut Cpu,
        mem: &mut Memory,
        max_instructions: u64,
        mut before_instruction: F,
    ) -> (u64, ExecResult<()>)
    where
        F: FnMut(&Cpu, &Memory),
    {
        if max_instructions == 0 {
            return (0, Ok(()));
        }

        let block_start = cpu.rip;
        match self.get_cached_block(block_start, mem, cpu.mode) {
            Ok(_) => {}
            Err(error) => return (0, Err(error)),
        }
        #[cfg(target_arch = "wasm32")]
        self.maybe_emit_hot_block(block_start, cpu.mode);
        let block = self.blocks.get(&block_start).unwrap();
        #[cfg(target_arch = "wasm32")]
        let (mut executed, start_index, interpreter_only) = match block.emitted.as_ref() {
            Some(emitted) => match emitted.execute(cpu, mem, max_instructions) {
                Some((executed, true)) => return (executed as u64, Ok(())),
                Some((executed, false)) => (executed as u64, emitted.instruction_count(), true),
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
            before_instruction(cpu, mem);
            let before_rip = cpu.rip;
            let before_epoch = mem.code_epoch;
            if interpreter_only {
                if let Err(error) = exec(cpu, mem, &cached.inst) {
                    return (executed, Err(error));
                }
            } else {
                if let Err(error) = execute_cached(cpu, mem, cached) {
                    return (executed, Err(error));
                }
            }
            executed += 1;

            let sequential_rip =
                ip_value(cpu.mode, before_rip.wrapping_add(cached.inst.len as u64));
            if mem.code_epoch != before_epoch
                || ends_basic_block(cached.inst.op)
                || cpu.rip != sequential_rip
            {
                break;
            }
        }
        (executed, Ok(()))
    }

    fn get_cached_block(
        &mut self,
        start: u64,
        mem: &Memory,
        mode: CpuMode,
    ) -> ExecResult<&CachedBlock> {
        if self.code_epoch != Some(mem.code_epoch) || self.mode != Some(mode) {
            self.blocks.clear();
            self.code_epoch = Some(mem.code_epoch);
            self.mode = Some(mode);
        }
        if self.blocks.len() >= MAX_CACHED_BLOCKS && !self.blocks.contains_key(&start) {
            self.blocks.clear();
        }
        match self.blocks.entry(start) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let decoded = decode_block(start, mem, mode)?;
                let instructions = decoded
                    .instructions
                    .iter()
                    .copied()
                    .map(translate_instruction)
                    .collect();
                let block = CachedBlock {
                    decoded,
                    instructions,
                    #[cfg(target_arch = "wasm32")]
                    emitted: None,
                    #[cfg(target_arch = "wasm32")]
                    jit_hits: 0,
                    #[cfg(target_arch = "wasm32")]
                    jit_attempted: false,
                };
                Ok(entry.insert(block))
            }
        }
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

    cpu.rip = ip_value(cpu.mode, cpu.rip.wrapping_add(cached.inst.len as u64));
    cpu.icount += 1;
    cpu.tsc += 1;
    Ok(())
}

fn decode_block(start: u64, mem: &Memory, mode: CpuMode) -> ExecResult<DecodedBlock> {
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
        let inst = match decode_mode(&buf[..n], rip, mode) {
            Ok(inst) => inst,
            Err(error) if !instructions.is_empty() => {
                return Ok(partial_block(start, instructions, rip, Exit::Decode(error)));
            }
            Err(error) => return Err(Exit::Decode(error)),
        };
        rip = ip_value(mode, rip.wrapping_add(inst.len as u64));
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

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn x86_mode_executes_32_bit_register_and_stack_semantics() {
        let mut mem = Memory::new();
        assert!(mem.map(0x1000, 0x1000, crate::mem::PROT_RWX));
        assert!(mem.map(0x2000, 0x1000, crate::mem::PROT_RW));
        // mov eax, -1; inc eax; push eax; pop ebx
        mem.write_force(0x1000, &[0xb8, 0xff, 0xff, 0xff, 0xff, 0x40, 0x50, 0x5b])
            .unwrap();

        let mut cpu = Cpu::new();
        cpu.mode = CpuMode::X86_32;
        cpu.rip = 0x1000;
        cpu.gpr[RSP] = 0x2800;
        let mut cache = DecodeCache::new();
        for _ in 0..4 {
            cache.step(&mut cpu, &mut mem).unwrap();
        }

        assert_eq!(cpu.gpr[RAX], 0);
        assert_eq!(cpu.gpr[RBX], 0);
        assert_eq!(cpu.gpr[RSP], 0x2800);
        assert!(cpu.flag(ZF));
        assert_eq!(cpu.rip, 0x1008);
    }

    #[test]
    fn x86_mode_wraps_instruction_pointer_at_32_bits() {
        let mut mem = Memory::new();
        assert!(mem.map(0xffff_f000, 0x1000, crate::mem::PROT_RWX));
        mem.write_force(0xffff_ffff, &[0x90]).unwrap();
        let mut cpu = Cpu::new();
        cpu.mode = CpuMode::X86_32;
        cpu.rip = 0xffff_ffff;
        let mut cache = DecodeCache::new();

        cache.step(&mut cpu, &mut mem).unwrap();

        assert_eq!(cpu.rip, 0);
    }

    #[test]
    fn x86_pushfd_and_popfd_use_four_byte_stack_slots() {
        let mut mem = Memory::new();
        assert!(mem.map(0x1000, 0x1000, crate::mem::PROT_RWX));
        assert!(mem.map(0x2000, 0x1000, crate::mem::PROT_RW));
        mem.write_force(0x1000, &[0x9c, 0x9d]).unwrap();
        let mut cpu = Cpu::new();
        cpu.mode = CpuMode::X86_32;
        cpu.rip = 0x1000;
        cpu.gpr[RSP] = 0x2800;
        cpu.rflags = 0x202;
        let mut cache = DecodeCache::new();

        cache.step(&mut cpu, &mut mem).unwrap();
        assert_eq!(cpu.gpr[RSP], 0x27fc);
        assert_eq!(mem.read_u32(0x27fc).unwrap(), 0x202);
        cache.step(&mut cpu, &mut mem).unwrap();
        assert_eq!(cpu.gpr[RSP], 0x2800);
        assert_eq!(cpu.rflags & USER_FLAGS, 0x202 & USER_FLAGS);
    }
}
