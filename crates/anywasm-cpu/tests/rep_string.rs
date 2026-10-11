use anywasm_cpu::cpu::{AF, ARITH_FLAGS, CF, DF, PF, RAX, RCX, RDI, RSI, SF, ZF};
use anywasm_cpu::mem::{PROT_RW, PROT_RWX};
use anywasm_cpu::{step, Cpu, Memory};

const CODE: u64 = 0x1000_0000;
const DATA: u64 = 0x1001_0000;
const SRC: u64 = DATA + 0x100;
const DST: u64 = DATA + 0x200;

fn machine(code: &[u8]) -> (Cpu, Memory) {
    let mut mem = Memory::new();
    assert!(mem.map(CODE, 0x1000, PROT_RWX));
    assert!(mem.map(DATA, 0x1000, PROT_RW));
    mem.write_force(CODE, code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    (cpu, mem)
}

fn execute(mut cpu: Cpu, mut mem: Memory, code_len: usize) -> (Cpu, Memory) {
    let end = CODE + code_len as u64;
    let mut steps = 0;
    while cpu.rip != end {
        step(&mut cpu, &mut mem).unwrap();
        steps += 1;
        assert!(steps <= 16, "REP string instruction did not finish");
    }
    (cpu, mem)
}

#[test]
fn rep_movsb_zero_count_preserves_state_and_memory() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xa4]);
    let flags = 0x202 | CF | ZF | DF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 0;
    mem.write_force(SRC, &[0x11, 0x22, 0x33]).unwrap();
    mem.write_force(DST, &[0xaa, 0xbb, 0xcc]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RSI], SRC);
    assert_eq!(cpu.gpr[RDI], DST);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 3).unwrap(), [0xaa, 0xbb, 0xcc]);
}

#[test]
fn rep_movsb_copies_forward_and_preserves_flags() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xa4]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 3;
    mem.write_force(SRC, &[0x11, 0x22, 0x33]).unwrap();
    mem.write_force(DST, &[0xaa, 0xbb, 0xcc]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RSI], SRC + 3);
    assert_eq!(cpu.gpr[RDI], DST + 3);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 3).unwrap(), [0x11, 0x22, 0x33]);
}

#[test]
fn rep_movsb_with_df_copies_backward_and_decrements_indices() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xa4]);
    let flags = 0x202 | CF | ZF | DF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC + 2;
    cpu.gpr[RDI] = DST + 2;
    cpu.gpr[RCX] = 3;
    mem.write_force(SRC, &[0x11, 0x22, 0x33]).unwrap();
    mem.write_force(DST, &[0xaa, 0xbb, 0xcc]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RSI], SRC - 1);
    assert_eq!(cpu.gpr[RDI], DST - 1);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 3).unwrap(), [0x11, 0x22, 0x33]);
}

#[test]
fn rep_stosb_with_df_fills_backward_without_changing_flags() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xaa]);
    let flags = 0x202 | CF | ZF | DF;
    cpu.rflags = flags;
    cpu.gpr[RAX] = 0x1122_3344_5566_775a;
    cpu.gpr[RDI] = DST + 2;
    cpu.gpr[RCX] = 3;
    mem.write_force(DST, &[0xaa, 0xbb, 0xcc]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RAX], 0x1122_3344_5566_775a);
    assert_eq!(cpu.gpr[RSI], 0);
    assert_eq!(cpu.gpr[RDI], DST - 1);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 3).unwrap(), [0x5a; 3]);
}

#[test]
fn rep_lodsb_loads_the_last_byte_and_advances_source() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xac]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RAX] = 0x1122_3344_5566_7700;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RCX] = 3;
    mem.write_force(SRC, &[0x11, 0x22, 0x33]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RAX], 0x1122_3344_5566_7733);
    assert_eq!(cpu.gpr[RSI], SRC + 3);
    assert_eq!(cpu.gpr[RDI], 0);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(SRC, 3).unwrap(), [0x11, 0x22, 0x33]);
}

#[test]
fn repe_cmpsb_stops_at_mismatch_with_flags_from_last_compare() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xa6]);
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 3;
    mem.write_force(SRC, &[0x10, 0x20, 0x30]).unwrap();
    mem.write_force(DST, &[0x10, 0x21, 0x30]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RSI], SRC + 2);
    assert_eq!(cpu.gpr[RDI], DST + 2);
    assert_eq!(cpu.gpr[RCX], 1);
    assert_eq!(cpu.rflags & ARITH_FLAGS, CF | PF | AF | SF);
    assert_eq!(cpu.rflags & DF, 0);
    assert_eq!(mem.read_vec(SRC, 3).unwrap(), [0x10, 0x20, 0x30]);
    assert_eq!(mem.read_vec(DST, 3).unwrap(), [0x10, 0x21, 0x30]);
}

#[test]
fn repne_scasb_stops_on_match_with_flags_from_match() {
    let (mut cpu, mut mem) = machine(&[0xf2, 0xae]);
    cpu.gpr[RAX] = 0x21;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 4;
    mem.write_force(DST, &[0x10, 0x20, 0x21, 0x30]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RAX], 0x21);
    assert_eq!(cpu.gpr[RSI], 0);
    assert_eq!(cpu.gpr[RDI], DST + 3);
    assert_eq!(cpu.gpr[RCX], 1);
    assert_eq!(cpu.rflags & ARITH_FLAGS, ZF | PF);
    assert_eq!(cpu.rflags & DF, 0);
    assert_eq!(mem.read_vec(DST, 4).unwrap(), [0x10, 0x20, 0x21, 0x30]);
}
