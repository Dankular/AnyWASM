//! Deterministic Winsock 2 translation layer.
//!
//! This module models Winsock startup, events, IPv4 address conversion, and an isolated
//! loopback socket network. It never opens host sockets. Requests to communicate with external
//! hosts fail explicitly so the guest cannot mistake an unavailable host network for success.

use crate::process::{Handle, HleFn, Process, Stop, ThreadWait};
use anywasm_cpu::CpuMode;
use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;

const AF_INET: u16 = 2;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_UDP: i32 = 17;
const SOL_SOCKET: i32 = 0xffff;
const INVALID_SOCKET: u64 = u32::MAX as u64;

const WSAEFAULT: i32 = 10014;
const WSAEINVAL: i32 = 10022;
const WSAEMFILE: i32 = 10024;
const WSAEWOULDBLOCK: i32 = 10035;
const WSAENOTSOCK: i32 = 10038;
const WSAEDESTADDRREQ: i32 = 10039;
const WSAEMSGSIZE: i32 = 10040;
const WSAEPROTOTYPE: i32 = 10041;
const WSAENOPROTOOPT: i32 = 10042;
const WSAEPROTONOSUPPORT: i32 = 10043;
const WSAESOCKTNOSUPPORT: i32 = 10044;
const WSAEOPNOTSUPP: i32 = 10045;
const WSAEAFNOSUPPORT: i32 = 10047;
const WSAEADDRINUSE: i32 = 10048;
const WSAEADDRNOTAVAIL: i32 = 10049;
const WSAECONNRESET: i32 = 10054;
const WSAENOBUFS: i32 = 10055;
const WSAEISCONN: i32 = 10056;
const WSAENOTCONN: i32 = 10057;
const WSAESHUTDOWN: i32 = 10058;
const WSAECONNREFUSED: i32 = 10061;
const WSAHOST_NOT_FOUND: i32 = 11001;
const WSANO_DATA: i32 = 11004;
const WSANOTINITIALISED: i32 = 10093;
const WSAVERNOTSUPPORTED: i32 = 10092;
const WSA_INVALID_HANDLE: i32 = 6;

const WSA_WAIT_EVENT_0: u32 = 0;
const WSA_WAIT_TIMEOUT: u32 = 258;
const WSA_WAIT_FAILED: u32 = u32::MAX;
const INFINITE: u32 = u32::MAX;

const FIONBIO: u32 = 0x8004_667e;
const FIONREAD: u32 = 0x4004_667f;
const MAX_STREAM_BUFFER: usize = 256 * 1024;
const MAX_DATAGRAMS: usize = 128;

fn write_u32(p: &mut Process, address: u64, value: u32) -> Result<(), i32> {
    p.mem.write_u32(address, value).map_err(|_| WSAEFAULT)
}

fn write_bytes(p: &mut Process, address: u64, bytes: &[u8]) -> Result<(), i32> {
    p.mem.write_from(address, bytes).map_err(|_| WSAEFAULT)
}

#[derive(Clone, Debug)]
struct Datagram {
    from: (Ipv4Addr, u16),
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SocketState {
    family: u16,
    kind: i32,
    protocol: i32,
    local: Option<(Ipv4Addr, u16)>,
    peer: Option<(Ipv4Addr, u16)>,
    peer_socket: Option<u32>,
    pending_accepts: VecDeque<u32>,
    receive_stream: VecDeque<u8>,
    receive_datagrams: VecDeque<Datagram>,
    options: HashMap<(i32, i32), Vec<u8>>,
    listening: bool,
    nonblocking: bool,
    read_shutdown: bool,
    write_shutdown: bool,
    peer_closed: bool,
    overlapped: bool,
}

impl SocketState {
    fn new(family: u16, kind: i32, protocol: i32, overlapped: bool) -> Self {
        Self {
            family,
            kind,
            protocol,
            local: None,
            peer: None,
            peer_socket: None,
            pending_accepts: VecDeque::new(),
            receive_stream: VecDeque::new(),
            receive_datagrams: VecDeque::new(),
            options: HashMap::new(),
            listening: false,
            nonblocking: false,
            read_shutdown: false,
            write_shutdown: false,
            peer_closed: false,
            overlapped,
        }
    }
}

#[derive(Debug)]
pub struct Ws2State {
    startup_refs: u32,
    next_socket: u32,
    next_ephemeral_port: u16,
    last_error: HashMap<u32, i32>,
    sockets: HashMap<u32, SocketState>,
    inet_ntoa_buffers: HashMap<u32, u64>,
    stable_records: HashMap<String, u64>,
}

impl Default for Ws2State {
    fn default() -> Self {
        Self {
            startup_refs: 0,
            next_socket: 0x8000_0000,
            next_ephemeral_port: 49_152,
            last_error: HashMap::new(),
            sockets: HashMap::new(),
            inet_ntoa_buffers: HashMap::new(),
            stable_records: HashMap::new(),
        }
    }
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "WSAStartup" | "#115" => wsa_startup,
        "WSACleanup" | "#116" => wsa_cleanup,
        "WSAGetLastError" | "#111" => wsa_get_last_error,
        "socket" | "#23" => socket,
        "WSASocketA" | "WSASocketW" => wsa_socket,
        "closesocket" | "#3" => closesocket,
        "bind" | "#2" => bind,
        "listen" | "#13" => listen,
        "accept" | "#1" => accept,
        "connect" | "#4" => connect,
        "shutdown" | "#22" => shutdown,
        "send" | "#19" => send,
        "sendto" | "#20" => sendto,
        "recv" | "#16" => recv,
        "recvfrom" | "#17" => recvfrom,
        "getsockname" | "#6" => getsockname_import,
        "getpeername" | "#5" => getpeername,
        "getsockopt" | "#7" => getsockopt,
        "setsockopt" | "#21" => setsockopt,
        "ioctlsocket" | "#10" => ioctlsocket,
        "select" | "#18" => select,
        "__WSAFDIsSet" | "#151" => wsa_fd_is_set,
        "WSAIoctl" => wsa_ioctl,
        "WSARecv" => wsa_recv,
        "WSARecvFrom" => wsa_recv_from,
        "WSAGetOverlappedResult" => wsa_get_overlapped_result,
        "WSACreateEvent" => wsa_create_event,
        "WSACloseEvent" => wsa_close_event,
        "WSASetEvent" => wsa_set_event,
        "WSAResetEvent" => wsa_reset_event,
        "WSAWaitForMultipleEvents" => wsa_wait_for_multiple_events,
        "htonl" | "#8" => |p| Ok((p.arg(0) as u32).swap_bytes() as u64),
        "htons" | "#9" => |p| Ok((p.arg(0) as u16).swap_bytes() as u64),
        "ntohl" | "#14" => |p| Ok((p.arg(0) as u32).swap_bytes() as u64),
        "ntohs" | "#15" => |p| Ok((p.arg(0) as u16).swap_bytes() as u64),
        "inet_addr" | "#11" => inet_addr,
        "inet_ntoa" | "#12" => inet_ntoa,
        "gethostname" | "#57" => gethostname,
        "gethostbyname" | "#52" => gethostbyname,
        "gethostbyaddr" | "#51" => gethostbyaddr,
        "getprotobyname" | "#53" => getprotobyname,
        _ => return None,
    })
}

/// Number of x86 stack bytes removed by Winsock's WINAPI/stdcall exports.
///
/// Name imports are normally resolved from the generated Windows SDK signatures. Ordinal
/// imports have no name for that lookup, so keep their ABI here next to the ordinal aliases.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    Some(match name {
        "accept" | "#1" => 12,
        "bind" | "#2" => 12,
        "closesocket" | "#3" => 4,
        "connect" | "#4" => 12,
        "getpeername" | "#5" => 12,
        "getsockname" | "#6" => 12,
        "getsockopt" | "#7" => 20,
        "htonl" | "#8" | "htons" | "#9" => 4,
        "ioctlsocket" | "#10" => 12,
        "inet_addr" | "#11" | "inet_ntoa" | "#12" => 4,
        "listen" | "#13" => 8,
        "ntohl" | "#14" | "ntohs" | "#15" => 4,
        "recv" | "#16" => 16,
        "recvfrom" | "#17" => 24,
        "select" | "#18" => 20,
        "send" | "#19" => 16,
        "sendto" | "#20" => 24,
        "setsockopt" | "#21" => 20,
        "shutdown" | "#22" => 8,
        "socket" | "#23" => 12,
        "gethostbyaddr" | "#51" => 12,
        "gethostbyname" | "#52" | "getprotobyname" | "#53" => 4,
        "gethostname" | "#57" => 8,
        "WSAGetLastError" | "#111" | "WSACleanup" | "#116" => 0,
        "WSAStartup" | "#115" => 8,
        "__WSAFDIsSet" | "#151" => 8,
        "WSACreateEvent" => 0,
        "WSACloseEvent" | "WSASetEvent" | "WSAResetEvent" => 4,
        "WSAWaitForMultipleEvents" => 20,
        "WSAGetOverlappedResult" => 20,
        "WSAIoctl" => 36,
        "WSARecv" => 28,
        "WSARecvFrom" => 36,
        "WSASocketA" | "WSASocketW" => 24,
        _ => return None,
    })
}

fn wsa_error(p: &mut Process, error: i32) {
    p.extra.ws2_32.last_error.insert(p.current_thread_id, error);
}

fn fail_socket(p: &mut Process, error: i32) -> u64 {
    wsa_error(p, error);
    INVALID_SOCKET
}

fn socket_error(p: &mut Process, error: i32) -> u64 {
    wsa_error(p, error);
    u32::MAX as u64
}

fn ensure_started(p: &mut Process) -> Result<(), i32> {
    if p.extra.ws2_32.startup_refs == 0 {
        Err(WSANOTINITIALISED)
    } else {
        Ok(())
    }
}

fn alloc_socket_id(p: &mut Process) -> Result<u32, i32> {
    if p.extra.ws2_32.sockets.len() >= 16_384 {
        return Err(WSAEMFILE);
    }
    for _ in 0..16_385 {
        let id = p.extra.ws2_32.next_socket;
        p.extra.ws2_32.next_socket = p.extra.ws2_32.next_socket.wrapping_add(4);
        if id != u32::MAX && !p.extra.ws2_32.sockets.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(WSAEMFILE)
}

fn create_socket(p: &mut Process, family: u16, kind: i32, protocol: i32, overlapped: bool) -> u64 {
    if let Err(error) = ensure_started(p) {
        return fail_socket(p, error);
    }
    if family != AF_INET {
        return fail_socket(p, WSAEAFNOSUPPORT);
    }
    if kind != SOCK_STREAM && kind != SOCK_DGRAM {
        return fail_socket(p, WSAESOCKTNOSUPPORT);
    }
    let protocol = match (kind, protocol) {
        (SOCK_STREAM, 0 | IPPROTO_TCP) => IPPROTO_TCP,
        (SOCK_DGRAM, 0 | IPPROTO_UDP) => IPPROTO_UDP,
        (SOCK_STREAM, _) | (SOCK_DGRAM, _) => return fail_socket(p, WSAEPROTONOSUPPORT),
        _ => unreachable!(),
    };
    let Ok(id) = alloc_socket_id(p) else {
        return fail_socket(p, WSAEMFILE);
    };
    p.extra
        .ws2_32
        .sockets
        .insert(id, SocketState::new(family, kind, protocol, overlapped));
    id as u64
}

fn wsa_startup(p: &mut Process) -> Result<u64, Stop> {
    let version = p.arg(0) as u16;
    let data = p.arg(1);
    let major = (version >> 8) as u8;
    let minor = version as u8;
    let supported = (major == 1 && minor == 1) || (major == 2 && minor <= 2);
    if !supported {
        return Ok(WSAVERNOTSUPPORTED as u32 as u64);
    }
    if data != 0 {
        // x86 WSADATA: four WORDs, a 32-bit vendor pointer, then 257 and 129 bytes.
        let mut bytes = vec![0u8; 400];
        bytes[0..2].copy_from_slice(&version.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x0202u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&0u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&0u16.to_le_bytes());
        bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
        let description = b"AnyWASM isolated IPv4 loopback provider";
        bytes[12..12 + description.len()].copy_from_slice(description);
        let status = b"Winsock 2.2 initialized";
        bytes[269..269 + status.len()].copy_from_slice(status);
        if p.mem.write_from(data, &bytes).is_err() {
            return Ok(WSAEFAULT as u32 as u64);
        }
    }
    p.extra.ws2_32.startup_refs = p.extra.ws2_32.startup_refs.saturating_add(1);
    Ok(0)
}

fn wsa_cleanup(p: &mut Process) -> Result<u64, Stop> {
    if p.extra.ws2_32.startup_refs == 0 {
        wsa_error(p, WSANOTINITIALISED);
        return Ok(u32::MAX as u64);
    }
    p.extra.ws2_32.startup_refs -= 1;
    if p.extra.ws2_32.startup_refs == 0 {
        // Winsock sockets are process resources, but cleanup invalidates the Winsock session.
        // Keep descriptors allocated so a later WSAStartup can use them consistently.
    }
    Ok(0)
}

fn wsa_get_last_error(p: &mut Process) -> Result<u64, Stop> {
    Ok(*p
        .extra
        .ws2_32
        .last_error
        .get(&p.current_thread_id)
        .unwrap_or(&0) as u32 as u64)
}

fn socket(p: &mut Process) -> Result<u64, Stop> {
    let (family, kind, protocol) = (p.arg(0) as u16, p.arg(1) as i32, p.arg(2) as i32);
    Ok(create_socket(p, family, kind, protocol, false))
}

fn wsa_socket(p: &mut Process) -> Result<u64, Stop> {
    let (family, kind, protocol, protocol_info, group, flags) = (
        p.arg(0) as u16,
        p.arg(1) as i32,
        p.arg(2) as i32,
        p.arg(3),
        p.arg(4),
        p.arg(5) as u32,
    );
    if protocol_info != 0 || group != 0 || flags & !0x81 != 0 {
        return Ok(fail_socket(p, WSAEINVAL));
    }
    Ok(create_socket(p, family, kind, protocol, flags & 1 != 0))
}

fn closesocket(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let id = p.arg(0) as u32;
    let Some(state) = p.extra.ws2_32.sockets.remove(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if let Some(peer_id) = state.peer_socket {
        if let Some(peer) = p.extra.ws2_32.sockets.get_mut(&peer_id) {
            peer.peer_closed = true;
        }
    }
    for pending in state.pending_accepts {
        p.extra.ws2_32.sockets.remove(&pending);
    }
    Ok(0)
}

fn alloc_ephemeral_port(p: &mut Process, kind: i32) -> Result<u16, i32> {
    for _ in 0..16_384 {
        let candidate = p.extra.ws2_32.next_ephemeral_port;
        p.extra.ws2_32.next_ephemeral_port = if candidate == u16::MAX {
            49_152
        } else {
            candidate + 1
        };
        let in_use = p.extra.ws2_32.sockets.values().any(|socket| {
            socket.kind == kind && socket.local.is_some_and(|(_, port)| port == candidate)
        });
        if !in_use {
            return Ok(candidate);
        }
    }
    Err(WSAENOBUFS)
}

fn read_sockaddr(p: &Process, address: u64, length: u32) -> Result<(Ipv4Addr, u16), i32> {
    if address == 0 || length < 16 {
        return Err(WSAEFAULT);
    }
    if p.rd16(address).map_err(|_| WSAEFAULT)? != AF_INET {
        return Err(WSAEAFNOSUPPORT);
    }
    let port = u16::from_be_bytes([
        p.rd8(address + 2).map_err(|_| WSAEFAULT)?,
        p.rd8(address + 3).map_err(|_| WSAEFAULT)?,
    ]);
    let ip = Ipv4Addr::new(
        p.rd8(address + 4).map_err(|_| WSAEFAULT)?,
        p.rd8(address + 5).map_err(|_| WSAEFAULT)?,
        p.rd8(address + 6).map_err(|_| WSAEFAULT)?,
        p.rd8(address + 7).map_err(|_| WSAEFAULT)?,
    );
    Ok((ip, port))
}

fn write_sockaddr(
    p: &mut Process,
    address: u64,
    length_ptr: u64,
    endpoint: (Ipv4Addr, u16),
) -> Result<(), i32> {
    if address == 0 || length_ptr == 0 {
        return Err(WSAEFAULT);
    }
    let capacity = p.rd32(length_ptr).map_err(|_| WSAEFAULT)?;
    if capacity < 16 {
        p.wr32(length_ptr, 16).map_err(|_| WSAEFAULT)?;
        return Err(WSAEFAULT);
    }
    let (ip, port) = endpoint;
    p.wr16(address, AF_INET).map_err(|_| WSAEFAULT)?;
    p.wr8(address + 2, (port >> 8) as u8)
        .map_err(|_| WSAEFAULT)?;
    p.wr8(address + 3, port as u8).map_err(|_| WSAEFAULT)?;
    p.write_bytes(address + 4, &ip.octets())
        .map_err(|_| WSAEFAULT)?;
    p.write_bytes(address + 8, &[0; 8]).map_err(|_| WSAEFAULT)?;
    p.wr32(length_ptr, 16).map_err(|_| WSAEFAULT)?;
    Ok(())
}

fn bind(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let id = p.arg(0) as u32;
    let address = p.arg(1);
    let length = p.arg(2) as u32;
    let endpoint = match read_sockaddr(p, address, length) {
        Ok(value) => value,
        Err(error) => return Ok(socket_error(p, error)),
    };
    if !endpoint.0.is_unspecified() && !endpoint.0.is_loopback() {
        return Ok(socket_error(p, WSAEADDRNOTAVAIL));
    }
    let Some(current) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if current.local.is_some() {
        return Ok(socket_error(p, WSAEINVAL));
    }
    let kind = current.kind;
    if p.extra.ws2_32.sockets.values().any(|other| {
        other.kind == kind
            && other.local.is_some_and(|(ip, port)| {
                port == endpoint.1
                    && endpoint.1 != 0
                    && (ip == endpoint.0 || ip.is_unspecified() || endpoint.0.is_unspecified())
            })
    }) {
        return Ok(socket_error(p, WSAEADDRINUSE));
    }
    let port = if endpoint.1 == 0 {
        match alloc_ephemeral_port(p, kind) {
            Ok(value) => value,
            Err(error) => return Ok(socket_error(p, error)),
        }
    } else {
        endpoint.1
    };
    p.extra.ws2_32.sockets.get_mut(&id).unwrap().local = Some((endpoint.0, port));
    Ok(0)
}

fn listen(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, backlog) = (p.arg(0) as u32, p.arg(1) as i32);
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.kind != SOCK_STREAM {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if socket.peer_socket.is_some() {
        return Ok(socket_error(p, WSAEISCONN));
    }
    if socket.local.is_none() {
        let Ok(port) = alloc_ephemeral_port(p, SOCK_STREAM) else {
            return Ok(socket_error(p, WSAENOBUFS));
        };
        p.extra.ws2_32.sockets.get_mut(&id).unwrap().local = Some((Ipv4Addr::UNSPECIFIED, port));
    }
    let socket = p.extra.ws2_32.sockets.get_mut(&id).unwrap();
    socket.listening = true;
    socket
        .options
        .insert((SOL_SOCKET, 0x1009), backlog.max(0).to_le_bytes().to_vec());
    Ok(0)
}

fn connect(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, address, length) = (p.arg(0) as u32, p.arg(1), p.arg(2) as u32);
    if !p.extra.ws2_32.sockets.contains_key(&id) {
        return Ok(socket_error(p, WSAENOTSOCK));
    }
    let remote = match read_sockaddr(p, address, length) {
        Ok(value) => value,
        Err(error) => return Ok(socket_error(p, error)),
    };
    if !remote.0.is_loopback() {
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name: format!(
                "connect to external IPv4 endpoint {}:{} is unavailable in the isolated network",
                remote.0, remote.1
            ),
        });
    }
    let Some(client) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if client.peer_socket.is_some() {
        return Ok(socket_error(p, WSAEISCONN));
    }
    if client.listening {
        return Ok(socket_error(p, WSAEINVAL));
    }
    let kind = client.kind;
    if kind == SOCK_STREAM {
        let listener_id = p
            .extra
            .ws2_32
            .sockets
            .iter()
            .find_map(|(&listener_id, listener)| {
                let matches = listener.listening
                    && listener.kind == SOCK_STREAM
                    && listener.local.is_some_and(|(ip, port)| {
                        port == remote.1 && (ip.is_unspecified() || ip == remote.0)
                    });
                matches.then_some(listener_id)
            });
        let Some(listener_id) = listener_id else {
            return Ok(socket_error(p, WSAECONNREFUSED));
        };
        let backlog = p.extra.ws2_32.sockets[&listener_id]
            .options
            .get(&(SOL_SOCKET, 0x1009))
            .and_then(|value| value.get(..4))
            .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()).max(1) as usize)
            .unwrap_or(1);
        if p.extra.ws2_32.sockets[&listener_id].pending_accepts.len() >= backlog {
            return Ok(socket_error(p, WSAECONNREFUSED));
        }
        let Ok(client_port) = alloc_ephemeral_port(p, SOCK_STREAM) else {
            return Ok(socket_error(p, WSAENOBUFS));
        };
        let Ok(accepted_id) = alloc_socket_id(p) else {
            return Ok(socket_error(p, WSAEMFILE));
        };
        let local = (Ipv4Addr::LOCALHOST, client_port);
        let listener_local = p.extra.ws2_32.sockets[&listener_id].local.unwrap();
        let accepted_local = (
            if listener_local.0.is_unspecified() {
                Ipv4Addr::LOCALHOST
            } else {
                listener_local.0
            },
            listener_local.1,
        );
        let (family, protocol, overlapped) = {
            let client = p.extra.ws2_32.sockets.get_mut(&id).unwrap();
            client.local = Some(local);
            client.peer = Some((accepted_local.0, accepted_local.1));
            client.peer_socket = Some(accepted_id);
            (client.family, client.protocol, client.overlapped)
        };
        let mut accepted = SocketState::new(family, SOCK_STREAM, protocol, overlapped);
        accepted.local = Some(accepted_local);
        accepted.peer = Some(local);
        accepted.peer_socket = Some(id);
        p.extra.ws2_32.sockets.insert(accepted_id, accepted);
        p.extra
            .ws2_32
            .sockets
            .get_mut(&listener_id)
            .unwrap()
            .pending_accepts
            .push_back(accepted_id);
        Ok(0)
    } else if kind == SOCK_DGRAM {
        let local_port = match p.extra.ws2_32.sockets[&id].local {
            Some((_, port)) => port,
            None => match alloc_ephemeral_port(p, SOCK_DGRAM) {
                Ok(value) => value,
                Err(error) => return Ok(socket_error(p, error)),
            },
        };
        let socket = p.extra.ws2_32.sockets.get_mut(&id).unwrap();
        socket.local = Some((Ipv4Addr::LOCALHOST, local_port));
        socket.peer = Some(remote);
        Ok(0)
    } else {
        Ok(socket_error(p, WSAEPROTOTYPE))
    }
}

fn accept(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(fail_socket(p, error));
    }
    let (listener_id, address, length_ptr) = (p.arg(0) as u32, p.arg(1), p.arg(2));
    let Some(listener) = p.extra.ws2_32.sockets.get(&listener_id) else {
        return Ok(fail_socket(p, WSAENOTSOCK));
    };
    if !listener.listening {
        return Ok(fail_socket(p, WSAEINVAL));
    }
    let Some(accepted) = p
        .extra
        .ws2_32
        .sockets
        .get_mut(&listener_id)
        .unwrap()
        .pending_accepts
        .pop_front()
    else {
        if p.extra.ws2_32.sockets[&listener_id].nonblocking {
            return Ok(fail_socket(p, WSAEWOULDBLOCK));
        }
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name:
                "blocking accept with no pending loopback connection cannot suspend a guest thread"
                    .into(),
        });
    };
    let peer = p.extra.ws2_32.sockets[&accepted]
        .peer
        .unwrap_or((Ipv4Addr::LOCALHOST, 0));
    if address != 0 || length_ptr != 0 {
        if let Err(error) = write_sockaddr(p, address, length_ptr, peer) {
            p.extra
                .ws2_32
                .sockets
                .get_mut(&listener_id)
                .unwrap()
                .pending_accepts
                .push_front(accepted);
            return Ok(fail_socket(p, error));
        }
    }
    Ok(accepted as u64)
}

fn shutdown(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, how) = (p.arg(0) as u32, p.arg(1) as i32);
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.peer_socket.is_none() {
        return Ok(socket_error(p, WSAENOTCONN));
    }
    let peer_id = socket.peer_socket;
    match how {
        0 => p.extra.ws2_32.sockets.get_mut(&id).unwrap().read_shutdown = true,
        1 => p.extra.ws2_32.sockets.get_mut(&id).unwrap().write_shutdown = true,
        2 => {
            let socket = p.extra.ws2_32.sockets.get_mut(&id).unwrap();
            socket.read_shutdown = true;
            socket.write_shutdown = true;
        }
        _ => return Ok(socket_error(p, WSAEINVAL)),
    }
    if p.extra.ws2_32.sockets[&id].write_shutdown {
        if let Some(peer) = peer_id.and_then(|peer| p.extra.ws2_32.sockets.get_mut(&peer)) {
            peer.peer_closed = true;
        }
    }
    Ok(0)
}

fn send(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, buffer, length, flags) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as usize,
        p.arg(3) as i32,
    );
    if flags & !0x4 != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.kind != SOCK_STREAM {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if socket.peer_socket.is_none() {
        return Ok(socket_error(p, WSAENOTCONN));
    }
    if socket.write_shutdown {
        return Ok(socket_error(p, WSAESHUTDOWN));
    }
    if length == 0 {
        return Ok(0);
    }
    let Some(peer_id) = socket.peer_socket else {
        unreachable!()
    };
    let Some(peer) = p.extra.ws2_32.sockets.get(&peer_id) else {
        return Ok(socket_error(p, WSAECONNRESET));
    };
    if peer.read_shutdown || peer.peer_closed {
        return Ok(socket_error(p, WSAECONNRESET));
    }
    let free = MAX_STREAM_BUFFER.saturating_sub(peer.receive_stream.len());
    if free == 0 {
        return if p.extra.ws2_32.sockets[&id].nonblocking {
            Ok(socket_error(p, WSAEWOULDBLOCK))
        } else {
            Err(Stop::Unimplemented {
                dll: "ws2_32".into(),
                name: "blocking send cannot wait for peer receive-buffer space".into(),
            })
        };
    }
    let count = length.min(free).min(64 * 1024);
    let bytes = match p.mem.read_vec(buffer, count) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(socket_error(p, WSAEFAULT)),
    };
    p.extra
        .ws2_32
        .sockets
        .get_mut(&peer_id)
        .unwrap()
        .receive_stream
        .extend(bytes);
    Ok(count as u64)
}

fn sendto(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, buffer, length, flags, address, address_length) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as usize,
        p.arg(3) as i32,
        p.arg(4),
        p.arg(5) as u32,
    );
    if flags & !0x4 != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.kind != SOCK_DGRAM {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if length > 65_507 {
        return Ok(socket_error(p, WSAEMSGSIZE));
    }
    let destination = if address == 0 {
        socket.peer
    } else {
        match read_sockaddr(p, address, address_length) {
            Ok(value) => Some(value),
            Err(error) => return Ok(socket_error(p, error)),
        }
    };
    let Some(destination) = destination else {
        return Ok(socket_error(p, WSAEDESTADDRREQ));
    };
    if !destination.0.is_loopback() {
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name: format!(
                "sendto external IPv4 endpoint {}:{} is unavailable in the isolated network",
                destination.0, destination.1
            ),
        });
    }
    let local = if let Some(local) = p.extra.ws2_32.sockets[&id].local {
        local
    } else {
        let Ok(port) = alloc_ephemeral_port(p, SOCK_DGRAM) else {
            return Ok(socket_error(p, WSAENOBUFS));
        };
        let local = (Ipv4Addr::LOCALHOST, port);
        p.extra.ws2_32.sockets.get_mut(&id).unwrap().local = Some(local);
        local
    };
    let bytes = match p.mem.read_vec(buffer, length) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(socket_error(p, WSAEFAULT)),
    };
    if let Some(target_id) = p
        .extra
        .ws2_32
        .sockets
        .iter()
        .find_map(|(&target_id, target)| {
            (target.kind == SOCK_DGRAM
                && target.local.is_some_and(|(ip, port)| {
                    port == destination.1 && (ip == destination.0 || ip.is_unspecified())
                }))
            .then_some(target_id)
        })
    {
        let target = p.extra.ws2_32.sockets.get_mut(&target_id).unwrap();
        if target.receive_datagrams.len() >= MAX_DATAGRAMS {
            return Ok(socket_error(p, WSAEWOULDBLOCK));
        }
        target
            .receive_datagrams
            .push_back(Datagram { from: local, bytes });
    }
    // UDP send completion confirms local submission only; it does not claim remote delivery.
    Ok(length as u64)
}

fn recv(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, buffer, length, flags) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as usize,
        p.arg(3) as i32,
    );
    if flags & !0x2 != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.kind != SOCK_STREAM {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if socket.peer_socket.is_none() {
        return Ok(socket_error(p, WSAENOTCONN));
    }
    if socket.read_shutdown {
        return Ok(socket_error(p, WSAESHUTDOWN));
    }
    if length == 0 {
        return Ok(0);
    }
    let available = socket.receive_stream.len();
    if available == 0 {
        if socket.peer_closed {
            return Ok(0);
        }
        if socket.nonblocking {
            return Ok(socket_error(p, WSAEWOULDBLOCK));
        }
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name: "blocking recv has no loopback data and host network input is unavailable".into(),
        });
    }
    let count = length.min(available);
    let bytes: Vec<u8> = p.extra.ws2_32.sockets[&id]
        .receive_stream
        .iter()
        .take(count)
        .copied()
        .collect();
    if write_bytes(p, buffer, &bytes).is_err() {
        return Ok(socket_error(p, WSAEFAULT));
    }
    if flags & 0x2 == 0 {
        p.extra
            .ws2_32
            .sockets
            .get_mut(&id)
            .unwrap()
            .receive_stream
            .drain(..count);
    }
    Ok(count as u64)
}

fn recvfrom(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, buffer, length, flags, from, from_length_ptr) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as usize,
        p.arg(3) as i32,
        p.arg(4),
        p.arg(5),
    );
    if flags & !0x2 != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if socket.kind == SOCK_STREAM {
        // Winsock permits recvfrom on a connected stream; source-address outputs are ignored.
        return recv(p);
    }
    if socket.read_shutdown {
        return Ok(socket_error(p, WSAESHUTDOWN));
    }
    let Some(datagram) = p.extra.ws2_32.sockets[&id]
        .receive_datagrams
        .front()
        .cloned()
    else {
        if p.extra.ws2_32.sockets[&id].nonblocking {
            return Ok(socket_error(p, WSAEWOULDBLOCK));
        }
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name:
                "blocking recvfrom has no loopback datagram and host network input is unavailable"
                    .into(),
        });
    };
    let copied = length.min(datagram.bytes.len());
    if from != 0 || from_length_ptr != 0 {
        if let Err(error) = write_sockaddr(p, from, from_length_ptr, datagram.from) {
            return Ok(socket_error(p, error));
        }
    }
    if write_bytes(p, buffer, &datagram.bytes[..copied]).is_err() {
        return Ok(socket_error(p, WSAEFAULT));
    }
    p.extra
        .ws2_32
        .sockets
        .get_mut(&id)
        .unwrap()
        .receive_datagrams
        .pop_front();
    Ok(copied as u64)
}

fn endpoint_for(p: &mut Process, id: u32, peer: bool) -> Result<(Ipv4Addr, u16), i32> {
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Err(WSAENOTSOCK);
    };
    if peer {
        socket.peer.ok_or(WSAENOTCONN)
    } else {
        Ok(socket.local.unwrap_or((Ipv4Addr::UNSPECIFIED, 0)))
    }
}

fn getsockname(p: &mut Process, peer: bool) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, address, length) = (p.arg(0) as u32, p.arg(1), p.arg(2));
    let endpoint = match endpoint_for(p, id, peer) {
        Ok(value) => value,
        Err(error) => return Ok(socket_error(p, error)),
    };
    if let Err(error) = write_sockaddr(p, address, length, endpoint) {
        return Ok(socket_error(p, error));
    }
    Ok(0)
}

fn getsockname_import(p: &mut Process) -> Result<u64, Stop> {
    getsockname(p, false)
}
fn getpeername(p: &mut Process) -> Result<u64, Stop> {
    getsockname(p, true)
}

fn option_default(socket: &SocketState, level: i32, name: i32) -> Option<Vec<u8>> {
    if let Some(value) = socket.options.get(&(level, name)) {
        return Some(value.clone());
    }
    if level == SOL_SOCKET {
        let value = match name {
            0x0004 | 0x0008 | 0x0020 | 0x0080 => 0i32,
            0x1001 | 0x1002 => 65_536i32,
            0x1007 => 0i32,
            0x1008 => i32::from(socket.peer_socket.is_some()),
            0x1009 => socket.kind,
            _ => return None,
        };
        return Some(value.to_le_bytes().to_vec());
    }
    if level == IPPROTO_TCP && name == 1 {
        return Some(0i32.to_le_bytes().to_vec());
    }
    None
}

fn getsockopt(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, level, name, value_ptr, length_ptr) = (
        p.arg(0) as u32,
        p.arg(1) as i32,
        p.arg(2) as i32,
        p.arg(3),
        p.arg(4),
    );
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    let Some(value) = option_default(socket, level, name) else {
        return Ok(socket_error(p, WSAENOPROTOOPT));
    };
    if value_ptr == 0 || length_ptr == 0 {
        return Ok(socket_error(p, WSAEFAULT));
    }
    let capacity = match p.rd32(length_ptr) {
        Ok(length) => length as usize,
        Err(_) => return Ok(socket_error(p, WSAEFAULT)),
    };
    if capacity < value.len() {
        return Ok(socket_error(p, WSAEFAULT));
    }
    if write_bytes(p, value_ptr, &value).is_err()
        || write_u32(p, length_ptr, value.len() as u32).is_err()
    {
        return Ok(socket_error(p, WSAEFAULT));
    }
    Ok(0)
}

fn setsockopt(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, level, name, value_ptr, length) = (
        p.arg(0) as u32,
        p.arg(1) as i32,
        p.arg(2) as i32,
        p.arg(3),
        p.arg(4) as usize,
    );
    let Some(_socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    let mutable = (level == SOL_SOCKET
        && matches!(
            name,
            0x0004 | 0x0008 | 0x0020 | 0x0080 | 0x1001 | 0x1002 | 0x1005 | 0x1006 | 0x1010
        ))
        || (level == IPPROTO_TCP && name == 1);
    if !mutable {
        return Ok(socket_error(
            p,
            if level == SOL_SOCKET || level == IPPROTO_TCP {
                WSAENOPROTOOPT
            } else {
                WSAEOPNOTSUPP
            },
        ));
    }
    if value_ptr == 0 || length == 0 || length > 256 {
        return Ok(socket_error(p, WSAEFAULT));
    }
    if matches!(
        name,
        0x0004 | 0x0008 | 0x0020 | 0x0080 | 0x1001 | 0x1002 | 0x1005 | 0x1006 | 0x1010
    ) && length != 4
    {
        return Ok(socket_error(p, WSAEINVAL));
    }
    let value = match p.mem.read_vec(value_ptr, length) {
        Ok(value) => value,
        Err(_) => return Ok(socket_error(p, WSAEFAULT)),
    };
    p.extra
        .ws2_32
        .sockets
        .get_mut(&id)
        .unwrap()
        .options
        .insert((level, name), value);
    Ok(0)
}

fn ioctlsocket(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, command, value_ptr) = (p.arg(0) as u32, p.arg(1) as u32, p.arg(2));
    let Some(socket) = p.extra.ws2_32.sockets.get(&id) else {
        return Ok(socket_error(p, WSAENOTSOCK));
    };
    if value_ptr == 0 {
        return Ok(socket_error(p, WSAEFAULT));
    }
    match command {
        FIONBIO => {
            let value = match p.rd32(value_ptr) {
                Ok(value) => value,
                Err(_) => return Ok(socket_error(p, WSAEFAULT)),
            };
            p.extra.ws2_32.sockets.get_mut(&id).unwrap().nonblocking = value != 0;
            Ok(0)
        }
        FIONREAD => {
            let available = if socket.kind == SOCK_STREAM {
                socket.receive_stream.len()
            } else {
                socket
                    .receive_datagrams
                    .front()
                    .map(|d| d.bytes.len())
                    .unwrap_or(0)
            };
            if write_u32(p, value_ptr, available.min(u32::MAX as usize) as u32).is_err() {
                return Ok(socket_error(p, WSAEFAULT));
            }
            Ok(0)
        }
        _ => Ok(socket_error(p, WSAEOPNOTSUPP)),
    }
}

fn wsa_ioctl(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, command, input, input_len, output, output_len, bytes_returned, overlapped, completion) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as usize,
        p.arg(4),
        p.arg(5) as usize,
        p.arg(6),
        p.arg(7),
        p.arg(8),
    );
    if !p.extra.ws2_32.sockets.contains_key(&id) {
        return Ok(socket_error(p, WSAENOTSOCK));
    }
    if overlapped != 0 || completion != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if bytes_returned == 0 {
        return Ok(socket_error(p, WSAEFAULT));
    }
    match command {
        FIONBIO if input_len >= 4 && input != 0 => {
            let nonblocking = match p.rd32(input) {
                Ok(v) => v != 0,
                Err(_) => return Ok(socket_error(p, WSAEFAULT)),
            };
            p.extra.ws2_32.sockets.get_mut(&id).unwrap().nonblocking = nonblocking;
            if write_u32(p, bytes_returned, 0).is_err() {
                return Ok(socket_error(p, WSAEFAULT));
            }
            Ok(0)
        }
        FIONREAD if output_len >= 4 && output != 0 => {
            let socket = &p.extra.ws2_32.sockets[&id];
            let count = if socket.kind == SOCK_STREAM {
                socket.receive_stream.len()
            } else {
                socket
                    .receive_datagrams
                    .front()
                    .map(|d| d.bytes.len())
                    .unwrap_or(0)
            };
            if write_u32(p, output, count.min(u32::MAX as usize) as u32).is_err()
                || write_u32(p, bytes_returned, 4).is_err()
            {
                return Ok(socket_error(p, WSAEFAULT));
            }
            Ok(0)
        }
        FIONBIO | FIONREAD => Ok(socket_error(p, WSAEFAULT)),
        _ => Ok(socket_error(p, WSAEOPNOTSUPP)),
    }
}

fn select(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (read_set, write_set, except_set, timeout) = (p.arg(1), p.arg(2), p.arg(3), p.arg(4));
    let mut read = match read_fdset(p, read_set) {
        Ok(v) => v,
        Err(error) => return Ok(socket_error(p, error)),
    };
    let mut write = match read_fdset(p, write_set) {
        Ok(v) => v,
        Err(error) => return Ok(socket_error(p, error)),
    };
    let mut except = match read_fdset(p, except_set) {
        Ok(v) => v,
        Err(error) => return Ok(socket_error(p, error)),
    };
    for &id in read.iter().chain(write.iter()).chain(except.iter()) {
        if !p.extra.ws2_32.sockets.contains_key(&id) {
            return Ok(socket_error(p, WSAENOTSOCK));
        }
    }
    read.retain(|id| {
        let socket = &p.extra.ws2_32.sockets[id];
        socket.listening && !socket.pending_accepts.is_empty()
            || !socket.receive_stream.is_empty()
            || !socket.receive_datagrams.is_empty()
            || socket.peer_closed
    });
    write.retain(|id| {
        let socket = &p.extra.ws2_32.sockets[id];
        socket.kind == SOCK_DGRAM
            || socket.peer_socket.is_some_and(|peer| {
                p.extra
                    .ws2_32
                    .sockets
                    .get(&peer)
                    .is_some_and(|s| s.receive_stream.len() < MAX_STREAM_BUFFER && !s.read_shutdown)
            })
    });
    except.retain(|_| false);
    if write_fdset(p, read_set, &read).is_err()
        || write_fdset(p, write_set, &write).is_err()
        || write_fdset(p, except_set, &except).is_err()
    {
        return Ok(socket_error(p, WSAEFAULT));
    }
    let ready = read.len() + write.len() + except.len();
    if ready != 0 {
        return Ok(ready as u64);
    }
    if timeout != 0 {
        let seconds = match p.rd32(timeout) {
            Ok(value) => value as i32,
            Err(_) => return Ok(socket_error(p, WSAEFAULT)),
        };
        let micros = match p.rd32(timeout + 4) {
            Ok(value) => value as i32,
            Err(_) => return Ok(socket_error(p, WSAEFAULT)),
        };
        if seconds < 0 || micros < 0 || micros >= 1_000_000 {
            return Ok(socket_error(p, WSAEINVAL));
        }
        let ticks_100ns = (seconds as u64)
            .saturating_mul(10_000_000)
            .saturating_add((micros as u64).saturating_mul(10));
        p.advance_time_100ns(ticks_100ns);
    }
    Ok(0)
}

fn read_fdset(p: &Process, address: u64) -> Result<Vec<u32>, i32> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let count = p.rd32(address).map_err(|_| WSAEFAULT)? as usize;
    if count > 64 {
        return Err(WSAEINVAL);
    }
    (0..count)
        .map(|i| p.rd32(address + 4 + i as u64 * 4).map_err(|_| WSAEFAULT))
        .collect()
}

fn write_fdset(p: &mut Process, address: u64, sockets: &[u32]) -> Result<(), i32> {
    if address == 0 {
        return Ok(());
    }
    write_u32(p, address, sockets.len() as u32)?;
    for (index, id) in sockets.iter().enumerate() {
        write_u32(p, address + 4 + index as u64 * 4, *id)?;
    }
    Ok(())
}

fn wsa_fd_is_set(p: &mut Process) -> Result<u64, Stop> {
    let (id, address) = (p.arg(0) as u32, p.arg(1));
    if address == 0 {
        return Ok(0);
    }
    let count = p.rd32(address)? as usize;
    if count > 64 {
        return Ok(0);
    }
    for index in 0..count {
        if p.rd32(address + 4 + index as u64 * 4)? == id {
            return Ok(1);
        }
    }
    Ok(0)
}

fn wsa_create_event(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        wsa_error(p, error);
        return Ok(0);
    }
    Ok(p.new_handle(Handle::Event {
        signaled: false,
        manual: true,
    }))
}

fn wsa_close_event(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    if !matches!(p.handles.get(&handle), Some(Handle::Event { .. })) {
        wsa_error(p, WSA_INVALID_HANDLE);
        return Ok(0);
    }
    p.handles.remove(&handle);
    Ok(1)
}

fn wsa_set_event(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    match p.handles.get_mut(&handle) {
        Some(Handle::Event { signaled, .. }) => {
            *signaled = true;
            Ok(1)
        }
        _ => {
            wsa_error(p, WSA_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn wsa_reset_event(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    match p.handles.get_mut(&handle) {
        Some(Handle::Event { signaled, .. }) => {
            *signaled = false;
            Ok(1)
        }
        _ => {
            wsa_error(p, WSA_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn wsa_wait_for_multiple_events(p: &mut Process) -> Result<u64, Stop> {
    let (count, events, wait_all, timeout, alertable) = (
        p.arg(0) as usize,
        p.arg(1),
        p.arg(2) != 0,
        p.arg(3) as u32,
        p.arg(4) != 0,
    );
    if count == 0 || count > 64 || events == 0 {
        wsa_error(p, WSAEINVAL);
        return Ok(WSA_WAIT_FAILED as u64);
    }
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let mut handles = Vec::with_capacity(count);
    for index in 0..count {
        let address = events + index as u64 * ptr_size;
        let handle = if ptr_size == 4 {
            p.rd32(address).map(u64::from)
        } else {
            p.rd64(address)
        };
        match handle {
            Ok(handle) => handles.push(handle),
            Err(_) => {
                wsa_error(p, WSAEFAULT);
                return Ok(WSA_WAIT_FAILED as u64);
            }
        }
    }
    match p.try_wait_handles(&handles, wait_all, p.current_thread_id) {
        Ok(Some(index)) => return Ok(WSA_WAIT_EVENT_0 as u64 + index),
        Err(_) => {
            wsa_error(p, WSA_INVALID_HANDLE);
            return Ok(WSA_WAIT_FAILED as u64);
        }
        Ok(None) => {}
    }
    if timeout == 0 {
        return Ok(WSA_WAIT_TIMEOUT as u64);
    }
    if alertable {
        return Err(Stop::Unimplemented {
            dll: "ws2_32".into(),
            name: "alertable WSAWaitForMultipleEvents requires APC delivery".into(),
        });
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented { dll: "ws2_32".into(), name: "blocking WSAWaitForMultipleEvents cannot be suspended from a nested synchronous callback".into() });
    }
    let deadline = (timeout != INFINITE).then(|| {
        p.now_ticks()
            .saturating_add((timeout as u64).saturating_mul(100_000))
    });
    p.block_current_thread(ThreadWait::Multiple {
        handles,
        wait_all,
        deadline,
    });
    Ok(WSA_WAIT_TIMEOUT as u64)
}

fn wsa_get_overlapped_result(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        wsa_error(p, error);
        return Ok(0);
    }
    let (socket, overlapped, bytes, wait, flags) =
        (p.arg(0) as u32, p.arg(1), p.arg(2), p.arg(3) != 0, p.arg(4));
    if !p.extra.ws2_32.sockets.contains_key(&socket) {
        wsa_error(p, WSAENOTSOCK);
        return Ok(0);
    }
    if overlapped == 0 || bytes == 0 || flags == 0 {
        wsa_error(p, WSAEFAULT);
        return Ok(0);
    }
    let _ = (wait, flags);
    // WSARecv/WSARecvFrom do not accept pending requests in this provider. A non-null OVERLAPPED
    // block therefore cannot identify a request completed by this implementation.
    wsa_error(p, WSAEINVAL);
    Ok(0)
}

fn read_wsabufs(p: &Process, address: u64, count: usize) -> Result<Vec<(u64, usize)>, i32> {
    if count == 0 || count > 64 || address == 0 {
        return Err(WSAEINVAL);
    }
    let x86 = p.cpu.mode == CpuMode::X86_32;
    let size = if x86 { 8 } else { 16 };
    let mut result = Vec::with_capacity(count);
    let mut total = 0usize;
    for index in 0..count {
        let base = address + index as u64 * size;
        let length = p.rd32(base).map_err(|_| WSAEFAULT)? as usize;
        let buffer = if x86 {
            p.rd32(base + 4).map(u64::from)
        } else {
            p.rd64(base + 8)
        }
        .map_err(|_| WSAEFAULT)?;
        total = total.checked_add(length).ok_or(WSAEMSGSIZE)?;
        if total > 1 << 20 {
            return Err(WSAEMSGSIZE);
        }
        if length != 0 && buffer == 0 {
            return Err(WSAEFAULT);
        }
        result.push((buffer, length));
    }
    Ok(result)
}

fn wsa_recv_common(p: &mut Process, from: bool) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (id, buffers, count, bytes_out, flags_out) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as usize,
        p.arg(3),
        p.arg(4),
    );
    let (from_address, from_length_ptr, overlapped, completion) = if from {
        (p.arg(5), p.arg(6), p.arg(7), p.arg(8))
    } else {
        (0, 0, p.arg(5), p.arg(6))
    };
    if !p.extra.ws2_32.sockets.contains_key(&id) {
        return Ok(socket_error(p, WSAENOTSOCK));
    }
    if overlapped != 0 || completion != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    if bytes_out == 0 || flags_out == 0 {
        return Ok(socket_error(p, WSAEFAULT));
    }
    let input_flags = match p.rd32(flags_out) {
        Ok(flags) => flags,
        Err(_) => return Ok(socket_error(p, WSAEFAULT)),
    };
    if input_flags & !0x2 != 0 {
        return Ok(socket_error(p, WSAEOPNOTSUPP));
    }
    let peek = input_flags & 0x2 != 0;
    let buffers = match read_wsabufs(p, buffers, count) {
        Ok(buffers) => buffers,
        Err(error) => return Ok(socket_error(p, error)),
    };
    let capacity = buffers.iter().map(|(_, length)| *length).sum::<usize>();
    let socket = &p.extra.ws2_32.sockets[&id];
    if socket.read_shutdown {
        return Ok(socket_error(p, WSAESHUTDOWN));
    }
    let datagram_mode = socket.kind == SOCK_DGRAM;
    let (payload, source) = if datagram_mode {
        let Some(datagram) = socket.receive_datagrams.front() else {
            if socket.nonblocking {
                return Ok(socket_error(p, WSAEWOULDBLOCK));
            }
            return Err(Stop::Unimplemented { dll: "ws2_32".into(), name: "blocking WSARecv has no loopback datagram and host network input is unavailable".into() });
        };
        (
            datagram.bytes[..capacity.min(datagram.bytes.len())].to_vec(),
            Some(datagram.from),
        )
    } else {
        if socket.peer_socket.is_none() {
            return Ok(socket_error(p, WSAENOTCONN));
        }
        if socket.receive_stream.is_empty() {
            if socket.peer_closed {
                (Vec::new(), None)
            } else if socket.nonblocking {
                return Ok(socket_error(p, WSAEWOULDBLOCK));
            } else {
                return Err(Stop::Unimplemented { dll: "ws2_32".into(), name: "blocking WSARecv has no loopback data and host network input is unavailable".into() });
            }
        } else {
            (
                socket
                    .receive_stream
                    .iter()
                    .take(capacity)
                    .copied()
                    .collect(),
                None,
            )
        }
    };
    if from && (from_address != 0 || from_length_ptr != 0) {
        let Some(source) = source else {
            return Ok(socket_error(p, WSAEFAULT));
        };
        if let Err(error) = write_sockaddr(p, from_address, from_length_ptr, source) {
            return Ok(socket_error(p, error));
        }
    }
    let mut offset = 0usize;
    for (buffer, length) in &buffers {
        let copied = (*length).min(payload.len().saturating_sub(offset));
        if copied != 0 {
            if write_bytes(p, *buffer, &payload[offset..offset + copied]).is_err() {
                return Ok(socket_error(p, WSAEFAULT));
            }
        }
        offset += copied;
    }
    if write_u32(p, bytes_out, payload.len() as u32).is_err() || write_u32(p, flags_out, 0).is_err()
    {
        return Ok(socket_error(p, WSAEFAULT));
    }
    if datagram_mode && !peek {
        p.extra
            .ws2_32
            .sockets
            .get_mut(&id)
            .unwrap()
            .receive_datagrams
            .pop_front();
    } else if !peek {
        p.extra
            .ws2_32
            .sockets
            .get_mut(&id)
            .unwrap()
            .receive_stream
            .drain(..payload.len());
    }
    Ok(0)
}

fn wsa_recv(p: &mut Process) -> Result<u64, Stop> {
    wsa_recv_common(p, false)
}
fn wsa_recv_from(p: &mut Process) -> Result<u64, Stop> {
    wsa_recv_common(p, true)
}

fn inet_addr(p: &mut Process) -> Result<u64, Stop> {
    let address = p.arg(0);
    if address == 0 {
        return Ok(u32::MAX as u64);
    }
    let text = match p.read_cstr(address) {
        Ok(text) => text,
        Err(_) => return Ok(u32::MAX as u64),
    };
    Ok(text
        .parse::<Ipv4Addr>()
        .map(|ip| u32::from_ne_bytes(ip.octets()) as u64)
        .unwrap_or(u32::MAX as u64))
}

fn inet_ntoa(p: &mut Process) -> Result<u64, Stop> {
    let ip = Ipv4Addr::from((p.arg(0) as u32).to_ne_bytes());
    let id = p.current_thread_id;
    let buffer = if let Some(buffer) = p.extra.ws2_32.inet_ntoa_buffers.get(&id) {
        *buffer
    } else {
        let buffer = p.alloc_virtual(16, anywasm_cpu::mem::PROT_RW);
        p.extra.ws2_32.inet_ntoa_buffers.insert(id, buffer);
        buffer
    };
    p.write_cstr(buffer, &ip.to_string())?;
    Ok(buffer)
}

fn gethostname(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        return Ok(socket_error(p, error));
    }
    let (buffer, length) = (p.arg(0), p.arg(1) as usize);
    const HOSTNAME: &[u8] = b"ANYWASM";
    if buffer == 0 || length <= HOSTNAME.len() {
        return Ok(socket_error(p, WSAEFAULT));
    }
    if write_bytes(p, buffer, &[HOSTNAME, &[0]].concat().as_slice()).is_err() {
        return Ok(socket_error(p, WSAEFAULT));
    }
    Ok(0)
}

fn stable_record(p: &mut Process, key: &str) -> u64 {
    if let Some(&address) = p.extra.ws2_32.stable_records.get(key) {
        return address;
    }
    let address = p.alloc_virtual(0x1000, anywasm_cpu::mem::PROT_RW);
    p.extra
        .ws2_32
        .stable_records
        .insert(key.to_string(), address);
    address
}

fn write_guest_ptr(p: &mut Process, address: u64, value: u64) -> Result<(), Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        p.wr32(address, value as u32)
    } else {
        p.wr64(address, value)
    }
}

fn create_hostent(p: &mut Process, name: &str, ip: Ipv4Addr) -> Result<u64, Stop> {
    let base = stable_record(p, &format!("hostent:{}:{}", name.to_ascii_lowercase(), ip));
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let name_address = base + 64;
    let aliases = base + 320;
    let address_list = base + 352;
    let address_data = base + 384;
    let family_offset = ptr_size * 2;
    let length_offset = family_offset + 2;
    let list_offset = if ptr_size == 4 { 12 } else { 24 };
    p.write_cstr(name_address, name)?;
    write_guest_ptr(p, base, name_address)?;
    write_guest_ptr(p, base + ptr_size, aliases)?;
    p.wr16(base + family_offset, AF_INET)?;
    p.wr16(base + length_offset, 4)?;
    write_guest_ptr(p, base + list_offset, address_list)?;
    write_guest_ptr(p, aliases, 0)?;
    write_guest_ptr(p, address_list, address_data)?;
    write_guest_ptr(p, address_list + ptr_size, 0)?;
    p.write_bytes(address_data, &ip.octets())?;
    Ok(base)
}

fn gethostbyname(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        wsa_error(p, error);
        return Ok(0);
    }
    let address = p.arg(0);
    if address == 0 {
        wsa_error(p, WSAEFAULT);
        return Ok(0);
    }
    let name = match p.read_cstr(address) {
        Ok(name) => name,
        Err(_) => {
            wsa_error(p, WSAEFAULT);
            return Ok(0);
        }
    };
    if name.eq_ignore_ascii_case("localhost") || name.eq_ignore_ascii_case("anywasm") {
        return create_hostent(
            p,
            if name.eq_ignore_ascii_case("localhost") {
                "localhost"
            } else {
                "ANYWASM"
            },
            Ipv4Addr::LOCALHOST,
        );
    }
    wsa_error(p, WSAHOST_NOT_FOUND);
    Ok(0)
}

fn gethostbyaddr(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        wsa_error(p, error);
        return Ok(0);
    }
    let (address, length, family) = (p.arg(0), p.arg(1) as u32, p.arg(2) as u16);
    if address == 0 {
        wsa_error(p, WSAEFAULT);
        return Ok(0);
    }
    if family != AF_INET {
        wsa_error(p, WSAEAFNOSUPPORT);
        return Ok(0);
    }
    if length != 4 {
        wsa_error(p, WSAEINVAL);
        return Ok(0);
    }
    let bytes = match p.mem.read_vec(address, 4) {
        Ok(bytes) => bytes,
        Err(_) => {
            wsa_error(p, WSAEFAULT);
            return Ok(0);
        }
    };
    let ip = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    if ip == Ipv4Addr::LOCALHOST {
        create_hostent(p, "localhost", ip)
    } else {
        wsa_error(p, WSANO_DATA);
        Ok(0)
    }
}

fn getprotobyname(p: &mut Process) -> Result<u64, Stop> {
    if let Err(error) = ensure_started(p) {
        wsa_error(p, error);
        return Ok(0);
    }
    let address = p.arg(0);
    if address == 0 {
        wsa_error(p, WSAEFAULT);
        return Ok(0);
    }
    let name = match p.read_cstr(address) {
        Ok(name) => name,
        Err(_) => {
            wsa_error(p, WSAEFAULT);
            return Ok(0);
        }
    };
    let (canonical, number) = match name.to_ascii_lowercase().as_str() {
        "ip" => ("ip", 0i32),
        "icmp" => ("icmp", 1),
        "tcp" => ("tcp", 6),
        "udp" => ("udp", 17),
        _ => {
            wsa_error(p, WSANO_DATA);
            return Ok(0);
        }
    };
    let base = stable_record(p, &format!("protoent:{canonical}"));
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let aliases = base + 64;
    let name_address = base + 96;
    let proto_offset = ptr_size * 2;
    p.write_cstr(name_address, canonical)?;
    write_guest_ptr(p, base, name_address)?;
    write_guest_ptr(p, base + ptr_size, aliases)?;
    p.wr16(aliases, 0)?;
    p.wr16(base + proto_offset, number as u16)?;
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::PROT_RW;

    fn process() -> Process {
        let mut process = Process::new(Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        process.vmem_next = 0x0010_0000;
        process
    }

    fn set_args(p: &mut Process, args: &[u64]) {
        if p.cpu.mode == CpuMode::X86_32 {
            let stack = p.alloc_virtual(0x1000, PROT_RW);
            p.cpu.gpr[RSP] = stack;
            for (index, value) in args.iter().enumerate() {
                p.wr32(stack + 4 + index as u64 * 4, *value as u32).unwrap();
            }
        } else {
            p.cpu.gpr[RCX] = args.first().copied().unwrap_or(0);
            p.cpu.gpr[RDX] = args.get(1).copied().unwrap_or(0);
            p.cpu.gpr[R8] = args.get(2).copied().unwrap_or(0);
            p.cpu.gpr[R9] = args.get(3).copied().unwrap_or(0);
            if args.len() > 4 {
                let stack = p.alloc_virtual(0x1000, PROT_RW);
                p.cpu.gpr[RSP] = stack;
                for (index, value) in args[4..].iter().enumerate() {
                    p.wr64(stack + 8 + index as u64 * 8, *value).unwrap();
                }
            }
        }
    }

    fn startup(p: &mut Process) {
        set_args(p, &[0x0202, 0]);
        assert_eq!(wsa_startup(p).unwrap(), 0);
    }

    fn sockaddr(p: &mut Process, ip: Ipv4Addr, port: u16) -> u64 {
        let addr = p.alloc_virtual(16, PROT_RW);
        p.wr16(addr, AF_INET).unwrap();
        p.wr8(addr + 2, (port >> 8) as u8).unwrap();
        p.wr8(addr + 3, port as u8).unwrap();
        p.write_bytes(addr + 4, &ip.octets()).unwrap();
        addr
    }

    #[test]
    fn exact_ordinals_and_names_resolve_to_implementations() {
        let names = [
            "#1",
            "#2",
            "#3",
            "#4",
            "#5",
            "#6",
            "#7",
            "#8",
            "#9",
            "#10",
            "#11",
            "#12",
            "#13",
            "#14",
            "#15",
            "#16",
            "#17",
            "#18",
            "#19",
            "#20",
            "#21",
            "#22",
            "#23",
            "#51",
            "#52",
            "#53",
            "#57",
            "#111",
            "#115",
            "#116",
            "#151",
            "WSASocketA",
            "WSASocketW",
            "WSACreateEvent",
            "WSACloseEvent",
            "WSAWaitForMultipleEvents",
            "WSAGetOverlappedResult",
            "WSASetEvent",
            "WSAResetEvent",
            "WSAIoctl",
            "WSARecv",
            "WSARecvFrom",
        ];
        for name in names {
            assert!(lookup(name).is_some(), "missing {name}");
        }
    }

    #[test]
    fn loopback_stream_pair_transfers_data_through_accept() {
        let mut p = process();
        startup(&mut p);
        let server = create_socket(&mut p, AF_INET, SOCK_STREAM, 0, false) as u32;
        let address = sockaddr(&mut p, Ipv4Addr::LOCALHOST, 23456);
        set_args(&mut p, &[server as u64, address, 16]);
        assert_eq!(bind(&mut p).unwrap(), 0);
        set_args(&mut p, &[server as u64, 4]);
        assert_eq!(listen(&mut p).unwrap(), 0);
        let client = create_socket(&mut p, AF_INET, SOCK_STREAM, 0, false) as u32;
        set_args(&mut p, &[client as u64, address, 16]);
        assert_eq!(connect(&mut p).unwrap(), 0);
        let accepted = p.extra.ws2_32.sockets[&server].pending_accepts[0];
        let message = p.alloc_virtual(5, PROT_RW);
        p.write_bytes(message, b"hello").unwrap();
        set_args(&mut p, &[client as u64, message, 5, 0]);
        assert_eq!(send(&mut p).unwrap(), 5);
        let output = p.alloc_virtual(5, PROT_RW);
        set_args(&mut p, &[accepted as u64, output, 5, 0]);
        assert_eq!(recv(&mut p).unwrap(), 5);
        assert_eq!(p.mem.read_vec(output, 5).unwrap(), b"hello");
    }

    #[test]
    fn loopback_datagram_sendto_recvfrom_preserves_payload_and_source() {
        let mut p = process();
        startup(&mut p);
        let receiver = create_socket(&mut p, AF_INET, SOCK_DGRAM, 0, false) as u32;
        let sender = create_socket(&mut p, AF_INET, SOCK_DGRAM, 0, false) as u32;
        let destination = sockaddr(&mut p, Ipv4Addr::LOCALHOST, 32123);
        set_args(&mut p, &[receiver as u64, destination, 16]);
        assert_eq!(bind(&mut p).unwrap(), 0);

        let message = p.alloc_virtual(8, PROT_RW);
        p.write_bytes(message, b"datagram").unwrap();
        set_args(&mut p, &[sender as u64, message, 8, 0, destination, 16]);
        assert_eq!(sendto(&mut p).unwrap(), 8);

        let output = p.alloc_virtual(8, PROT_RW);
        let source = p.alloc_virtual(16, PROT_RW);
        let source_length = p.alloc_virtual(4, PROT_RW);
        p.wr32(source_length, 16).unwrap();
        set_args(
            &mut p,
            &[receiver as u64, output, 8, 0, source, source_length],
        );
        assert_eq!(recvfrom(&mut p).unwrap(), 8);
        assert_eq!(p.mem.read_vec(output, 8).unwrap(), b"datagram");
        assert_eq!(p.rd16(source).unwrap(), AF_INET);
        assert_eq!(p.rd32(source_length).unwrap(), 16);
        assert_eq!(
            [
                p.rd8(source + 4).unwrap(),
                p.rd8(source + 5).unwrap(),
                p.rd8(source + 6).unwrap(),
                p.rd8(source + 7).unwrap(),
            ],
            [127, 0, 0, 1]
        );
    }

    #[test]
    fn wsa_events_use_manual_reset_kernel_event_state_and_wait_semantics() {
        let mut p = process();
        startup(&mut p);
        let event = p.new_handle(Handle::Event {
            signaled: false,
            manual: true,
        });
        set_args(&mut p, &[event]);
        assert_eq!(wsa_set_event(&mut p).unwrap(), 1);
        set_args(&mut p, &[event]);
        assert_eq!(wsa_reset_event(&mut p).unwrap(), 1);
        let events = p.alloc_virtual(4, PROT_RW);
        p.wr32(events, event as u32).unwrap();
        set_args(&mut p, &[1, events, 0, 0, 0]);
        assert_eq!(
            wsa_wait_for_multiple_events(&mut p).unwrap(),
            WSA_WAIT_TIMEOUT as u64
        );
        set_args(&mut p, &[event]);
        assert_eq!(wsa_set_event(&mut p).unwrap(), 1);
        set_args(&mut p, &[1, events, 0, 0, 0]);
        assert_eq!(
            wsa_wait_for_multiple_events(&mut p).unwrap(),
            WSA_WAIT_EVENT_0 as u64
        );
        assert!(matches!(
            p.handles.get(&event),
            Some(Handle::Event {
                signaled: true,
                manual: true
            })
        ));
    }

    #[test]
    fn host_and_protocol_queries_are_stable_and_report_unknown_names() {
        let mut p = process();
        startup(&mut p);
        let name = p.alloc_virtual(32, PROT_RW);
        p.write_cstr(name, "localhost").unwrap();
        set_args(&mut p, &[name]);
        let host = gethostbyname(&mut p).unwrap();
        assert_ne!(host, 0);
        assert_eq!(p.rd16(host + 8).unwrap(), AF_INET);
        let proto_name = p.alloc_virtual(16, PROT_RW);
        p.write_cstr(proto_name, "tcp").unwrap();
        set_args(&mut p, &[proto_name]);
        let proto = getprotobyname(&mut p).unwrap();
        assert_eq!(p.rd32(proto + 8).unwrap(), IPPROTO_TCP as u32);
        p.write_cstr(proto_name, "not-a-protocol").unwrap();
        set_args(&mut p, &[proto_name]);
        assert_eq!(getprotobyname(&mut p).unwrap(), 0);
        assert_eq!(
            wsa_get_last_error(&mut p).unwrap(),
            WSANO_DATA as u32 as u64
        );
    }

    #[test]
    fn external_connect_fails_closed_instead_of_claiming_success() {
        let mut p = process();
        startup(&mut p);
        let socket = create_socket(&mut p, AF_INET, SOCK_STREAM, 0, false) as u32;
        let address = sockaddr(&mut p, Ipv4Addr::new(203, 0, 113, 1), 443);
        set_args(&mut p, &[socket as u64, address, 16]);
        assert!(matches!(connect(&mut p), Err(Stop::Unimplemented { dll, .. }) if dll == "ws2_32"));
    }
}
