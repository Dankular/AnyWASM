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
                0,
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
    let pipe_id = match process.handles.get(&reader).unwrap() {
        Handle::Pipe { pipe_id, .. } => *pipe_id,
        _ => panic!("read handle must reference a pipe"),
    };
    assert_eq!(
        process.pipes[&pipe_id].capacity_to_server,
        DEFAULT_PIPE_BUFFER_SIZE
    );
    assert_eq!(
        crate::process::x86_import_cleanup_bytes("kernel32", "CreatePipe"),
        Some(16)
    );
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
fn message_type_pipe_preserves_messages_reports_partial_reads_and_peeks_message_state() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\MessagePipeFixture");
    let server = call_x86(
        &mut process,
        "CreateNamedPipeW",
        &[
            name,
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE,
            255,
            1024,
            1024,
            200,
            0,
        ],
    ) as u32;
    assert_ne!(server, INVALID_HANDLE as u32);
    let pipe_id = match process.handles.get(&(server as u64)).unwrap() {
        Handle::Pipe { pipe_id, .. } => *pipe_id,
        _ => panic!("server handle must reference a pipe"),
    };
    assert!(process.pipes[&pipe_id].message_type);
    assert!(process.pipes[&pipe_id].server_message_read_mode);
    assert_eq!(
        crate::process::x86_import_cleanup_bytes("kernel32", "CreateNamedPipeW"),
        Some(32)
    );
    assert_eq!(
        crate::process::x86_import_cleanup_bytes("kernel32", "ConnectNamedPipe"),
        Some(8)
    );
    assert_eq!(
        crate::process::x86_import_cleanup_bytes("kernel32", "DisconnectNamedPipe"),
        Some(4)
    );

    let client = call_x86(
        &mut process,
        "CreateFileW",
        &[name, GENERIC_WRITE, 0, 0, 3, 0, 0],
    ) as u32;
    assert_ne!(client, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_CONNECTED);

    process.write_bytes(DATA, b"first").unwrap();
    process.write_bytes(DATA + 16, b"second").unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[client, DATA as u32, 5, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[client, (DATA + 16) as u32, 6, COUNT_OUT as u32, 0],
        ),
        1
    );

    // FSCTL_PIPE_PEEK exposes both message count and the first message's remaining length.
    assert_eq!(
        call_x86(
            &mut process,
            "DeviceIoControl",
            &[
                server,
                FSCTL_PIPE_PEEK,
                0,
                0,
                (DATA + 0x80) as u32,
                64,
                (DATA + 0x70) as u32,
                0,
            ],
        ),
        1
    );
    assert_eq!(process.rd32(DATA + 0x70).unwrap(), 27);
    assert_eq!(process.rd32(DATA + 0x80 + 8).unwrap(), 2);
    assert_eq!(process.rd32(DATA + 0x80 + 12).unwrap(), 5);
    assert_eq!(
        process.read_bytes(DATA + 0x80 + 16, 11).unwrap(),
        b"firstsecond"
    );

    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 2, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_MORE_DATA);
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 2);
    assert_eq!(process.read_bytes(DATA, 2).unwrap(), b"fi");
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 8, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 3);
    assert_eq!(process.read_bytes(DATA, 3).unwrap(), b"rst");
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 8, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 6);
    assert_eq!(process.read_bytes(DATA, 6).unwrap(), b"second");
    assert_eq!(call_x86(&mut process, "DisconnectNamedPipe", &[server]), 1);
}

#[test]
fn nonblocking_message_pipe_writes_are_atomic_when_capacity_is_exhausted() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\NonblockingMessagePipeFixture");
    let server = call_x86(
        &mut process,
        "CreateNamedPipeW",
        &[
            name,
            3,
            PIPE_NOWAIT | PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE,
            1,
            4,
            4,
            0,
            0,
        ],
    ) as u32;
    let client = open_duplex_client(&mut process, name);
    assert_ne!(server, INVALID_HANDLE as u32);
    assert_ne!(client, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_CONNECTED);

    process.write_bytes(DATA, b"abc").unwrap();
    process.write_bytes(DATA + 16, b"de").unwrap();
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[server, DATA as u32, 3, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 3);
    assert_eq!(
        call_x86(
            &mut process,
            "WriteFile",
            &[server, (DATA + 16) as u32, 2, COUNT_OUT as u32, 0],
        ),
        0
    );
    assert_eq!(last_error(&process), ERROR_NO_DATA);
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 0);
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[client, DATA as u32, 4, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 3);
    assert_eq!(process.read_bytes(DATA, 3).unwrap(), b"abc");
}

#[test]
fn byte_read_mode_consumes_message_lengths_across_write_boundaries() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\ByteReadMessagePipeFixture");
    let server = call_x86(
        &mut process,
        "CreateNamedPipeW",
        &[
            name,
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_MESSAGE,
            1,
            64,
            64,
            0,
            0,
        ],
    ) as u32;
    let client = call_x86(
        &mut process,
        "CreateFileW",
        &[name, GENERIC_WRITE, 0, 0, 3, 0, 0],
    ) as u32;
    assert_ne!(server, INVALID_HANDLE as u32);
    assert_ne!(client, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "ConnectNamedPipe", &[server, 0]), 0);
    assert_eq!(last_error(&process), ERROR_PIPE_CONNECTED);

    process.write_bytes(DATA, b"abc").unwrap();
    process.write_bytes(DATA + 16, b"defg").unwrap();
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
            "WriteFile",
            &[client, (DATA + 16) as u32, 4, COUNT_OUT as u32, 0],
        ),
        1
    );

    // Byte read mode may cross a message boundary. PEEK must then report only the
    // unconsumed tail of the second message.
    assert_eq!(
        call_x86(
            &mut process,
            "ReadFile",
            &[server, DATA as u32, 4, COUNT_OUT as u32, 0],
        ),
        1
    );
    assert_eq!(process.rd32(COUNT_OUT as u64).unwrap(), 4);
    assert_eq!(process.read_bytes(DATA, 4).unwrap(), b"abcd");
    assert_eq!(
        call_x86(
            &mut process,
            "DeviceIoControl",
            &[
                server,
                FSCTL_PIPE_PEEK,
                0,
                0,
                (DATA + 0x80) as u32,
                64,
                (DATA + 0x70) as u32,
                0,
            ],
        ),
        1
    );
    assert_eq!(process.rd32(DATA + 0x70).unwrap(), 19);
    assert_eq!(process.rd32(DATA + 0x80 + 4).unwrap(), 3);
    assert_eq!(process.rd32(DATA + 0x80 + 8).unwrap(), 1);
    assert_eq!(process.rd32(DATA + 0x80 + 12).unwrap(), 3);
    assert_eq!(process.read_bytes(DATA + 0x80 + 16, 3).unwrap(), b"efg");
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
    // Message-type pipes are modeled; message read mode requires message pipe type.
    let message_pipe = call_x86(
        &mut process,
        "CreateNamedPipeW",
        &[name, 3, PIPE_TYPE_MESSAGE, 1, 64, 64, 0, 0],
    ) as u32;
    assert_ne!(message_pipe, INVALID_HANDLE as u32);
    assert_eq!(call_x86(&mut process, "CloseHandle", &[message_pipe]), 1);
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
    assert_eq!(
        invoke_x86(&mut process, "ConnectNamedPipe", &[blocking, 0]),
        Ok(0)
    );
    assert!(matches!(
        process.current_thread_state,
        ThreadState::Blocked(ThreadWait::Multiple { .. })
    ));
}

#[test]
fn blocking_connect_named_pipe_waits_for_client_and_returns_true_on_completion() {
    let mut process = process();
    let main_thread_id = process.current_thread_id;
    let name = pipe_name(&mut process, r"\\.\pipe\BlockingConnectFixture");
    let server = create_named_pipe(&mut process, name, 0, 1, 64);
    assert_ne!(server, INVALID_HANDLE as u32);

    // Keep a guest client thread ready so the synchronous server wait can be suspended and
    // completed by a later CreateFileW call in the same process.
    let client_thread_id = 0x2000;
    process.vmem_next = 0x0010_0000;
    process
        .create_guest_thread(client_thread_id, BASE + 0x700, 0, false)
        .unwrap();

    assert_eq!(
        invoke_x86(&mut process, "ConnectNamedPipe", &[server, 0]),
        Ok(0)
    );
    assert!(matches!(
        process.current_thread_state,
        ThreadState::Blocked(ThreadWait::Multiple { .. })
    ));
    assert!(process.schedule_thread().unwrap());
    assert_eq!(process.current_thread_id, client_thread_id);

    let client = open_duplex_client(&mut process, name);
    assert_ne!(client, INVALID_HANDLE as u32);
    assert!(matches!(
        process
            .guest_threads
            .get(&main_thread_id)
            .map(|thread| &thread.state),
        Some(ThreadState::Ready)
    ));

    process.request_thread_yield();
    assert!(process.schedule_thread().unwrap());
    assert_eq!(process.current_thread_id, main_thread_id);
    assert_eq!(process.cpu.gpr[0], 1, "connect wait returns TRUE");
    let pipe_id = match process.handles.get(&(server as u64)).unwrap() {
        Handle::Pipe { pipe_id, .. } => *pipe_id,
        _ => panic!("server handle must remain a pipe"),
    };
    assert!(process.pipes[&pipe_id].connected);
    assert!(
        process
            .named_kernel_objects
            .keys()
            .all(|name| !name.starts_with(PIPE_CONNECT_EVENT_PREFIX)
                && !name.starts_with(PIPE_CONNECT_DUMMY_PREFIX)),
        "private connect wait objects are released after completion"
    );
}

#[test]
fn closing_server_handle_cancels_a_blocking_connect_wait() {
    let mut process = process();
    let main_thread_id = process.current_thread_id;
    let name = pipe_name(&mut process, r"\\.\pipe\CancelledConnectFixture");
    let server = create_named_pipe(&mut process, name, 0, 1, 64);
    let client_thread_id = 0x2000;
    process.vmem_next = 0x0010_0000;
    process
        .create_guest_thread(client_thread_id, BASE + 0x700, 0, false)
        .unwrap();

    assert_eq!(
        invoke_x86(&mut process, "ConnectNamedPipe", &[server, 0]),
        Ok(0)
    );
    assert!(process.schedule_thread().unwrap());
    assert_eq!(process.current_thread_id, client_thread_id);

    assert_eq!(call_x86(&mut process, "CloseHandle", &[server]), 1);
    assert!(matches!(
        process
            .guest_threads
            .get(&main_thread_id)
            .map(|thread| &thread.state),
        Some(ThreadState::Ready)
    ));
    process.request_thread_yield();
    assert!(process.schedule_thread().unwrap());
    assert_eq!(process.current_thread_id, main_thread_id);
    assert_eq!(process.cpu.gpr[0], WAIT_FAILED);
    assert_eq!(last_error(&process), ERROR_INVALID_HANDLE);
}

#[test]
fn named_pipe_client_custom_security_fails_explicitly_without_connecting() {
    let mut process = process();
    let name = pipe_name(&mut process, r"\\.\pipe\CustomClientSecurityFixture");
    let server = create_named_pipe(&mut process, name, PIPE_NOWAIT, 1, 64);
    assert_ne!(server, INVALID_HANDLE as u32);
    process.wr32(SECURITY_ATTRIBUTES, 12).unwrap();
    process
        .wr32(SECURITY_ATTRIBUTES + 4, (BASE + 0x600) as u32)
        .unwrap();
    process.wr32(SECURITY_ATTRIBUTES + 8, 0).unwrap();

    assert!(matches!(
        invoke_x86(
            &mut process,
            "CreateFileW",
            &[
                name,
                GENERIC_READ | GENERIC_WRITE,
                0,
                SECURITY_ATTRIBUTES as u32,
                3,
                0,
                0,
            ],
        ),
        Err(Stop::Unimplemented { dll, name })
            if dll == "kernel32" && name.contains("custom client security descriptors")
    ));
    let pipe_id = match process.handles.get(&(server as u64)).unwrap() {
        Handle::Pipe { pipe_id, .. } => *pipe_id,
        _ => panic!("server handle must remain a pipe"),
    };
    assert!(!process.pipes[&pipe_id].connected);
    assert_eq!(process.pipes[&pipe_id].client_handles, 0);
}
