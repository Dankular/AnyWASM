//! IMM32 translation backed by process-local input-context state.
//!
//! The runtime has no host IME conversion engine. It does preserve application-supplied
//! composition data, open state, and candidate-list state faithfully; requests that require
//! language conversion or synthesizing candidates fail explicitly.

use crate::process::{HleFn, Process, Stop};
use std::collections::HashMap;

const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_NO_DATA: u32 = 232;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;

const IMM_ERROR_NODATA: i32 = -1;
const IMM_ERROR_GENERAL: i32 = -2;

const GCS_COMPREADSTR: u32 = 0x0001;
const GCS_COMPREADATTR: u32 = 0x0002;
const GCS_COMPREADCLAUSE: u32 = 0x0004;
const GCS_COMPSTR: u32 = 0x0008;
const GCS_COMPATTR: u32 = 0x0010;
const GCS_COMPCLAUSE: u32 = 0x0020;
const GCS_CURSORPOS: u32 = 0x0080;
const GCS_DELTASTART: u32 = 0x0100;
const GCS_RESULTREADSTR: u32 = 0x0200;
const GCS_RESULTREADCLAUSE: u32 = 0x0400;
const GCS_RESULTSTR: u32 = 0x0800;
const GCS_RESULTCLAUSE: u32 = 0x1000;

const SCS_SETSTR: u32 = GCS_COMPREADSTR | GCS_COMPSTR;

const NI_CHANGECANDIDATELIST: u32 = 0x0013;
const NI_CLOSECANDIDATE: u32 = 0x0011;
const NI_FINALIZECONVERSIONRESULT: u32 = 0x0014;
const NI_COMPOSITIONSTR: u32 = 0x0015;
const NI_OPENCANDIDATE: u32 = 0x0010;
const NI_SELECTCANDIDATESTR: u32 = 0x0012;
const NI_SETCANDIDATE_PAGESIZE: u32 = 0x0017;
const NI_SETCANDIDATE_PAGESTART: u32 = 0x0016;
const NI_IMEMENUSELECTED: u32 = 0x0018;

const CPS_COMPLETE: u32 = 0x0001;
const CPS_CONVERT: u32 = 0x0002;
const CPS_REVERT: u32 = 0x0003;
const CPS_CANCEL: u32 = 0x0004;

const SCS_CHANGEATTR: u32 = 0x0012;
const SCS_CHANGECLAUSE: u32 = 0x0014;
const SCS_SETRECONVERTSTRING: u32 = 0x0001_0000;
const SCS_QUERYRECONVERTSTRING: u32 = 0x0002_0000;

const HIMC_BASE: u32 = 0x6000_0000;
const MAX_CONTEXTS: u32 = 0x00ff_ffff;
const MAX_IMM_BUFFER: usize = 1 << 20;

#[derive(Default)]
pub struct ImmState {
    next_context: u32,
    contexts_by_window: HashMap<u32, u32>,
    contexts: HashMap<u32, InputContext>,
}

#[derive(Default)]
struct InputContext {
    window: u32,
    references: u32,
    open: bool,
    composition: Option<Vec<u16>>,
    reading: Option<Vec<u16>>,
    result: Option<Vec<u16>>,
    result_reading: Option<Vec<u16>>,
    composition_attributes: Option<Vec<u8>>,
    reading_attributes: Option<Vec<u8>>,
    composition_clauses: Option<Vec<u32>>,
    reading_clauses: Option<Vec<u32>>,
    result_clauses: Option<Vec<u32>>,
    result_reading_clauses: Option<Vec<u32>>,
    cursor_position: Option<u32>,
    delta_start: Option<u32>,
    candidate_lists: HashMap<u32, CandidateList>,
    selected_candidate_list: Option<u32>,
}

#[derive(Clone, Default)]
struct CandidateList {
    style: u32,
    strings: Vec<Vec<u16>>,
    selection: u32,
    page_start: u32,
    page_size: u32,
}

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "ImmGetCompositionStringW" => Some(imm_get_composition_string_w),
        "ImmSetCompositionStringW" => Some(imm_set_composition_string_w),
        "ImmNotifyIME" => Some(imm_notify_ime),
        "ImmSetOpenStatus" => Some(imm_set_open_status),
        "ImmGetOpenStatus" => Some(imm_get_open_status),
        "ImmGetContext" => Some(imm_get_context),
        "ImmReleaseContext" => Some(imm_release_context),
        "ImmGetCandidateListW" => Some(imm_get_candidate_list_w),
        _ => None,
    }
}

fn invalid_handle(p: &mut Process) {
    p.set_last_error(ERROR_INVALID_HANDLE);
}

fn context_is_locked(p: &Process, himc: u64) -> bool {
    p.extra
        .imm32
        .contexts
        .get(&(himc as u32))
        .is_some_and(|context| context.references != 0)
}

fn utf16_bytes(value: &[u16]) -> Vec<u8> {
    value.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn dword_bytes(value: &[u32]) -> Vec<u8> {
    value.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn composition_data(context: &InputContext, index: u32) -> Result<Option<Vec<u8>>, ()> {
    let data = match index {
        GCS_COMPREADSTR => context.reading.as_ref().map(|value| utf16_bytes(value)),
        GCS_COMPREADATTR => context.reading_attributes.as_ref().cloned(),
        GCS_COMPREADCLAUSE => context
            .reading_clauses
            .as_ref()
            .map(|value| dword_bytes(value)),
        GCS_COMPSTR => context.composition.as_ref().map(|value| utf16_bytes(value)),
        GCS_COMPATTR => context.composition_attributes.as_ref().cloned(),
        GCS_COMPCLAUSE => context
            .composition_clauses
            .as_ref()
            .map(|value| dword_bytes(value)),
        GCS_CURSORPOS => context
            .cursor_position
            .map(|value| value.to_le_bytes().to_vec()),
        GCS_DELTASTART => context
            .delta_start
            .map(|value| value.to_le_bytes().to_vec()),
        GCS_RESULTREADSTR => context
            .result_reading
            .as_ref()
            .map(|value| utf16_bytes(value)),
        GCS_RESULTREADCLAUSE => context
            .result_reading_clauses
            .as_ref()
            .map(|value| dword_bytes(value)),
        GCS_RESULTSTR => context.result.as_ref().map(|value| utf16_bytes(value)),
        GCS_RESULTCLAUSE => context
            .result_clauses
            .as_ref()
            .map(|value| dword_bytes(value)),
        _ => return Err(()),
    };
    Ok(data)
}

fn signed_result(value: i32) -> u64 {
    value as u32 as u64
}

fn copy_sized_result(
    p: &mut Process,
    bytes: &[u8],
    buffer: u64,
    capacity: u32,
) -> Result<u64, Stop> {
    if capacity == 0 {
        return Ok(bytes.len() as u64);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(signed_result(IMM_ERROR_GENERAL));
    }
    let copied = bytes.len().min(capacity as usize);
    if copied != 0 {
        p.write_bytes(buffer, &bytes[..copied])?;
    }
    Ok(copied as u64)
}

fn imm_get_context(p: &mut Process) -> Result<u64, Stop> {
    let hwnd = p.arg(0) as u32;
    if !p.extra.win.windows.contains_key(&(hwnd as u64)) {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    }

    let existing = p.extra.imm32.contexts_by_window.get(&hwnd).copied();
    let handle = if let Some(handle) = existing {
        handle
    } else {
        let state = &mut p.extra.imm32;
        let mut allocated = None;
        for _ in 0..MAX_CONTEXTS {
            state.next_context = state.next_context % MAX_CONTEXTS + 1;
            let candidate = HIMC_BASE | state.next_context;
            if !state.contexts.contains_key(&candidate) {
                allocated = Some(candidate);
                break;
            }
        }
        let Some(handle) = allocated else {
            p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return Ok(0);
        };
        state.contexts_by_window.insert(hwnd, handle);
        state.contexts.insert(
            handle,
            InputContext {
                window: hwnd,
                ..InputContext::default()
            },
        );
        handle
    };
    let context = p
        .extra
        .imm32
        .contexts
        .get_mut(&handle)
        .expect("mapped HIMC");
    let Some(references) = context.references.checked_add(1) else {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    };
    context.references = references;
    Ok(handle as u64)
}

fn imm_release_context(p: &mut Process) -> Result<u64, Stop> {
    let hwnd = p.arg(0) as u32;
    let himc = p.arg(1) as u32;
    let Some(context) = p.extra.imm32.contexts.get_mut(&himc) else {
        invalid_handle(p);
        return Ok(0);
    };
    if context.window != hwnd || context.references == 0 {
        invalid_handle(p);
        return Ok(0);
    }
    context.references -= 1;
    Ok(1)
}

fn imm_get_open_status(p: &mut Process) -> Result<u64, Stop> {
    let himc = p.arg(0);
    let Some(context) = p.extra.imm32.contexts.get(&(himc as u32)) else {
        invalid_handle(p);
        return Ok(0);
    };
    if context.references == 0 {
        invalid_handle(p);
        return Ok(0);
    }
    Ok(context.open as u64)
}

fn imm_set_open_status(p: &mut Process) -> Result<u64, Stop> {
    let (himc, open) = (p.arg(0) as u32, p.arg(1) != 0);
    let Some(context) = p.extra.imm32.contexts.get_mut(&himc) else {
        invalid_handle(p);
        return Ok(0);
    };
    if context.references == 0 {
        invalid_handle(p);
        return Ok(0);
    }
    context.open = open;
    Ok(1)
}

fn read_ime_buffer(p: &mut Process, pointer: u64, length: u32) -> Result<Option<Vec<u8>>, Stop> {
    if pointer == 0 {
        if length == 0 {
            return Ok(None);
        }
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(None);
    }
    if length as usize > MAX_IMM_BUFFER {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(None);
    }
    Ok(Some(p.read_bytes(pointer, length as usize)?))
}

fn to_utf16(bytes: &[u8]) -> Option<Vec<u16>> {
    if bytes.len() & 1 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

fn imm_set_composition_string_w(p: &mut Process) -> Result<u64, Stop> {
    let (himc, index, comp_ptr, comp_len, read_ptr, read_len) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as u32,
        p.arg(4),
        p.arg(5) as u32,
    );
    if index != SCS_SETSTR {
        p.set_last_error(
            if matches!(
                index,
                SCS_CHANGEATTR
                    | SCS_CHANGECLAUSE
                    | SCS_SETRECONVERTSTRING
                    | SCS_QUERYRECONVERTSTRING
            ) {
                ERROR_CALL_NOT_IMPLEMENTED
            } else {
                ERROR_INVALID_PARAMETER
            },
        );
        return Ok(0);
    }
    if (comp_ptr == 0 && comp_len != 0)
        || (read_ptr == 0 && read_len != 0)
        || (comp_ptr == 0 && read_ptr == 0)
        || comp_len as usize > MAX_IMM_BUFFER
        || read_len as usize > MAX_IMM_BUFFER
        || comp_len & 1 != 0
        || read_len & 1 != 0
    {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !context_is_locked(p, himc as u64) {
        invalid_handle(p);
        return Ok(0);
    }

    let comp_bytes = read_ime_buffer(p, comp_ptr, comp_len)?;
    let read_bytes = read_ime_buffer(p, read_ptr, read_len)?;
    // Pointer/length validation above guarantees both optional byte strings have even size.
    let comp = comp_bytes.map(|bytes| to_utf16(&bytes).expect("validated UTF-16 byte length"));
    let read = read_bytes.map(|bytes| to_utf16(&bytes).expect("validated UTF-16 byte length"));
    let context = p
        .extra
        .imm32
        .contexts
        .get_mut(&himc)
        .expect("locked context");
    if let Some(comp) = comp {
        context.composition = Some(comp);
        context.composition_attributes = None;
        context.composition_clauses = None;
        context.cursor_position = context
            .composition
            .as_ref()
            .map(|value| value.len().min(u32::MAX as usize) as u32);
        context.delta_start = Some(0);
        // A new composition supersedes the previous result data in this context.
        context.result = None;
        context.result_reading = None;
        context.result_clauses = None;
        context.result_reading_clauses = None;
    }
    if let Some(read) = read {
        context.reading = Some(read);
        context.reading_attributes = None;
        context.reading_clauses = None;
    }
    Ok(1)
}

fn imm_get_composition_string_w(p: &mut Process) -> Result<u64, Stop> {
    let (himc, index, buffer, capacity) =
        (p.arg(0) as u32, p.arg(1) as u32, p.arg(2), p.arg(3) as u32);
    let data = {
        let Some(context) = p.extra.imm32.contexts.get(&(himc as u32)) else {
            invalid_handle(p);
            return Ok(signed_result(IMM_ERROR_GENERAL));
        };
        if context.references == 0 {
            invalid_handle(p);
            return Ok(signed_result(IMM_ERROR_GENERAL));
        }
        composition_data(context, index)
    };
    let bytes = match data {
        Err(()) => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(signed_result(IMM_ERROR_GENERAL));
        }
        Ok(None) => return Ok(signed_result(IMM_ERROR_NODATA)),
        Ok(Some(bytes)) => bytes,
    };
    if bytes.len() > MAX_IMM_BUFFER {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(signed_result(IMM_ERROR_GENERAL));
    }
    copy_sized_result(p, &bytes, buffer, capacity)
}

fn serialize_candidate_list(list: &CandidateList) -> Option<Vec<u8>> {
    let count = list.strings.len();
    let offsets_bytes = count.checked_mul(4)?;
    let mut bytes = vec![0; 24usize.checked_add(offsets_bytes)?];
    bytes[4..8].copy_from_slice(&list.style.to_le_bytes());
    bytes[8..12].copy_from_slice(&(count as u32).to_le_bytes());
    bytes[12..16].copy_from_slice(&list.selection.to_le_bytes());
    bytes[16..20].copy_from_slice(&list.page_start.to_le_bytes());
    bytes[20..24].copy_from_slice(&list.page_size.to_le_bytes());
    for (index, candidate) in list.strings.iter().enumerate() {
        let offset = u32::try_from(bytes.len()).ok()?;
        bytes[24 + index * 4..28 + index * 4].copy_from_slice(&offset.to_le_bytes());
        for unit in candidate {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    let size = u32::try_from(bytes.len()).ok()?;
    bytes[0..4].copy_from_slice(&size.to_le_bytes());
    Some(bytes)
}

fn imm_get_candidate_list_w(p: &mut Process) -> Result<u64, Stop> {
    let (himc, index, buffer, capacity) =
        (p.arg(0) as u32, p.arg(1) as u32, p.arg(2), p.arg(3) as u32);
    let Some(context) = p.extra.imm32.contexts.get(&himc) else {
        invalid_handle(p);
        return Ok(0);
    };
    if context.references == 0 {
        invalid_handle(p);
        return Ok(0);
    }
    let Some(list) = context.candidate_lists.get(&index) else {
        // No IME has supplied a candidate list. This is a real empty-list result;
        // callers can distinguish it from an active list by the zero return value.
        return Ok(0);
    };
    let Some(bytes) = serialize_candidate_list(list) else {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    };
    if capacity == 0 {
        return Ok(bytes.len() as u64);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let copied = bytes.len().min(capacity as usize);
    if copied != 0 {
        p.write_bytes(buffer, &bytes[..copied])?;
    }
    Ok(copied as u64)
}

fn imm_notify_ime(p: &mut Process) -> Result<u64, Stop> {
    let (himc, action, index, value) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
    );
    let Some(context) = p.extra.imm32.contexts.get_mut(&himc) else {
        invalid_handle(p);
        return Ok(0);
    };
    if context.references == 0 {
        invalid_handle(p);
        return Ok(0);
    }
    match action {
        NI_FINALIZECONVERSIONRESULT => {
            if finalize_composition(context) {
                Ok(1)
            } else {
                p.set_last_error(ERROR_NO_DATA);
                Ok(0)
            }
        }
        NI_COMPOSITIONSTR => match index {
            CPS_CANCEL => {
                context.composition = None;
                context.reading = None;
                context.composition_attributes = None;
                context.reading_attributes = None;
                context.composition_clauses = None;
                context.reading_clauses = None;
                context.result = None;
                context.result_reading = None;
                context.result_clauses = None;
                context.result_reading_clauses = None;
                context.cursor_position = None;
                context.delta_start = None;
                Ok(1)
            }
            CPS_COMPLETE => {
                if finalize_composition(context) {
                    Ok(1)
                } else {
                    p.set_last_error(ERROR_NO_DATA);
                    Ok(0)
                }
            }
            CPS_REVERT if context.composition.is_some() => Ok(1),
            CPS_REVERT => {
                p.set_last_error(ERROR_NO_DATA);
                Ok(0)
            }
            CPS_CONVERT => {
                // The real operation requires an installed language-specific IME.
                p.set_last_error(ERROR_CALL_NOT_IMPLEMENTED);
                Ok(0)
            }
            _ => {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                Ok(0)
            }
        },
        NI_CLOSECANDIDATE => {
            if index > 3 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            if context.candidate_lists.remove(&index).is_some() {
                if context.selected_candidate_list == Some(index) {
                    context.selected_candidate_list = None;
                }
                Ok(1)
            } else {
                p.set_last_error(ERROR_NO_DATA);
                Ok(0)
            }
        }
        NI_CHANGECANDIDATELIST => {
            if index > 3 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            if context.candidate_lists.contains_key(&index) {
                context.selected_candidate_list = Some(index);
                Ok(1)
            } else {
                p.set_last_error(ERROR_NO_DATA);
                Ok(0)
            }
        }
        NI_SELECTCANDIDATESTR => {
            if index > 3 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            let Some(list) = context.candidate_lists.get_mut(&index) else {
                p.set_last_error(ERROR_NO_DATA);
                return Ok(0);
            };
            if value as usize >= list.strings.len() {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            list.selection = value;
            Ok(1)
        }
        NI_SETCANDIDATE_PAGESIZE => {
            if index > 3 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            let Some(list) = context.candidate_lists.get_mut(&index) else {
                p.set_last_error(ERROR_NO_DATA);
                return Ok(0);
            };
            if value == 0 || value as usize > list.strings.len() {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            list.page_size = value;
            Ok(1)
        }
        NI_SETCANDIDATE_PAGESTART => {
            if index > 3 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            let Some(list) = context.candidate_lists.get_mut(&index) else {
                p.set_last_error(ERROR_NO_DATA);
                return Ok(0);
            };
            if value as usize >= list.strings.len() && !list.strings.is_empty() {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            list.page_start = value;
            Ok(1)
        }
        NI_OPENCANDIDATE | NI_IMEMENUSELECTED => {
            // Neither candidate generation nor IME menu dispatch has a provider in this runtime.
            let _ = value;
            p.set_last_error(ERROR_CALL_NOT_IMPLEMENTED);
            Ok(0)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(0)
        }
    }
}

fn finalize_composition(context: &mut InputContext) -> bool {
    let Some(composition) = context.composition.take() else {
        return false;
    };
    context.result = Some(composition);
    context.result_reading = context.reading.take();
    context.result_clauses = context.composition_clauses.take();
    context.result_reading_clauses = context.reading_clauses.take();
    context.composition_attributes = None;
    context.reading_attributes = None;
    context.cursor_position = None;
    context.delta_start = None;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::user32::Window;
    use crate::process::TEB_ADDR;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x20000;
    const STACK: u64 = BASE + 0x800;
    const HWND: u32 = 0x0001_2340;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE * 3, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.extra.win.windows.insert(
            HWND as u64,
            Window {
                class: "immtest".to_owned(),
                wndproc: 0,
                title: String::new(),
                x: 0,
                y: 0,
                w: 100,
                h: 100,
                client_x: 0,
                client_y: 0,
                client_w: 100,
                client_h: 100,
                visible: true,
                style: 0,
                ex_style: 0,
                has_menu: false,
                menu: 0,
                hinstance: 0,
                user_data: 0,
                parent: 0,
                control_id: 0,
                unicode: true,
                needs_paint: false,
                gpu_id: 0,
            },
        );
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, Stop> {
        for (index, arg) in args.iter().enumerate() {
            p.wr32(STACK + 4 + 4 * index as u64, *arg).unwrap();
        }
        lookup(name).expect("IMM32 import should resolve")(p)
    }

    fn get_himc(p: &mut Process) -> u32 {
        call(p, "ImmGetContext", &[HWND]).unwrap() as u32
    }

    fn last_error(p: &Process) -> u32 {
        p.rd32(TEB_ADDR + 0x34).unwrap()
    }

    fn bytes_of_utf16(value: &str) -> Vec<u8> {
        value
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect()
    }

    fn put_bytes(p: &mut Process, address: u64, bytes: &[u8]) {
        p.write_bytes(address, bytes).unwrap();
    }

    #[test]
    fn sims3_imm32_imports_resolve_with_x86_cleanup_metadata() {
        let names = [
            "ImmGetCompositionStringW",
            "ImmSetCompositionStringW",
            "ImmNotifyIME",
            "ImmSetOpenStatus",
            "ImmGetOpenStatus",
            "ImmGetContext",
            "ImmReleaseContext",
            "ImmGetCandidateListW",
        ];
        assert_eq!(crate::hle::group_of("imm32"), Some("imm32"));
        for name in names {
            assert!(lookup(name).is_some(), "imm32!{name} should resolve");
            assert!(crate::hle::lookup("imm32", name).is_some());
            assert_eq!(
                crate::x86_abi_generated::cleanup_bytes("imm32", name),
                Some(match name {
                    "ImmGetCompositionStringW" | "ImmGetCandidateListW" => 16,
                    "ImmNotifyIME" => 16,
                    "ImmSetCompositionStringW" => 24,
                    "ImmReleaseContext" => 8,
                    "ImmSetOpenStatus" => 8,
                    "ImmGetContext" | "ImmGetOpenStatus" => 4,
                    _ => unreachable!(),
                })
            );
        }
    }

    #[test]
    fn context_handles_are_per_window_locked_and_release_is_balanced() {
        let mut p = process();
        let himc = get_himc(&mut p);
        assert_ne!(himc, 0);
        assert_eq!(get_himc(&mut p), himc);
        let second_hwnd = HWND + 4;
        let second_window = p.extra.win.windows[&(HWND as u64)].clone();
        p.extra
            .win
            .windows
            .insert(second_hwnd as u64, second_window);
        let second_himc = call(&mut p, "ImmGetContext", &[second_hwnd]).unwrap() as u32;
        assert_ne!(second_himc, himc);
        assert_eq!(call(&mut p, "ImmGetOpenStatus", &[himc]), Ok(0));
        assert_eq!(call(&mut p, "ImmSetOpenStatus", &[himc, 1]), Ok(1));
        assert_eq!(call(&mut p, "ImmGetOpenStatus", &[himc]), Ok(1));
        assert_eq!(call(&mut p, "ImmGetOpenStatus", &[second_himc]), Ok(0));
        assert_eq!(call(&mut p, "ImmReleaseContext", &[HWND, himc]), Ok(1));
        assert_eq!(call(&mut p, "ImmReleaseContext", &[HWND, himc]), Ok(1));
        assert_eq!(call(&mut p, "ImmGetOpenStatus", &[himc]), Ok(0));
        assert_eq!(last_error(&p), ERROR_INVALID_HANDLE);
        assert_eq!(call(&mut p, "ImmReleaseContext", &[HWND, himc]), Ok(0));
    }

    #[test]
    fn composition_set_query_truncation_and_complete_use_utf16_byte_counts() {
        let mut p = process();
        let himc = get_himc(&mut p);
        let input = BASE + 0x1000;
        let output = BASE + 0x1100;
        let input_bytes = bytes_of_utf16("猫🙂");
        put_bytes(&mut p, input, &input_bytes);
        assert_eq!(
            call(
                &mut p,
                "ImmSetCompositionStringW",
                &[
                    himc,
                    SCS_SETSTR,
                    input as u32,
                    input_bytes.len() as u32,
                    0,
                    0
                ]
            ),
            Ok(1)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_COMPSTR, 0, 0]
            ),
            Ok(input_bytes.len() as u64)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_COMPSTR, output as u32, 3]
            ),
            Ok(3)
        );
        assert_eq!(&p.read_bytes(output, 3).unwrap(), &input_bytes[..3]);

        assert_eq!(
            call(
                &mut p,
                "ImmNotifyIME",
                &[himc, NI_COMPOSITIONSTR, CPS_COMPLETE, 0]
            ),
            Ok(1)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_COMPSTR, 0, 0]
            ),
            Ok(signed_result(IMM_ERROR_NODATA))
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_RESULTSTR, 0, 0]
            ),
            Ok(input_bytes.len() as u64)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmNotifyIME",
                &[himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0]
            ),
            Ok(1)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_RESULTSTR, 0, 0]
            ),
            Ok(signed_result(IMM_ERROR_NODATA))
        );
    }

    #[test]
    fn composition_set_accepts_reading_only_and_rejects_bad_abi_lengths() {
        let mut p = process();
        let himc = get_himc(&mut p);
        let reading = BASE + 0x1000;
        let output = BASE + 0x1100;
        let bytes = bytes_of_utf16("よみ");
        put_bytes(&mut p, reading, &bytes);
        assert_eq!(
            call(
                &mut p,
                "ImmSetCompositionStringW",
                &[himc, SCS_SETSTR, 0, 0, reading as u32, bytes.len() as u32]
            ),
            Ok(1)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCompositionStringW",
                &[himc, GCS_COMPREADSTR, output as u32, 32]
            ),
            Ok(bytes.len() as u64)
        );
        assert_eq!(p.read_bytes(output, bytes.len()).unwrap(), bytes);
        assert_eq!(
            call(
                &mut p,
                "ImmSetCompositionStringW",
                &[himc, SCS_SETSTR, reading as u32, 3, 0, 0]
            ),
            Ok(0)
        );
        assert_eq!(last_error(&p), ERROR_INVALID_PARAMETER);
        assert_eq!(
            call(
                &mut p,
                "ImmSetCompositionStringW",
                &[himc, SCS_CHANGEATTR, 0, 0, 0, 0]
            ),
            Ok(0)
        );
        assert_eq!(last_error(&p), ERROR_CALL_NOT_IMPLEMENTED);
    }

    #[test]
    fn candidate_list_layout_and_missing_candidate_state_are_truthful() {
        let mut p = process();
        let himc = get_himc(&mut p);
        assert_eq!(
            call(&mut p, "ImmGetCandidateListW", &[himc, 0, 0, 0]),
            Ok(0)
        );
        let list = CandidateList {
            style: 2,
            strings: vec!["一".encode_utf16().collect(), "二".encode_utf16().collect()],
            selection: 1,
            page_start: 0,
            page_size: 2,
        };
        let bytes = serialize_candidate_list(&list).unwrap();
        let output = BASE + 0x1000;
        p.extra
            .imm32
            .contexts
            .get_mut(&himc)
            .unwrap()
            .candidate_lists
            .insert(0, list);
        assert_eq!(
            call(&mut p, "ImmGetCandidateListW", &[himc, 0, 0, 0]),
            Ok(bytes.len() as u64)
        );
        assert_eq!(
            call(
                &mut p,
                "ImmGetCandidateListW",
                &[himc, 0, output as u32, bytes.len() as u32]
            ),
            Ok(bytes.len() as u64)
        );
        assert_eq!(p.read_bytes(output, bytes.len()).unwrap(), bytes);
        assert_eq!(
            call(&mut p, "ImmNotifyIME", &[himc, NI_SELECTCANDIDATESTR, 0, 0]),
            Ok(1)
        );
        assert_eq!(
            call(&mut p, "ImmNotifyIME", &[himc, NI_CLOSECANDIDATE, 0, 0]),
            Ok(1)
        );
        assert_eq!(
            call(&mut p, "ImmGetCandidateListW", &[himc, 0, 0, 0]),
            Ok(0)
        );
        assert_eq!(
            call(&mut p, "ImmNotifyIME", &[himc, NI_OPENCANDIDATE, 0, 0]),
            Ok(0)
        );
        assert_eq!(last_error(&p), ERROR_CALL_NOT_IMPLEMENTED);
    }

    #[test]
    fn notify_rejects_bad_himc_and_unavailable_conversion_engine() {
        let mut p = process();
        assert_eq!(call(&mut p, "ImmGetContext", &[0xdead_beef]), Ok(0));
        assert_eq!(last_error(&p), ERROR_INVALID_WINDOW_HANDLE);
        let himc = get_himc(&mut p);
        assert_eq!(
            call(
                &mut p,
                "ImmNotifyIME",
                &[himc, NI_COMPOSITIONSTR, CPS_CONVERT, 0]
            ),
            Ok(0)
        );
        assert_eq!(last_error(&p), ERROR_CALL_NOT_IMPLEMENTED);
    }
}
