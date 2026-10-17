//! Process-local WinINet session, HTTP request, URL, option, and cookie support.
//!
//! The virtual process has no external network transport. Session and request handles are real
//! process-local objects and request state is retained, but operations that would cross the
//! network fail with `ERROR_INTERNET_CANNOT_CONNECT` instead of returning fabricated responses.

use crate::process::{HleFn, Process, Stop};
use anywasm_cpu::CpuMode;
use std::collections::HashMap;

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INTERNET_INCORRECT_FORMAT: u32 = 12027;
const ERROR_INTERNET_CANNOT_CONNECT: u32 = 12029;
const ERROR_INTERNET_NEED_UI: u32 = 12034;
const ERROR_INTERNET_INCORRECT_HANDLE_TYPE: u32 = 12018;
const ERROR_INTERNET_INCORRECT_HANDLE_STATE: u32 = 12019;
const ERROR_INTERNET_BAD_OPTION: u32 = 12009;
const ERROR_HTTP_HEADER_NOT_FOUND: u32 = 12150;
const ERROR_INTERNET_INVALID_URL: u32 = 12005;
const ERROR_INTERNET_UNRECOGNIZED_SCHEME: u32 = 12006;
const ERROR_NO_MORE_ITEMS: u32 = 259;
const INTERNET_INVALID_STATUS_CALLBACK: u64 = u64::MAX;

const INTERNET_SERVICE_FTP: u32 = 1;
const INTERNET_SERVICE_GOPHER: u32 = 2;
const INTERNET_SERVICE_HTTP: u32 = 3;

const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
const INTERNET_OPEN_TYPE_DIRECT: u32 = 1;
const INTERNET_OPEN_TYPE_PROXY: u32 = 3;
const INTERNET_OPEN_TYPE_PRECONFIG_WITH_NO_AUTOPROXY: u32 = 4;

const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
const INTERNET_OPTION_CONNECT_RETRIES: u32 = 3;
const INTERNET_OPTION_SEND_TIMEOUT: u32 = 5;
const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
const INTERNET_OPTION_USER_AGENT: u32 = 41;
const INTERNET_OPTION_DATA_SEND_TIMEOUT: u32 = 45;
const INTERNET_OPTION_DATA_RECEIVE_TIMEOUT: u32 = 46;
const INTERNET_OPTION_OFFLINE_MODE: u32 = 77;
const INTERNET_OPTION_VERSION: u32 = 40;

const HTTP_QUERY_REQUEST_METHOD: u32 = 45;
const HTTP_QUERY_CUSTOM: u32 = 65_535;
const HTTP_QUERY_RAW_HEADERS: u32 = 21;
const HTTP_QUERY_RAW_HEADERS_CRLF: u32 = 22;
const HTTP_QUERY_FLAG_REQUEST_HEADERS: u32 = 0x8000_0000;
const HTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

const HTTP_ADDREQ_FLAGS_MASK: u32 = 0xffff_0000;
const HTTP_ADDREQ_FLAG_ADD_IF_NEW: u32 = 0x1000_0000;
const HTTP_ADDREQ_FLAG_ADD: u32 = 0x2000_0000;
const HTTP_ADDREQ_FLAG_REPLACE: u32 = 0x8000_0000;

const INTERNET_SCHEME_FTP: i32 = 1;
const INTERNET_SCHEME_GOPHER: i32 = 2;
const INTERNET_SCHEME_HTTP: i32 = 3;
const INTERNET_SCHEME_HTTPS: i32 = 4;
const INTERNET_SCHEME_FILE: i32 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WinInetHandle {
    Session {
        agent: String,
        access_type: u32,
        proxy: Option<String>,
        proxy_bypass: Option<String>,
        flags: u32,
    },
    Connection {
        parent: u64,
        host: String,
        port: u16,
        service: u32,
        username: Option<String>,
        password: Option<String>,
        flags: u32,
        context: u64,
    },
    Request {
        parent: u64,
        verb: String,
        object: String,
        version: Option<String>,
        referrer: Option<String>,
        accept_types: Vec<String>,
        flags: u32,
        context: u64,
        headers: Vec<String>,
        body: Vec<u8>,
        response: Option<Vec<u8>>,
        response_offset: usize,
        transport_error: Option<u32>,
        write_phase: bool,
    },
}

#[derive(Default)]
pub struct WinInetState {
    pub handles: HashMap<u64, WinInetHandle>,
    pub next_handle: u64,
    pub callbacks: HashMap<u64, u64>,
    pub options: HashMap<(u64, u32), Vec<u8>>,
    pub global_options: HashMap<u32, Vec<u8>>,
    /// In-memory session cookies, indexed by normalized origin and cookie name.
    pub cookies: HashMap<String, HashMap<String, String>>,
}

impl core::fmt::Debug for WinInetState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WinInetState")
            .field("handles", &self.handles)
            .field("callbacks", &self.callbacks)
            .field("options", &self.options.keys().collect::<Vec<_>>())
            .field(
                "global_options",
                &self.global_options.keys().collect::<Vec<_>>(),
            )
            .field("cookie_origins", &self.cookies.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "HttpAddRequestHeadersA" => http_add_request_headers_a,
        "InternetErrorDlg" => internet_error_dlg,
        "InternetSetOptionA" => internet_set_option_a,
        "InternetReadFile" => internet_read_file,
        "InternetCrackUrlA" => internet_crack_url_a,
        "InternetCloseHandle" => internet_close_handle,
        "InternetConnectA" => internet_connect_a,
        "HttpOpenRequestA" => http_open_request_a,
        "InternetGetConnectedState" => internet_get_connected_state,
        "InternetSetCookieA" => internet_set_cookie_a,
        "InternetGetCookieW" => internet_get_cookie_w,
        "HttpQueryInfoA" => http_query_info_a,
        "InternetQueryOptionA" => internet_query_option_a,
        "HttpSendRequestA" => http_send_request_a,
        "InternetSetStatusCallback" => internet_set_status_callback,
        "InternetOpenA" => internet_open_a,
        "HttpSendRequestExA" => http_send_request_ex_a,
        "InternetWriteFile" => internet_write_file,
        "HttpEndRequestA" => http_end_request_a,
        _ => return None,
    })
}

/// Byte count popped by the x86 stdcall wrappers for these exports.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    Some(match name {
        "HttpAddRequestHeadersA" => 16,
        "InternetErrorDlg" => 20,
        "InternetSetOptionA" => 16,
        "InternetReadFile" => 16,
        "InternetCrackUrlA" => 16,
        "InternetCloseHandle" => 4,
        "InternetConnectA" => 32,
        "HttpOpenRequestA" => 32,
        "InternetGetConnectedState" => 8,
        "InternetSetCookieA" => 12,
        "InternetGetCookieW" => 16,
        "HttpQueryInfoA" => 20,
        "InternetQueryOptionA" => 16,
        "HttpSendRequestA" => 20,
        "InternetSetStatusCallback" => 8,
        "InternetOpenA" => 20,
        "HttpSendRequestExA" => 20,
        "InternetWriteFile" => 16,
        "HttpEndRequestA" => 16,
        _ => return None,
    })
}

fn is_x86(p: &Process) -> bool {
    p.cpu.mode == CpuMode::X86_32
}

fn ptr_size(p: &Process) -> usize {
    if is_x86(p) {
        4
    } else {
        8
    }
}

fn read_u32(p: &Process, address: u64) -> Option<u32> {
    p.mem.read_u32(address).ok()
}

fn read_ptr(p: &Process, address: u64) -> Option<u64> {
    if is_x86(p) {
        read_u32(p, address).map(u64::from)
    } else {
        p.mem.read_u64(address).ok()
    }
}

fn write_ptr(p: &mut Process, address: u64, value: u64) -> bool {
    if is_x86(p) {
        p.mem.write_u32(address, value as u32).is_ok()
    } else {
        p.mem.write_u64(address, value).is_ok()
    }
}

fn read_a_string(p: &Process, address: u64) -> Option<String> {
    if address == 0 {
        return None;
    }
    let bytes = p.mem.read_cstr(address, 1024 * 1024).ok()?;
    Some(String::from_utf16_lossy(
        &bytes
            .iter()
            .copied()
            .map(crate::hle::kernel32::cp1252)
            .collect::<Vec<_>>(),
    ))
}

fn read_a_string_optional(p: &Process, address: u64) -> Option<Option<String>> {
    if address == 0 {
        Some(None)
    } else {
        read_a_string(p, address).map(Some)
    }
}

fn read_w_string(p: &Process, address: u64) -> Option<String> {
    if address == 0 {
        return None;
    }
    let mut units = Vec::new();
    for i in 0..1024 * 1024u64 {
        let unit = p
            .mem
            .read_u16(address.checked_add(i.checked_mul(2)?)?)
            .ok()?;
        if unit == 0 {
            return Some(String::from_utf16_lossy(&units));
        }
        units.push(unit);
    }
    None
}

fn encode_a(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .map(|unit| {
            if unit <= 0x7f || (0xa0..=0xff).contains(&unit) {
                unit as u8
            } else {
                (0x80..=0x9f)
                    .find(|byte| crate::hle::kernel32::cp1252(*byte) == unit)
                    .unwrap_or(b'?')
            }
        })
        .collect()
}

fn write_last_error(p: &mut Process, error: u32) {
    p.set_last_error(error);
}

fn fail(p: &mut Process, error: u32) -> u64 {
    write_last_error(p, error);
    0
}

fn allocate_handle(p: &mut Process) -> Option<u64> {
    let mut handle = p.extra.wininet.next_handle;
    if handle < 0x100 {
        handle = 0x8000_1000;
    }
    for _ in 0..16_384 {
        if !p.extra.wininet.handles.contains_key(&handle) && !p.handles.contains_key(&handle) {
            p.extra.wininet.next_handle = handle.wrapping_add(4).max(0x8000_1000);
            return Some(handle);
        }
        handle = handle.wrapping_add(4).max(0x8000_1000);
    }
    None
}

fn push_handle(p: &mut Process, value: WinInetHandle) -> Option<u64> {
    let handle = allocate_handle(p)?;
    p.extra.wininet.handles.insert(handle, value);
    write_last_error(p, 0);
    Some(handle)
}

fn internet_open_a(p: &mut Process) -> Result<u64, Stop> {
    let agent = match read_a_string_optional(p, p.arg(0)) {
        Some(Some(value)) => value,
        Some(None) => String::new(),
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let access_type = p.arg(1) as u32;
    if !matches!(
        access_type,
        INTERNET_OPEN_TYPE_PRECONFIG
            | INTERNET_OPEN_TYPE_DIRECT
            | INTERNET_OPEN_TYPE_PROXY
            | INTERNET_OPEN_TYPE_PRECONFIG_WITH_NO_AUTOPROXY
    ) {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let proxy = match read_a_string_optional(p, p.arg(2)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let proxy_bypass = match read_a_string_optional(p, p.arg(3)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let value = WinInetHandle::Session {
        agent: agent.clone(),
        access_type,
        proxy,
        proxy_bypass,
        flags: p.arg(4) as u32,
    };
    let Some(handle) = push_handle(p, value) else {
        return Ok(fail(p, 8));
    };
    p.extra.wininet.options.insert(
        (handle, INTERNET_OPTION_USER_AGENT),
        encode_a(&agent).into_iter().chain([0]).collect(),
    );
    Ok(handle)
}

fn internet_connect_a(p: &mut Process) -> Result<u64, Stop> {
    let parent = p.arg(0);
    if !matches!(
        p.extra.wininet.handles.get(&parent),
        Some(WinInetHandle::Session { .. })
    ) {
        return Ok(fail(
            p,
            if p.extra.wininet.handles.contains_key(&parent) {
                ERROR_INTERNET_INCORRECT_HANDLE_TYPE
            } else {
                ERROR_INVALID_HANDLE
            },
        ));
    }
    let Some(host) = read_a_string(p, p.arg(1)) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let port = p.arg(2) as u16;
    let username = match read_a_string_optional(p, p.arg(3)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let password = match read_a_string_optional(p, p.arg(4)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let service = p.arg(5) as u32;
    if !matches!(
        service,
        INTERNET_SERVICE_HTTP | INTERNET_SERVICE_FTP | INTERNET_SERVICE_GOPHER
    ) {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let value = WinInetHandle::Connection {
        parent,
        host,
        port,
        service,
        username,
        password,
        flags: p.arg(6) as u32,
        context: p.arg(7),
    };
    let Some(handle) = push_handle(p, value) else {
        return Ok(fail(p, 8));
    };
    Ok(handle)
}

fn read_accept_types(p: &Process, address: u64) -> Option<Vec<String>> {
    if address == 0 {
        return Some(Vec::new());
    }
    let mut values = Vec::new();
    for index in 0..64u64 {
        let slot = address.checked_add(index.checked_mul(ptr_size(p) as u64)?)?;
        let value = read_ptr(p, slot)?;
        if value == 0 {
            return Some(values);
        }
        values.push(read_a_string(p, value)?);
    }
    None
}

fn http_open_request_a(p: &mut Process) -> Result<u64, Stop> {
    let parent = p.arg(0);
    let Some(WinInetHandle::Connection {
        service: INTERNET_SERVICE_HTTP,
        ..
    }) = p.extra.wininet.handles.get(&parent)
    else {
        return Ok(fail(
            p,
            if p.extra.wininet.handles.contains_key(&parent) {
                ERROR_INTERNET_INCORRECT_HANDLE_TYPE
            } else {
                ERROR_INVALID_HANDLE
            },
        ));
    };
    let verb = match read_a_string_optional(p, p.arg(1)) {
        Some(Some(value)) if !value.is_empty() => value,
        Some(_) => "GET".into(),
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let object = match read_a_string_optional(p, p.arg(2)) {
        Some(Some(value)) => value,
        Some(None) => String::new(),
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let version = match read_a_string_optional(p, p.arg(3)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let referrer = match read_a_string_optional(p, p.arg(4)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let Some(accept_types) = read_accept_types(p, p.arg(5)) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let value = WinInetHandle::Request {
        parent,
        verb,
        object,
        version,
        referrer,
        accept_types,
        flags: p.arg(6) as u32,
        context: p.arg(7),
        headers: Vec::new(),
        body: Vec::new(),
        response: None,
        response_offset: 0,
        transport_error: None,
        write_phase: false,
    };
    let Some(handle) = push_handle(p, value) else {
        return Ok(fail(p, 8));
    };
    Ok(handle)
}

fn request_mut(p: &mut Process, handle: u64) -> Result<&mut WinInetHandle, u32> {
    let Some(value) = p.extra.wininet.handles.get_mut(&handle) else {
        return Err(ERROR_INVALID_HANDLE);
    };
    if !matches!(value, WinInetHandle::Request { .. }) {
        return Err(ERROR_INTERNET_INCORRECT_HANDLE_TYPE);
    }
    Ok(value)
}

fn parse_header_lines(value: &str) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    for raw_line in value.split('\n').filter(|line| !line.is_empty()) {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            return None;
        }
        let (name, _) = line.split_once(':')?;
        if name.is_empty() || name.bytes().any(|b| b <= 0x20 || b >= 0x7f || b == b':') {
            return None;
        }
        lines.push(line.to_string());
    }
    Some(lines)
}

fn apply_headers(headers: &mut Vec<String>, input: &[String], flags: u32) -> bool {
    let operation = flags & HTTP_ADDREQ_FLAGS_MASK;
    if !matches!(
        operation,
        HTTP_ADDREQ_FLAG_ADD | HTTP_ADDREQ_FLAG_REPLACE | HTTP_ADDREQ_FLAG_ADD_IF_NEW
    ) {
        return false;
    }
    for line in input {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        let existing = headers.iter().position(|old| {
            old.split_once(':')
                .is_some_and(|(old_name, _)| old_name.eq_ignore_ascii_case(name))
        });
        match operation {
            HTTP_ADDREQ_FLAG_REPLACE => {
                if let Some(index) = existing {
                    headers[index] = line.clone();
                } else {
                    headers.push(line.clone());
                }
            }
            HTTP_ADDREQ_FLAG_ADD_IF_NEW => {
                if existing.is_none() {
                    headers.push(line.clone());
                }
            }
            HTTP_ADDREQ_FLAG_ADD => {
                if let Some(index) = existing {
                    let old = &headers[index];
                    let (_, old_value) = old.split_once(':').unwrap();
                    headers[index] = format!("{name}: {}, {}", old_value.trim(), value.trim());
                } else {
                    headers.push(line.clone());
                }
            }
            _ => return false,
        }
    }
    true
}

fn read_a_buffer(p: &Process, address: u64, length: u32) -> Option<Vec<u8>> {
    if length == 0 {
        return Some(Vec::new());
    }
    if address == 0 {
        return None;
    }
    p.mem.read_vec(address, length as usize).ok()
}

fn decode_a(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .iter()
            .copied()
            .map(crate::hle::kernel32::cp1252)
            .collect::<Vec<_>>(),
    )
}

fn http_add_request_headers_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let length = p.arg(2) as u32;
    let raw = if length == u32::MAX {
        let Some(value) = read_a_string(p, p.arg(1)) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        value
    } else {
        let Some(bytes) = read_a_buffer(p, p.arg(1), length) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        String::from_utf16_lossy(
            &bytes
                .iter()
                .copied()
                .map(crate::hle::kernel32::cp1252)
                .collect::<Vec<_>>(),
        )
    };
    let Some(input) = parse_header_lines(&raw) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let flags = p.arg(3) as u32;
    let Ok(WinInetHandle::Request {
        headers,
        transport_error,
        ..
    }) = request_mut(p, handle)
    else {
        let error = request_mut(p, handle)
            .err()
            .unwrap_or(ERROR_INTERNET_INCORRECT_HANDLE_TYPE);
        return Ok(fail(p, error));
    };
    if transport_error.is_some() {
        return Ok(fail(p, ERROR_INTERNET_INCORRECT_HANDLE_STATE));
    }
    if !apply_headers(headers, &input, flags) {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn send_unavailable(
    p: &mut Process,
    handle: u64,
    appended_headers: Vec<String>,
    body: Vec<u8>,
) -> u64 {
    match request_mut(p, handle) {
        Ok(WinInetHandle::Request {
            headers,
            body: request_body,
            transport_error,
            ..
        }) => {
            headers.extend(appended_headers);
            request_body.extend(body);
            *transport_error = Some(ERROR_INTERNET_CANNOT_CONNECT);
            fail(p, ERROR_INTERNET_CANNOT_CONNECT)
        }
        Ok(_) => fail(p, ERROR_INTERNET_INCORRECT_HANDLE_TYPE),
        Err(error) => fail(p, error),
    }
}

fn http_send_request_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let header_length = p.arg(2) as u32;
    let header_text = if p.arg(1) == 0 {
        String::new()
    } else if header_length == u32::MAX {
        match read_a_string(p, p.arg(1)) {
            Some(value) => value,
            None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
        }
    } else {
        let Some(bytes) = read_a_buffer(p, p.arg(1), header_length) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        String::from_utf16_lossy(
            &bytes
                .iter()
                .copied()
                .map(crate::hle::kernel32::cp1252)
                .collect::<Vec<_>>(),
        )
    };
    let Some(headers) = parse_header_lines(&header_text) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let body_length = p.arg(4) as u32;
    let Some(body) = read_a_buffer(p, p.arg(3), body_length) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    if !matches!(
        p.extra.wininet.handles.get(&handle),
        Some(WinInetHandle::Request { .. })
    ) {
        return Ok(fail(
            p,
            if p.extra.wininet.handles.contains_key(&handle) {
                ERROR_INTERNET_INCORRECT_HANDLE_TYPE
            } else {
                ERROR_INVALID_HANDLE
            },
        ));
    }
    if matches!(
        p.extra.wininet.handles.get(&handle),
        Some(WinInetHandle::Request {
            transport_error: Some(_),
            ..
        })
    ) {
        return Ok(fail(p, ERROR_INTERNET_INCORRECT_HANDLE_STATE));
    }
    Ok(send_unavailable(p, handle, headers, body))
}

#[derive(Clone, Copy)]
struct InternetBuffersLayout {
    size: usize,
    next: usize,
    headers: usize,
    headers_len: usize,
    headers_total: usize,
    buffer: usize,
    buffer_len: usize,
    buffer_total: usize,
}

fn internet_buffers_layout(pointer: usize) -> InternetBuffersLayout {
    if pointer == 4 {
        InternetBuffersLayout {
            size: 40,
            next: 4,
            headers: 8,
            headers_len: 12,
            headers_total: 16,
            buffer: 20,
            buffer_len: 24,
            buffer_total: 28,
        }
    } else {
        InternetBuffersLayout {
            size: 64,
            next: 8,
            headers: 16,
            headers_len: 24,
            headers_total: 28,
            buffer: 40,
            buffer_len: 48,
            buffer_total: 52,
        }
    }
}

fn http_send_request_ex_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let input = p.arg(1);
    let ptr = ptr_size(p);
    let layout = internet_buffers_layout(ptr);
    let mut headers = Vec::new();
    let mut body = Vec::new();
    let mut current = input;
    let mut total_headers = None;
    let mut total_body = None;
    for _ in 0..64 {
        let Some(struct_size) = read_u32(p, current) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        if struct_size < layout.size as u32 {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        }
        let Some(next_address) = current.checked_add(layout.next as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(headers_address) = current.checked_add(layout.headers as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(headers_len_address) = current.checked_add(layout.headers_len as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(headers_total_address) = current.checked_add(layout.headers_total as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(buffer_address) = current.checked_add(layout.buffer as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(buffer_len_address) = current.checked_add(layout.buffer_len as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(buffer_total_address) = current.checked_add(layout.buffer_total as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(next) = read_ptr(p, next_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(headers_ptr) = read_ptr(p, headers_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(header_len) = read_u32(p, headers_len_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(header_total) = read_u32(p, headers_total_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(data_ptr) = read_ptr(p, buffer_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(data_len) = read_u32(p, buffer_len_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(data_total) = read_u32(p, buffer_total_address) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(header_bytes) = read_a_buffer(p, headers_ptr, header_len) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        if header_total != 0 {
            total_headers = Some(header_total);
        }
        if data_total != 0 {
            total_body = Some(data_total);
        }
        if header_len != 0 {
            let Some(mut parsed) = parse_header_lines(&decode_a(&header_bytes)) else {
                return Ok(fail(p, ERROR_INVALID_PARAMETER));
            };
            headers.append(&mut parsed);
        }
        let Some(data) = read_a_buffer(p, data_ptr, data_len) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        body.extend(data);
        if next == 0 {
            break;
        }
        current = next;
    }
    if total_headers
        .is_some_and(|total| (total as usize) < headers.iter().map(|h| h.len() + 2).sum::<usize>())
        || total_body.is_some_and(|total| (total as usize) < body.len())
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    if !matches!(
        p.extra.wininet.handles.get(&handle),
        Some(WinInetHandle::Request { .. })
    ) {
        return Ok(fail(
            p,
            if p.extra.wininet.handles.contains_key(&handle) {
                ERROR_INTERNET_INCORRECT_HANDLE_TYPE
            } else {
                ERROR_INVALID_HANDLE
            },
        ));
    }
    Ok(send_unavailable(p, handle, headers, body))
}

fn http_end_request_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    if !matches!(
        p.extra.wininet.handles.get(&handle),
        Some(WinInetHandle::Request { .. })
    ) {
        return Ok(fail(
            p,
            if p.extra.wininet.handles.contains_key(&handle) {
                ERROR_INTERNET_INCORRECT_HANDLE_TYPE
            } else {
                ERROR_INVALID_HANDLE
            },
        ));
    }
    Ok(fail(p, ERROR_INTERNET_CANNOT_CONNECT))
}

fn internet_write_file(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let bytes = p.arg(1);
    let length = p.arg(2) as u32;
    let bytes_written = p.arg(3);
    if bytes_written == 0 || p.mem.write_u32(bytes_written, 0).is_err() {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let Some(data) = read_a_buffer(p, bytes, length) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let failure = match request_mut(p, handle) {
        Ok(WinInetHandle::Request {
            body,
            write_phase,
            transport_error,
            ..
        }) => {
            if !*write_phase || transport_error.is_some() {
                Some(if transport_error.is_some() {
                    ERROR_INTERNET_CANNOT_CONNECT
                } else {
                    ERROR_INTERNET_INCORRECT_HANDLE_STATE
                })
            } else {
                body.extend(data);
                None
            }
        }
        Ok(_) => Some(ERROR_INTERNET_INCORRECT_HANDLE_TYPE),
        Err(error) => Some(error),
    };
    if let Some(error) = failure {
        return Ok(fail(p, error));
    }
    if p.mem.write_u32(bytes_written, length).is_err() {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn internet_read_file(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let buffer = p.arg(1);
    let requested = p.arg(2) as u32;
    let bytes_read = p.arg(3);
    if bytes_read == 0 || p.mem.write_u32(bytes_read, 0).is_err() {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    if requested > 0 && buffer == 0 {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let read_result = match p.extra.wininet.handles.get_mut(&handle) {
        Some(WinInetHandle::Request {
            response: Some(response),
            response_offset,
            ..
        }) => {
            let count = (requested as usize).min(response.len().saturating_sub(*response_offset));
            let bytes = response[*response_offset..*response_offset + count].to_vec();
            *response_offset += count;
            Ok(bytes)
        }
        Some(WinInetHandle::Request {
            transport_error: Some(error),
            ..
        }) => Err(*error),
        Some(WinInetHandle::Request { .. }) => Err(ERROR_INTERNET_INCORRECT_HANDLE_STATE),
        Some(_) => Err(ERROR_INTERNET_INCORRECT_HANDLE_TYPE),
        None => Err(ERROR_INVALID_HANDLE),
    };
    let bytes = match read_result {
        Ok(bytes) => bytes,
        Err(error) => return Ok(fail(p, error)),
    };
    if p.mem.write_from(buffer, &bytes).is_err()
        || p.mem.write_u32(bytes_read, bytes.len() as u32).is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn internet_close_handle(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    if !p.extra.wininet.handles.contains_key(&handle) {
        return Ok(fail(p, ERROR_INVALID_HANDLE));
    }
    let mut closing = vec![handle];
    let mut index = 0;
    while index < closing.len() {
        let parent = closing[index];
        for (&child, value) in &p.extra.wininet.handles {
            if handle_parent(value) == Some(parent) && !closing.contains(&child) {
                closing.push(child);
            }
        }
        index += 1;
    }
    for closing_handle in closing {
        p.extra.wininet.handles.remove(&closing_handle);
        p.extra.wininet.callbacks.remove(&closing_handle);
        p.extra
            .wininet
            .options
            .retain(|(h, _), _| *h != closing_handle);
    }
    write_last_error(p, 0);
    Ok(1)
}

fn handle_parent(handle: &WinInetHandle) -> Option<u64> {
    match handle {
        WinInetHandle::Session { .. } => None,
        WinInetHandle::Connection { parent, .. } | WinInetHandle::Request { parent, .. } => {
            Some(*parent)
        }
    }
}

fn internet_get_connected_state(p: &mut Process) -> Result<u64, Stop> {
    let flags = p.arg(0);
    if flags != 0 && p.mem.write_u32(flags, 0).is_err() {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    // The virtual process has no host-network binding; an offline result is the actual state.
    write_last_error(p, 0);
    Ok(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedUrl {
    scheme: String,
    scheme_id: i32,
    host: String,
    port: u16,
    username: String,
    password: String,
    path: String,
    extra: String,
    scheme_range: (usize, usize),
    host_range: (usize, usize),
    username_range: (usize, usize),
    password_range: (usize, usize),
    path_range: (usize, usize),
    extra_range: (usize, usize),
}

fn parse_url(url: &str) -> Result<ParsedUrl, u32> {
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err(ERROR_INTERNET_INVALID_URL);
    };
    let scheme = scheme.to_ascii_lowercase();
    let scheme_id = match scheme.as_str() {
        "ftp" => INTERNET_SCHEME_FTP,
        "gopher" => INTERNET_SCHEME_GOPHER,
        "http" => INTERNET_SCHEME_HTTP,
        "https" => INTERNET_SCHEME_HTTPS,
        "file" => INTERNET_SCHEME_FILE,
        _ => return Err(ERROR_INTERNET_UNRECOGNIZED_SCHEME),
    };
    let rest_offset = encode_a(&scheme).len() + 1;
    let rest = rest.strip_prefix("//").ok_or(ERROR_INTERNET_INVALID_URL)?;
    let authority_offset = rest_offset + 2;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() && scheme_id != INTERNET_SCHEME_FILE {
        return Err(ERROR_INTERNET_INVALID_URL);
    }
    let resource = &rest[authority_end..];
    let authority_bytes = encode_a(authority).len();
    let (user_info, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(userinfo), host),
        None => (None, authority),
    };
    let (username, password) = user_info
        .map(|v| v.split_once(':').map_or((v, ""), |(u, p)| (u, p)))
        .unwrap_or(("", ""));
    let host_port_offset = authority_offset + authority_bytes - encode_a(host_port).len();
    let (host, port, host_offset) = if host_port.starts_with('[') {
        let end = host_port.find(']').ok_or(ERROR_INTERNET_INVALID_URL)?;
        let host = &host_port[1..end];
        let suffix = &host_port[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or(ERROR_INTERNET_INVALID_URL)?
                    .parse::<u16>()
                    .map_err(|_| ERROR_INTERNET_INVALID_URL)?,
            )
        };
        (host, port, host_port_offset + 1)
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            let host_offset = host_port_offset;
            (
                host,
                Some(
                    port.parse::<u16>()
                        .map_err(|_| ERROR_INTERNET_INVALID_URL)?,
                ),
                host_offset,
            )
        } else {
            (host_port, None, host_port_offset)
        }
    } else {
        (host_port, None, host_port_offset)
    };
    let resource_offset = authority_offset + authority_bytes;
    let (path, extra, path_range, extra_range) = if resource.is_empty() {
        ("/", "", (resource_offset, 0), (resource_offset, 0))
    } else {
        let first_extra = resource.find(['?', '#']).unwrap_or(resource.len());
        let path = &resource[..first_extra];
        let path = if path.is_empty() { "/" } else { path };
        let extra = &resource[first_extra..];
        let path_offset = resource_offset;
        let path_length = if resource[..first_extra].is_empty() {
            0
        } else {
            encode_a(path).len()
        };
        let extra_offset = resource_offset + encode_a(&resource[..first_extra]).len();
        (
            path,
            extra,
            (path_offset, path_length),
            (extra_offset, encode_a(extra).len()),
        )
    };
    let (username_offset, username_length, password_offset, password_length) =
        if let Some(user_info) = user_info {
            let user_offset = authority_offset;
            if let Some((user, password)) = user_info.split_once(':') {
                let pass_offset = user_offset + encode_a(user).len() + 1;
                (
                    user_offset,
                    encode_a(user).len(),
                    pass_offset,
                    encode_a(password).len(),
                )
            } else {
                (
                    user_offset,
                    encode_a(user_info).len(),
                    user_offset + encode_a(user_info).len(),
                    0,
                )
            }
        } else {
            (authority_offset, 0, authority_offset, 0)
        };
    let host_length = encode_a(host).len();
    let default_port = match scheme_id {
        INTERNET_SCHEME_FTP => 21,
        INTERNET_SCHEME_HTTP => 80,
        INTERNET_SCHEME_HTTPS => 443,
        _ => 0,
    };
    let scheme_len = encode_a(&scheme).len();
    Ok(ParsedUrl {
        scheme,
        scheme_id,
        host: host.to_string(),
        port: port.unwrap_or(default_port),
        username: username.to_string(),
        password: password.to_string(),
        path: path.to_string(),
        extra: extra.to_string(),
        scheme_range: (0, scheme_len),
        host_range: (host_offset, host_length),
        username_range: (username_offset, username_length),
        password_range: (password_offset, password_length),
        path_range,
        extra_range,
    })
}

#[derive(Clone, Copy)]
struct UrlComponentsLayout {
    size: usize,
    scheme_ptr: usize,
    scheme_len: usize,
    scheme_id: usize,
    host_ptr: usize,
    host_len: usize,
    port: usize,
    username_ptr: usize,
    username_len: usize,
    password_ptr: usize,
    password_len: usize,
    path_ptr: usize,
    path_len: usize,
    extra_ptr: usize,
    extra_len: usize,
}

fn url_components_layout(pointer: usize) -> UrlComponentsLayout {
    if pointer == 4 {
        UrlComponentsLayout {
            size: 60,
            scheme_ptr: 4,
            scheme_len: 8,
            scheme_id: 12,
            host_ptr: 16,
            host_len: 20,
            port: 24,
            username_ptr: 28,
            username_len: 32,
            password_ptr: 36,
            password_len: 40,
            path_ptr: 44,
            path_len: 48,
            extra_ptr: 52,
            extra_len: 56,
        }
    } else {
        UrlComponentsLayout {
            size: 104,
            scheme_ptr: 8,
            scheme_len: 16,
            scheme_id: 20,
            host_ptr: 24,
            host_len: 32,
            port: 36,
            username_ptr: 40,
            username_len: 48,
            password_ptr: 56,
            password_len: 64,
            path_ptr: 72,
            path_len: 80,
            extra_ptr: 88,
            extra_len: 96,
        }
    }
}

#[derive(Clone, Copy)]
struct UrlOutput {
    ptr_offset: usize,
    len_offset: usize,
}

fn internet_crack_url_a(p: &mut Process) -> Result<u64, Stop> {
    let url_ptr = p.arg(0);
    let url_length = p.arg(1) as u32;
    let flags = p.arg(2) as u32;
    let components = p.arg(3);
    if flags != 0 {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let layout = url_components_layout(ptr_size(p));
    let Some(size) = read_u32(p, components) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    if size < layout.size as u32 {
        return Ok(fail(p, ERROR_INTERNET_INCORRECT_FORMAT));
    }
    let input = if url_length == 0 {
        match read_a_string(p, url_ptr) {
            Some(value) => value,
            None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
        }
    } else {
        let Some(bytes) = read_a_buffer(p, url_ptr, url_length) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        String::from_utf16_lossy(
            &bytes
                .iter()
                .copied()
                .map(crate::hle::kernel32::cp1252)
                .collect::<Vec<_>>(),
        )
    };
    let parsed = match parse_url(&input) {
        Ok(value) => value,
        Err(error) => return Ok(fail(p, error)),
    };
    let fields = [
        (
            UrlOutput {
                ptr_offset: layout.scheme_ptr,
                len_offset: layout.scheme_len,
            },
            parsed.scheme.as_str(),
            parsed.scheme_range,
        ),
        (
            UrlOutput {
                ptr_offset: layout.host_ptr,
                len_offset: layout.host_len,
            },
            parsed.host.as_str(),
            parsed.host_range,
        ),
        (
            UrlOutput {
                ptr_offset: layout.username_ptr,
                len_offset: layout.username_len,
            },
            parsed.username.as_str(),
            parsed.username_range,
        ),
        (
            UrlOutput {
                ptr_offset: layout.password_ptr,
                len_offset: layout.password_len,
            },
            parsed.password.as_str(),
            parsed.password_range,
        ),
        (
            UrlOutput {
                ptr_offset: layout.path_ptr,
                len_offset: layout.path_len,
            },
            parsed.path.as_str(),
            parsed.path_range,
        ),
        (
            UrlOutput {
                ptr_offset: layout.extra_ptr,
                len_offset: layout.extra_len,
            },
            parsed.extra.as_str(),
            parsed.extra_range,
        ),
    ];
    for (field, value, (url_offset, url_component_length)) in fields {
        let Some(output_ptr) = read_ptr(p, components + field.ptr_offset as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        let Some(capacity) = read_u32(p, components + field.len_offset as u64) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        if output_ptr != 0 {
            let bytes = encode_a(value);
            if capacity as usize <= bytes.len() {
                let _ = p
                    .mem
                    .write_u32(components + field.len_offset as u64, bytes.len() as u32);
                return Ok(fail(p, ERROR_INSUFFICIENT_BUFFER));
            }
            let mut terminated = bytes.clone();
            terminated.push(0);
            if p.mem.write_from(output_ptr, &terminated).is_err() {
                return Ok(fail(p, ERROR_INVALID_PARAMETER));
            }
            if p.mem
                .write_u32(components + field.len_offset as u64, bytes.len() as u32)
                .is_err()
            {
                return Ok(fail(p, ERROR_INVALID_PARAMETER));
            }
        } else if capacity != 0 {
            if !write_ptr(
                p,
                components + field.ptr_offset as u64,
                url_ptr + url_offset as u64,
            ) || p
                .mem
                .write_u32(
                    components + field.len_offset as u64,
                    url_component_length as u32,
                )
                .is_err()
            {
                return Ok(fail(p, ERROR_INVALID_PARAMETER));
            }
        }
    }
    if p.mem
        .write_u32(
            components + layout.scheme_id as u64,
            parsed.scheme_id as u32,
        )
        .is_err()
        || p.mem
            .write_u16(components + layout.port as u64, parsed.port)
            .is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn origin_key(url: &str) -> Result<String, u32> {
    let parsed = parse_url(url)?;
    if !matches!(
        parsed.scheme_id,
        INTERNET_SCHEME_HTTP | INTERNET_SCHEME_HTTPS
    ) {
        return Err(ERROR_INTERNET_UNRECOGNIZED_SCHEME);
    }
    Ok(format!(
        "{}://{}:{}",
        parsed.scheme,
        parsed.host.to_ascii_lowercase(),
        parsed.port
    ))
}

fn internet_set_cookie_a(p: &mut Process) -> Result<u64, Stop> {
    let Some(url) = read_a_string(p, p.arg(0)) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let origin = match origin_key(&url) {
        Ok(value) => value,
        Err(error) => return Ok(fail(p, error)),
    };
    let name = match read_a_string_optional(p, p.arg(1)) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    let Some(data) = read_a_string(p, p.arg(2)) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let data_value = data.split(';').next().unwrap_or("").trim().to_string();
    let (name, value) = if let Some(name) = name {
        (name, data_value)
    } else if let Some((name, value)) = data.split_once('=') {
        (
            name.trim().to_string(),
            value.split(';').next().unwrap_or("").trim().to_string(),
        )
    } else {
        (String::new(), data_value)
    };
    if name.bytes().any(|b| b <= 0x20 || b == b';' || b == b'=') {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let cookies = p.extra.wininet.cookies.entry(origin).or_default();
    if value.is_empty() && data.to_ascii_lowercase().contains("max-age=0") {
        cookies.remove(&name);
    } else {
        cookies.insert(name, value);
    }
    write_last_error(p, 0);
    Ok(1)
}

fn internet_get_cookie_w(p: &mut Process) -> Result<u64, Stop> {
    let Some(url) = read_w_string(p, p.arg(0)) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let origin = match origin_key(&url) {
        Ok(value) => value,
        Err(error) => return Ok(fail(p, error)),
    };
    // WinINet documents this parameter as not implemented for InternetGetCookieW.
    let _cookie_name = p.arg(1);
    let buffer = p.arg(2);
    let size_pointer = p.arg(3);
    if size_pointer == 0 {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let Some(capacity) = read_u32(p, size_pointer) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let Some(cookies) = p.extra.wininet.cookies.get(&origin) else {
        return Ok(fail(p, ERROR_NO_MORE_ITEMS));
    };
    let text = cookies
        .iter()
        .map(|(name, value)| {
            if name.is_empty() {
                value.clone()
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let needed_bytes = u32::try_from((utf16.len() + 1) * 2).unwrap_or(u32::MAX);
    if buffer == 0 || capacity < utf16.len() as u32 + 1 {
        let _ = p.mem.write_u32(size_pointer, needed_bytes);
        return Ok(fail(p, ERROR_INSUFFICIENT_BUFFER));
    }
    for (index, unit) in utf16.iter().copied().enumerate() {
        if p.mem.write_u16(buffer + index as u64 * 2, unit).is_err() {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        }
    }
    if p.mem.write_u16(buffer + utf16.len() as u64 * 2, 0).is_err()
        || p.mem.write_u32(size_pointer, utf16.len() as u32).is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn option_value(p: &Process, handle: u64, option: u32) -> Option<Vec<u8>> {
    let state = &p.extra.wininet;
    if let Some(value) = state.options.get(&(handle, option)) {
        return Some(value.clone());
    }
    let mut current = handle;
    for _ in 0..32 {
        let Some(value) = state.handles.get(&current) else {
            break;
        };
        let Some(parent) = handle_parent(value) else {
            break;
        };
        if let Some(value) = state.options.get(&(parent, option)) {
            return Some(value.clone());
        }
        current = parent;
    }
    if let Some(value) = state.global_options.get(&option) {
        return Some(value.clone());
    }
    Some(match option {
        INTERNET_OPTION_CONNECT_TIMEOUT => 60_000u32.to_le_bytes().to_vec(),
        INTERNET_OPTION_CONNECT_RETRIES => 5u32.to_le_bytes().to_vec(),
        INTERNET_OPTION_SEND_TIMEOUT
        | INTERNET_OPTION_RECEIVE_TIMEOUT
        | INTERNET_OPTION_DATA_SEND_TIMEOUT
        | INTERNET_OPTION_DATA_RECEIVE_TIMEOUT => 30_000u32.to_le_bytes().to_vec(),
        INTERNET_OPTION_OFFLINE_MODE => 1u32.to_le_bytes().to_vec(),
        INTERNET_OPTION_USER_AGENT => match state.handles.get(&handle) {
            Some(WinInetHandle::Session { agent, .. }) => {
                encode_a(agent).into_iter().chain([0]).collect()
            }
            _ if handle == 0 => {
                let agent = state
                    .handles
                    .values()
                    .find_map(|candidate| match candidate {
                        WinInetHandle::Session { agent, .. } => Some(agent.as_str()),
                        _ => None,
                    })
                    .unwrap_or("AnyWASM");
                encode_a(agent).into_iter().chain([0]).collect()
            }
            _ => return None,
        },
        INTERNET_OPTION_VERSION => [9u32.to_le_bytes(), 0u32.to_le_bytes()].concat(),
        _ => return None,
    })
}

fn internet_set_option_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let option = p.arg(1) as u32;
    let buffer = p.arg(2);
    let length = p.arg(3) as u32;
    if handle != 0 && !p.extra.wininet.handles.contains_key(&handle) {
        return Ok(fail(p, ERROR_INVALID_HANDLE));
    }
    if !matches!(
        option,
        INTERNET_OPTION_CONNECT_TIMEOUT
            | INTERNET_OPTION_CONNECT_RETRIES
            | INTERNET_OPTION_SEND_TIMEOUT
            | INTERNET_OPTION_RECEIVE_TIMEOUT
            | INTERNET_OPTION_USER_AGENT
            | INTERNET_OPTION_DATA_SEND_TIMEOUT
            | INTERNET_OPTION_DATA_RECEIVE_TIMEOUT
            | INTERNET_OPTION_OFFLINE_MODE
    ) {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let Some(mut bytes) = read_a_buffer(p, buffer, length) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    if option == INTERNET_OPTION_USER_AGENT {
        if bytes.is_empty() || bytes.last() != Some(&0) {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        }
    } else if length != 4 {
        return Ok(fail(p, ERROR_INTERNET_BAD_OPTION));
    }
    if option == INTERNET_OPTION_OFFLINE_MODE {
        let value = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        if value == 0 {
            return Ok(fail(p, ERROR_INTERNET_CANNOT_CONNECT));
        }
    }
    if option == INTERNET_OPTION_USER_AGENT {
        let Some(last) = bytes.iter().position(|b| *b == 0) else {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        };
        bytes.truncate(last + 1);
    }
    if handle == 0 {
        p.extra.wininet.global_options.insert(option, bytes);
    } else {
        p.extra.wininet.options.insert((handle, option), bytes);
    }
    write_last_error(p, 0);
    Ok(1)
}

fn internet_query_option_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let option = p.arg(1) as u32;
    let buffer = p.arg(2);
    let length_pointer = p.arg(3);
    if handle != 0 && !p.extra.wininet.handles.contains_key(&handle) {
        return Ok(fail(p, ERROR_INVALID_HANDLE));
    }
    if length_pointer == 0 {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let Some(capacity) = read_u32(p, length_pointer) else {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    };
    let Some(value) = option_value(p, handle, option) else {
        return Ok(fail(p, ERROR_INTERNET_BAD_OPTION));
    };
    if buffer == 0 || capacity < value.len() as u32 {
        if p.mem.write_u32(length_pointer, value.len() as u32).is_err() {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        }
        return Ok(fail(p, ERROR_INSUFFICIENT_BUFFER));
    }
    if p.mem.write_from(buffer, &value).is_err()
        || p.mem.write_u32(length_pointer, value.len() as u32).is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    write_last_error(p, 0);
    Ok(1)
}

fn internet_set_status_callback(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let callback = p.arg(1);
    if !p.extra.wininet.handles.contains_key(&handle) {
        return Ok(u64::MAX);
    }
    let mut current = handle;
    let mut previous = 0;
    for _ in 0..32 {
        if let Some(callback) = p.extra.wininet.callbacks.get(&current) {
            previous = *callback;
            break;
        }
        let Some(value) = p.extra.wininet.handles.get(&current) else {
            break;
        };
        let Some(parent) = handle_parent(value) else {
            break;
        };
        current = parent;
    }
    if callback == INTERNET_INVALID_STATUS_CALLBACK {
        p.extra.wininet.callbacks.remove(&handle);
    } else {
        p.extra.wininet.callbacks.insert(handle, callback);
    }
    write_last_error(p, 0);
    Ok(previous)
}

fn http_query_info_a(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    let query = p.arg(1) as u32;
    let buffer = p.arg(2);
    let length_pointer = p.arg(3);
    let index_pointer = p.arg(4);
    let Some(request) = p.extra.wininet.handles.get(&handle) else {
        return Ok(fail(p, ERROR_INVALID_HANDLE));
    };
    let WinInetHandle::Request {
        verb,
        headers,
        transport_error,
        response,
        ..
    } = request
    else {
        return Ok(fail(p, ERROR_INTERNET_INCORRECT_HANDLE_TYPE));
    };
    if length_pointer == 0 {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    let request_headers = query & HTTP_QUERY_FLAG_REQUEST_HEADERS != 0;
    let numeric = query & HTTP_QUERY_FLAG_NUMBER != 0;
    let base_query =
        query & !(HTTP_QUERY_FLAG_REQUEST_HEADERS | HTTP_QUERY_FLAG_NUMBER | 0x4000_0000);
    let query_index = if index_pointer == 0 {
        0
    } else {
        match read_u32(p, index_pointer) {
            Some(value) => value as usize,
            None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
        }
    };
    let raw_headers = base_query == HTTP_QUERY_RAW_HEADERS;
    let value = if request_headers {
        if base_query == HTTP_QUERY_REQUEST_METHOD {
            Some(encode_a(verb))
        } else if matches!(
            base_query,
            HTTP_QUERY_RAW_HEADERS | HTTP_QUERY_RAW_HEADERS_CRLF
        ) {
            if base_query == HTTP_QUERY_RAW_HEADERS {
                let mut raw = headers.join("\0").into_bytes();
                raw.extend([0, 0]);
                Some(raw)
            } else {
                Some(headers.join("\r\n").into_bytes())
            }
        } else if base_query == HTTP_QUERY_CUSTOM {
            let name = read_a_string(p, buffer).unwrap_or_default();
            headers
                .iter()
                .filter_map(|header| {
                    let (key, value) = header.split_once(':')?;
                    key.eq_ignore_ascii_case(&name)
                        .then(|| encode_a(value.trim()))
                })
                .nth(query_index)
        } else {
            None
        }
    } else if transport_error.is_some() {
        return Ok(fail(p, transport_error.unwrap()));
    } else if response.is_none() {
        return Ok(fail(p, ERROR_INTERNET_INCORRECT_HANDLE_STATE));
    } else {
        None
    };
    let Some(value) = value else {
        return Ok(fail(p, ERROR_HTTP_HEADER_NOT_FOUND));
    };
    let numeric_value = if numeric {
        let Some(value) = std::str::from_utf8(&value)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            return Ok(fail(p, ERROR_HTTP_HEADER_NOT_FOUND));
        };
        Some(value)
    } else {
        None
    };
    let mut output = if let Some(value) = numeric_value {
        value.to_le_bytes().to_vec()
    } else if raw_headers {
        value
    } else {
        let mut bytes = value;
        bytes.push(0);
        bytes
    };
    let capacity = match read_u32(p, length_pointer) {
        Some(value) => value,
        None => return Ok(fail(p, ERROR_INVALID_PARAMETER)),
    };
    if buffer == 0 || capacity < output.len() as u32 {
        if p.mem
            .write_u32(length_pointer, output.len() as u32)
            .is_err()
        {
            return Ok(fail(p, ERROR_INVALID_PARAMETER));
        }
        return Ok(fail(p, ERROR_INSUFFICIENT_BUFFER));
    }
    if p.mem.write_from(buffer, &output).is_err() {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    if p.mem
        .write_u32(length_pointer, output.len() as u32)
        .is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    if index_pointer != 0
        && p.mem
            .write_u32(index_pointer, query_index.saturating_add(1) as u32)
            .is_err()
    {
        return Ok(fail(p, ERROR_INVALID_PARAMETER));
    }
    output.clear();
    write_last_error(p, 0);
    Ok(1)
}

fn internet_error_dlg(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(1);
    if !matches!(
        p.extra.wininet.handles.get(&handle),
        Some(WinInetHandle::Request { .. })
    ) {
        return Ok(if p.extra.wininet.handles.contains_key(&handle) {
            ERROR_INTERNET_INCORRECT_HANDLE_TYPE as u64
        } else {
            ERROR_INVALID_HANDLE as u64
        });
    }
    // A browser credential/certificate prompt cannot be serviced in this headless guest.
    write_last_error(p, ERROR_INTERNET_NEED_UI);
    Ok(ERROR_INTERNET_NEED_UI as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{process::TEB_ADDR, Vfs};
    use anywasm_cpu::mem::PROT_RW;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        // WinInet imports are exercised with PE32; guest buffers and the x86 stack must fit in
        // the 32-bit address space before arguments are truncated to DWORDs.
        p.vmem_next = 0x1000_0000;
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        p.extra.wininet.next_handle = 0x8000_1000;
        p
    }

    fn cstr(p: &mut Process, value: &str) -> u64 {
        let address = p.alloc_virtual(value.len() as u64 + 1, PROT_RW);
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        p.mem.write_from(address, &bytes).unwrap();
        address
    }

    fn wstr(p: &mut Process, value: &str) -> u64 {
        let bytes: Vec<u8> = value
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain([0, 0])
            .collect();
        let address = p.alloc_virtual(bytes.len() as u64, PROT_RW);
        p.mem.write_from(address, &bytes).unwrap();
        address
    }

    fn invoke_x86(p: &mut Process, f: HleFn, args: &[u32]) -> u64 {
        let stack = p.alloc_virtual((4 * (args.len() + 1)) as u64, PROT_RW);
        p.mem.write_u32(stack, 0xfeed).unwrap();
        for (i, arg) in args.iter().enumerate() {
            p.mem.write_u32(stack + 4 + 4 * i as u64, *arg).unwrap();
        }
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[anywasm_cpu::cpu::RSP] = stack;
        f(p).unwrap()
    }

    fn set_args_x64(p: &mut Process, args: &[u64]) {
        use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RCX] = args.first().copied().unwrap_or(0);
        p.cpu.gpr[RDX] = args.get(1).copied().unwrap_or(0);
        p.cpu.gpr[R8] = args.get(2).copied().unwrap_or(0);
        p.cpu.gpr[R9] = args.get(3).copied().unwrap_or(0);
        let stack = p.alloc_virtual(0x100, PROT_RW);
        p.cpu.gpr[RSP] = stack;
        for i in 4..args.len() {
            p.mem
                .write_u64(stack + 0x28 + 8 * (i - 4) as u64, args[i])
                .unwrap();
        }
    }

    #[test]
    fn x86_stdcall_bytes_match_all_import_signatures() {
        let expected = [
            ("HttpAddRequestHeadersA", 16),
            ("InternetErrorDlg", 20),
            ("InternetSetOptionA", 16),
            ("InternetReadFile", 16),
            ("InternetCrackUrlA", 16),
            ("InternetCloseHandle", 4),
            ("InternetConnectA", 32),
            ("HttpOpenRequestA", 32),
            ("InternetGetConnectedState", 8),
            ("InternetSetCookieA", 12),
            ("InternetGetCookieW", 16),
            ("HttpQueryInfoA", 20),
            ("InternetQueryOptionA", 16),
            ("HttpSendRequestA", 20),
            ("InternetSetStatusCallback", 8),
            ("InternetOpenA", 20),
            ("HttpSendRequestExA", 20),
            ("InternetWriteFile", 16),
            ("HttpEndRequestA", 16),
        ];
        for (name, bytes) in expected {
            assert_eq!(stdcall_stack_bytes(name), Some(bytes), "{name}");
            assert!(lookup(name).is_some(), "{name} must resolve");
        }
        assert_eq!(stdcall_stack_bytes("not-an-import"), None);
    }

    #[test]
    fn x86_session_connection_request_and_close_follow_handle_tree() {
        let mut p = process();
        let agent = cstr(&mut p, "AnyWASM test");
        let host = cstr(&mut p, "example.invalid");
        let object = cstr(&mut p, "/status");
        let session = invoke_x86(&mut p, internet_open_a, &[agent as u32, 1, 0, 0, 0]);
        let connection = invoke_x86(
            &mut p,
            internet_connect_a,
            &[
                session as u32,
                host as u32,
                443,
                0,
                0,
                INTERNET_SERVICE_HTTP,
                0,
                0,
            ],
        );
        let request = invoke_x86(
            &mut p,
            http_open_request_a,
            &[connection as u32, 0, object as u32, 0, 0, 0, 0, 0],
        );
        assert!(matches!(
            p.extra.wininet.handles[&session],
            WinInetHandle::Session { .. }
        ));
        assert!(matches!(
            p.extra.wininet.handles[&connection],
            WinInetHandle::Connection { .. }
        ));
        assert!(
            matches!(&p.extra.wininet.handles[&request], WinInetHandle::Request { verb, .. } if verb == "GET")
        );
        assert_eq!(
            invoke_x86(&mut p, internet_close_handle, &[session as u32]),
            1
        );
        assert!(p.extra.wininet.handles.is_empty());
    }

    #[test]
    fn http_send_fails_with_real_offline_transport_error_and_read_sets_zero() {
        let mut p = process();
        let session = 0x8000_1000;
        let connection = 0x8000_1004;
        let request = 0x8000_1008;
        p.extra.wininet.handles.insert(
            session,
            WinInetHandle::Session {
                agent: "test".into(),
                access_type: 1,
                proxy: None,
                proxy_bypass: None,
                flags: 0,
            },
        );
        p.extra.wininet.handles.insert(
            connection,
            WinInetHandle::Connection {
                parent: session,
                host: "example.invalid".into(),
                port: 80,
                service: INTERNET_SERVICE_HTTP,
                username: None,
                password: None,
                flags: 0,
                context: 0,
            },
        );
        p.extra.wininet.handles.insert(
            request,
            WinInetHandle::Request {
                parent: connection,
                verb: "GET".into(),
                object: "/".into(),
                version: None,
                referrer: None,
                accept_types: vec![],
                flags: 0,
                context: 0,
                headers: vec![],
                body: vec![],
                response: None,
                response_offset: 0,
                transport_error: None,
                write_phase: false,
            },
        );
        let result = invoke_x86(&mut p, http_send_request_a, &[request as u32, 0, 0, 0, 0]);
        assert_eq!(result, 0);
        assert_eq!(
            p.rd32(TEB_ADDR + 0x34).unwrap(),
            ERROR_INTERNET_CANNOT_CONNECT
        );
        let read_count = p.alloc_virtual(4, PROT_RW);
        let read_buffer = p.alloc_virtual(16, PROT_RW);
        p.wr32(read_count, 0xfeed).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_read_file,
                &[request as u32, read_buffer as u32, 16, read_count as u32]
            ),
            0
        );
        assert_eq!(p.rd32(read_count).unwrap(), 0);
        assert_eq!(
            p.rd32(TEB_ADDR + 0x34).unwrap(),
            ERROR_INTERNET_CANNOT_CONNECT
        );
    }

    #[test]
    fn options_support_two_call_buffer_sizing_and_stateful_timeouts() {
        let mut p = process();
        let session = 0x8000_1000;
        p.extra.wininet.handles.insert(
            session,
            WinInetHandle::Session {
                agent: "agent".into(),
                access_type: 1,
                proxy: None,
                proxy_bypass: None,
                flags: 0,
            },
        );
        let timeout = p.alloc_virtual(4, PROT_RW);
        p.wr32(timeout, 12_345).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_set_option_a,
                &[
                    session as u32,
                    INTERNET_OPTION_CONNECT_TIMEOUT,
                    timeout as u32,
                    4
                ]
            ),
            1
        );
        let length = p.alloc_virtual(4, PROT_RW);
        p.wr32(length, 0).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_query_option_a,
                &[
                    session as u32,
                    INTERNET_OPTION_CONNECT_TIMEOUT,
                    0,
                    length as u32
                ]
            ),
            0
        );
        assert_eq!(p.rd32(length).unwrap(), 4);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INSUFFICIENT_BUFFER);
        let output = p.alloc_virtual(4, PROT_RW);
        p.wr32(length, 4).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_query_option_a,
                &[
                    session as u32,
                    INTERNET_OPTION_CONNECT_TIMEOUT,
                    output as u32,
                    length as u32
                ]
            ),
            1
        );
        assert_eq!(p.rd32(output).unwrap(), 12_345);
    }

    #[test]
    fn cookies_are_process_local_and_unicode_query_reports_capacity() {
        let mut p = process();
        let url = cstr(&mut p, "https://example.invalid/game");
        let name = cstr(&mut p, "sid");
        let value = cstr(&mut p, "abc123");
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_set_cookie_a,
                &[url as u32, name as u32, value as u32]
            ),
            1
        );
        let wide_url = wstr(&mut p, "https://example.invalid/game");
        let length = p.alloc_virtual(4, PROT_RW);
        p.wr32(length, 0).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_get_cookie_w,
                &[wide_url as u32, 0, 0, length as u32]
            ),
            0
        );
        let needed = p.rd32(length).unwrap();
        assert_eq!(needed, ("sid=abc123".encode_utf16().count() as u32 + 1) * 2);
        let capacity_chars = "sid=abc123".encode_utf16().count() as u32 + 1;
        let short_output = p.alloc_virtual(needed as u64, PROT_RW);
        p.wr32(length, capacity_chars - 1).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_get_cookie_w,
                &[wide_url as u32, 0, short_output as u32, length as u32]
            ),
            0
        );
        assert_eq!(p.rd32(length).unwrap(), needed);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INSUFFICIENT_BUFFER);
        let output = p.alloc_virtual(needed as u64, PROT_RW);
        p.wr32(length, capacity_chars).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_get_cookie_w,
                &[wide_url as u32, 0, output as u32, length as u32]
            ),
            1
        );
        assert_eq!(p.read_wstr(output).unwrap(), "sid=abc123");
    }

    #[test]
    fn crack_url_writes_x86_url_components_and_rejects_short_buffers() {
        let mut p = process();
        let url = cstr(&mut p, "https://user:pw@example.invalid:8443/a/b?q=1#frag");
        let components = p.alloc_virtual(60, PROT_RW);
        let scheme = p.alloc_virtual(16, PROT_RW);
        let host = p.alloc_virtual(32, PROT_RW);
        let path = p.alloc_virtual(64, PROT_RW);
        let extra = p.alloc_virtual(64, PROT_RW);
        p.wr32(components, 60).unwrap();
        p.wr32(components + 4, scheme as u32).unwrap();
        p.wr32(components + 8, 16).unwrap();
        p.wr32(components + 16, host as u32).unwrap();
        p.wr32(components + 20, 32).unwrap();
        p.wr32(components + 44, path as u32).unwrap();
        p.wr32(components + 48, 64).unwrap();
        p.wr32(components + 52, extra as u32).unwrap();
        p.wr32(components + 56, 64).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_crack_url_a,
                &[url as u32, 0, 0, components as u32]
            ),
            1
        );
        assert_eq!(p.read_cstr(scheme).unwrap(), "https");
        assert_eq!(p.read_cstr(host).unwrap(), "example.invalid");
        assert_eq!(p.read_cstr(path).unwrap(), "/a/b");
        assert_eq!(p.read_cstr(extra).unwrap(), "?q=1#frag");
        assert_eq!(p.rd16(components + 24).unwrap(), 8443);
        assert_eq!(
            p.rd32(components + 12).unwrap(),
            INTERNET_SCHEME_HTTPS as u32
        );
        p.wr32(components + 8, 2).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_crack_url_a,
                &[url as u32, 0, 0, components as u32]
            ),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INSUFFICIENT_BUFFER);
    }

    #[test]
    fn request_headers_are_queryable_before_transfer_and_callback_is_registered() {
        let mut p = process();
        let session = 0x8000_1000;
        let connection = 0x8000_1004;
        let request = 0x8000_1008;
        p.extra.wininet.handles.insert(
            session,
            WinInetHandle::Session {
                agent: "agent".into(),
                access_type: 1,
                proxy: None,
                proxy_bypass: None,
                flags: 0,
            },
        );
        p.extra.wininet.handles.insert(
            connection,
            WinInetHandle::Connection {
                parent: session,
                host: "example.invalid".into(),
                port: 80,
                service: INTERNET_SERVICE_HTTP,
                username: None,
                password: None,
                flags: 0,
                context: 0,
            },
        );
        p.extra.wininet.handles.insert(
            request,
            WinInetHandle::Request {
                parent: connection,
                verb: "GET".into(),
                object: "/".into(),
                version: None,
                referrer: None,
                accept_types: vec![],
                flags: 0,
                context: 0,
                headers: vec![],
                body: vec![],
                response: None,
                response_offset: 0,
                transport_error: None,
                write_phase: false,
            },
        );
        let header = cstr(&mut p, "X-Game: Sims\r\n");
        assert_eq!(
            invoke_x86(
                &mut p,
                http_add_request_headers_a,
                &[
                    request as u32,
                    header as u32,
                    u32::MAX,
                    HTTP_ADDREQ_FLAG_REPLACE
                ]
            ),
            1
        );
        let output = cstr(&mut p, "X-Game");
        let length = p.alloc_virtual(4, PROT_RW);
        p.wr32(length, 16).unwrap();
        assert_eq!(
            invoke_x86(
                &mut p,
                http_query_info_a,
                &[
                    request as u32,
                    (HTTP_QUERY_CUSTOM | HTTP_QUERY_FLAG_REQUEST_HEADERS),
                    output as u32,
                    length as u32,
                    0
                ]
            ),
            1
        );
        assert_eq!(p.read_cstr(output).unwrap(), "Sims");
        assert_eq!(
            invoke_x86(
                &mut p,
                internet_set_status_callback,
                &[request as u32, 0x1234]
            ),
            0
        );
        assert_eq!(
            invoke_x86(&mut p, internet_set_status_callback, &[request as u32, 0]),
            0x1234
        );
    }

    #[test]
    fn connected_state_and_error_dialog_report_offline_headless_state() {
        let mut p = process();
        let flags = p.alloc_virtual(4, PROT_RW);
        p.wr32(flags, 0xffff).unwrap();
        set_args_x64(&mut p, &[flags]);
        assert_eq!(internet_get_connected_state(&mut p).unwrap(), 0);
        assert_eq!(p.rd32(flags).unwrap(), 0);
        let request = 0x8000_1000;
        p.extra.wininet.handles.insert(
            request,
            WinInetHandle::Request {
                parent: 0,
                verb: "GET".into(),
                object: "/".into(),
                version: None,
                referrer: None,
                accept_types: vec![],
                flags: 0,
                context: 0,
                headers: vec![],
                body: vec![],
                response: None,
                response_offset: 0,
                transport_error: None,
                write_phase: false,
            },
        );
        set_args_x64(&mut p, &[0, request, 0, 0, 0]);
        assert_eq!(
            internet_error_dlg(&mut p).unwrap(),
            ERROR_INTERNET_NEED_UI as u64
        );
    }
}
