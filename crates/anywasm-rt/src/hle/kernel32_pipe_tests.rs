use super::*;
use anywasm_cpu::cpu::{CpuMode, RSP};
use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

const BASE: u64 = 0x30000;
const STACK: u64 = BASE + 0x800;
const READ_OUT: u64 = BASE + 0x100;
const WRITE_OUT: u64 = BASE + 0x104;
const COUNT_OUT: u64 = BASE + 0x108;
const DATA: u64 = BASE + 0x200;
const OVERLAPPED: u64 = BASE + 0x300;
const SECURITY_ATTRIBUTES: u64 = BASE + 0x400;
const PIPE_NAME: u64 = BASE + 0x500;

fn process() -> Process {
    let mut process = Process::new(crate::vfs::Vfs::new());
    assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
    assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
    process.cpu.mode = CpuMode::X86_32;
    process.cpu.gpr[RSP] = STACK;
    process.wr32(STACK, 0).unwrap();
    process
}

fn invoke_x86(process: &mut Process, api: &str, args: &[u32]) -> Result<u64, Stop> {
    process.cpu.mode = CpuMode::X86_32;
    process.cpu.gpr[RSP] = STACK;
    process.wr32(STACK, 0).unwrap();
    for (index, value) in args.iter().enumerate() {
        process.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
    }
    lookup(api).unwrap_or_else(|| panic!("kernel32!{api} must be explicitly bound"))(process)
}

fn call_x86(process: &mut Process, api: &str, args: &[u32]) -> u64 {
    invoke_x86(process, api, args).unwrap()
}

fn last_error(process: &Process) -> u32 {
    process.rd32(TEB_ADDR + 0x34).unwrap()
}

fn pipe_name(process: &mut Process, name: &str) -> u32 {
    process.write_wstr(PIPE_NAME, name).unwrap();
    PIPE_NAME as u32
}

fn create_named_pipe(
    process: &mut Process,
    name: u32,
    mode: u32,
    instances: u32,
    size: u32,
) -> u32 {
    call_x86(
        process,
        "CreateNamedPipeW",
        &[name, 3, mode, instances, size, size, 0, 0],
    ) as u32
}

fn open_duplex_client(process: &mut Process, name: u32) -> u32 {
    call_x86(
        process,
        "CreateFileW",
        &[name, GENERIC_READ | GENERIC_WRITE, 0, 0, 3, 0, 0],
    ) as u32
}

#[test]
fn create_pipe_keeps_inheritable_handles_and_transfers_guest_bytes() {
    let mut process = process();
    process.wr32(SECURITY_ATTRIBUTES, 12).unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 4, 0).unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 8, 1).unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "CreatePipe",
            &[
                READ_OUT as u32,
                WRITE_OUT as u32,
                SECURITY_ATTRIBUTES as u32,
                128,
            ],
        ),
        1
    );
    let reader = process.rd32(READ_OUT).unwrap() as u64;
    let writer = process.rd32(WRITE_OUT).unwrap() as u64;
    assert!(matches!(
        process.handles.get(&reader),
        Some(Handle::Pipe {
            endpoint: PipeEndpoint::AnonymousRead,
            inheritable: true,
            ..
        })
    ));
    assert!(matches!(
        process.handles.get(&writer),
        Some(Handle::Pipe {
            endpoint: PipeEndpoint::AnonymousWrite,
            inheritable: true,
            ..
        })
    ));

    process.write_bytes(DATA, b"pipe bytes").unwrap();
    process.write_bytes(OVERLAPPED, &[0xa5; 20]).unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[
                writer as u32,
                DATA as u32,
                10,
                COUNT_OUT as u32,
                OVERLAPPED as u32,
            ],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 10);
    assert_eq!(process.read_bytes(OVERLAPPED, 20).unwrap(), [0xa5; 20]);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[
                reader as u32,
                DATA as u32,
                10,
                COUNT_OUT as u32,
                OVERLAPPED as u32,
            ],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 10);
    assert_eq!(process.read_bytes(DATA, 10).unwrap(), b"pipe bytes");

    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[reader as u32, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_ACCESS_DENIED);
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 0);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[writer as u32, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_ACCESS_DENIED);
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 0);

    assert!(process.close_handle(writer).is_some());
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[reader as u32, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_BROKEN_PIPE);
    assert!(process.close_handle(reader).is_some());
    assert!(process.pipes.is_empty());
}

#[test]
fn create_pipe_validates_outputs_security_and_capacity() {
    let mut process = process();
    assert_eq!(
        call_x86(&mut process, "CreatePipe", &[0, WRITE_OUT as u32, 0, 0],),
        0
    );
    assert_eq!(last_error(&process), ERROR_INVALID_PARAMETER);

    process.wr32(SECURITY_ATTRIBUTES, 12).unwrap();
    process
        .wr32(SECURITY_ATTRIBUTES + 4, (BASE + 0x600) as u32)
        .unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 8, 0).unwrap();
    assert!(matches!(
        invoke_x86(
            &mut process,
            "CreatePipe",
            &[
                READ_OUT as u32,
                WRITE_OUT as u32,
                SECURITY_ATTRIBUTES as u32,
                0,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("CreatePipe") && name.contains("security descriptor")
    ));
    assert!(process.pipes.is_empty());

    process.wr32(SECURITY_ATTRIBUTES, 8).unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 4, 0).unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 8, 0).unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "CreatePipe",
            &[
                READ_OUT as u32,
                WRITE_OUT as u32,
                SECURITY_ATTRIBUTES as u32,
                0,
            ],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_INVALID_PARAMETER);

    assert_eq!(
        call_x86(
            &mut process,
            "CreatePipe",
            &[
                READ_OUT as u32,
                WRITE_OUT as u32,
                0,
                (MAX_PIPE_BUFFER_SIZE + 1) as u32,
            ],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_NOT_ENOUGH_MEMORY);
}

#[test]
fn named_pipe_transfer_and_connection_state_match_nonblocking_byte_mode() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\PipeStateFixture");
    let server = create_named_pipe(
        &mut process,
        name,
        PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
        1,
        4,
    );
    assert_ne!(server, INVALID_HANDLE as u32);

    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_LISTENING);
    let client = open_duplex_client(&mut process, name);
    assert_ne!(client, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_CONNECTED);

    // PIPE_NOWAIT on the server permits a byte-mode partial write when its output queue fills.
    process.write_bytes(DATA, b"abcde").unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[server, DATA as u32, 5, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 4);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[client, DATA as u32, 4, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.read_bytes(DATA, 4).unwrap(), b"abcd");
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[server, (DATA + 4) as u32, 1, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT).unwrap(), 1);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[client, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.read_bytes(DATA, 1).unwrap(), b"e");

    // Client-to-server bytes use the independent input queue.
    process.write_bytes(DATA, b"req").unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[client, DATA as u32, 3, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 3, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.read_bytes(DATA, 3).unwrap(), b"req");
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_NO_DATA);

    assert_eq!(call_x86(&mut process, "DisconnectNamedPipe", &[server]), 1);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[client, DATA as u32, 1, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_PIPE_NOT_CONNECTED);

    // After an explicit disconnect, nonblocking ConnectNamedPipe reports readiness once.
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 1);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_LISTENING);

    let next_client = open_duplex_client(&mut process, name);
    assert_ne!(next_client, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_CONNECTED);
    assert_eq!(call_x86(&mut process, "CloseHandle", &[next_client]), 1);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_NO_DATA);
    assert_eq!(call_x86(&mut process, "DisconnectNamedPipe", &[server]), 1);
    assert_eq!(call_x86(&mut process, "DisconnectNamedPipe", &[server]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_NOT_CONNECTED);

    assert_eq!(call_x86(&mut process, "CloseHandle", &[client]), 1);
    assert_eq!(call_x86(&mut process, "CloseHandle", &[server]), 1);
    assert!(process.pipes.is_empty());
}

#[test]
fn unsupported_pipe_modes_and_overlapped_operations_stop_explicitly() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\PipeUnsupportedFixture");
    assert!(matches!(
        invoke_x86(
            &mut process,
            "CreateNamedPipeW",
            &[
                name,
                3 | FILE_FLAG_OVERLAPPED,
                0,
                1,
                64,
                64,
                0,
                0,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("CreateNamedPipeW") && name.contains("overlapped")
    ));
    assert!(matches!(
        invoke_x86(
            &mut process,
            "CreateNamedPipeW",
            &[name, 3, PIPE_TYPE_MESSAGE, 1, 64, 64, 0, 0],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("message-type pipes")
    ));
    assert_eq!(
        call_x86(
            &mut process,
            "CreateNamedPipeW",
            &[name, 3, PIPE_READMODE_MESSAGE, 1, 64, 64, 0, 0],
        ),
        INVALID_HANDLE
    );
    assert_eq!(last_error(&process), ERROR_INVALID_PARAMETER);

    let server = create_named_pipe(&mut process, name, PIPE_NOWAIT, 1, 64);
    assert_ne!(server, INVALID_HANDLE as u32);
    assert!(matches!(
        invoke_x86(
            &mut process,
            "ConnectNamedPipe",
            &[server, OVERLAPPED as u32],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("OVERLAPPED")
    ));
    assert!(matches!(
        invoke_x86(
            &mut process,
            "CreateFileW",
            &[
                name,
                GENERIC_READ | GENERIC_WRITE,
                0,
                0,
                3,
                FILE_FLAG_OVERLAPPED,
                0,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("overlapped pipe I/O")
    ));
    let client = open_duplex_client(&mut process, name);
    assert_ne!(client, INVALID_HANDLE as u32);
    assert!(matches!(
        invoke_x86(
            &mut process,
            "ReadFile",
            &[
                client,
                DATA as u32,
                1,
                COUNT_OUT as u32,
                OVERLAPPED as u32,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("OVERLAPPED arguments")
    ));
    assert!(matches!(
        invoke_x86(
            &mut process,
            "WriteFile",
            &[
                client,
                DATA as u32,
                1,
                COUNT_OUT as u32,
                OVERLAPPED as u32,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("OVERLAPPED arguments")
    ));

    let blocking_name = pipe_name(&mut process, r"\\.\pipe\BlockingPipeFixture");
    let blocking = create_named_pipe(&mut process, blocking_name, 0, 1, 64);
    assert!(matches!(
        invoke_x86(&mut process, "ConnectNamedPipe", &[blocking, 0]),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("schedulable guest thread wait")
    ));
}
