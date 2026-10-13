use anywasm_cpu::cpu::{RAX, RBX, RCX, RDX};
use anywasm_cpu::mem::{PAGE_SIZE, PROT_RWX};
use anywasm_cpu::{step, Cpu, Exit, Memory};

const CODE: u64 = 0x1000_0000;

fn machine(code: &[u8]) -> (Cpu, Memory) {
    let mut mem = Memory::new();
    assert!(mem.map(CODE, PAGE_SIZE, PROT_RWX));
    mem.write_force(CODE, code).unwrap();
    let mut cpu = Cpu::new();
    cpu.rip = CODE;
    (cpu, mem)
}

#[test]
fn cpuid_reports_the_guest_vendor_and_hides_unsupported_vector_features() {
    let (mut cpu, mut mem) = machine(&[0x0f, 0xa2]);
    cpu.gpr[RAX] = 0;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(cpu.gpr[RAX], 0x16);
    let vendor = [
        (cpu.gpr[RBX] as u32).to_le_bytes(),
        (cpu.gpr[RDX] as u32).to_le_bytes(),
        (cpu.gpr[RCX] as u32).to_le_bytes(),
    ]
    .concat();
    assert_eq!(&vendor[..12], b"GenuineIntel");

    let (mut cpu, mut mem) = machine(&[0x0f, 0xa2]);
    cpu.gpr[RAX] = 1;
    step(&mut cpu, &mut mem).unwrap();
    assert_ne!(cpu.gpr[RDX] & (1 << 26), 0, "SSE2 must be advertised");
    assert_eq!(cpu.gpr[RCX] & (1 << 28), 0, "AVX is not implemented");

    let (mut cpu, mut mem) = machine(&[0x0f, 0xa2]);
    cpu.gpr[RAX] = 7;
    cpu.gpr[RCX] = 0;
    step(&mut cpu, &mut mem).unwrap();
    // Leaf 7 sub-leaf 0 advertises only BMI1 (EBX bit 3) and BMI2 (EBX bit 8), which are
    // implemented; AVX2, AVX-512, SHA, etc. stay hidden.
    assert_eq!(
        [cpu.gpr[RAX], cpu.gpr[RBX], cpu.gpr[RCX], cpu.gpr[RDX]],
        [0, (1 << 3) | (1 << 8), 0, 0]
    );

    let (mut cpu, mut mem) = machine(&[0x0f, 0xa2]);
    cpu.gpr[RAX] = 7;
    cpu.gpr[RCX] = 1;
    step(&mut cpu, &mut mem).unwrap();
    assert_eq!(
        [cpu.gpr[RAX], cpu.gpr[RBX], cpu.gpr[RCX], cpu.gpr[RDX]],
        [0; 4]
    );
}

#[test]
fn xgetbv_reports_only_x87_and_sse_state() {
    let (mut cpu, mut mem) = machine(&[0x0f, 0x01, 0xd0]);
    cpu.gpr[RCX] = 0;
    cpu.gpr[RAX] = u64::MAX;
    cpu.gpr[RDX] = u64::MAX;

    step(&mut cpu, &mut mem).unwrap();

    assert_eq!(cpu.gpr[RAX], 3);
    assert_eq!(cpu.gpr[RDX], 0);
    assert_eq!(cpu.rip, CODE + 3);
    assert_eq!(cpu.icount, 1);
}

#[test]
fn rdtsc_and_rdtscp_return_the_pre_instruction_counter() {
    const INITIAL_TSC: u64 = 0x1234_5678_9abc_def0;
    for (code, has_aux) in [(&[0x0f, 0x31][..], false), (&[0x0f, 0x01, 0xf9][..], true)] {
        let (mut cpu, mut mem) = machine(code);
        cpu.tsc = INITIAL_TSC;
        cpu.gpr[RCX] = u64::MAX;

        step(&mut cpu, &mut mem).unwrap();

        assert_eq!(cpu.gpr[RAX], INITIAL_TSC as u32 as u64);
        assert_eq!(cpu.gpr[RDX], (INITIAL_TSC >> 32) as u32 as u64);
        if has_aux {
            assert_eq!(cpu.gpr[RCX], 0, "RDTSCP reports the virtual AUX value");
        } else {
            assert_eq!(cpu.gpr[RCX], u64::MAX, "RDTSC preserves RCX");
        }
        assert_eq!(cpu.tsc, INITIAL_TSC + 1);
        assert_eq!(cpu.icount, 1);
    }
}

#[test]
fn syscall_returns_an_explicit_exit_after_its_instruction_bytes() {
    let (mut cpu, mut mem) = machine(&[0x0f, 0x05]);

    assert_eq!(step(&mut cpu, &mut mem), Err(Exit::Syscall));
    assert_eq!(cpu.rip, CODE + 2);
}

#[test]
fn ud2_keeps_an_illegal_instruction_distinct_from_generic_undefined() {
    let (mut cpu, mut mem) = machine(&[0x0f, 0x0b]);

    let result = step(&mut cpu, &mut mem);

    assert_eq!(result, Err(Exit::IllegalInstruction));
    assert_eq!(cpu.rip, CODE);
}

#[test]
fn cli_and_sti_keep_privileged_instruction_faults_distinct() {
    for instruction in [&[0xfa][..], &[0xfb][..]] {
        let (mut cpu, mut mem) = machine(instruction);

        let result = step(&mut cpu, &mut mem);

        assert_eq!(result, Err(Exit::PrivilegedInstruction));
        assert_eq!(cpu.rip, CODE);
    }
}
