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

#[test]
fn rep_movsw_copies_words_and_advances_by_operand_width() {
    let (mut cpu, mut mem) = machine(&[0x66, 0xf3, 0xa5]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 3;
    mem.write_force(SRC, &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66])
        .unwrap();
    mem.write_force(DST, &[0xaa; 6]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RSI], SRC + 6);
    assert_eq!(cpu.gpr[RDI], DST + 6);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(
        mem.read_vec(DST, 6).unwrap(),
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]
    );
}

#[test]
fn rep_movsd_copies_dwords_and_advances_by_operand_width() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xa5]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 2;
    mem.write_force(SRC, &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88])
        .unwrap();
    mem.write_force(DST, &[0xaa; 8]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RSI], SRC + 8);
    assert_eq!(cpu.gpr[RDI], DST + 8);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(
        mem.read_vec(DST, 8).unwrap(),
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
    );
}

#[test]
fn rep_movsq_copies_qwords_and_advances_by_operand_width() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0x48, 0xa5]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 2;
    let source = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xf0,
        0x10,
    ];
    mem.write_force(SRC, &source).unwrap();
    mem.write_force(DST, &[0xaa; 16]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RSI], SRC + 16);
    assert_eq!(cpu.gpr[RDI], DST + 16);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 16).unwrap(), source);
}

#[test]
fn rep_stosw_writes_ax_without_changing_flags() {
    let (mut cpu, mut mem) = machine(&[0x66, 0xf3, 0xab]);
    let flags = 0x202 | CF | ZF | DF;
    cpu.rflags = flags;
    cpu.gpr[RAX] = 0x1122_3344_5566_beef;
    cpu.gpr[RDI] = DST + 4;
    cpu.gpr[RCX] = 3;
    mem.write_force(DST, &[0xaa; 6]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RAX], 0x1122_3344_5566_beef);
    assert_eq!(cpu.gpr[RDI], DST - 2);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(
        mem.read_vec(DST, 6).unwrap(),
        [0xef, 0xbe, 0xef, 0xbe, 0xef, 0xbe]
    );
}

#[test]
fn rep_lodsd_zero_extends_eax_after_the_last_load() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0xad]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RAX] = 0x1122_3344_5566_7788;
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RCX] = 2;
    mem.write_force(SRC, &[0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd])
        .unwrap();

    let (cpu, mem) = execute(cpu, mem, 2);

    assert_eq!(cpu.gpr[RAX], 0x0000_0000_ddcc_bbaa);
    assert_eq!(cpu.gpr[RSI], SRC + 8);
    assert_eq!(cpu.gpr[RDI], 0);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(
        mem.read_vec(SRC, 8).unwrap(),
        [0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd]
    );
}

#[test]
fn repe_cmpsq_uses_qword_comparison_for_early_termination() {
    let (mut cpu, mut mem) = machine(&[0xf3, 0x48, 0xa7]);
    cpu.gpr[RSI] = SRC;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 3;
    let equal = [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
    let mismatch = [0x09, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
    mem.write_force(SRC, &[equal, equal, equal].concat())
        .unwrap();
    mem.write_force(DST, &[equal, mismatch, equal].concat())
        .unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RSI], SRC + 16);
    assert_eq!(cpu.gpr[RDI], DST + 16);
    assert_eq!(cpu.gpr[RCX], 1);
    assert_eq!(cpu.rflags & ARITH_FLAGS, CF | PF | AF | SF);
    assert_eq!(
        mem.read_vec(SRC, 24).unwrap(),
        [equal, equal, equal].concat()
    );
    assert_eq!(
        mem.read_vec(DST, 24).unwrap(),
        [equal, mismatch, equal].concat()
    );
}

#[test]
fn repne_scasw_stops_on_word_match() {
    let (mut cpu, mut mem) = machine(&[0x66, 0xf2, 0xaf]);
    cpu.gpr[RAX] = 0x1234;
    cpu.gpr[RDI] = DST;
    cpu.gpr[RCX] = 4;
    let bytes = [0x10, 0x00, 0x20, 0x00, 0x34, 0x12, 0x40, 0x00];
    mem.write_force(DST, &bytes).unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RAX], 0x1234);
    assert_eq!(cpu.gpr[RDI], DST + 6);
    assert_eq!(cpu.gpr[RCX], 1);
    assert_eq!(cpu.rflags & ARITH_FLAGS, ZF | PF);
    assert_eq!(mem.read_vec(DST, 8).unwrap(), bytes);
}

#[test]
fn rep_movsd_with_address_size_override_uses_ecx_esi_and_edi() {
    let (mut cpu, mut mem) = machine(&[0x67, 0xf3, 0xa5]);
    let flags = 0x202 | CF | ZF;
    cpu.rflags = flags;
    cpu.gpr[RSI] = 0xdead_beef_0000_0000 | SRC;
    cpu.gpr[RDI] = 0xcafe_babe_0000_0000 | DST;
    cpu.gpr[RCX] = 0xaabb_ccdd_0000_0002;
    let source = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    mem.write_force(SRC, &source).unwrap();
    mem.write_force(DST, &[0xaa; 8]).unwrap();

    let (cpu, mem) = execute(cpu, mem, 3);

    assert_eq!(cpu.gpr[RSI], SRC + 8);
    assert_eq!(cpu.gpr[RDI], DST + 8);
    assert_eq!(cpu.gpr[RCX], 0);
    assert_eq!(cpu.rflags, flags);
    assert_eq!(mem.read_vec(DST, 8).unwrap(), source);
}
