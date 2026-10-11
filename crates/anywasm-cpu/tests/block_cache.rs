use anywasm_cpu::exec::{BlockCache, DecodeCache, Exit};
use anywasm_cpu::mem::{Access, Fault, PAGE_SIZE, PROT_RW, PROT_RWX};
use anywasm_cpu::{Cpu, Memory, Op};

const BASE: u64 = 0x1000;

fn memory_with(prot: u8, code: &[u8]) -> Memory {
    let mut mem = Memory::new();
    assert!(mem.map(BASE, PAGE_SIZE, prot));
    mem.write_force(BASE, code).unwrap();
    mem
}

#[test]
fn discovers_a_block_through_its_conditional_branch() {
    let mem = memory_with(PROT_RWX, &[0x90, 0x74, 0x00, 0x0f, 0x0b]);
    let mut cache = BlockCache::new();

    let block = cache.get_block(BASE, &mem).unwrap();

    assert_eq!(block.start, BASE);
    assert_eq!(
        block.instructions.iter().map(|i| i.op).collect::<Vec<_>>(),
        [Op::Nop, Op::Jcc]
    );
    assert_eq!(block.fallthrough_rip, BASE + 3);
    assert!(block.terminated);
}

#[test]
fn caps_a_linear_block_before_decoding_past_its_limit() {
    let mut code = vec![0x90; 64];
    code.push(0xc3);
    let mem = memory_with(PROT_RWX, &code);
    let mut cache = BlockCache::new();

    let block = cache.get_block(BASE, &mem).unwrap();

    assert_eq!(block.instructions.len(), 64);
    assert!(block.instructions.iter().all(|i| i.op == Op::Nop));
    assert_eq!(block.fallthrough_rip, BASE + 64);
    assert!(!block.terminated);
}

#[test]
fn redecodes_a_block_after_executable_memory_is_written() {
    let mut mem = memory_with(PROT_RWX, &[0x90, 0x90, 0xc3]);
    let mut cache = BlockCache::new();
    assert_eq!(cache.get_block(BASE, &mem).unwrap().instructions.len(), 3);

    mem.write_u8(BASE, 0xeb).unwrap();

    let block = cache.get_block(BASE, &mem).unwrap();
    assert_eq!(
        block.instructions.iter().map(|i| i.op).collect::<Vec<_>>(),
        [Op::Jmp]
    );
    assert!(block.terminated);
}

#[test]
fn does_not_use_a_cached_block_after_execute_permission_is_removed() {
    let mut mem = memory_with(PROT_RWX, &[0x90, 0xc3]);
    let mut cache = BlockCache::new();
    assert_eq!(cache.get_block(BASE, &mem).unwrap().instructions.len(), 2);

    assert!(mem.protect(BASE, PAGE_SIZE, PROT_RW));

    assert_eq!(
        cache.get_block(BASE, &mem).unwrap_err(),
        Exit::Fault(Fault {
            addr: BASE,
            access: Access::Exec,
            protection: true
        })
    );
}

#[test]
fn executes_translated_moves_and_interpreter_fallback_with_matching_state() {
    let code = [
        0x90, // nop
        0x48, 0xb8, 5, 0, 0, 0, 0, 0, 0, 0, // mov rax, 5
        0x89, 0xc3, // mov ebx, eax (clears upper bits)
        0x48, 0x83, 0xc0, 1, // add rax, 1 (interpreter fallback)
        0xeb, 0, // jmp to the next block
    ];
    let mut block_mem = memory_with(PROT_RWX, &code);
    let mut interpreter_mem = memory_with(PROT_RWX, &code);
    let mut block_cpu = Cpu::new();
    let mut interpreter_cpu = Cpu::new();
    block_cpu.rip = BASE;
    interpreter_cpu.rip = BASE;
    let mut blocks = BlockCache::new();
    let mut dcache = DecodeCache::new();
    block_cpu.gpr[3] = u64::MAX;
    interpreter_cpu.gpr[3] = u64::MAX;

    assert_eq!(
        blocks.execute(&mut block_cpu, &mut block_mem, 5).unwrap(),
        5
    );
    for _ in 0..5 {
        dcache
            .step(&mut interpreter_cpu, &mut interpreter_mem)
            .unwrap();
    }

    assert_eq!(block_cpu.gpr, interpreter_cpu.gpr);
    assert_eq!(block_cpu.rflags, interpreter_cpu.rflags);
    assert_eq!(block_cpu.rip, interpreter_cpu.rip);
    assert_eq!(block_cpu.icount, interpreter_cpu.icount);
    assert_eq!(block_cpu.tsc, interpreter_cpu.tsc);
}

#[test]
fn respects_the_instruction_budget_inside_a_decoded_block() {
    let code = [
        0x48, 0xb8, 5, 0, 0, 0, 0, 0, 0, 0, // mov rax, 5
        0x48, 0x89, 0xc3, // mov rbx, rax
        0x48, 0x83, 0xc0, 1, // add rax, 1
        0xeb, 0,
    ];
    let mut mem = memory_with(PROT_RWX, &code);
    let mut cpu = Cpu::new();
    cpu.rip = BASE;
    let mut blocks = BlockCache::new();

    assert_eq!(blocks.execute(&mut cpu, &mut mem, 2).unwrap(), 2);
    assert_eq!(cpu.rip, BASE + 13);
    assert_eq!(cpu.icount, 2);
    assert_eq!(cpu.gpr[0], 5);
    assert_eq!(cpu.gpr[3], 5);
}

#[test]
fn stops_a_block_after_an_instruction_modifies_executable_code() {
    let mut code = vec![0x48, 0xb8]; // mov rax, imm64
    code.extend_from_slice(&(BASE + 13).to_le_bytes());
    code.extend_from_slice(&[0xc6, 0x00, 0xc3, 0x90, 0xc3]); // mov byte [rax], ret; nop; ret
    let mut mem = memory_with(PROT_RWX, &code);
    let mut cpu = Cpu::new();
    cpu.rip = BASE;
    let mut blocks = BlockCache::new();

    assert_eq!(blocks.execute(&mut cpu, &mut mem, 4).unwrap(), 2);
    assert_eq!(cpu.rip, BASE + 13);
    let next = blocks.get_block(cpu.rip, &mem).unwrap();
    assert_eq!(next.instructions.len(), 1);
    assert_eq!(next.instructions[0].op, Op::Ret);
    assert!(next.terminated);
}

#[test]
fn translated_lea_ignores_a_segment_override() {
    let code = [0x64, 0x48, 0x8d, 0x00]; // lea rax, fs:[rax] (LEA ignores FS)
    let mut block_mem = memory_with(PROT_RWX, &code);
    let mut interpreter_mem = memory_with(PROT_RWX, &code);
    let mut block_cpu = Cpu::new();
    let mut interpreter_cpu = Cpu::new();
    block_cpu.rip = BASE;
    interpreter_cpu.rip = BASE;
    block_cpu.gpr[0] = 0x1234;
    interpreter_cpu.gpr[0] = 0x1234;
    block_cpu.fs_base = 0x100000;
    interpreter_cpu.fs_base = 0x100000;
    let mut blocks = BlockCache::new();
    let mut dcache = DecodeCache::new();

    assert_eq!(
        blocks.execute(&mut block_cpu, &mut block_mem, 1).unwrap(),
        1
    );
    dcache
        .step(&mut interpreter_cpu, &mut interpreter_mem)
        .unwrap();

    assert_eq!(block_cpu.gpr[0], 0x1234);
    assert_eq!(block_cpu.gpr, interpreter_cpu.gpr);
    assert_eq!(block_cpu.rip, interpreter_cpu.rip);
}

#[test]
fn retires_valid_instructions_before_a_later_block_fetch_fault() {
    let mut mem = Memory::new();
    assert!(mem.map(BASE, PAGE_SIZE, PROT_RWX));
    let last_byte = BASE + PAGE_SIZE - 1;
    mem.write_force(last_byte, &[0x90]).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = last_byte;
    let mut blocks = BlockCache::new();

    assert_eq!(blocks.execute(&mut cpu, &mut mem, 2).unwrap(), 1);
    assert_eq!(cpu.rip, BASE + PAGE_SIZE);
    assert_eq!(cpu.icount, 1);
    assert!(matches!(
        blocks.execute(&mut cpu, &mut mem, 2),
        Err(Exit::Fault(Fault { addr, access: Access::Exec, protection: false }))
            if addr == BASE + PAGE_SIZE
    ));
}
