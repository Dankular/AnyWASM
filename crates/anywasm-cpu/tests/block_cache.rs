use anywasm_cpu::exec::{BlockCache, Exit};
use anywasm_cpu::mem::{Access, Fault, PAGE_SIZE, PROT_RW, PROT_RWX};
use anywasm_cpu::{Memory, Op};

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
