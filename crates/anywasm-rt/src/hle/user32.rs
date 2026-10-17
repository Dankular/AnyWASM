//! user32 subset: window classes, windows, message queue/pump, input state, plus winmm timers.
//!
//! Windows are virtual; the first top-level window is mirrored to the browser canvas through the
//! GPU command stream (`gpu::op::WINDOW`). Window procedures are real guest code, entered with
//! `Process::call_guest`. `GetMessage` with an empty queue returns `Stop::Idle` *without* advancing
//! the guest, so the host can push input and resume at the same instruction.

use super::gpu::{op, Pk};
use super::*;
use crate::process::*;
use anywasm_cpu::mem::PROT_RW;
use anywasm_cpu::CpuMode;
use std::collections::{HashMap, HashSet, VecDeque};

pub const WM_CREATE: u64 = 0x0001;
pub const WM_DESTROY: u64 = 0x0002;
pub const WM_SIZE: u64 = 0x0005;
pub const WM_SETFOCUS: u64 = 0x0007;
pub const WM_KILLFOCUS: u64 = 0x0008;
pub const WM_WINDOWPOSCHANGING: u64 = 0x0046;
pub const WM_WINDOWPOSCHANGED: u64 = 0x0047;
pub const WM_PAINT: u64 = 0x000f;
pub const WM_CLOSE: u64 = 0x0010;
pub const WM_QUIT: u64 = 0x0012;
pub const WM_ERASEBKGND: u64 = 0x0014;
pub const WM_SHOWWINDOW: u64 = 0x0018;
pub const WM_TIMER: u64 = 0x0113;
pub const WM_DESTROYCLIPBOARD: u64 = 0x0307;
pub const WM_DRAWCLIPBOARD: u64 = 0x0308;
pub const WM_CHANGECBCHAIN: u64 = 0x030d;
pub const WM_ACTIVATE: u64 = 0x0006;
pub const WM_NCCREATE: u64 = 0x0081;
pub const WM_NCDESTROY: u64 = 0x0082;
pub const WM_NCCALCSIZE: u64 = 0x0083;
pub const WM_NCACTIVATE: u64 = 0x0086;
pub const WM_KEYDOWN: u64 = 0x0100;
pub const WM_KEYUP: u64 = 0x0101;

const GWL_WNDPROC: i32 = -4;
const GWL_HINSTANCE: i32 = -6;
const GWL_HWNDPARENT: i32 = -8;
const GWL_ID: i32 = -12;
const GWL_STYLE: i32 = -16;
const GWL_EXSTYLE: i32 = -20;
const GWL_USERDATA: i32 = -21;

fn window_long_stop(api: &str, index: i32) -> Stop {
    Stop::Unimplemented {
        dll: "user32".into(),
        name: format!("{api} index {index}"),
    }
}

fn get_window_long(p: &mut Process, pointer_sized: bool) -> R {
    let api = if pointer_sized {
        "GetWindowLongPtr"
    } else {
        "GetWindowLong"
    };
    let (hwnd, index) = (p.arg(0), p.arg(1) as i32);
    let Some(window) = p.extra.win.windows.get(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    let value = match index {
        GWL_WNDPROC => window.wndproc,
        GWL_HINSTANCE => window.hinstance,
        GWL_HWNDPARENT => window.parent,
        GWL_ID => window.control_id as u64,
        GWL_STYLE => window.style as u64,
        GWL_EXSTYLE => window.ex_style as u64,
        GWL_USERDATA => window.user_data,
        _ => return Err(window_long_stop(api, index)),
    };
    Ok(if pointer_sized {
        value
    } else {
        value as u32 as u64
    })
}

fn set_window_long(p: &mut Process, pointer_sized: bool) -> R {
    let api = if pointer_sized {
        "SetWindowLongPtr"
    } else {
        "SetWindowLong"
    };
    let (hwnd, index, raw_value) = (p.arg(0), p.arg(1) as i32, p.arg(2));
    let value = if pointer_sized {
        raw_value
    } else {
        raw_value as u32 as u64
    };
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    let previous = match index {
        GWL_WNDPROC => std::mem::replace(&mut window.wndproc, value),
        GWL_HINSTANCE => std::mem::replace(&mut window.hinstance, value),
        GWL_HWNDPARENT => std::mem::replace(&mut window.parent, value),
        GWL_ID => std::mem::replace(&mut window.control_id, value as u32) as u64,
        GWL_STYLE => std::mem::replace(&mut window.style, value as u32) as u64,
        GWL_EXSTYLE => std::mem::replace(&mut window.ex_style, value as u32) as u64,
        GWL_USERDATA => std::mem::replace(&mut window.user_data, value),
        _ => return Err(window_long_stop(api, index)),
    };
    Ok(if pointer_sized {
        previous
    } else {
        previous as u32 as u64
    })
}
pub const WM_CHAR: u64 = 0x0102;
pub const WM_SYSKEYDOWN: u64 = 0x0104;
pub const WM_SYSCHAR: u64 = 0x0106;
pub const WM_MOUSEMOVE: u64 = 0x0200;
pub const WM_LBUTTONDOWN: u64 = 0x0201;
pub const WM_LBUTTONUP: u64 = 0x0202;
pub const WM_RBUTTONDOWN: u64 = 0x0204;
pub const WM_RBUTTONUP: u64 = 0x0205;
pub const WM_MBUTTONDOWN: u64 = 0x0207;
pub const WM_MBUTTONUP: u64 = 0x0208;
pub const WM_MOUSEWHEEL: u64 = 0x020a;
pub const WM_MOUSELEAVE: u64 = 0x02a3;

const PM_REMOVE: u64 = 1;
const TME_LEAVE: u32 = 0x0000_0002;
const TME_CANCEL: u32 = 0x8000_0000;
const HWND_BASE: u64 = 0x0001_0010;
const HWND_DESKTOP: u64 = HWND_BASE;
const WS_CHILD: u32 = 0x4000_0000;
const WS_POPUP: u32 = 0x8000_0000;
const WS_MINIMIZE: u32 = 0x2000_0000;
const WS_MAXIMIZE: u32 = 0x0100_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const HWND_TOPMOST: u64 = 0xffff_ffff;
const HWND_NOTOPMOST: u64 = 0xffff_fffe;
const SW_SHOWNORMAL: u32 = 1;
const SW_SHOWMINIMIZED: u32 = 2;
const SW_MAXIMIZE: u32 = 3;
const SW_MINIMIZE: u32 = 6;
const SW_SHOWMINNOACTIVE: u32 = 7;
const SW_RESTORE: u32 = 9;
const SW_FORCEMINIMIZE: u32 = 11;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOREDRAW: u32 = 0x0008;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_FRAMECHANGED: u32 = 0x0020;
const SWP_SHOWWINDOW: u32 = 0x0040;
const SWP_HIDEWINDOW: u32 = 0x0080;
const SPI_GETCURSORSHADOW: u32 = 0x101a;
const SPI_SETMOUSETRAILS: u32 = 0x005d;
const SPI_GETMOUSETRAILS: u32 = 0x005e;
const SPI_GETMOUSESONAR: u32 = 0x101c;
const SPI_GETWORKAREA: u32 = 0x0030;
const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
const SPI_SETFOREGROUNDLOCKTIMEOUT: u32 = 0x2001;
const SPI_SETCURSORSHADOW: u32 = 0x101b;
const SPI_SETMOUSESONAR: u32 = 0x101d;

const TIME_PERIOD_MIN_MS: u32 = 1;
const TIME_PERIOD_MAX_MS: u32 = 1_000_000;
const TIMERR_NOERROR: u32 = 0;
const TIMERR_NOCANDO: u32 = 97;

fn set_windows_hook(p: &mut Process) -> R {
    let (hook_type, callback, module, thread_id) =
        (p.arg(0) as u32 as i32, p.arg(1), p.arg(2), p.arg(3) as u32);
    if callback == 0 || !(-1..=14).contains(&hook_type) {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let sequence = p.extra.win.next_hook.wrapping_add(1).max(1);
    p.extra.win.next_hook = sequence;
    let handle = 0x5000_0000 | sequence as u64;
    p.extra.win.hooks.insert(
        handle,
        Hook {
            hook_type,
            callback,
            module,
            thread_id,
        },
    );
    Ok(handle)
}

fn register_window_message(p: &mut Process, wide: bool) -> R {
    let ptr = p.arg(0);
    let name = if wide {
        p.read_wstr(ptr)?
    } else {
        p.read_cstr(ptr)?
    };
    if let Some(&id) = p.extra.win.registered_messages.get(&name) {
        return Ok(id as u64);
    }
    let id = p.extra.win.next_registered_message.max(0xc000);
    if id > 0xffff {
        p.set_last_error(8); // ERROR_NOT_ENOUGH_MEMORY
        return Ok(0);
    }
    p.extra.win.next_registered_message = id + 1;
    p.extra.win.registered_messages.insert(name, id);
    Ok(id as u64)
}

#[derive(Clone)]
pub struct Class {
    pub wndproc: u64,
    pub name: String,
    pub unicode: bool,
}

#[derive(Clone)]
pub struct Window {
    pub class: String,
    pub wndproc: u64,
    pub title: String,
    pub x: i32,
    pub y: i32,
    /// Outer window rectangle, as returned by GetWindowRect.
    pub w: i32,
    pub h: i32,
    /// Client area, as returned by GetClientRect.
    pub client_x: i32,
    pub client_y: i32,
    pub client_w: i32,
    pub client_h: i32,
    pub visible: bool,
    pub style: u32,
    pub ex_style: u32,
    pub has_menu: bool,
    /// The HMENU passed for a top-level window. On child windows CreateWindowEx uses
    /// the same parameter as a control identifier, so child windows store no menu handle.
    pub menu: u64,
    pub hinstance: u64,
    pub user_data: u64,
    pub parent: u64,
    pub control_id: u32,
    pub unicode: bool,
    pub needs_paint: bool,
    pub gpu_id: u32,
}

#[derive(Clone, Copy)]
pub struct Msg {
    pub hwnd: u64,
    pub msg: u64,
    pub wparam: u64,
    pub lparam: u64,
    pub time: u32,
    pub pt: (i32, i32),
}

#[derive(Clone, Copy)]
struct Timer {
    thread_id: u32,
    hwnd: u64,
    id: u64,
    interval_ticks: u64,
    deadline_ticks: u64,
    callback: u64,
    pending: bool,
}

#[derive(Clone, Copy)]
pub struct Hook {
    pub hook_type: i32,
    pub callback: u64,
    pub module: u64,
    pub thread_id: u32,
}

#[derive(Default)]
pub struct WinState {
    pub classes: HashMap<String, Class>,
    pub atoms: HashMap<u16, String>,
    pub windows: HashMap<u64, Window>,
    z_order: Vec<u64>,
    window_threads: HashMap<u64, u32>,
    pub queue: VecDeque<Msg>,
    timers: HashMap<(u32, u64, u64), Timer>,
    next_timer_id: u32,
    /// Balanced multimedia timer resolution requests, keyed by requested period in ms.
    timer_period_requests: HashMap<u32, u64>,
    /// Time from the last message retrieved by GetMessage, as Win32 LONG bits.
    pub last_get_message_time: i32,
    /// Thread message metadata exposed by GetMessageExtraInfo/SetMessageExtraInfo.
    pub message_extra_info: u64,
    pub quit: Option<i32>,
    next: u64,
    next_atom: u16,
    pub keys: Vec<u8>,
    pub cursor: (i32, i32),
    pub current_cursor: u64,
    pub cursor_show_count: i32,
    pub cursor_clip: Option<(i32, i32, i32, i32)>,
    pub mouse_position_valid: bool,
    pub mouse_dx: i32,
    pub mouse_dy: i32,
    pub mouse_wheel: i32,
    pub mouse_tracking: HashSet<u64>,
    pub focus: u64,
    pub capture: u64,
    pub main_window: u64,
    pub paint_windows: HashSet<u64>,
    pub device_context_refs: HashMap<(u64, u64), u32>,
    pub props: HashMap<(u64, String), u64>,
    pub hooks: HashMap<u64, Hook>,
    next_hook: u32,
    pub registered_messages: HashMap<String, u32>,
    next_registered_message: u32,
    clipboard_open_by: Option<u32>,
    clipboard_open_window: u64,
    clipboard_owner: u64,
    clipboard_data: HashMap<u32, u64>,
    clipboard_viewers: Vec<u64>,
}

fn lc(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Class names may be an atom (low word) instead of a pointer.
fn class_name(p: &Process, ptr: u64, wide: bool) -> Result<String, Stop> {
    if ptr < 0x10000 {
        return Ok(format!("#atom{ptr}"));
    }
    if wide {
        p.read_wstr(ptr)
    } else {
        p.read_cstr(ptr)
    }
}

pub fn lookup_winmm(name: &str) -> Option<HleFn> {
    Some(match name {
        "timeBeginPeriod" => time_begin_period,
        "timeEndPeriod" => time_end_period,
        "timeGetTime" => |p| Ok((p.cpu.icount + p.extra.sleep_adjust) / 100_000 & 0xffff_ffff),
        "timeGetDevCaps" => |p| {
            let a = p.arg(0);
            p.wr32(a, TIME_PERIOD_MIN_MS)?;
            p.wr32(a + 4, TIME_PERIOD_MAX_MS)?;
            Ok(0)
        },
        _ => return None,
    })
}

fn time_begin_period(p: &mut Process) -> R {
    let period = p.arg(0) as u32;
    if !(TIME_PERIOD_MIN_MS..=TIME_PERIOD_MAX_MS).contains(&period) {
        return Ok(TIMERR_NOCANDO as u64);
    }
    let count = p.extra.win.timer_period_requests.entry(period).or_default();
    let Some(next) = count.checked_add(1) else {
        return Ok(TIMERR_NOCANDO as u64);
    };
    *count = next;
    Ok(TIMERR_NOERROR as u64)
}

fn time_end_period(p: &mut Process) -> R {
    let period = p.arg(0) as u32;
    if !(TIME_PERIOD_MIN_MS..=TIME_PERIOD_MAX_MS).contains(&period) {
        return Ok(TIMERR_NOCANDO as u64);
    }
    let Some(count) = p.extra.win.timer_period_requests.get_mut(&period) else {
        return Ok(TIMERR_NOCANDO as u64);
    };
    *count -= 1;
    if *count == 0 {
        p.extra.win.timer_period_requests.remove(&period);
    }
    Ok(TIMERR_NOERROR as u64)
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "SystemParametersInfoA" => system_parameters_info_a,
        "SystemParametersInfoW" => system_parameters_info_w,
        "RegisterClassA" => |p| register_class(p, false, false),
        "RegisterClassW" => |p| register_class(p, true, false),
        "RegisterClassExA" => |p| register_class(p, false, true),
        "RegisterClassExW" => |p| register_class(p, true, true),
        "wsprintfA" => wsprintf_a,
        "CharUpperA" => |p| char_case_a(p, true, false),
        "CharLowerA" => |p| char_case_a(p, false, false),
        "CharUpperBuffA" => |p| char_case_a(p, true, true),
        "CharLowerBuffA" => |p| char_case_a(p, false, true),
        "UnregisterClassA" => |p| unregister_class(p, false),
        "UnregisterClassW" => |p| unregister_class(p, true),
        "RegisterWindowMessageA" => |p| register_window_message(p, false),
        "RegisterWindowMessageW" => |p| register_window_message(p, true),
        "CreateWindowExA" => |p| create_window(p, false),
        "CreateWindowExW" => |p| create_window(p, true),
        "DestroyWindow" => |p| {
            let h = p.arg(0);
            if !p.extra.win.windows.contains_key(&h) {
                p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
                return Ok(0);
            }
            destroy_window(p, h)?;
            Ok(1)
        },
        "ShowWindow" => show_window,
        "IsIconic" => is_iconic,
        "IsZoomed" => is_zoomed,
        "GetWindowPlacement" => get_window_placement,
        "GetKeyboardType" => |p| {
            Ok(match p.arg(0) {
                0 => 4, // enhanced 101/102-key keyboard
                1 => 0,
                2 => 12,
                _ => 0,
            })
        },
        "SetWindowPlacement" => set_window_placement,
        "UpdateWindow" => update_window,
        "RegisterDeviceNotificationW" => register_device_notification_unimplemented,
        "UnregisterDeviceNotification" => |p| Ok(p.handles.remove(&p.arg(0)).is_some() as u64),
        "EnumDisplayDevicesW" => enum_display_devices_w,
        "EnumDisplaySettingsW" => enum_display_settings_w,
        "EnumDisplayMonitors" => enum_display_monitors,
        "InvalidateRect" => invalidate_window,
        "RedrawWindow" => redraw_window,
        "IsWindow" => |p| {
            let hwnd = p.arg(0);
            Ok((hwnd == HWND_DESKTOP || p.extra.win.windows.contains_key(&hwnd)) as u64)
        },
        "IsWindowVisible" => |p| {
            let hwnd = p.arg(0);
            Ok(if hwnd == HWND_DESKTOP {
                1
            } else {
                p.extra
                    .win
                    .windows
                    .get(&hwnd)
                    .map(|w| w.visible as u64)
                    .unwrap_or(0)
            })
        },
        "IsWindowUnicode" => |p| {
            Ok(p.extra
                .win
                .windows
                .get(&p.arg(0))
                .map(|w| w.unicode as u64)
                .unwrap_or(0))
        },
        "GetMessageA" | "GetMessageW" => get_message,
        "PeekMessageA" | "PeekMessageW" => peek_message,
        "GetMessageTime" => |p| Ok(p.extra.win.last_get_message_time as i64 as u64),
        "GetMessageExtraInfo" => |p| Ok(p.extra.win.message_extra_info),
        "SetMessageExtraInfo" => |p| {
            let previous = p.extra.win.message_extra_info;
            p.extra.win.message_extra_info = p.arg(0);
            Ok(previous)
        },
        "DispatchMessageA" | "DispatchMessageW" => dispatch_message,
        "SetTimer" => set_timer,
        "KillTimer" => kill_timer,
        "PostQuitMessage" => |p| {
            p.extra.win.quit = Some(p.arg(0) as i32);
            Ok(0)
        },
        "PostMessageA" | "PostMessageW" => post_message,
        "TranslateMessage" => translate_message,
        "SetPropA" => |p| set_prop(p, false),
        "SetPropW" => |p| set_prop(p, true),
        "GetPropA" => |p| get_prop(p, false, false),
        "GetPropW" => |p| get_prop(p, true, false),
        "RemovePropA" => |p| get_prop(p, false, true),
        "RemovePropW" => |p| get_prop(p, true, true),
        "OffsetRect" => offset_rect,
        "InflateRect" => inflate_rect,
        "IntersectRect" => intersect_rect,
        "UnionRect" => union_rect,
        "SetRect" => set_rect,
        "SetRectEmpty" => set_rect_empty,
        "CopyRect" => copy_rect,
        "IsRectEmpty" => |p| {
            let r = read_rect(p, p.arg(0))?;
            Ok((r.0 >= r.2 || r.1 >= r.3) as u64)
        },
        "EqualRect" => |p| Ok((read_rect(p, p.arg(0))? == read_rect(p, p.arg(1))?) as u64),
        "PtInRect" => |p| {
            let r = read_rect(p, p.arg(0))?;
            let point = p.arg(1);
            let (x, y) = (point as u32 as i32, (point >> 32) as u32 as i32);
            Ok((x >= r.0 && x < r.2 && y >= r.1 && y < r.3) as u64)
        },
        "SendMessageA" | "SendMessageW" => |p| {
            let (h, m, w, l) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            send(p, h, m, w, l)
        },
        "DefWindowProcA" | "DefWindowProcW" => |p| {
            let (h, m, w, l) = (p.arg(0), p.arg(1) & 0xffff_ffff, p.arg(2), p.arg(3));
            def_window_proc(p, h, m, w, l)
        },
        "GetClientRect" => |p| {
            let (h, r) = (p.arg(0), p.arg(1));
            let (w, ht) = if h == HWND_DESKTOP {
                (system_metric(0) as i32, system_metric(1) as i32)
            } else {
                p.extra
                    .win
                    .windows
                    .get(&h)
                    .map(|x| (x.client_w, x.client_h))
                    .unwrap_or((0, 0))
            };
            write_rect(p, r, 0, 0, w, ht)?;
            Ok((h == HWND_DESKTOP || p.extra.win.windows.contains_key(&h)) as u64)
        },
        "GetWindowRect" => get_window_rect,
        "GetMenu" => get_menu,
        "AdjustWindowRect" => |p| adjust_window_rect(p, false, false),
        "AdjustWindowRectEx" => |p| adjust_window_rect(p, true, false),
        "AdjustWindowRectExForDpi" => |p| adjust_window_rect(p, true, true),
        "SetWindowPos" => set_window_pos,
        "MoveWindow" => |p| {
            let (h, x, y, w, ht, repaint) = (
                p.arg(0),
                p.arg(1) as i32,
                p.arg(2) as i32,
                p.arg(3) as i32,
                p.arg(4) as i32,
                p.arg(5) != 0,
            );
            if !p.extra.win.windows.contains_key(&h) {
                p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
                return Ok(0);
            }
            resize_window(p, h, Some((x, y)), Some((w, ht)), false)?;
            if repaint {
                if let Some(window) = p.extra.win.windows.get_mut(&h) {
                    window.needs_paint |= window.visible;
                }
            }
            Ok(1)
        },
        "SetWindowTextA" => |p| set_window_text(p, false),
        "SetWindowTextW" => |p| set_window_text(p, true),
        "GetWindowTextA" => |p| {
            let (h, buf, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
            let t = p
                .extra
                .win
                .windows
                .get(&h)
                .map(|w| w.title.clone())
                .unwrap_or_default();
            if n > 0 {
                let s = &t[..t.len().min(n - 1)];
                p.write_cstr(buf, s)?;
                return Ok(s.len() as u64);
            }
            Ok(0)
        },
        "SetWindowLongPtrA" | "SetWindowLongPtrW" => |p| set_window_long(p, true),
        "SetWindowLongA" | "SetWindowLongW" => |p| set_window_long(p, false),
        "GetWindowLongPtrA" | "GetWindowLongPtrW" => |p| get_window_long(p, true),
        "GetWindowLongA" | "GetWindowLongW" => |p| get_window_long(p, false),
        "GetParent" => |p| {
            Ok(p.extra
                .win
                .windows
                .get(&p.arg(0))
                .map(|w| w.parent)
                .unwrap_or(0))
        },
        "GetAncestor" => get_ancestor,
        "GetTopWindow" => get_top_window,
        "FindWindowA" => find_window_a,
        "GetDlgItem" => |p| {
            let (parent, id) = (p.arg(0), p.arg(1) as u32);
            if !p.extra.win.windows.contains_key(&parent) {
                return Ok(0);
            }
            Ok(p.extra
                .win
                .windows
                .iter()
                .find_map(|(&h, w)| {
                    (w.parent == parent && w.style & WS_CHILD != 0 && w.control_id == id)
                        .then_some(h)
                })
                .unwrap_or(0))
        },
        "SetDlgItemTextW" => set_dlg_item_text_w,
        "MapVirtualKeyA" | "MapVirtualKeyW" => map_virtual_key_w,
        "ToUnicode" => to_unicode,
        "GetDC" | "GetWindowDC" => get_dc,
        "ReleaseDC" => release_dc,
        "BeginPaint" => begin_paint,
        "EndPaint" => end_paint,
        "ValidateRect" => validate_rect,
        "LoadCursorA" | "LoadCursorW" => load_cursor,
        "LoadIconA" | "LoadIconW" => load_icon,
        "LoadImageA" | "LoadImageW" => load_image,
        "LoadStringW" => load_string_w,
        "GetWindowThreadProcessId" => get_window_thread_process_id,
        "OpenClipboard" => open_clipboard,
        "CloseClipboard" => close_clipboard,
        "EmptyClipboard" => empty_clipboard,
        "IsClipboardFormatAvailable" => is_clipboard_format_available,
        "GetClipboardData" => get_clipboard_data,
        "SetClipboardData" => set_clipboard_data,
        "SetClipboardViewer" => set_clipboard_viewer,
        "ChangeClipboardChain" => change_clipboard_chain,
        "SetCursor" => set_cursor,
        "ShowCursor" => show_cursor,
        "ClipCursor" => clip_cursor,
        "GetCursorPos" => |p| {
            let a = p.arg(0);
            let c = p.extra.win.cursor;
            p.wr32(a, c.0 as u32)?;
            p.wr32(a + 4, c.1 as u32)?;
            Ok(1)
        },
        "SetCursorPos" => |p| {
            let point = clamp_cursor(&p.extra.win, (p.arg(0) as i32, p.arg(1) as i32));
            p.extra.win.cursor = point;
            Ok(1)
        },
        "ScreenToClient" => |p| transform_point(p, false),
        "ClientToScreen" => |p| transform_point(p, true),
        "MapWindowPoints" => map_window_points,
        "GetAsyncKeyState" | "GetKeyState" => |p| {
            let vk = (p.arg(0) & 0xff) as usize;
            Ok(p.extra
                .win
                .keys
                .get(vk)
                .map(|&d| if d != 0 { 0x8000u64 } else { 0 })
                .unwrap_or(0))
        },
        "GetKeyboardLayout" => |_| Ok(0x0409_0409), // US English HKL for the current thread.
        "GetDoubleClickTime" => |_| Ok(500),        // Stable virtual Windows default: 500 ms.
        "SetWindowsHookExA" | "SetWindowsHookExW" => set_windows_hook,
        "UnhookWindowsHookEx" => |p| Ok(p.extra.win.hooks.remove(&p.arg(0)).is_some() as u64),
        "GetKeyboardState" => |p| {
            let a = p.arg(0);
            let mut k = p.extra.win.keys.clone();
            k.resize(256, 0);
            p.write_bytes(a, &k)?;
            Ok(1)
        },
        // The runtime has one guest UI thread; this is its captured window handle.
        "GetCapture" => |p| Ok(p.extra.win.capture),
        "SetCapture" => set_capture,
        "ReleaseCapture" => |p| {
            let had_capture = p.extra.win.capture != 0;
            p.extra.win.capture = 0;
            Ok(had_capture as u64)
        },
        "TrackMouseEvent" => track_mouse_event,
        "SetFocus" => set_focus,
        "GetFocus" => |p| Ok(p.extra.win.focus),
        "GetForegroundWindow" => |p| Ok(p.extra.win.main_window),
        "GetActiveWindow" => |p| Ok(p.extra.win.focus),
        "GetDesktopWindow" => |_| Ok(HWND_DESKTOP),
        "SetForegroundWindow" => set_foreground_window,
        "SetActiveWindow" => |p| {
            let h = p.arg(0);
            if h != 0 && !p.extra.win.windows.contains_key(&h) {
                p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
                return Ok(0);
            }
            activate_window(p, h)
        },
        "GetSystemMetrics" => |p| Ok(system_metric(p.arg(0) as u32)),
        "GetSystemMetricsForDpi" => |p| {
            let (index, dpi) = (p.arg(0) as u32, p.arg(1) as u32);
            Ok(system_metric(index).saturating_mul(dpi as u64) / 96)
        },
        "MessageBoxA" => message_box_a_unimplemented,
        "MessageBoxW" => message_box_w_unimplemented,
        "MonitorFromWindow" => monitor_from_window,
        "MonitorFromPoint" => monitor_from_point,
        "GetMonitorInfoA" => |p| get_monitor_info(p, false),
        "GetMonitorInfoW" => |p| get_monitor_info(p, true),
        "GetDpiForWindow" => get_dpi_for_window,
        "GetDpiForSystem" => |_| Ok(96), // The emulated display runs at 96 DPI.
        _ => return None,
    })
}

fn message_box_a_unimplemented(p: &mut Process) -> R {
    let flags = p.arg(3) as u32;
    Err(Stop::Unimplemented {
        dll: "user32".into(),
        name: format!("MessageBoxA interactive dialog (type={flags:#x})"),
    })
}

fn message_box_w_unimplemented(p: &mut Process) -> R {
    let flags = p.arg(3) as u32;
    Err(Stop::Unimplemented {
        dll: "user32".into(),
        name: format!("MessageBoxW interactive dialog (type={flags:#x})"),
    })
}

fn system_parameters_info_a(p: &mut Process) -> R {
    system_parameters_info(p, "SystemParametersInfoA")
}

fn system_parameters_info_w(p: &mut Process) -> R {
    system_parameters_info(p, "SystemParametersInfoW")
}

fn system_parameters_info(p: &mut Process, api: &str) -> R {
    let action = p.arg(0) as u32;
    if action == SPI_SETFOREGROUNDLOCKTIMEOUT {
        // This action reads the new timeout through pvParam. GTA SA supplies NULL, which is an
        // invalid request on Win32; return the real failure so startup can continue without
        // pretending that a setting or profile update occurred.
        if p.arg(2) == 0 {
            p.set_last_error(87); // ERROR_INVALID_PARAMETER
            return Ok(0);
        }
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("{api}(SPI_SETFOREGROUNDLOCKTIMEOUT profile and foreground policy)"),
        });
    }
    if action == SPI_GETWORKAREA {
        // The virtual desktop has no emulated taskbar, so its entire screen is the work area.
        let output = p.arg(2);
        if output == 0 {
            p.set_last_error(87); // ERROR_INVALID_PARAMETER
            return Ok(0);
        }
        p.wr32(output, 0)?;
        p.wr32(output + 4, 0)?;
        p.wr32(output + 8, system_metric(0) as u32)?;
        p.wr32(output + 12, system_metric(1) as u32)?;
        return Ok(1);
    }
    if action == SPI_SETCURSORSHADOW {
        if p.arg(2) == 0 {
            p.set_last_error(87); // ERROR_INVALID_PARAMETER: pvParam must point to a BOOL.
            return Ok(0);
        }
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("{api}(SPI_SETCURSORSHADOW virtual cursor rendering is not translated)"),
        });
    }
    if action == SPI_SETMOUSETRAILS {
        let requested = p.arg(1) as u32;
        if p.arg(3) != 0 {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!(
                    "{api}(SPI_SETMOUSETRAILS fWinIni={:#x}; profile update and setting-change broadcast are not translated)",
                    p.arg(3)
                ),
            });
        }
        let Some(current) = p.system_parameters.mouse_trails else {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!("{api}(SPI_SETMOUSETRAILS host value unavailable)"),
            });
        };
        if requested == current || (requested <= 1 && current <= 1) {
            // The host already has the requested effective state. This call is idempotent and
            // does not pretend to change the browser or operating-system pointer renderer.
            return Ok(1);
        }
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!(
                "{api}(SPI_SETMOUSETRAILS requested={requested}, host={current}; mouse trail rendering is not translated)"
            ),
        });
    }
    if action == SPI_SETMOUSESONAR {
        // Win32 encodes SPI_SETMOUSESONAR's BOOL directly in pvParam. In particular, zero
        // is the valid request to turn sonar off; this is not a pointer to a BOOL.
        let requested = p.arg(2) != 0;
        if p.arg(3) != 0 {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!(
                    "{api}(SPI_SETMOUSESONAR fWinIni={:#x}; profile update and setting-change broadcast are not translated)",
                    p.arg(3)
                ),
            });
        }
        let Some(current) = p.system_parameters.mouse_sonar else {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!("{api}(SPI_SETMOUSESONAR host value unavailable)"),
            });
        };
        if requested == current {
            // The host already exposes this state. No browser cursor behavior is changed here.
            return Ok(1);
        }
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!(
                "{api}(SPI_SETMOUSESONAR requested={requested}, host={current}; sonar cursor rendering is not translated)"
            ),
        });
    }
    let (setting_name, value) = match action {
        SPI_GETCURSORSHADOW => (
            "SPI_GETCURSORSHADOW",
            p.system_parameters.cursor_shadow.map(u32::from),
        ),
        SPI_GETMOUSETRAILS => ("SPI_GETMOUSETRAILS", p.system_parameters.mouse_trails),
        SPI_GETMOUSESONAR => (
            "SPI_GETMOUSESONAR",
            p.system_parameters.mouse_sonar.map(u32::from),
        ),
        SPI_GETWHEELSCROLLLINES => (
            "SPI_GETWHEELSCROLLLINES",
            p.system_parameters.wheel_scroll_lines,
        ),
        _ => {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!("{api}(uiAction={action:#x})"),
            });
        }
    };

    let output = p.arg(2);
    if output == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let Some(value) = value else {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("{api}({setting_name} host value unavailable)"),
        });
    };
    p.wr32(output, value)?;
    Ok(1)
}

fn root_parent_window(p: &Process, hwnd: u64) -> u64 {
    let mut current = hwnd;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return 0;
        }
        let Some(window) = p.extra.win.windows.get(&current) else {
            return 0;
        };
        if window.style & WS_CHILD == 0 || window.parent == 0 {
            return current;
        }
        current = window.parent;
    }
}

fn get_ancestor(p: &mut Process) -> R {
    const GA_PARENT: u32 = 1;
    const GA_ROOT: u32 = 2;
    const GA_ROOTOWNER: u32 = 3;

    let (hwnd, flags) = (p.arg(0), p.arg(1) as u32);
    if hwnd == HWND_DESKTOP || !p.extra.win.windows.contains_key(&hwnd) {
        return Ok(0);
    }
    match flags {
        GA_PARENT => Ok(p
            .extra
            .win
            .windows
            .get(&hwnd)
            .filter(|window| window.style & WS_CHILD != 0)
            .map(|window| window.parent)
            .unwrap_or(0)),
        GA_ROOT => Ok(root_parent_window(p, hwnd)),
        GA_ROOTOWNER => {
            let mut current = hwnd;
            let mut visited = HashSet::new();
            loop {
                let root = root_parent_window(p, current);
                if root == 0 || !visited.insert(root) {
                    return Ok(0);
                }
                let owner = p
                    .extra
                    .win
                    .windows
                    .get(&root)
                    .map(|window| window.parent)
                    .unwrap_or(0);
                if owner == 0 {
                    return Ok(root);
                }
                current = owner;
            }
        }
        _ => {
            p.set_last_error(87); // ERROR_INVALID_PARAMETER
            Ok(0)
        }
    }
}

fn windows_front_to_back(p: &Process) -> Vec<u64> {
    let mut ordered = Vec::with_capacity(p.extra.win.windows.len());
    let mut seen = HashSet::new();
    for hwnd in &p.extra.win.z_order {
        if p.extra.win.windows.contains_key(hwnd) && seen.insert(*hwnd) {
            ordered.push(*hwnd);
        }
    }
    let mut untracked = p
        .extra
        .win
        .windows
        .keys()
        .copied()
        .filter(|hwnd| seen.insert(*hwnd))
        .collect::<Vec<_>>();
    untracked.sort_by_key(|hwnd| std::cmp::Reverse(*hwnd));
    ordered.extend(untracked);
    ordered
}

fn get_top_window(p: &mut Process) -> R {
    let requested_parent = p.arg(0);
    let parent = if requested_parent == 0 || requested_parent == HWND_DESKTOP {
        0
    } else if p.extra.win.windows.contains_key(&requested_parent) {
        requested_parent
    } else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    Ok(windows_front_to_back(p)
        .into_iter()
        .find(|hwnd| {
            let window = &p.extra.win.windows[hwnd];
            if parent == 0 {
                window.parent == 0 && window.style & WS_CHILD == 0
            } else {
                window.parent == parent && window.style & WS_CHILD != 0
            }
        })
        .unwrap_or(0))
}

fn find_window_a(p: &mut Process) -> R {
    let (class_ptr, title_ptr) = (p.arg(0), p.arg(1));
    let class = if class_ptr == 0 {
        None
    } else {
        Some(class_name(p, class_ptr, false)?)
    };
    let title = if title_ptr == 0 {
        None
    } else {
        Some(p.read_cstr(title_ptr)?)
    };
    Ok(windows_front_to_back(p)
        .into_iter()
        .find(|hwnd| {
            let window = &p.extra.win.windows[hwnd];
            window.parent == 0
                && window.style & WS_CHILD == 0
                && class
                    .as_ref()
                    .is_none_or(|name| lc(name) == lc(&window.class))
                && title
                    .as_ref()
                    .is_none_or(|text| lc(text) == lc(&window.title))
        })
        .unwrap_or(0))
}

fn set_dlg_item_text_w(p: &mut Process) -> R {
    let (parent, control_id, text_ptr) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if !p.extra.win.windows.contains_key(&parent) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    let Some(child) = windows_front_to_back(p).into_iter().find(|hwnd| {
        let window = &p.extra.win.windows[hwnd];
        window.parent == parent && window.style & WS_CHILD != 0 && window.control_id == control_id
    }) else {
        return Ok(0);
    };
    let text = p.read_wstr(text_ptr)?;
    let window = p.extra.win.windows.get_mut(&child).unwrap();
    window.title = text;
    window.needs_paint |= window.visible;
    Ok(1)
}

fn get_window_thread_process_id(p: &mut Process) -> R {
    const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
    let (hwnd, process_id_out) = (p.arg(0), p.arg(1));
    let Some(&thread_id) = p.extra.win.window_threads.get(&hwnd) else {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    };
    if process_id_out != 0 {
        let process_id_offset = if p.cpu.mode == CpuMode::X86_32 {
            0x20
        } else {
            0x40
        };
        let process_id = p.rdptr(TEB_ADDR + process_id_offset)?;
        p.wr32(process_id_out, process_id as u32)?;
    }
    Ok(thread_id as u64)
}

fn window_client_origin(p: &Process, hwnd: u64) -> Option<(i32, i32)> {
    if hwnd == 0 || hwnd == HWND_DESKTOP {
        Some((0, 0))
    } else {
        p.extra.win.windows.get(&hwnd).map(|window| {
            (
                window.x.saturating_add(window.client_x),
                window.y.saturating_add(window.client_y),
            )
        })
    }
}

fn map_window_points(p: &mut Process) -> R {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
    let (from, to, points_ptr, count) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as u32 as usize);
    let Some((from_x, from_y)) = window_client_origin(p, from) else {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    };
    let Some((to_x, to_y)) = window_client_origin(p, to) else {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    };
    if count == 0 {
        return Ok(0);
    }
    if points_ptr == 0 || count.checked_mul(8).is_none() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if count == 2 {
        const WS_EX_LAYOUTRTL: u32 = 0x0040_0000;
        let from_rtl = p
            .extra
            .win
            .windows
            .get(&from)
            .is_some_and(|w| w.ex_style & WS_EX_LAYOUTRTL != 0);
        let to_rtl = p
            .extra
            .win
            .windows
            .get(&to)
            .is_some_and(|w| w.ex_style & WS_EX_LAYOUTRTL != 0);
        if from_rtl != to_rtl {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: "MapWindowPoints RECT mapping between opposite RTL layouts".into(),
            });
        }
    }
    let dx = from_x.saturating_sub(to_x);
    let dy = from_y.saturating_sub(to_y);
    let mut mapped = Vec::with_capacity(count);
    for index in 0..count {
        let address = points_ptr
            .checked_add((index as u64).saturating_mul(8))
            .ok_or_else(|| Stop::Crash("MapWindowPoints POINT address overflow".into()))?;
        let x = p.rd32(address)? as i32;
        let y = p.rd32(address + 4)? as i32;
        mapped.push((x.saturating_add(dx), y.saturating_add(dy)));
    }
    for (index, (x, y)) in mapped.into_iter().enumerate() {
        let address = points_ptr + index as u64 * 8;
        p.wr32(address, x as u32)?;
        p.wr32(address + 4, y as u32)?;
    }
    Ok(((dy as u16 as u32) << 16 | dx as u16 as u32) as u64)
}

fn register_device_notification_unimplemented(_p: &mut Process) -> R {
    Err(Stop::Unimplemented {
        dll: "user32".into(),
        name: "RegisterDeviceNotification device-change delivery is not modeled".into(),
    })
}

fn remove_clipboard_viewer(p: &mut Process, hwnd: u64) -> bool {
    let Some(index) = p
        .extra
        .win
        .clipboard_viewers
        .iter()
        .position(|viewer| *viewer == hwnd)
    else {
        return false;
    };
    let next = p
        .extra
        .win
        .clipboard_viewers
        .get(index + 1)
        .copied()
        .unwrap_or(0);
    let previous = index
        .checked_sub(1)
        .and_then(|previous_index| p.extra.win.clipboard_viewers.get(previous_index).copied());
    p.extra.win.clipboard_viewers.remove(index);
    if let Some(previous) = previous {
        post(p, previous, WM_CHANGECBCHAIN, hwnd, next);
    }
    true
}

fn notify_clipboard_viewer(p: &mut Process) {
    if let Some(&viewer) = p.extra.win.clipboard_viewers.first() {
        if p.extra.win.windows.contains_key(&viewer) {
            post(p, viewer, WM_DRAWCLIPBOARD, 0, 0);
        }
    }
}

fn clipboard_is_open_by_current_thread(p: &Process) -> bool {
    p.extra.win.clipboard_open_by == Some(p.current_thread_id)
}

fn open_clipboard(p: &mut Process) -> R {
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
    let hwnd = p.arg(0);
    if hwnd != 0 && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    }
    if p.extra.win.clipboard_open_by.is_some() {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    p.extra.win.clipboard_open_by = Some(p.current_thread_id);
    p.extra.win.clipboard_open_window = hwnd;
    p.set_last_error(0);
    Ok(1)
}

fn close_clipboard(p: &mut Process) -> R {
    const ERROR_CLIPBOARD_NOT_OPEN: u32 = 1418;
    if !clipboard_is_open_by_current_thread(p) {
        p.set_last_error(ERROR_CLIPBOARD_NOT_OPEN);
        return Ok(0);
    }
    p.extra.win.clipboard_open_by = None;
    p.extra.win.clipboard_open_window = 0;
    p.set_last_error(0);
    Ok(1)
}

fn free_clipboard_data(p: &mut Process) {
    let mut released = HashSet::new();
    let data = std::mem::take(&mut p.extra.win.clipboard_data);
    for handle in data.into_values() {
        if released.insert(handle) && p.heap.size_of(handle).is_some() {
            kernel32::free_block(p, handle);
        }
    }
}

fn empty_clipboard(p: &mut Process) -> R {
    const ERROR_CLIPBOARD_NOT_OPEN: u32 = 1418;
    if !clipboard_is_open_by_current_thread(p) {
        p.set_last_error(ERROR_CLIPBOARD_NOT_OPEN);
        return Ok(0);
    }
    let previous_owner = p.extra.win.clipboard_owner;
    if previous_owner != 0 && p.extra.win.windows.contains_key(&previous_owner) {
        send(p, previous_owner, WM_DESTROYCLIPBOARD, 0, 0)?;
    }
    free_clipboard_data(p);
    p.extra.win.clipboard_owner = p.extra.win.clipboard_open_window;
    p.set_last_error(0);
    Ok(1)
}

fn is_clipboard_format_available(p: &mut Process) -> R {
    let format = p.arg(0) as u32;
    Ok((format != 0 && p.extra.win.clipboard_data.contains_key(&format)) as u64)
}

fn get_clipboard_data(p: &mut Process) -> R {
    const ERROR_CLIPBOARD_NOT_OPEN: u32 = 1418;
    if !clipboard_is_open_by_current_thread(p) {
        p.set_last_error(ERROR_CLIPBOARD_NOT_OPEN);
        return Ok(0);
    }
    let format = p.arg(0) as u32;
    let Some(&handle) = p.extra.win.clipboard_data.get(&format) else {
        return Ok(0);
    };
    if p.heap.size_of(handle).is_none() {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("GetClipboardData format {format} references freed guest memory"),
        });
    }
    p.set_last_error(0);
    Ok(handle)
}

fn set_clipboard_data(p: &mut Process) -> R {
    const ERROR_CLIPBOARD_NOT_OPEN: u32 = 1418;
    const ERROR_INVALID_HANDLE: u32 = 6;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
    if !clipboard_is_open_by_current_thread(p) || p.extra.win.clipboard_owner == 0 {
        p.set_last_error(ERROR_CLIPBOARD_NOT_OPEN);
        return Ok(0);
    }
    let (format, handle) = (p.arg(0) as u32, p.arg(1));
    if format == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if handle == 0 {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("SetClipboardData format {format} delayed rendering"),
        });
    }
    if matches!(format, 2 | 3 | 9 | 14 | 15 | 0x0080) {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!(
                "SetClipboardData format {format} requires a GDI or delayed-render object"
            ),
        });
    }
    if p.heap.size_of(handle).is_none() {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if p.extra
        .win
        .clipboard_data
        .values()
        .any(|current| *current == handle)
    {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if p.heap.size_of(handle).unwrap_or(0) == 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    let replaced = p.extra.win.clipboard_data.insert(format, handle);
    if let Some(previous) = replaced.filter(|previous| *previous != handle) {
        if p.heap.size_of(previous).is_some() {
            kernel32::free_block(p, previous);
        }
    }
    p.set_last_error(0);
    notify_clipboard_viewer(p);
    Ok(handle)
}

fn set_clipboard_viewer(p: &mut Process) -> R {
    const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
    let hwnd = p.arg(0);
    if !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(ERROR_INVALID_WINDOW_HANDLE);
        return Ok(0);
    }
    if p.extra.win.clipboard_viewers.contains(&hwnd) {
        return Ok(p.extra.win.clipboard_viewers.first().copied().unwrap_or(0));
    }
    let next = p.extra.win.clipboard_viewers.first().copied().unwrap_or(0);
    p.extra.win.clipboard_viewers.insert(0, hwnd);
    p.set_last_error(0);
    Ok(next)
}

fn change_clipboard_chain(p: &mut Process) -> R {
    let (remove, next) = (p.arg(0), p.arg(1));
    if let Some(index) = p
        .extra
        .win
        .clipboard_viewers
        .iter()
        .position(|viewer| *viewer == remove)
    {
        let expected_next = p
            .extra
            .win
            .clipboard_viewers
            .get(index + 1)
            .copied()
            .unwrap_or(0);
        if next != expected_next {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: format!(
                    "ChangeClipboardChain replacement {next:#x} differs from modeled successor {expected_next:#x}"
                ),
            });
        }
    }
    Ok(remove_clipboard_viewer(p, remove) as u64)
}

fn get_window_rect(p: &mut Process) -> R {
    let (hwnd, rect) = (p.arg(0), p.arg(1));
    let bounds = if hwnd == HWND_DESKTOP {
        (0, 0, system_metric(0) as i32, system_metric(1) as i32)
    } else if let Some(window) = p.extra.win.windows.get(&hwnd) {
        (
            window.x,
            window.y,
            window.x.saturating_add(window.w),
            window.y.saturating_add(window.h),
        )
    } else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    write_rect(p, rect, bounds.0, bounds.1, bounds.2, bounds.3)?;
    Ok(1)
}

fn get_menu(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd == HWND_DESKTOP {
        return Ok(0);
    }
    let Some(window) = p.extra.win.windows.get(&hwnd) else {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("GetMenu received invalid HWND {hwnd:#x}"),
        });
    };
    if window.style & WS_CHILD != 0 {
        // Windows documents the result for a child HWND as undefined. Do not turn that
        // unspecified case into a success-shaped NULL result.
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("GetMenu on child HWND {hwnd:#x} has undefined Windows behavior"),
        });
    }
    Ok(window.menu)
}

fn set_window_text(p: &mut Process, wide: bool) -> R {
    let (hwnd, text) = (p.arg(0), p.arg(1));
    if !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    let title = if wide {
        p.read_wstr(text)?
    } else {
        p.read_cstr(text)?
    };
    p.extra.win.windows.get_mut(&hwnd).unwrap().title = title;
    Ok(1)
}

fn invalidate_window(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd == HWND_DESKTOP {
        return Ok(1);
    }
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    window.needs_paint = true;
    Ok(1)
}

fn redraw_window(p: &mut Process) -> R {
    const RDW_INVALIDATE: u64 = 0x0001;
    const RDW_INTERNALPAINT: u64 = 0x0002;
    const RDW_VALIDATE: u64 = 0x0008;
    const RDW_UPDATENOW: u64 = 0x0100;
    let (hwnd, flags) = (p.arg(0), p.arg(3));
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    if flags & (RDW_INVALIDATE | RDW_INTERNALPAINT) != 0 {
        window.needs_paint = true;
    }
    if flags & RDW_VALIDATE != 0 {
        window.needs_paint = false;
    }
    if flags & RDW_UPDATENOW != 0 {
        paint_now(p, hwnd)?;
    }
    Ok(1)
}

fn validate_rect(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd == HWND_DESKTOP {
        return Ok(1);
    }
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    window.needs_paint = false;
    Ok(1)
}

fn post_message(p: &mut Process) -> R {
    let (hwnd, message, wparam, lparam) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if hwnd != HWND_DESKTOP && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    post(p, hwnd, message, wparam, lparam);
    Ok(1)
}

fn translate_message(p: &mut Process) -> R {
    let (hwnd, message, wparam, lparam) = read_message_fields(p, p.arg(0))?;
    let translated_message = match message {
        WM_KEYDOWN => WM_CHAR,
        WM_SYSKEYDOWN => WM_SYSCHAR,
        _ => return Ok(0),
    };

    let (shift, control, alt, caps) = {
        let keys = &p.extra.win.keys;
        let down = |vk: usize| keys.get(vk).is_some_and(|&state| state != 0);
        (
            down(0x10) || down(0xa0) || down(0xa1),
            down(0x11) || down(0xa2) || down(0xa3),
            down(0x12) || down(0xa4) || down(0xa5),
            keys.get(0x14).is_some_and(|&state| state & 1 != 0),
        )
    };
    if control || (alt && message != WM_SYSKEYDOWN) {
        return Ok(0);
    }
    let Some(character) = translate_us_key(wparam as u32, shift, caps) else {
        return Ok(0);
    };

    post(p, hwnd, translated_message, character as u64, lparam);
    Ok(1)
}

fn set_focus(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd != 0 && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    Ok(std::mem::replace(&mut p.extra.win.focus, hwnd))
}

fn set_capture(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd != 0 && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    Ok(std::mem::replace(&mut p.extra.win.capture, hwnd))
}

fn monitor_from_window(p: &mut Process) -> R {
    const MONITOR_DEFAULTTOPRIMARY: u64 = 1;
    const MONITOR_DEFAULTTONEAREST: u64 = 2;
    let (hwnd, flags) = (p.arg(0), p.arg(1));
    let valid = hwnd == HWND_DESKTOP || p.extra.win.windows.contains_key(&hwnd);
    Ok(
        if valid || flags & (MONITOR_DEFAULTTOPRIMARY | MONITOR_DEFAULTTONEAREST) != 0 {
            0x5000
        } else {
            0
        },
    )
}

fn monitor_from_point(p: &mut Process) -> R {
    const MONITOR_DEFAULTTOPRIMARY: u64 = 1;
    const MONITOR_DEFAULTTONEAREST: u64 = 2;
    let (x, y, flags) = if p.cpu.mode == CpuMode::X86_32 {
        (p.arg(0) as i32, p.arg(1) as i32, p.arg(2))
    } else {
        let point = p.arg(0);
        (point as u32 as i32, (point >> 32) as u32 as i32, p.arg(1))
    };
    let inside = x >= 0 && y >= 0 && x < 1920 && y < 1080;
    Ok(
        if inside || flags & (MONITOR_DEFAULTTOPRIMARY | MONITOR_DEFAULTTONEAREST) != 0 {
            0x5000
        } else {
            0
        },
    )
}

fn get_monitor_info(p: &mut Process, wide: bool) -> R {
    const MONITORINFO_SIZE: u32 = 40;
    const MONITORINFOEXA_SIZE: u32 = MONITORINFO_SIZE + 32;
    const MONITORINFOEXW_SIZE: u32 = MONITORINFO_SIZE + 64;
    let (monitor, info) = (p.arg(0), p.arg(1));
    if monitor != 0x5000 || info == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let size = p.rd32(info)?;
    let extended_size = if wide {
        MONITORINFOEXW_SIZE
    } else {
        MONITORINFOEXA_SIZE
    };
    if size < MONITORINFO_SIZE || (size != MONITORINFO_SIZE && size < extended_size) {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    p.wr32(info + 4, 0)?;
    p.wr32(info + 8, 0)?;
    p.wr32(info + 12, 1920)?;
    p.wr32(info + 16, 1080)?;
    p.wr32(info + 20, 0)?;
    p.wr32(info + 24, 0)?;
    p.wr32(info + 28, 1920)?;
    p.wr32(info + 32, 1080)?;
    p.wr32(info + 36, 1)?;
    if size >= extended_size {
        if wide {
            p.write_bytes(info + MONITORINFO_SIZE as u64, &[0; 64])?;
            p.write_wstr(info + MONITORINFO_SIZE as u64, "\\\\.\\DISPLAY1")?;
        } else {
            p.write_bytes(info + MONITORINFO_SIZE as u64, &[0; 32])?;
            p.write_cstr(info + MONITORINFO_SIZE as u64, "\\\\.\\DISPLAY1")?;
        }
    }
    Ok(1)
}

fn unregister_class(p: &mut Process, wide: bool) -> R {
    let class_ptr = p.arg(0);
    let name = if class_ptr < 0x1_0000 {
        p.extra.win.atoms.get(&(class_ptr as u16)).cloned()
    } else if wide {
        Some(p.read_wstr(class_ptr)?)
    } else {
        Some(p.read_cstr(class_ptr)?)
    };
    let Some(name) = name else {
        p.set_last_error(1407); // ERROR_CANNOT_FIND_WND_CLASS
        return Ok(0);
    };
    let key = lc(&name);
    if p.extra
        .win
        .windows
        .values()
        .any(|window| window.class == key)
    {
        p.set_last_error(1412); // ERROR_CLASS_HAS_WINDOWS
        return Ok(0);
    }
    if p.extra.win.classes.remove(&key).is_none() {
        p.set_last_error(1411); // ERROR_CLASS_DOES_NOT_EXIST
        return Ok(0);
    }
    p.extra.win.atoms.retain(|_, class| class != &key);
    Ok(1)
}

fn show_window(p: &mut Process) -> R {
    let (hwnd, command) = (p.arg(0), p.arg(1));
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    let was_visible = window.visible;
    let visible = command != 0;
    window.visible = visible;
    match command as u32 {
        SW_SHOWMINIMIZED | SW_MINIMIZE | SW_SHOWMINNOACTIVE | SW_FORCEMINIMIZE => {
            window.style = (window.style | WS_MINIMIZE) & !WS_MAXIMIZE;
        }
        SW_MAXIMIZE => window.style = (window.style | WS_MAXIMIZE) & !WS_MINIMIZE,
        SW_SHOWNORMAL | SW_RESTORE => window.style &= !(WS_MINIMIZE | WS_MAXIMIZE),
        _ => {}
    }
    window.needs_paint |= visible;
    send(p, hwnd, WM_SHOWWINDOW, visible as u64, 0)?;
    Ok(was_visible as u64)
}

fn is_iconic(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    let Some(window) = p.extra.win.windows.get(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    Ok((window.style & WS_MINIMIZE != 0) as u64)
}

fn is_zoomed(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    let Some(window) = p.extra.win.windows.get(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    Ok((window.style & WS_MAXIMIZE != 0) as u64)
}

fn update_window(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    paint_now(p, hwnd)?;
    Ok(1)
}

fn get_dc(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd != 0 && hwnd != HWND_DESKTOP && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    let hdc = 0x7000_0000 + hwnd;
    let references = p
        .extra
        .win
        .device_context_refs
        .entry((hwnd, hdc))
        .or_default();
    *references = references.saturating_add(1);
    Ok(hdc)
}

fn release_dc(p: &mut Process) -> R {
    let (hwnd, hdc) = (p.arg(0), p.arg(1));
    let key = (hwnd, hdc);
    let Some(references) = p.extra.win.device_context_refs.get(&key).copied() else {
        return Ok(0);
    };
    if references > 1 {
        p.extra.win.device_context_refs.insert(key, references - 1);
    } else {
        p.extra.win.device_context_refs.remove(&key);
    }
    Ok(1)
}

fn begin_paint(p: &mut Process) -> R {
    let (hwnd, paint) = (p.arg(0), p.arg(1));
    if !p.extra.win.windows.contains_key(&hwnd) || paint == 0 {
        p.set_last_error(if paint == 0 {
            87 // ERROR_INVALID_PARAMETER
        } else {
            1400 // ERROR_INVALID_WINDOW_HANDLE
        });
        return Ok(0);
    }
    let hdc = 0x7000_0000 + hwnd;
    let paint_size = if p.cpu.mode == CpuMode::X86_32 {
        64
    } else {
        72
    };
    p.write_bytes(paint, &vec![0u8; paint_size])?;
    p.wrptr(paint, hdc)?;
    p.extra.win.paint_windows.insert(hwnd);
    if let Some(window) = p.extra.win.windows.get_mut(&hwnd) {
        window.needs_paint = false;
    }
    Ok(hdc)
}

fn end_paint(p: &mut Process) -> R {
    let (hwnd, paint) = (p.arg(0), p.arg(1));
    if paint == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let hdc = p.rdptr(paint)?;
    if hdc != 0x7000_0000 + hwnd || !p.extra.win.paint_windows.remove(&hwnd) {
        return Ok(0);
    }
    Ok(1)
}

fn stock_resource_id(arg: u64, instance: u64, supported: &[u16]) -> Option<u16> {
    if instance != 0 || arg >= 0x1_0000 {
        return None;
    }
    let id = arg as u16;
    supported.contains(&id).then_some(id)
}

fn stock_cursor_handle(id: u16) -> u64 {
    0x6000_0000 | id as u64
}

fn load_cursor(p: &mut Process) -> R {
    const IDS: &[u16] = &[
        32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649,
    ];
    if let Some(id) = stock_resource_id(p.arg(1), p.arg(0), IDS) {
        Ok(stock_cursor_handle(id))
    } else {
        p.set_last_error(1813); // ERROR_RESOURCE_DATA_NOT_FOUND
        Ok(0)
    }
}

fn load_icon(p: &mut Process) -> R {
    const IDS: &[u16] = &[32512, 32513, 32514, 32515, 32516, 32517, 32518];
    if let Some(id) = stock_resource_id(p.arg(1), p.arg(0), IDS) {
        Ok(stock_cursor_handle(id))
    } else {
        p.set_last_error(1813); // ERROR_RESOURCE_DATA_NOT_FOUND
        Ok(0)
    }
}

fn load_image(p: &mut Process) -> R {
    const LR_SHARED: u32 = 0x8000;
    let (instance, name, image_type, flags) =
        (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(5) as u32);
    if flags & LR_SHARED == 0 {
        p.set_last_error(1813); // This model only shares immutable stock resources.
        return Ok(0);
    }
    let supported = match image_type {
        1 => &[32512, 32513, 32514, 32515, 32516, 32517, 32518][..],
        2 => &[
            32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649,
        ][..],
        _ => &[][..],
    };
    if let Some(id) = stock_resource_id(name, instance, supported) {
        Ok(stock_cursor_handle(id))
    } else {
        p.set_last_error(1813); // ERROR_RESOURCE_DATA_NOT_FOUND
        Ok(0)
    }
}

fn load_string_w(p: &mut Process) -> R {
    let (instance, resource_id, buffer) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    let capacity = p.arg(3) as u32 as i32;
    if capacity < 0 || resource_id > u16::MAX as u32 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let block_id = (resource_id >> 4) + 1;
    let string_index = resource_id & 0xf;
    let Some((data, size)) = super::kernel32::resource_payload_by_id(
        p,
        instance,
        6, // RT_STRING
        block_id as u16,
    )?
    else {
        return Ok(0);
    };

    let mut offset = 0u64;
    for index in 0..=string_index {
        if offset + 2 > size as u64 {
            return Err(Stop::Crash(
                "malformed RT_STRING block: missing string length".into(),
            ));
        }
        let length = p.rd16(data + offset)? as u64;
        offset += 2;
        let string_bytes = length * 2;
        if offset + string_bytes > size as u64 {
            return Err(Stop::Crash(
                "malformed RT_STRING block: string exceeds resource size".into(),
            ));
        }
        if index == string_index {
            if capacity == 0 {
                p.wrptr(buffer, data + offset)?;
                return Ok(length);
            }
            let count = length.min(capacity as u64 - 1);
            for unit in 0..count {
                p.wr16(buffer + unit * 2, p.rd16(data + offset + unit * 2)?)?;
            }
            p.wr16(buffer + count * 2, 0)?;
            return Ok(count);
        }
        offset += string_bytes;
    }
    Ok(0)
}

fn set_cursor(p: &mut Process) -> R {
    let cursor = p.arg(0);
    Ok(std::mem::replace(&mut p.extra.win.current_cursor, cursor))
}

fn show_cursor(p: &mut Process) -> R {
    let show = p.arg(0) != 0;
    let count = &mut p.extra.win.cursor_show_count;
    *count = if show {
        count.saturating_add(1)
    } else {
        count.saturating_sub(1)
    };
    Ok(*count as i64 as u64)
}

fn clamp_cursor(win: &WinState, point: (i32, i32)) -> (i32, i32) {
    let Some((left, top, right, bottom)) = win.cursor_clip else {
        return point;
    };
    (
        point.0.clamp(left, right.saturating_sub(1)),
        point.1.clamp(top, bottom.saturating_sub(1)),
    )
}

fn clip_cursor(p: &mut Process) -> R {
    let rect = p.arg(0);
    if rect == 0 {
        p.extra.win.cursor_clip = None;
        return Ok(1);
    }
    let bounds = read_rect(p, rect)?;
    if bounds.2 <= bounds.0 || bounds.3 <= bounds.1 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    p.extra.win.cursor_clip = Some(bounds);
    let cursor = clamp_cursor(&p.extra.win, p.extra.win.cursor);
    p.extra.win.cursor = cursor;
    Ok(1)
}

fn transform_point(p: &mut Process, to_screen: bool) -> R {
    let (hwnd, point) = (p.arg(0), p.arg(1));
    if point == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (origin_x, origin_y) = if hwnd == HWND_DESKTOP {
        (0, 0)
    } else if let Some(window) = p.extra.win.windows.get(&hwnd) {
        (
            window.x.saturating_add(window.client_x),
            window.y.saturating_add(window.client_y),
        )
    } else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    let (x, y) = (p.rd32(point)? as i32, p.rd32(point + 4)? as i32);
    let (x, y) = if to_screen {
        (x.saturating_add(origin_x), y.saturating_add(origin_y))
    } else {
        (x.saturating_sub(origin_x), y.saturating_sub(origin_y))
    };
    p.wr32(point, x as u32)?;
    p.wr32(point + 4, y as u32)?;
    Ok(1)
}

fn set_foreground_window(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd == 0 || !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    p.extra.win.main_window = hwnd;
    activate_window(p, hwnd)?;
    Ok(1)
}

fn get_dpi_for_window(p: &mut Process) -> R {
    let hwnd = p.arg(0);
    if hwnd == HWND_DESKTOP || p.extra.win.windows.contains_key(&hwnd) {
        Ok(96)
    } else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        Ok(0)
    }
}

/// Register or cancel the Win32 TME_LEAVE notification used by GLFW's Win32 backend.
fn track_mouse_event(p: &mut Process) -> R {
    let request = p.arg(0);
    let size = p.rd32(request)?;
    if size < 24 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let flags = p.rd32(request + 4)?;
    let hwnd = p.rd64(request + 8)?;
    if flags != TME_LEAVE && flags != (TME_CANCEL | TME_LEAVE) && flags != TME_CANCEL {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    if flags & TME_CANCEL != 0 {
        p.extra.win.mouse_tracking.remove(&hwnd);
    } else {
        p.extra.win.mouse_tracking.insert(hwnd);
    }
    Ok(1)
}

fn system_metric(index: u32) -> u64 {
    match index {
        0 | 16 | 76 | 78 => 1920, // screen/fullscreen/virtual width
        1 | 17 | 77 | 79 => 1080, // screen/fullscreen/virtual height
        4 => 23,                  // caption height
        5 | 6 => 1,               // border width / height
        7 | 8 => 3,               // dialog frame width / height
        13 | 14 => 32,            // cursor width / height
        15 => 20,                 // menu height
        28 => 112,                // minimum window width
        29 => 27,                 // minimum window height
        32 | 33 => 8,             // sizing frame width / height
        80 => 1,                  // monitor count
        _ => 0,
    }
}

fn get_window_placement(p: &mut Process) -> R {
    const WINDOWPLACEMENT_SIZE: u32 = 44;
    let (hwnd, placement) = (p.arg(0), p.arg(1));
    if placement == 0 || p.rd32(placement)? != WINDOWPLACEMENT_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let Some((x, y, w, h, visible)) = p
        .extra
        .win
        .windows
        .get(&hwnd)
        .map(|win| (win.x, win.y, win.w, win.h, win.visible))
    else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    p.wr32(placement + 4, 0)?;
    p.wr32(placement + 8, visible as u32)?;
    p.wr32(placement + 12, 0)?;
    p.wr32(placement + 16, 0)?;
    p.wr32(placement + 20, 0)?;
    p.wr32(placement + 24, 0)?;
    p.wr32(placement + 28, x as u32)?;
    p.wr32(placement + 32, y as u32)?;
    p.wr32(placement + 36, x.wrapping_add(w) as u32)?;
    p.wr32(placement + 40, y.wrapping_add(h) as u32)?;
    Ok(1)
}

fn set_window_placement(p: &mut Process) -> R {
    const WINDOWPLACEMENT_SIZE: u32 = 44;
    let (hwnd, placement) = (p.arg(0), p.arg(1));
    if placement == 0 || p.rd32(placement)? != WINDOWPLACEMENT_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (left, top, right, bottom) = (
        p.rd32(placement + 28)? as i32,
        p.rd32(placement + 32)? as i32,
        p.rd32(placement + 36)? as i32,
        p.rd32(placement + 40)? as i32,
    );
    let visible = p.rd32(placement + 8)? != 0;
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    let outer_w = right.saturating_sub(left);
    let outer_h = bottom.saturating_sub(top);
    let (client_w, client_h) = client_size_from_outer(
        outer_w,
        outer_h,
        window.style,
        window.ex_style,
        window.has_menu,
    );
    let changed = (window.client_w, window.client_h) != (client_w, client_h);
    window.x = left;
    window.y = top;
    window.w = outer_w;
    window.h = outer_h;
    window.client_w = client_w;
    window.client_h = client_h;
    window.visible = visible;
    window.needs_paint |= window.visible;
    let gpu_id = window.gpu_id;
    if changed {
        p.extra.gpu.emit(
            op::WINDOW,
            Pk::new()
                .u32(gpu_id)
                .u32(client_w as u32)
                .u32(client_h as u32),
        );
        send(
            p,
            hwnd,
            WM_SIZE,
            0,
            ((client_h as u64) << 16) | (client_w as u64 & 0xffff),
        )?;
    }
    Ok(1)
}

fn adjust_window_rect(p: &mut Process, extended: bool, dpi_aware: bool) -> R {
    let rect = p.arg(0);
    if rect == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (style, menu, ex_style) = (
        p.arg(1) as u32,
        p.arg(2) != 0,
        if extended { p.arg(3) as u32 } else { 0 },
    );
    let dpi = if dpi_aware {
        (p.arg(4) as u32).max(1)
    } else {
        96
    };
    let (x, y, bottom_inset) = nonclient_insets(style, ex_style, menu, dpi);
    let (left, top, right, bottom) = (
        p.rd32(rect)? as i32,
        p.rd32(rect + 4)? as i32,
        p.rd32(rect + 8)? as i32,
        p.rd32(rect + 12)? as i32,
    );
    p.wr32(rect, left.wrapping_sub(x) as u32)?;
    p.wr32(rect + 4, top.wrapping_sub(y) as u32)?;
    p.wr32(rect + 8, right.wrapping_add(x) as u32)?;
    p.wr32(rect + 12, bottom.wrapping_add(bottom_inset) as u32)?;
    Ok(1)
}

fn nonclient_insets(style: u32, ex_style: u32, menu: bool, dpi: u32) -> (i32, i32, i32) {
    let scale = |v: u32| ((v as u64 * dpi as u64 + 48) / 96) as i32;
    let thick = style & 0x0004_0000 != 0;
    let border = style & (0x0080_0000 | 0x0040_0000) != 0;
    let edge = ex_style & (0x0000_0200 | 0x0001_0000) != 0;
    let x = scale(if thick {
        8
    } else if border {
        1
    } else {
        0
    }) + scale(edge as u32 * 2);
    let mut y = scale(if thick {
        8
    } else if border {
        1
    } else {
        0
    }) + scale(edge as u32 * 2);
    if style & 0x00c0_0000 != 0 {
        y += scale(system_metric(4) as u32);
    }
    if menu {
        y += scale(system_metric(15) as u32);
    }
    let bottom = scale(if thick {
        8
    } else if border {
        1
    } else {
        0
    });
    (x, y, bottom)
}

fn client_size_from_outer(
    outer_w: i32,
    outer_h: i32,
    style: u32,
    ex_style: u32,
    has_menu: bool,
) -> (i32, i32) {
    let (left_right, top, bottom) = nonclient_insets(style, ex_style, has_menu, 96);
    (
        outer_w.saturating_sub(left_right.saturating_mul(2)).max(0),
        outer_h.saturating_sub(top.saturating_add(bottom)).max(0),
    )
}

fn map_virtual_key_w(p: &mut Process) -> R {
    let (code, map_type) = (p.arg(0) as u32, p.arg(1) as u32);
    Ok(map_virtual_key(code, map_type) as u64)
}

fn enum_display_devices_w(p: &mut Process) -> R {
    const DISPLAY_DEVICE_SIZE: u32 = 840;
    let (device, index, out, flags) = (p.arg(0), p.arg(1) as u32, p.arg(2), p.arg(3) as u32);
    if out == 0 || p.rd32(out)? < DISPLAY_DEVICE_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if index != 0 || flags & !1 != 0 {
        return Ok(0);
    }
    if device != 0 {
        let name = p.read_wstr(device)?;
        if !name.eq_ignore_ascii_case("\\\\.\\DISPLAY1") {
            return Ok(0);
        }
    }

    p.write_bytes(out, &[0; DISPLAY_DEVICE_SIZE as usize])?;
    p.wr32(out, DISPLAY_DEVICE_SIZE)?;
    p.write_wstr(out + 4, "\\\\.\\DISPLAY1")?;
    p.write_wstr(out + 68, "AnyWASM Virtual Display")?;
    p.wr32(out + 324, 0x1 | 0x4)?; // ATTACHED_TO_DESKTOP | PRIMARY_DEVICE
    if flags & 1 != 0 {
        p.write_wstr(out + 328, "\\\\?\\DISPLAY#ANYWASM#0001")?;
    }
    Ok(1)
}

fn enum_display_settings_w(p: &mut Process) -> R {
    const DEVMODEW_SIZE: u16 = 220;
    let (device, mode_num, out) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if out == 0 || p.rd16(out + 68)? < DEVMODEW_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if device != 0 && !p.read_wstr(device)?.eq_ignore_ascii_case("\\\\.\\DISPLAY1") {
        return Ok(0);
    }

    let (width, height, refresh) = match mode_num {
        0xffff_ffff | 0 => (1920, 1080, 60), // current / registry mode
        1 => (1280, 720, 60),
        2 => (640, 480, 60),
        _ => return Ok(0),
    };
    p.write_bytes(out, &[0; DEVMODEW_SIZE as usize])?;
    p.write_wstr(out, "\\\\.\\DISPLAY1")?;
    p.wr16(out + 64, 0x0401)?; // DM_SPECVERSION
    p.wr16(out + 66, 1)?;
    p.wr16(out + 68, DEVMODEW_SIZE)?;
    p.wr32(out + 72, 0x007c_0000)?; // bits-per-pixel, width, height, flags, frequency
    p.wr32(out + 168, 32)?;
    p.wr32(out + 172, width)?;
    p.wr32(out + 176, height)?;
    p.wr32(out + 180, 0)?;
    p.wr32(out + 184, refresh)?;
    Ok(1)
}

fn enum_display_monitors(p: &mut Process) -> R {
    let (hdc, clip, callback, data) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if callback == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (mut left, mut top, mut right, mut bottom) = (0i32, 0i32, 1920i32, 1080i32);
    if clip != 0 {
        let (clip_left, clip_top, clip_right, clip_bottom) = (
            p.rd32(clip)? as i32,
            p.rd32(clip + 4)? as i32,
            p.rd32(clip + 8)? as i32,
            p.rd32(clip + 12)? as i32,
        );
        left = left.max(clip_left);
        top = top.max(clip_top);
        right = right.min(clip_right);
        bottom = bottom.min(clip_bottom);
        if left >= right || top >= bottom {
            return Ok(1);
        }
    }

    let rect = super::kernel32::alloc_block(p, 16, true);
    if rect == 0 {
        return Ok(0);
    }
    p.wr32(rect, left as u32)?;
    p.wr32(rect + 4, top as u32)?;
    p.wr32(rect + 8, right as u32)?;
    p.wr32(rect + 12, bottom as u32)?;
    let result = p.call_guest(callback, &[0x5000, hdc, rect, data]);
    super::kernel32::free_block(p, rect);
    Ok((result? != 0) as u64)
}

fn to_unicode(p: &mut Process) -> R {
    let (vk, scan, key_state, buffer, capacity, flags) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3),
        p.arg(4) as u32 as i32,
        p.arg(5) as u32,
    );
    let _ = (scan, flags);
    if capacity <= 0 || buffer == 0 {
        return Ok(0);
    }
    let state: [u8; 256] = if key_state == 0 {
        [0; 256]
    } else {
        p.read_bytes(key_state, 256)?.try_into().unwrap()
    };
    let shift = [0x10, 0xa0, 0xa1].iter().any(|&key| state[key] & 0x80 != 0);
    let control = [0x11, 0xa2, 0xa3].iter().any(|&key| state[key] & 0x80 != 0);
    let alt = [0x12, 0xa4, 0xa5].iter().any(|&key| state[key] & 0x80 != 0);
    if control || alt {
        return Ok(0);
    }
    let caps = state[0x14] & 1 != 0;
    let Some(ch) = translate_us_key(vk, shift, caps) else {
        return Ok(0);
    };
    p.wr16(buffer, ch)?;
    Ok(1)
}

fn translate_us_key(vk: u32, shift: bool, caps: bool) -> Option<u16> {
    if (0x41..=0x5a).contains(&vk) {
        let upper = shift ^ caps;
        return Some(if upper { vk as u16 } else { (vk + 0x20) as u16 });
    }
    if (0x30..=0x39).contains(&vk) {
        let shifted = b")!@#$%^&*("[(vk - 0x30) as usize] as u16;
        return Some(if shift { shifted } else { vk as u16 });
    }
    if (0x60..=0x69).contains(&vk) {
        return Some((b'0' + (vk - 0x60) as u8) as u16);
    }
    Some(match vk {
        0x08 => 0x0008,
        0x09 => 0x0009,
        0x0d => 0x000d,
        0x20 => 0x0020,
        0xba => {
            if shift {
                b':' as u16
            } else {
                b';' as u16
            }
        }
        0xbb => {
            if shift {
                b'+' as u16
            } else {
                b'=' as u16
            }
        }
        0xbc => {
            if shift {
                b'<' as u16
            } else {
                b',' as u16
            }
        }
        0xbd => {
            if shift {
                b'_' as u16
            } else {
                b'-' as u16
            }
        }
        0xbe => {
            if shift {
                b'>' as u16
            } else {
                b'.' as u16
            }
        }
        0xbf => {
            if shift {
                b'?' as u16
            } else {
                b'/' as u16
            }
        }
        0xc0 => {
            if shift {
                b'~' as u16
            } else {
                b'`' as u16
            }
        }
        0xdb => {
            if shift {
                b'{' as u16
            } else {
                b'[' as u16
            }
        }
        0xdc => {
            if shift {
                b'|' as u16
            } else {
                b'\\' as u16
            }
        }
        0xdd => {
            if shift {
                b'}' as u16
            } else {
                b']' as u16
            }
        }
        0xde => {
            if shift {
                b'"' as u16
            } else {
                b'\'' as u16
            }
        }
        0xe2 => {
            if shift {
                b'|' as u16
            } else {
                b'\\' as u16
            }
        }
        _ => return None,
    })
}

fn map_virtual_key(code: u32, map_type: u32) -> u32 {
    match map_type {
        0 => vk_to_scan(code).map(|(scan, _)| scan as u32).unwrap_or(0),
        1 => scan_to_vk(code, false),
        2 => vk_to_char(code),
        3 => scan_to_vk(code, true),
        4 => vk_to_scan(code)
            .map(|(scan, extended)| {
                if extended {
                    0xe000 | scan as u32
                } else {
                    scan as u32
                }
            })
            .unwrap_or(0),
        _ => 0,
    }
}

fn vk_to_scan(vk: u32) -> Option<(u8, bool)> {
    const LETTERS: [u8; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    const KEYPAD: [u8; 10] = [0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49];
    if (0x41..=0x5a).contains(&vk) {
        return Some((LETTERS[(vk - 0x41) as usize], false));
    }
    if (0x60..=0x69).contains(&vk) {
        return Some((KEYPAD[(vk - 0x60) as usize], false));
    }
    if (0x70..=0x79).contains(&vk) {
        return Some((0x3b + (vk - 0x70) as u8, false));
    }
    if (0x30..=0x39).contains(&vk) {
        return Some((
            match vk {
                0x30 => 0x0b,
                _ => 0x02 + (vk - 0x31) as u8,
            },
            false,
        ));
    }
    Some(match vk {
        0x08 => (0x0e, false), // backspace
        0x09 => (0x0f, false), // tab
        0x0d => (0x1c, false), // enter
        0x10 | 0xa0 => (0x2a, false),
        0xa1 => (0x36, false),
        0x11 | 0xa2 => (0x1d, false),
        0xa3 => (0x1d, true),
        0x12 | 0xa4 => (0x38, false),
        0xa5 => (0x38, true),
        0x13 => (0x1d, true),  // pause (E1)
        0x14 => (0x3a, false), // caps lock
        0x1b => (0x01, false), // escape
        0x20 => (0x39, false), // space
        0x21 => (0x49, false), // page up
        0x22 => (0x51, false), // page down
        0x23 => (0x4f, false), // end
        0x24 => (0x47, false), // home
        0x25 => (0x4b, false), // left
        0x26 => (0x48, false), // up
        0x27 => (0x4d, false), // right
        0x28 => (0x50, false), // down
        0x2c => (0x54, false), // print screen
        0x2d => (0x52, false), // insert
        0x2e => (0x53, false), // delete
        0x5b => (0x5b, true),  // left Windows
        0x5c => (0x5c, true),  // right Windows
        0x5d => (0x5d, true),  // applications
        0x6a => (0x37, false), // keypad multiply
        0x6b => (0x4e, false), // keypad add
        0x6d => (0x4a, false), // keypad subtract
        0x6e => (0x53, false), // keypad decimal
        0x6f => (0x35, true),  // keypad divide
        0x7a => (0x57, false), // F11
        0x7b => (0x58, false), // F12
        0x90 => (0x45, false), // num lock
        0x91 => (0x46, false), // scroll lock
        0xba => (0x27, false), // OEM 1 ;
        0xbb => (0x0d, false), // OEM plus
        0xbc => (0x33, false), // OEM comma
        0xbd => (0x0c, false), // OEM minus
        0xbe => (0x34, false), // OEM period
        0xbf => (0x35, false), // OEM 2 /
        0xc0 => (0x29, false), // OEM 3 `
        0xdb => (0x1a, false), // OEM 4 [
        0xdc => (0x2b, false), // OEM 5 backslash
        0xdd => (0x1b, false), // OEM 6 ]
        0xde => (0x28, false), // OEM 7 '
        0xe2 => (0x56, false), // OEM 102
        _ => return None,
    })
}

fn scan_to_vk(code: u32, distinguish: bool) -> u32 {
    let prefix = (code >> 8) as u8;
    let scan = code as u8;
    if prefix != 0 && prefix != 0xe0 && prefix != 0xe1 {
        return 0;
    }
    if prefix == 0xe1 && scan == 0x1d {
        return 0x13; // pause
    }
    if prefix == 0xe0 {
        return match scan {
            0x1d => {
                if distinguish {
                    0xa3
                } else {
                    0x11
                }
            }
            0x38 => {
                if distinguish {
                    0xa5
                } else {
                    0x12
                }
            }
            0x47 => 0x24,
            0x48 => 0x26,
            0x49 => 0x21,
            0x4b => 0x25,
            0x4d => 0x27,
            0x4f => 0x23,
            0x50 => 0x28,
            0x51 => 0x22,
            0x52 => 0x2d,
            0x53 => 0x2e,
            0x5b => 0x5b,
            0x5c => 0x5c,
            0x5d => 0x5d,
            _ => 0,
        };
    }
    match scan {
        0x2a => return if distinguish { 0xa0 } else { 0x10 },
        0x36 => return if distinguish { 0xa1 } else { 0x10 },
        0x1d => return if distinguish { 0xa2 } else { 0x11 },
        0x38 => return if distinguish { 0xa4 } else { 0x12 },
        _ => {}
    }
    (0..=0xff)
        .find_map(|vk| {
            vk_to_scan(vk)
                .filter(|(s, extended)| *s == scan && !extended)
                .map(|_| vk)
        })
        .unwrap_or(0)
}

fn vk_to_char(vk: u32) -> u32 {
    match vk {
        0x30..=0x39 | 0x41..=0x5a => vk,
        0x20 => 0x20,
        0xba => b';' as u32,
        0xbb => b'=' as u32,
        0xbc => b',' as u32,
        0xbd => b'-' as u32,
        0xbe => b'.' as u32,
        0xbf => b'/' as u32,
        0xc0 => b'`' as u32,
        0xdb => b'[' as u32,
        0xdc => b'\\' as u32,
        0xdd => b']' as u32,
        0xde => b'\'' as u32,
        0xe2 => b'\\' as u32,
        _ => 0,
    }
}

fn write_rect(p: &mut Process, a: u64, l: i32, t: i32, r: i32, b: i32) -> Result<(), Stop> {
    p.wr32(a, l as u32)?;
    p.wr32(a + 4, t as u32)?;
    p.wr32(a + 8, r as u32)?;
    p.wr32(a + 12, b as u32)
}

fn read_rect(p: &Process, a: u64) -> Result<(i32, i32, i32, i32), Stop> {
    Ok((
        p.rd32(a)? as i32,
        p.rd32(a + 4)? as i32,
        p.rd32(a + 8)? as i32,
        p.rd32(a + 12)? as i32,
    ))
}

fn offset_rect(p: &mut Process) -> R {
    let (a, dx, dy) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32);
    let (l, t, r, b) = read_rect(p, a)?;
    write_rect(
        p,
        a,
        l.wrapping_add(dx),
        t.wrapping_add(dy),
        r.wrapping_add(dx),
        b.wrapping_add(dy),
    )?;
    Ok(1)
}

fn inflate_rect(p: &mut Process) -> R {
    let (a, dx, dy) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32);
    let (l, t, r, b) = read_rect(p, a)?;
    write_rect(
        p,
        a,
        l.wrapping_sub(dx),
        t.wrapping_sub(dy),
        r.wrapping_add(dx),
        b.wrapping_add(dy),
    )?;
    Ok(1)
}

fn intersect_rect(p: &mut Process) -> R {
    let (dst, a, b) = (p.arg(0), p.arg(1), p.arg(2));
    let (r1, r2) = (read_rect(p, a)?, read_rect(p, b)?);
    let out = (
        r1.0.max(r2.0),
        r1.1.max(r2.1),
        r1.2.min(r2.2),
        r1.3.min(r2.3),
    );
    let nonempty = out.0 < out.2 && out.1 < out.3;
    if nonempty {
        write_rect(p, dst, out.0, out.1, out.2, out.3)?;
    } else {
        write_rect(p, dst, 0, 0, 0, 0)?;
    }
    Ok(nonempty as u64)
}

fn union_rect(p: &mut Process) -> R {
    let (dst, a, b) = (p.arg(0), p.arg(1), p.arg(2));
    let (r1, r2) = (read_rect(p, a)?, read_rect(p, b)?);
    let empty1 = r1.0 >= r1.2 || r1.1 >= r1.3;
    let empty2 = r2.0 >= r2.2 || r2.1 >= r2.3;
    let out = match (empty1, empty2) {
        (true, true) => (0, 0, 0, 0),
        (true, false) => r2,
        (false, true) => r1,
        (false, false) => (
            r1.0.min(r2.0),
            r1.1.min(r2.1),
            r1.2.max(r2.2),
            r1.3.max(r2.3),
        ),
    };
    write_rect(p, dst, out.0, out.1, out.2, out.3)?;
    Ok((!empty1 || !empty2) as u64)
}

fn set_rect(p: &mut Process) -> R {
    let (a, l, t, r, b) = (
        p.arg(0),
        p.arg(1) as i32,
        p.arg(2) as i32,
        p.arg(3) as i32,
        p.arg(4) as i32,
    );
    write_rect(p, a, l, t, r, b)?;
    Ok(1)
}

fn set_rect_empty(p: &mut Process) -> R {
    let a = p.arg(0);
    write_rect(p, a, 0, 0, 0, 0)?;
    Ok(1)
}

fn copy_rect(p: &mut Process) -> R {
    let (dst, src) = (p.arg(0), p.arg(1));
    let r = read_rect(p, src)?;
    write_rect(p, dst, r.0, r.1, r.2, r.3)?;
    Ok(1)
}

fn register_class(p: &mut Process, wide: bool, ex: bool) -> R {
    let a = p.arg(0);
    // WNDCLASS and WNDCLASSEX have different field offsets in PE32. In PE32+,
    // the pointer-sized fields and alignment move the same fields again.
    let x86 = p.cpu.mode == CpuMode::X86_32;
    let wndproc_off = if ex || !x86 { 8 } else { 4 };
    let name_off = match (x86, ex) {
        (true, false) => 36,
        (true, true) => 40,
        (false, _) => 64,
    };
    let wndproc = p.rdptr(a + wndproc_off)?;
    let name_ptr = p.rdptr(a + name_off)?;
    let name = lc(&class_name(p, name_ptr, wide)?);
    p.extra.win.next_atom += 1;
    let atom = 0xc000 + p.extra.win.next_atom;
    p.extra.win.atoms.insert(atom, name.clone());
    p.extra.win.classes.insert(
        name.clone(),
        Class {
            wndproc,
            name,
            unicode: wide,
        },
    );
    Ok(atom as u64)
}

fn wsprintf_a(p: &mut Process) -> R {
    let (buffer, format_ptr) = (p.arg(0), p.arg(1));
    let format = p.read_cstr(format_ptr)?;
    let output = printf::format(p, format.as_bytes(), &mut printf::SlotArgs(2), false)?;
    p.write_bytes(buffer, &output)?;
    p.wr8(buffer + output.len() as u64, 0)?;
    Ok(output.len() as u64)
}

fn char_case_a(p: &mut Process, upper: bool, buffered: bool) -> R {
    let pointer = p.arg(0);
    if pointer == 0 {
        return Ok(0);
    }
    if !buffered && pointer <= 0xffff {
        let byte = pointer as u8;
        return Ok(if upper {
            byte.to_ascii_uppercase() as u64
        } else {
            byte.to_ascii_lowercase() as u64
        });
    }
    let bytes = if buffered {
        p.read_bytes(pointer, usize::try_from(p.arg(1)).unwrap_or(usize::MAX))?
    } else {
        let mut bytes = Vec::new();
        for offset in 0..(1 << 20) {
            let byte = p.rd8(pointer + offset)?;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        bytes
    };
    let bytes: Vec<u8> = bytes
        .into_iter()
        .map(|byte| {
            if upper {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            }
        })
        .collect();
    if buffered {
        let len = bytes.len();
        p.write_bytes(pointer, &bytes)?;
        Ok(len as u64)
    } else {
        p.write_bytes(pointer, &bytes)?;
        p.wr8(pointer + bytes.len() as u64, 0)?;
        Ok(pointer)
    }
}

fn create_window(p: &mut Process, wide: bool) -> R {
    let (ex_style, cls, title, style) = (p.arg(0) as u32, p.arg(1), p.arg(2), p.arg(3) as u32);
    let (x, y, w, h) = (
        p.arg(4) as i32,
        p.arg(5) as i32,
        p.arg(6) as i32,
        p.arg(7) as i32,
    );
    let (parent, menu, inst, param) = (p.arg(8), p.arg(9), p.arg(10), p.arg(11));
    let cname = if cls < 0x10000 && cls >= 0xc000 {
        p.extra
            .win
            .atoms
            .get(&(cls as u16))
            .cloned()
            .unwrap_or_default()
    } else {
        lc(&class_name(p, cls, wide)?)
    };
    let Some(class) = p
        .extra
        .win
        .classes
        .get(&cname)
        .cloned()
        .or_else(|| builtin_class(&cname))
    else {
        p.set_last_error(1407); // ERROR_CANNOT_FIND_WND_CLASS
        return Ok(0);
    };
    let title_s = if title == 0 {
        String::new()
    } else if wide {
        p.read_wstr(title)?
    } else {
        p.read_cstr(title)?
    };
    let cw = |v: i32, d: i32| {
        if v == i32::MIN || v == 0x8000_0000u32 as i32 {
            d
        } else {
            v
        }
    };
    let (w, h) = (cw(w, 1280), cw(h, 720));
    let (x, y) = (cw(x, 0), cw(y, 0));
    let is_child = style & WS_CHILD != 0;
    let menu_handle = if is_child { 0 } else { menu };
    let has_menu = menu_handle != 0;
    let (client_x, client_y, _) = nonclient_insets(style, ex_style, has_menu, 96);
    let (client_w, client_h) = client_size_from_outer(w, h, style, ex_style, has_menu);
    p.extra.win.next = p.extra.win.next.max(HWND_BASE) + 0x10;
    let hwnd = p.extra.win.next;
    let gpu_id = p.extra.gpu.new_id();
    let control_id = if parent != 0 && style & WS_CHILD != 0 {
        menu as u32
    } else {
        0
    };
    p.extra.win.windows.insert(
        hwnd,
        Window {
            class: cname,
            wndproc: class.wndproc,
            title: title_s.clone(),
            x,
            y,
            w,
            h,
            client_x,
            client_y,
            client_w,
            client_h,
            visible: false,
            style,
            ex_style,
            has_menu,
            menu: menu_handle,
            hinstance: inst,
            user_data: 0,
            parent,
            control_id,
            unicode: class.unicode,
            needs_paint: false,
            gpu_id,
        },
    );
    p.extra.win.z_order.retain(|existing| *existing != hwnd);
    p.extra.win.z_order.insert(0, hwnd);
    p.extra.win.window_threads.insert(hwnd, p.current_thread_id);
    if p.extra.win.main_window == 0 && parent == 0 {
        p.extra.win.main_window = hwnd;
        p.extra.win.focus = hwnd;
    }
    p.extra.gpu.emit(
        op::WINDOW,
        Pk::new()
            .u32(gpu_id)
            .u32(client_w as u32)
            .u32(client_h as u32)
            .bytes(title_s.as_bytes()),
    );
    // CREATESTRUCT is pointer-sized. Sims 3 is 32-bit, so it requires the packed 48-byte
    // layout; using the x64 offsets here corrupts hMenu/parent/geometry in its WndProc.
    let is_x86 = p.cpu.mode == CpuMode::X86_32;
    let cs = kernel32::alloc_block(p, if is_x86 { 48 } else { 80 }, true);
    if is_x86 {
        p.wr32(cs, param as u32)?;
        p.wr32(cs + 4, inst as u32)?;
        p.wr32(cs + 8, menu as u32)?;
        p.wr32(cs + 12, parent as u32)?;
        p.wr32(cs + 16, h as u32)?;
        p.wr32(cs + 20, w as u32)?;
        p.wr32(cs + 24, y as u32)?;
        p.wr32(cs + 28, x as u32)?;
        p.wr32(cs + 32, style)?;
        p.wr32(cs + 36, title as u32)?;
        p.wr32(cs + 40, cls as u32)?;
        p.wr32(cs + 44, ex_style)?;
    } else {
        p.wr64(cs, param)?;
        p.wr64(cs + 8, inst)?;
        p.wr64(cs + 16, menu)?;
        p.wr64(cs + 24, parent)?;
        p.wr32(cs + 32, h as u32)?;
        p.wr32(cs + 36, w as u32)?;
        p.wr32(cs + 40, y as u32)?;
        p.wr32(cs + 44, x as u32)?;
        p.wr32(cs + 48, style)?;
        p.wr64(cs + 56, title)?;
        p.wr64(cs + 64, cls)?;
        p.wr32(cs + 72, ex_style)?;
    }
    let ok = send(p, hwnd, WM_NCCREATE, 0, cs)?;
    if ok == 0 {
        p.extra.win.windows.remove(&hwnd);
        return Ok(0);
    }
    let r = send(p, hwnd, WM_CREATE, 0, cs)?;
    kernel32::free_block(p, cs);
    if r as i32 == -1 {
        p.extra.win.windows.remove(&hwnd);
        return Ok(0);
    }
    send(
        p,
        hwnd,
        WM_SIZE,
        0,
        ((client_h as u64) << 16) | (client_w as u64 & 0xffff),
    )?;
    Ok(hwnd)
}

/// System classes created by name (BUTTON, EDIT, STATIC...) behave as inert windows for now.
fn builtin_class(name: &str) -> Option<Class> {
    matches!(
        name,
        "button" | "edit" | "static" | "combobox" | "listbox" | "scrollbar" | "#32770"
    )
    .then(|| Class {
        wndproc: 0,
        name: name.to_string(),
        unicode: true,
    })
}

fn destroy_window(p: &mut Process, h: u64) -> Result<(), Stop> {
    if !p.extra.win.windows.contains_key(&h) {
        return Ok(());
    }
    p.extra.win.mouse_tracking.remove(&h);
    send(p, h, WM_DESTROY, 0, 0)?;
    send(p, h, WM_NCDESTROY, 0, 0)?;
    p.extra.win.windows.remove(&h);
    p.extra.win.z_order.retain(|hwnd| *hwnd != h);
    p.extra.win.window_threads.remove(&h);
    remove_clipboard_viewer(p, h);
    if p.extra.win.clipboard_owner == h {
        p.extra.win.clipboard_owner = 0;
    }
    p.extra.win.timers.retain(|(_, hwnd, _), _| *hwnd != h);
    p.extra.win.props.retain(|(hwnd, _), _| *hwnd != h);
    p.extra.win.paint_windows.remove(&h);
    p.extra
        .win
        .device_context_refs
        .retain(|(hwnd, _), _| *hwnd != h);
    if p.extra.win.focus == h {
        p.extra.win.focus = 0;
    }
    if p.extra.win.capture == h {
        p.extra.win.capture = 0;
    }
    if p.extra.win.main_window == h {
        p.extra.win.main_window = 0;
    }
    Ok(())
}

fn prop_name(p: &Process, ptr: u64, wide: bool) -> Result<String, Stop> {
    if ptr < 0x10000 {
        Ok(format!("#{}", ptr as u16))
    } else if wide {
        p.read_wstr(ptr)
    } else {
        p.read_cstr(ptr)
    }
}

fn set_prop(p: &mut Process, wide: bool) -> R {
    let (hwnd, name_ptr, value) = (p.arg(0), p.arg(1), p.arg(2));
    if name_ptr == 0 || !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(if name_ptr == 0 { 87 } else { 1400 });
        return Ok(0);
    }
    let name = prop_name(p, name_ptr, wide)?;
    p.extra.win.props.insert((hwnd, name), value);
    Ok(1)
}

fn get_prop(p: &mut Process, wide: bool, remove: bool) -> R {
    let (hwnd, name_ptr) = (p.arg(0), p.arg(1));
    if name_ptr == 0 {
        return Ok(0);
    }
    let name = prop_name(p, name_ptr, wide)?;
    Ok(if remove {
        p.extra.win.props.remove(&(hwnd, name)).unwrap_or(0)
    } else {
        p.extra.win.props.get(&(hwnd, name)).copied().unwrap_or(0)
    })
}

pub fn set_fullscreen_window(
    p: &mut Process,
    hwnd: u64,
    width: u32,
    height: u32,
) -> Result<(), Stop> {
    let width = width.min(i32::MAX as u32) as i32;
    let height = height.min(i32::MAX as u32) as i32;
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        return Ok(());
    };
    let changed = (
        window.x,
        window.y,
        window.w,
        window.h,
        window.client_w,
        window.client_h,
        window.style,
    ) != (0, 0, width, height, width, height, WS_POPUP);
    window.x = 0;
    window.y = 0;
    window.w = width;
    window.h = height;
    window.client_x = 0;
    window.client_y = 0;
    window.client_w = width;
    window.client_h = height;
    window.style = WS_POPUP;
    window.ex_style = 0;
    window.has_menu = false;
    let gpu_id = window.gpu_id;
    if changed {
        p.extra.gpu.emit(
            op::WINDOW,
            Pk::new().u32(gpu_id).u32(width as u32).u32(height as u32),
        );
        send(
            p,
            hwnd,
            WM_SIZE,
            0,
            ((height as u64) << 16) | (width as u64 & 0xffff),
        )?;
    }
    Ok(())
}

fn resize_window(
    p: &mut Process,
    h: u64,
    pos: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    repaint: bool,
) -> Result<(), Stop> {
    let mut changed = None;
    if let Some(w) = p.extra.win.windows.get_mut(&h) {
        if let Some((x, y)) = pos {
            if (x, y) != (w.x, w.y) && repaint {
                w.needs_paint |= w.visible;
            }
            w.x = x;
            w.y = y;
        }
        if let Some((cx, cy)) = size {
            if (cx, cy) != (w.w, w.h) {
                w.w = cx;
                w.h = cy;
                (w.client_x, w.client_y, _) = nonclient_insets(w.style, w.ex_style, w.has_menu, 96);
                (w.client_w, w.client_h) =
                    client_size_from_outer(cx, cy, w.style, w.ex_style, w.has_menu);
                changed = Some((w.client_w, w.client_h, w.gpu_id));
                if repaint {
                    w.needs_paint |= w.visible;
                }
            }
        }
    }
    if let Some((cx, cy, id)) = changed {
        p.extra
            .gpu
            .emit(op::WINDOW, Pk::new().u32(id).u32(cx as u32).u32(cy as u32));
        send(p, h, WM_SIZE, 0, ((cy as u64) << 16) | (cx as u64 & 0xffff))?;
    }
    Ok(())
}

fn reorder_window_by_topmost(p: &mut Process, hwnd: u64, topmost: bool) {
    p.extra.win.z_order.retain(|existing| *existing != hwnd);
    let index = if topmost {
        0
    } else {
        p.extra
            .win
            .z_order
            .iter()
            .position(|existing| {
                p.extra
                    .win
                    .windows
                    .get(existing)
                    .is_some_and(|window| window.ex_style & WS_EX_TOPMOST != 0)
            })
            .unwrap_or(0)
    };
    p.extra.win.z_order.insert(index, hwnd);
}

fn set_window_pos(p: &mut Process) -> R {
    let (hwnd, insert_after, x, y, cx, cy, flags) = (
        p.arg(0),
        p.arg(1),
        p.arg(2) as i32,
        p.arg(3) as i32,
        p.arg(4) as i32,
        p.arg(5) as i32,
        p.arg(6) as u32,
    );
    let Some(window) = p.extra.win.windows.get(&hwnd).cloned() else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };

    let supported_flags = SWP_NOSIZE
        | SWP_NOMOVE
        | SWP_NOZORDER
        | SWP_NOREDRAW
        | SWP_NOACTIVATE
        | SWP_FRAMECHANGED
        | SWP_SHOWWINDOW
        | SWP_HIDEWINDOW;
    if flags & !supported_flags != 0 {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("SetWindowPos flags {:#x}", flags & !supported_flags),
        });
    }
    if flags & (SWP_SHOWWINDOW | SWP_HIDEWINDOW) == (SWP_SHOWWINDOW | SWP_HIDEWINDOW) {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos with both SWP_SHOWWINDOW and SWP_HIDEWINDOW".into(),
        });
    }
    let change_zorder = flags & SWP_NOZORDER == 0;
    let topmost = if change_zorder {
        if is_special_hwnd(p, insert_after, HWND_TOPMOST) {
            Some(true)
        } else if is_special_hwnd(p, insert_after, HWND_NOTOPMOST) {
            Some(false)
        } else {
            return Err(Stop::Unimplemented {
                dll: "user32".into(),
                name: "SetWindowPos z-order changes other than HWND_TOPMOST/HWND_NOTOPMOST".into(),
            });
        }
    } else {
        None
    };
    if topmost.is_some() && window.parent != 0 {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos topmost changes for child windows".into(),
        });
    }
    if flags & SWP_NOACTIVATE == 0 && window.parent != 0 {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos activation for child windows".into(),
        });
    }
    if flags & SWP_FRAMECHANGED != 0
        && flags & (SWP_NOMOVE | SWP_NOSIZE) != (SWP_NOMOVE | SWP_NOSIZE)
    {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos SWP_FRAMECHANGED with movement or resizing".into(),
        });
    }
    if flags & SWP_FRAMECHANGED != 0 && window.parent != 0 {
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos SWP_FRAMECHANGED for child windows".into(),
        });
    }

    let repaint = flags & SWP_NOREDRAW == 0;
    let needs_paint_before = window.needs_paint;
    let effective_flags = if flags & SWP_FRAMECHANGED != 0 {
        set_window_pos_frame_changed(p, hwnd, insert_after, (x, y, cx, cy), flags)?
    } else {
        resize_window(
            p,
            hwnd,
            (flags & SWP_NOMOVE == 0).then_some((x, y)),
            (flags & SWP_NOSIZE == 0).then_some((cx, cy)),
            repaint,
        )?;
        if let Some(topmost) = topmost {
            let window = p.extra.win.windows.get_mut(&hwnd).unwrap();
            if topmost {
                window.ex_style |= WS_EX_TOPMOST;
            } else {
                window.ex_style &= !WS_EX_TOPMOST;
            }
        }
        if flags & SWP_NOACTIVATE == 0 {
            activate_window(p, hwnd)?;
        }
        flags
    };

    if effective_flags & (SWP_SHOWWINDOW | SWP_HIDEWINDOW) != 0 {
        let visible = effective_flags & SWP_SHOWWINDOW != 0;
        if p.extra.win.windows[&hwnd].visible != visible {
            p.extra.win.windows.get_mut(&hwnd).unwrap().visible = visible;
            send(p, hwnd, WM_SHOWWINDOW, visible as u64, 0)?;
            if visible && effective_flags & SWP_NOREDRAW == 0 {
                p.extra.win.windows.get_mut(&hwnd).unwrap().needs_paint = true;
            }
        }
    }
    if effective_flags & SWP_NOREDRAW != 0 {
        // WM_SHOWWINDOW's default processing normally invalidates a newly shown window;
        // SWP_NOREDRAW explicitly suppresses that paint work for this operation.
        p.extra.win.windows.get_mut(&hwnd).unwrap().needs_paint = needs_paint_before;
    }
    if let Some(topmost) = topmost {
        reorder_window_by_topmost(p, hwnd, topmost);
    }
    Ok(1)
}

fn is_special_hwnd(p: &Process, value: u64, x86_value: u64) -> bool {
    if p.cpu.mode == CpuMode::X86_32 {
        value as u32 == x86_value as u32
    } else {
        value == x86_value || value == (x86_value as u32 as i32 as i64 as u64)
    }
}

fn set_window_pos_frame_changed(
    p: &mut Process,
    hwnd: u64,
    insert_after: u64,
    geometry: (i32, i32, i32, i32),
    flags: u32,
) -> Result<u32, Stop> {
    let window = p
        .extra
        .win
        .windows
        .get(&hwnd)
        .cloned()
        .ok_or_else(|| Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("SetWindowPos for destroyed window {hwnd:#x}"),
        })?;
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let windowpos_size = if ptr_size == 4 { 28 } else { 40 };
    let windowpos = kernel32::alloc_block(p, windowpos_size, true);
    if windowpos == 0 {
        return Err(Stop::Crash(
            "cannot allocate WINDOWPOS for SetWindowPos".into(),
        ));
    }
    if let Err(error) = write_windowpos(p, windowpos, hwnd, insert_after, geometry, flags) {
        kernel32::free_block(p, windowpos);
        return Err(error);
    }
    let changing = send(p, hwnd, WM_WINDOWPOSCHANGING, 0, windowpos);
    if let Err(error) = changing {
        kernel32::free_block(p, windowpos);
        return Err(error);
    }

    let pos_result = read_windowpos(p, windowpos);
    let (pos_hwnd, insert_after, _pos_geometry, flags) = match pos_result {
        Ok(position) => position,
        Err(error) => {
            kernel32::free_block(p, windowpos);
            return Err(error);
        }
    };
    if pos_hwnd != hwnd
        || flags & (SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE)
            != (SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE)
        || flags
            & !(SWP_NOSIZE
                | SWP_NOMOVE
                | SWP_NOZORDER
                | SWP_NOREDRAW
                | SWP_NOACTIVATE
                | SWP_FRAMECHANGED
                | SWP_SHOWWINDOW
                | SWP_HIDEWINDOW)
            != 0
    {
        kernel32::free_block(p, windowpos);
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos WM_WINDOWPOSCHANGING modifications outside supported frame change"
                .into(),
        });
    }
    let topmost = if flags & SWP_NOZORDER != 0 {
        None
    } else if is_special_hwnd(p, insert_after, HWND_TOPMOST) {
        Some(true)
    } else if is_special_hwnd(p, insert_after, HWND_NOTOPMOST) {
        Some(false)
    } else {
        kernel32::free_block(p, windowpos);
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos z-order changes requested by WM_WINDOWPOSCHANGING".into(),
        });
    };
    if flags & (SWP_SHOWWINDOW | SWP_HIDEWINDOW) == (SWP_SHOWWINDOW | SWP_HIDEWINDOW) {
        kernel32::free_block(p, windowpos);
        return Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: "SetWindowPos WM_WINDOWPOSCHANGING set both SHOWWINDOW and HIDEWINDOW".into(),
        });
    }

    let params_size = if ptr_size == 4 { 52 } else { 56 };
    let params = kernel32::alloc_block(p, params_size + ptr_size, true);
    if params == 0 {
        kernel32::free_block(p, windowpos);
        return Err(Stop::Crash(
            "cannot allocate NCCALCSIZE_PARAMS for SetWindowPos".into(),
        ));
    }
    let outer = (
        window.x,
        window.y,
        window.x.saturating_add(window.w),
        window.y.saturating_add(window.h),
    );
    let old_client = (
        window.x.saturating_add(window.client_x),
        window.y.saturating_add(window.client_y),
        window
            .x
            .saturating_add(window.client_x)
            .saturating_add(window.client_w),
        window
            .y
            .saturating_add(window.client_y)
            .saturating_add(window.client_h),
    );
    let params_result = (|| {
        write_rect(p, params, outer.0, outer.1, outer.2, outer.3)?;
        write_rect(p, params + 16, outer.0, outer.1, outer.2, outer.3)?;
        write_rect(
            p,
            params + 32,
            old_client.0,
            old_client.1,
            old_client.2,
            old_client.3,
        )?;
        p.wrptr(params + 48, windowpos)?;
        send(p, hwnd, WM_NCCALCSIZE, 1, params)?;
        read_rect(p, params)
    })();
    kernel32::free_block(p, params);
    let client_rect = match params_result {
        Ok(rect) => rect,
        Err(error) => {
            kernel32::free_block(p, windowpos);
            return Err(error);
        }
    };

    if let Some(topmost) = topmost {
        let window = p.extra.win.windows.get_mut(&hwnd).unwrap();
        if topmost {
            window.ex_style |= WS_EX_TOPMOST;
        } else {
            window.ex_style &= !WS_EX_TOPMOST;
        }
    }
    let (client_w, client_h, gpu_id) = {
        let window = p.extra.win.windows.get_mut(&hwnd).unwrap();
        window.client_x = client_rect.0.saturating_sub(window.x);
        window.client_y = client_rect.1.saturating_sub(window.y);
        window.client_w = client_rect.2.saturating_sub(client_rect.0).max(0);
        window.client_h = client_rect.3.saturating_sub(client_rect.1).max(0);
        if flags & SWP_NOREDRAW == 0 && window.visible {
            window.needs_paint = true;
        }
        (window.client_w, window.client_h, window.gpu_id)
    };
    let client_changed = (client_w, client_h) != (window.client_w, window.client_h);

    if flags & SWP_NOACTIVATE == 0 {
        if let Err(error) = activate_window(p, hwnd) {
            kernel32::free_block(p, windowpos);
            return Err(error);
        }
    }
    let changed_result = send(p, hwnd, WM_WINDOWPOSCHANGED, 0, windowpos);
    kernel32::free_block(p, windowpos);
    changed_result?;
    if client_changed {
        p.extra.gpu.emit(
            op::WINDOW,
            Pk::new()
                .u32(gpu_id)
                .u32(client_w as u32)
                .u32(client_h as u32),
        );
        send(
            p,
            hwnd,
            WM_SIZE,
            0,
            ((client_h as u64) << 16) | (client_w as u64 & 0xffff),
        )?;
    }
    let _ = geometry;
    Ok(flags)
}

fn write_windowpos(
    p: &mut Process,
    address: u64,
    hwnd: u64,
    insert_after: u64,
    (x, y, cx, cy): (i32, i32, i32, i32),
    flags: u32,
) -> Result<(), Stop> {
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    p.wrptr(address, hwnd)?;
    p.wrptr(address + ptr_size, insert_after)?;
    let geometry = address + ptr_size * 2;
    p.wr32(geometry, x as u32)?;
    p.wr32(geometry + 4, y as u32)?;
    p.wr32(geometry + 8, cx as u32)?;
    p.wr32(geometry + 12, cy as u32)?;
    p.wr32(geometry + 16, flags)
}

fn read_windowpos(
    p: &Process,
    address: u64,
) -> Result<(u64, u64, (i32, i32, i32, i32), u32), Stop> {
    let ptr_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let hwnd = p.rdptr(address)?;
    let insert_after = p.rdptr(address + ptr_size)?;
    let geometry = address + ptr_size * 2;
    Ok((
        hwnd,
        insert_after,
        (
            p.rd32(geometry)? as i32,
            p.rd32(geometry + 4)? as i32,
            p.rd32(geometry + 8)? as i32,
            p.rd32(geometry + 12)? as i32,
        ),
        p.rd32(geometry + 16)?,
    ))
}

fn activate_window(p: &mut Process, hwnd: u64) -> Result<u64, Stop> {
    if hwnd != 0 && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    let previous = p.extra.win.focus;
    if previous == hwnd {
        return Ok(previous);
    }
    if previous != 0 && p.extra.win.windows.contains_key(&previous) {
        send(p, previous, WM_NCACTIVATE, 0, hwnd)?;
        let previous_minimized = p
            .extra
            .win
            .windows
            .get(&previous)
            .is_some_and(|window| window.style & WS_MINIMIZE != 0);
        send(
            p,
            previous,
            WM_ACTIVATE,
            (previous_minimized as u64) << 16,
            hwnd,
        )?;
        send(p, previous, WM_KILLFOCUS, hwnd, 0)?;
    }
    p.extra.win.focus = hwnd;
    if hwnd != 0 {
        send(p, hwnd, WM_NCACTIVATE, 1, previous)?;
        let minimized = p
            .extra
            .win
            .windows
            .get(&hwnd)
            .is_some_and(|window| window.style & WS_MINIMIZE != 0);
        send(
            p,
            hwnd,
            WM_ACTIVATE,
            1 | ((minimized as u64) << 16),
            previous,
        )?;
        send(p, hwnd, WM_SETFOCUS, previous, 0)?;
    }
    Ok(previous)
}

pub fn send(p: &mut Process, hwnd: u64, msg: u64, wparam: u64, lparam: u64) -> R {
    let Some(w) = p.extra.win.windows.get(&hwnd) else {
        return Ok(0);
    };
    let wndproc = w.wndproc;
    if wndproc == 0 {
        return def_window_proc(p, hwnd, msg, wparam, lparam);
    }
    let result = p.call_guest(wndproc, &[hwnd, msg, wparam, lparam])?;
    if p.trace_modules_only {
        p.stderr.extend_from_slice(
            format!("[wnd-message] hwnd={hwnd:#x} msg={msg:#x} wparam={wparam:#x} lparam={lparam:#x} result={result:#x}\n")
                .as_bytes(),
        );
    }
    Ok(result)
}

fn post(p: &mut Process, hwnd: u64, msg: u64, wparam: u64, lparam: u64) {
    let t = (p.now_ticks() / 100_000) as u32;
    let pt = p.extra.win.cursor;
    p.extra.win.queue.push_back(Msg {
        hwnd,
        msg,
        wparam,
        lparam,
        time: t,
        pt,
    });
}

fn allocate_timer_id(win: &mut WinState) -> Option<u64> {
    const FIRST_SYSTEM_TIMER_ID: u32 = 0xc000;
    const LAST_SYSTEM_TIMER_ID: u32 = 0xffff;
    if !(FIRST_SYSTEM_TIMER_ID..=LAST_SYSTEM_TIMER_ID).contains(&win.next_timer_id) {
        win.next_timer_id = FIRST_SYSTEM_TIMER_ID;
    }
    for _ in FIRST_SYSTEM_TIMER_ID..=LAST_SYSTEM_TIMER_ID {
        let id = win.next_timer_id;
        win.next_timer_id = if id == LAST_SYSTEM_TIMER_ID {
            FIRST_SYSTEM_TIMER_ID
        } else {
            id + 1
        };
        if !win.timers.values().any(|timer| timer.id == id as u64) {
            return Some(id as u64);
        }
    }
    None
}

fn set_timer(p: &mut Process) -> R {
    let (hwnd, requested_id, elapse, callback) = (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(3));
    if hwnd != 0 && !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }

    let thread_id = p.current_thread_id;
    let existing_thread_timer = hwnd == 0
        && requested_id != 0
        && p.extra
            .win
            .timers
            .contains_key(&(thread_id, 0, requested_id));
    let id = if hwnd != 0 && requested_id != 0 {
        requested_id
    } else if existing_thread_timer {
        requested_id
    } else {
        let Some(id) = allocate_timer_id(&mut p.extra.win) else {
            p.set_last_error(8); // ERROR_NOT_ENOUGH_MEMORY
            return Ok(0);
        };
        id
    };

    let interval_ms = elapse.clamp(10, 0x7fff_ffff) as u64;
    let interval_ticks = interval_ms * 100_000;
    let key = (thread_id, hwnd, id);
    let deadline_ticks = p.now_ticks().saturating_add(interval_ticks);
    p.extra.win.timers.insert(
        key,
        Timer {
            thread_id,
            hwnd,
            id,
            interval_ticks,
            deadline_ticks,
            callback,
            pending: false,
        },
    );
    Ok(id)
}

fn kill_timer(p: &mut Process) -> R {
    let key = (p.current_thread_id, p.arg(0), p.arg(1));
    Ok(p.extra.win.timers.remove(&key).is_some() as u64)
}

/// Delay until the next timer owned by the currently running guest thread.
/// `None` means the thread has no active timer that needs host time to advance.
pub fn next_timer_delay_ms(p: &Process) -> Option<u32> {
    let now = p.now_ticks();
    p.extra
        .win
        .timers
        .values()
        .filter(|timer| timer.thread_id == p.current_thread_id && !timer.pending)
        .map(|timer| timer.deadline_ticks.saturating_sub(now).div_ceil(100_000))
        .min()
        .map(|ms| ms.min(u32::MAX as u64) as u32)
}

fn queue_due_timer(p: &mut Process, filter_hwnd: u64) -> Option<Msg> {
    let now = p.now_ticks();
    let timer = p
        .extra
        .win
        .timers
        .values()
        .filter(|timer| {
            timer.thread_id == p.current_thread_id
                && !timer.pending
                && timer.deadline_ticks <= now
                && (filter_hwnd == 0 || timer.hwnd == filter_hwnd)
        })
        .min_by_key(|timer| timer.deadline_ticks)
        .copied()?;
    if p.extra.win.queue.iter().any(|message| {
        message.msg == WM_TIMER && message.hwnd == timer.hwnd && message.wparam == timer.id
    }) {
        return None;
    }
    let key = (timer.thread_id, timer.hwnd, timer.id);
    if let Some(active) = p.extra.win.timers.get_mut(&key) {
        active.pending = true;
    }
    let message = Msg {
        hwnd: timer.hwnd,
        msg: WM_TIMER,
        wparam: timer.id,
        lparam: timer.callback,
        time: (now / 100_000) as u32,
        pt: p.extra.win.cursor,
    };
    p.extra.win.queue.push_back(message);
    Some(message)
}

fn consume_timer_message(p: &mut Process, message: Msg) {
    let key = (p.current_thread_id, message.hwnd, message.wparam);
    let now = p.now_ticks();
    if let Some(timer) = p.extra.win.timers.get_mut(&key) {
        if timer.pending {
            timer.pending = false;
            timer.deadline_ticks = now.saturating_add(timer.interval_ticks);
        }
    }
}

fn paint_now(p: &mut Process, h: u64) -> Result<(), Stop> {
    if p.extra
        .win
        .windows
        .get(&h)
        .map(|w| w.needs_paint && w.visible)
        .unwrap_or(false)
    {
        send(p, h, WM_PAINT, 0, 0)?;
    }
    Ok(())
}

fn def_window_proc(p: &mut Process, h: u64, msg: u64, w: u64, l: u64) -> R {
    match msg {
        // WM_CREATE's documented success result is zero; the window object has already
        // been installed by CreateWindowEx before this message is dispatched.
        WM_CREATE => Ok(0),
        WM_NCCREATE => Ok(1),
        WM_NCCALCSIZE => {
            let Some(window) = p.extra.win.windows.get(&h).cloned() else {
                return Ok(0);
            };
            let (left, top, right, bottom) = read_rect(p, l)?;
            let (inset_x, inset_top, inset_bottom) =
                nonclient_insets(window.style, window.ex_style, window.has_menu, 96);
            write_rect(
                p,
                l,
                left.saturating_add(inset_x),
                top.saturating_add(inset_top),
                right.saturating_sub(inset_x),
                bottom.saturating_sub(inset_bottom),
            )?;
            Ok(0)
        }
        WM_NCACTIVATE => Ok(1),
        WM_ACTIVATE | WM_SETFOCUS | WM_KILLFOCUS | WM_WINDOWPOSCHANGING | WM_WINDOWPOSCHANGED => {
            Ok(0)
        }
        // The window manager updates client/outer dimensions before delivering this
        // notification; resize_window/create_window own that state transition here.
        WM_SIZE => Ok(0),
        WM_SHOWWINDOW => {
            let Some(window) = p.extra.win.windows.get_mut(&h) else {
                return Err(Stop::Unimplemented {
                    dll: "user32".into(),
                    name: format!("DefWindowProc WM_SHOWWINDOW for window {h:#x}"),
                });
            };
            window.visible = w != 0;
            window.needs_paint |= window.visible;
            Ok(0)
        }
        WM_CLOSE => {
            destroy_window(p, h)?;
            Ok(0)
        }
        // This notification lets the clipboard owner release resources it allocated for
        // delayed rendering. DefWindowProc has no work to perform for the message.
        WM_DESTROYCLIPBOARD => Ok(0),
        WM_ERASEBKGND => Ok(1),
        WM_PAINT => {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = false;
            }
            Ok(0)
        }
        _ => Err(Stop::Unimplemented {
            dll: "user32".into(),
            name: format!("DefWindowProc message {msg:#x}"),
        }),
    }
}

/// Fill the `MSG` at `out`. Returns false when nothing is available.
fn fetch(p: &mut Process, out: u64, filter_hwnd: u64, remove: bool) -> Result<bool, Stop> {
    // Queued messages first (input, posted messages).
    let idx = p
        .extra
        .win
        .queue
        .iter()
        .position(|m| filter_hwnd == 0 || m.hwnd == filter_hwnd);
    let mut queue_index = idx;
    let msg = if let Some(i) = idx {
        Some(p.extra.win.queue[i])
    } else if let Some(code) = p.extra.win.quit {
        Some(Msg {
            hwnd: 0,
            msg: WM_QUIT,
            wparam: code as u64,
            lparam: 0,
            time: 0,
            pt: (0, 0),
        })
    } else if let Some((&h, _)) = p
        .extra
        .win
        .windows
        .iter()
        .find(|(_, w)| w.needs_paint && w.visible)
    {
        // WM_PAINT is generated lazily, like Windows does when the queue is otherwise empty.
        Some(Msg {
            hwnd: h,
            msg: WM_PAINT,
            wparam: 0,
            lparam: 0,
            time: 0,
            pt: (0, 0),
        })
    } else if let Some(timer_message) = queue_due_timer(p, filter_hwnd) {
        queue_index = Some(p.extra.win.queue.len() - 1);
        Some(timer_message)
    } else {
        None
    };
    let Some(m) = msg else { return Ok(false) };
    if remove {
        if let Some(i) = queue_index {
            p.extra.win.queue.remove(i);
        }
        if m.msg == WM_QUIT {
            p.extra.win.quit = None;
            p.extra.win.queue.clear();
        } else if m.msg == WM_PAINT {
            if let Some(w) = p.extra.win.windows.get_mut(&m.hwnd) {
                w.needs_paint = false;
            }
        } else if m.msg == WM_TIMER {
            consume_timer_message(p, m);
        }
    }
    if p.cpu.mode == CpuMode::X86_32 {
        p.wr32(out, m.hwnd as u32)?;
        p.wr32(out + 4, m.msg as u32)?;
        p.wr32(out + 8, m.wparam as u32)?;
        p.wr32(out + 12, m.lparam as u32)?;
        p.wr32(out + 16, m.time)?;
        p.wr32(out + 20, m.pt.0 as u32)?;
        p.wr32(out + 24, m.pt.1 as u32)?;
    } else {
        p.wr64(out, m.hwnd)?;
        p.wr32(out + 8, m.msg as u32)?;
        p.wr32(out + 12, 0)?;
        p.wr64(out + 16, m.wparam)?;
        p.wr64(out + 24, m.lparam)?;
        p.wr32(out + 32, m.time)?;
        p.wr32(out + 36, m.pt.0 as u32)?;
        p.wr32(out + 40, m.pt.1 as u32)?;
    }
    Ok(true)
}

fn read_message_fields(p: &Process, m: u64) -> Result<(u64, u64, u64, u64), Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        Ok((
            p.rd32(m)? as u64,
            p.rd32(m + 4)? as u64,
            p.rd32(m + 8)? as u64,
            p.rd32(m + 12)? as u64,
        ))
    } else {
        Ok((
            p.rd64(m)?,
            p.rd32(m + 8)? as u64,
            p.rd64(m + 16)?,
            p.rd64(m + 24)?,
        ))
    }
}

fn read_message_type(p: &Process, m: u64) -> Result<u64, Stop> {
    let offset = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    Ok(p.rd32(m + offset)? as u64)
}

fn read_message_time(p: &Process, m: u64) -> Result<i32, Stop> {
    let offset = if p.cpu.mode == CpuMode::X86_32 {
        16
    } else {
        32
    };
    Ok(p.rd32(m + offset)? as i32)
}

fn get_message(p: &mut Process) -> R {
    let (m, h) = (p.arg(0), p.arg(1));
    if fetch(p, m, h, true)? {
        p.extra.win.last_get_message_time = read_message_time(p, m)?;
        let msg = read_message_type(p, m)?;
        return Ok((msg != WM_QUIT) as u64);
    }
    // Nothing to do: hand control back to the host and retry this call when input arrives.
    Err(Stop::Idle)
}

fn dispatch_message(p: &mut Process) -> R {
    let message = p.arg(0);
    let (hwnd, kind, wparam, lparam) = read_message_fields(p, message)?;
    if kind == WM_TIMER && lparam != 0 {
        let elapsed_ms = (p.now_ticks() / 100_000) as u32;
        p.call_guest(lparam, &[hwnd, WM_TIMER, wparam, elapsed_ms as u64])?;
        return Ok(0);
    }
    send(p, hwnd, kind, wparam, lparam)
}

fn peek_message(p: &mut Process) -> R {
    let (m, h, flags) = (p.arg(0), p.arg(1), p.arg(4));
    let before = p.extra.win.queue.len();
    let result = fetch(p, m, h, flags & PM_REMOVE != 0)?;
    if result {
        p.extra.win.last_get_message_time = read_message_time(p, m)?;
    }
    if p.trace_gta_eventvol && before != 0 {
        let next = p.extra.win.queue.front().map(|m| (m.hwnd, m.msg));
        p.stderr.extend_from_slice(
            format!(
                "[gta-eventvol] PeekMessageA filter={h:#x} flags={flags:#x} removed={} queue={before}->{} next={next:?}\n",
                result && (flags & PM_REMOVE != 0),
                p.extra.win.queue.len(),
            )
            .as_bytes(),
        );
    }
    Ok(result as u64)
}

/// Select the browser input target, skipping hidden GLFW helper windows.
fn input_target_window(win: &WinState) -> u64 {
    let is_visible_top_level = |h: u64| {
        win.windows
            .get(&h)
            .map(|w| w.visible && w.parent == 0)
            .unwrap_or(false)
    };
    if is_visible_top_level(win.focus) {
        win.focus
    } else if is_visible_top_level(win.main_window) {
        win.main_window
    } else {
        win.windows
            .iter()
            .find_map(|(&h, w)| (w.visible && w.parent == 0).then_some(h))
            .unwrap_or(win.main_window)
    }
}

/// Translate a browser input event into window messages. Kinds:
/// 0 key down (a=vk), 1 key up, 2 char (a=utf16), 3 mouse move (a=x,b=y), 4 button down (a=btn 0/1/2,b=x,c=y),
/// 5 button up, 6 wheel (a=delta, b=x, c=y), 7 resize (a=w,b=h), 8 focus (a=1/0), 9 close request,
/// 10 mouse leave.
fn invoke_low_level_hooks(
    p: &mut Process,
    hook_type: i32,
    wparam: u64,
    point: (i32, i32),
    value: u32,
    flags: u32,
) -> Result<bool, Stop> {
    let hooks: Vec<Hook> = p
        .extra
        .win
        .hooks
        .values()
        .copied()
        .filter(|hook| {
            hook.hook_type == hook_type
                && (hook.thread_id == 0 || hook.thread_id == p.current_thread_id)
        })
        .collect();
    if hooks.is_empty() {
        return Ok(false);
    }

    let is_x86 = p.cpu.mode == CpuMode::X86_32;
    let pointer_bytes = if is_x86 { 4 } else { 8 };
    let now = ((p.cpu.icount + p.extra.sleep_adjust) / 100_000) as u32;
    let (size, data) = if hook_type == 14 {
        // MSLLHOOKSTRUCT: POINT, mouseData, flags, time, ULONG_PTR dwExtraInfo.
        let extra_offset = if is_x86 { 20 } else { 24 };
        let mut bytes = vec![0; extra_offset + pointer_bytes];
        bytes[0..4].copy_from_slice(&(point.0 as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&(point.1 as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&value.to_le_bytes());
        bytes[12..16].copy_from_slice(&flags.to_le_bytes());
        bytes[16..20].copy_from_slice(&now.to_le_bytes());
        (bytes.len(), bytes)
    } else {
        // KBDLLHOOKSTRUCT: vkCode, scanCode, flags, time, ULONG_PTR dwExtraInfo.
        let extra_offset = 16;
        let mut bytes = vec![0; extra_offset + pointer_bytes];
        bytes[0..4].copy_from_slice(&value.to_le_bytes());
        bytes[4..8].copy_from_slice(&map_virtual_key(value, 0).to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[12..16].copy_from_slice(&now.to_le_bytes());
        (bytes.len(), bytes)
    };
    let event = p.alloc_virtual(size as u64, PROT_RW);
    p.write_bytes(event, &data)?;
    let mut suppress = false;
    for hook in hooks {
        if p.call_guest(hook.callback, &[0, wparam, event])? != 0 {
            suppress = true;
            break;
        }
    }
    p.mem.unmap(event, 0x1000);
    p.vmem.remove(&event);
    Ok(suppress)
}

pub fn input_event(p: &mut Process, kind: u32, a: i64, b: i64, c: i64) -> Result<(), Stop> {
    let h = input_target_window(&p.extra.win);
    let packed = |x: i64, y: i64| ((y as u64 & 0xffff) << 16) | (x as u64 & 0xffff);
    match kind {
        0 | 1 => {
            if p.extra.win.keys.len() < 256 {
                p.extra.win.keys.resize(256, 0);
            }
            p.extra.win.keys[(a & 0xff) as usize] = (kind == 0) as u8;
            let up = kind == 1;
            let msg = if up { WM_KEYUP } else { WM_KEYDOWN };
            let key_flags = if up { 0x80 } else { 0 };
            if invoke_low_level_hooks(p, 13, msg, (0, 0), a as u32, key_flags)? || h == 0 {
                return Ok(());
            }
            post(p, h, msg, a as u64, if up { 0xc000_0001 } else { 1 });
        }
        2 => {
            if h != 0 {
                post(p, h, WM_CHAR, a as u64, 1);
            }
        }
        3 => {
            let (x, y) = clamp_cursor(&p.extra.win, (a as i32, b as i32));
            if p.extra.win.mouse_position_valid {
                p.extra.win.mouse_dx = p
                    .extra
                    .win
                    .mouse_dx
                    .saturating_add(x.saturating_sub(p.extra.win.cursor.0));
                p.extra.win.mouse_dy = p
                    .extra
                    .win
                    .mouse_dy
                    .saturating_add(y.saturating_sub(p.extra.win.cursor.1));
            }
            p.extra.win.cursor = (x, y);
            p.extra.win.mouse_position_valid = true;
            if !invoke_low_level_hooks(p, 14, WM_MOUSEMOVE, (x, y), 0, 0)? && h != 0 {
                post(p, h, WM_MOUSEMOVE, 0, packed(x as i64, y as i64));
            }
        }
        4 | 5 => {
            let (x, y) = clamp_cursor(&p.extra.win, (b as i32, c as i32));
            let (m_down, m_up) = match a {
                0 => (WM_LBUTTONDOWN, WM_LBUTTONUP),
                1 => (WM_MBUTTONDOWN, WM_MBUTTONUP),
                _ => (WM_RBUTTONDOWN, WM_RBUTTONUP),
            };
            let vk = [1usize, 4, 2][a.clamp(0, 2) as usize];
            if p.extra.win.keys.len() < 256 {
                p.extra.win.keys.resize(256, 0);
            }
            p.extra.win.keys[vk] = (kind == 4) as u8;
            let msg = if kind == 4 { m_down } else { m_up };
            if !invoke_low_level_hooks(p, 14, msg, (x, y), 0, 0)? && h != 0 {
                post(p, h, msg, 0, packed(x as i64, y as i64));
            }
        }
        6 => {
            p.extra.win.mouse_wheel = p.extra.win.mouse_wheel.saturating_add(a as i32);
            let (x, y) = clamp_cursor(&p.extra.win, (b as i32, c as i32));
            let mouse_data = (a as i32 as u32) << 16;
            if !invoke_low_level_hooks(p, 14, WM_MOUSEWHEEL, (x, y), mouse_data, 0)? && h != 0 {
                post(
                    p,
                    h,
                    WM_MOUSEWHEEL,
                    ((a as u64) & 0xffff) << 16,
                    packed(x as i64, y as i64),
                );
            }
        }
        7 => {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.w = a as i32;
                w.h = b as i32;
            }
            if h != 0 {
                post(p, h, WM_SIZE, 0, packed(a, b));
            }
        }
        8 if h != 0 => post(p, h, if a != 0 { WM_SETFOCUS } else { WM_KILLFOCUS }, 0, 0),
        9 if h != 0 => post(p, h, WM_CLOSE, 0, 0),
        10 if h != 0 && p.extra.win.mouse_tracking.remove(&h) => post(p, h, WM_MOUSELEAVE, 0, 0),
        10 => {}
        11 => {
            p.extra.win.mouse_dx = p.extra.win.mouse_dx.saturating_add(a as i32);
            p.extra.win.mouse_dy = p.extra.win.mouse_dy.saturating_add(b as i32);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::PROT_RWX;

    #[test]
    fn target_imported_user32_apis_resolve_only_when_the_runtime_has_a_state_model() {
        for name in [
            "GetTopWindow",
            "FindWindowA",
            "SetDlgItemTextW",
            "MapWindowPoints",
            "GetDoubleClickTime",
            "SetClipboardViewer",
            "IsClipboardFormatAvailable",
            "GetClipboardData",
            "OpenClipboard",
            "EmptyClipboard",
            "SetClipboardData",
            "CloseClipboard",
            "ChangeClipboardChain",
            "GetWindowThreadProcessId",
        ] {
            assert!(lookup(name).is_some(), "{name} should be translated");
        }
        for name in [
            "DialogBoxParamA",
            "EndDialog",
            "CreateIconIndirect",
            "CreateIconFromResource",
            "RegisterDeviceNotificationA",
            "WaitForInputIdle",
            "FillRect",
        ] {
            assert!(
                lookup(name).is_none(),
                "{name} must remain exposed as untranslated"
            );
        }
    }

    #[test]
    fn top_window_find_window_and_dialog_text_use_modeled_window_state() {
        const FRONT: u64 = HWND_BASE + 0x100;
        const BACK: u64 = HWND_BASE + 0x200;
        const PARENT: u64 = HWND_BASE + 0x300;
        const CHILD: u64 = HWND_BASE + 0x400;
        let mut p = test_process_with_args(&[]);
        p.extra.win.windows.insert(FRONT, test_window(true, 0));
        p.extra.win.windows.insert(BACK, test_window(true, 0));
        p.extra.win.windows.insert(PARENT, test_window(true, 0));
        p.extra.win.windows.insert(CHILD, test_window(true, PARENT));
        p.extra.win.windows.get_mut(&FRONT).unwrap().class = "GameWindow".into();
        p.extra.win.windows.get_mut(&FRONT).unwrap().title = "The Sims 3".into();
        p.extra.win.windows.get_mut(&CHILD).unwrap().style = WS_CHILD;
        p.extra.win.windows.get_mut(&CHILD).unwrap().control_id = 17;
        p.extra.win.z_order = vec![FRONT, BACK, PARENT, CHILD];

        set_args(&mut p, &[0]);
        assert_eq!(lookup("GetTopWindow").unwrap()(&mut p), Ok(FRONT));

        let class = p.alloc_virtual(0x1000, PROT_RW);
        let title = class + 0x100;
        p.write_cstr(class, "gamewindow").unwrap();
        p.write_cstr(title, "THE SIMS 3").unwrap();
        set_args(&mut p, &[class as u32, title as u32]);
        assert_eq!(lookup("FindWindowA").unwrap()(&mut p), Ok(FRONT));

        let text = class + 0x200;
        p.write_wstr(text, "New label").unwrap();
        set_args(&mut p, &[PARENT as u32, 17, text as u32]);
        assert_eq!(lookup("SetDlgItemTextW").unwrap()(&mut p), Ok(1));
        assert_eq!(p.extra.win.windows[&CHILD].title, "New label");
        assert!(p.extra.win.windows[&CHILD].needs_paint);
    }

    #[test]
    fn get_window_thread_process_id_and_map_window_points_use_guest_layouts() {
        const FROM: u64 = HWND_BASE + 0x500;
        const TO: u64 = HWND_BASE + 0x600;
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        let process_id_out = p.alloc_virtual(0x1000, PROT_RW);
        let points = process_id_out + 0x100;
        p.wr32(TEB_ADDR + 0x20, 0x1234).unwrap();
        p.current_thread_id = 0x5678;
        p.extra.win.windows.insert(FROM, test_window(true, 0));
        p.extra.win.windows.insert(TO, test_window(true, 0));
        let thread_id = p.current_thread_id;
        p.extra.win.window_threads.insert(FROM, thread_id);
        {
            let from = p.extra.win.windows.get_mut(&FROM).unwrap();
            from.x = 100;
            from.y = 200;
            from.client_x = 5;
            from.client_y = 6;
        }
        {
            let to = p.extra.win.windows.get_mut(&TO).unwrap();
            to.x = 20;
            to.y = 30;
            to.client_x = 2;
            to.client_y = 3;
        }

        set_args(&mut p, &[FROM as u32, process_id_out as u32]);
        assert_eq!(
            lookup("GetWindowThreadProcessId").unwrap()(&mut p),
            Ok(0x5678)
        );
        assert_eq!(p.rd32(process_id_out).unwrap(), 0x1234);

        p.wr32(points, 7).unwrap();
        p.wr32(points + 4, (-3i32) as u32).unwrap();
        set_args(&mut p, &[FROM as u32, TO as u32, points as u32, 1]);
        assert_eq!(
            lookup("MapWindowPoints").unwrap()(&mut p),
            Ok(((173u16 as u32) << 16 | 83) as u64)
        );
        assert_eq!(p.rd32(points).unwrap() as i32, 90);
        assert_eq!(p.rd32(points + 4).unwrap() as i32, 170);
    }

    #[test]
    fn clipboard_hglobal_formats_require_open_state_and_transfer_owned_data() {
        const WINDOW: u64 = HWND_BASE + 0x700;
        const CF_TEXT: u32 = 1;
        let mut p = test_process_with_args(&[]);
        p.extra.win.windows.insert(WINDOW, test_window(true, 0));
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));

        set_args(&mut p, &[WINDOW as u32]);
        assert_eq!(lookup("OpenClipboard").unwrap()(&mut p), Ok(1));
        set_args(&mut p, &[]);
        assert_eq!(lookup("EmptyClipboard").unwrap()(&mut p), Ok(1));

        let global = kernel32::alloc_block(&mut p, 16, true);
        p.write_bytes(global, b"clipboard text\0").unwrap();
        set_args(&mut p, &[CF_TEXT, global as u32]);
        assert_eq!(lookup("SetClipboardData").unwrap()(&mut p), Ok(global));
        set_args(&mut p, &[CF_TEXT]);
        assert_eq!(lookup("IsClipboardFormatAvailable").unwrap()(&mut p), Ok(1));
        assert_eq!(lookup("GetClipboardData").unwrap()(&mut p), Ok(global));

        set_args(&mut p, &[]);
        assert_eq!(lookup("CloseClipboard").unwrap()(&mut p), Ok(1));
        set_args(&mut p, &[CF_TEXT]);
        assert_eq!(lookup("GetClipboardData").unwrap()(&mut p), Ok(0));
        set_args(&mut p, &[WINDOW as u32]);
        assert_eq!(lookup("OpenClipboard").unwrap()(&mut p), Ok(1));
        set_args(&mut p, &[]);
        assert_eq!(lookup("EmptyClipboard").unwrap()(&mut p), Ok(1));
        assert!(p.heap.size_of(global).is_none());
    }

    #[test]
    fn clipboard_viewer_removal_updates_the_modeled_chain_and_posts_to_predecessor() {
        const FIRST: u64 = HWND_BASE + 0x800;
        const MIDDLE: u64 = HWND_BASE + 0x900;
        const LAST: u64 = HWND_BASE + 0xa00;
        let mut p = test_process_with_args(&[]);
        for hwnd in [FIRST, MIDDLE, LAST] {
            p.extra.win.windows.insert(hwnd, test_window(true, 0));
        }

        for (hwnd, expected_next) in [(FIRST, 0), (MIDDLE, FIRST), (LAST, MIDDLE)] {
            set_args(&mut p, &[hwnd as u32]);
            assert_eq!(
                lookup("SetClipboardViewer").unwrap()(&mut p),
                Ok(expected_next)
            );
        }
        set_args(&mut p, &[MIDDLE as u32, FIRST as u32]);
        assert_eq!(lookup("ChangeClipboardChain").unwrap()(&mut p), Ok(1));
        assert_eq!(p.extra.win.clipboard_viewers, vec![LAST, FIRST]);
        let notification = p.extra.win.queue.back().unwrap();
        assert_eq!(notification.hwnd, LAST);
        assert_eq!(notification.msg, WM_CHANGECBCHAIN);
        assert_eq!(notification.wparam, MIDDLE);
        assert_eq!(notification.lparam, FIRST);

        set_args(&mut p, &[FIRST as u32, LAST as u32]);
        assert!(matches!(
            lookup("ChangeClipboardChain").unwrap()(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("differs from modeled successor")
        ));
    }

    #[test]
    fn message_box_requires_an_interactive_translation() {
        const TEXT: u64 = 0x30000;
        const CAPTION: u64 = 0x31000;
        let mut p = test_process_with_args(&[0, TEXT as u32, CAPTION as u32, 0]);
        assert!(p.mem.map(TEXT, 0x1000, PROT_RW));
        assert!(p.mem.map(CAPTION, 0x1000, PROT_RW));
        p.write_wstr(TEXT, "Unable to start game").unwrap();
        p.write_wstr(CAPTION, "The Sims 3").unwrap();

        let result = lookup("MessageBoxW").unwrap()(&mut p);

        assert!(matches!(
            result,
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("MessageBoxW")
        ));
    }

    #[test]
    fn message_box_a_requires_an_interactive_translation() {
        const TEXT: u64 = 0x32000;
        const CAPTION: u64 = 0x33000;
        let mut p = test_process_with_args(&[0, TEXT as u32, CAPTION as u32, 0]);
        assert!(p.mem.map(TEXT, 0x1000, PROT_RW));
        assert!(p.mem.map(CAPTION, 0x1000, PROT_RW));
        p.write_cstr(TEXT, "Unable to start game").unwrap();
        p.write_cstr(CAPTION, "The Sims 3").unwrap();

        let result = lookup("MessageBoxA").unwrap()(&mut p);

        assert!(matches!(
            result,
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("MessageBoxA")
        ));
    }

    #[test]
    fn set_window_long_updates_all_modeled_standard_fields_and_returns_previous_values() {
        let hwnd = HWND_BASE + 0x40;
        let mut p = test_process_with_args(&[hwnd as u32, 0, 0]);
        p.extra.win.windows.insert(hwnd, test_window(false, 0));
        {
            let window = p.extra.win.windows.get_mut(&hwnd).unwrap();
            window.style = 0x10;
            window.ex_style = 0x20;
            window.wndproc = 0x1234;
            window.user_data = 0x5678;
            window.parent = 0x9abc;
            window.control_id = 7;
            window.hinstance = 0x1357;
        }

        let set_long = lookup("SetWindowLongA").unwrap();
        for (index, value, expected_previous, expected_current) in [
            (-16, 0x31, 0x10, 0x31),
            (-20, 0x42, 0x20, 0x42),
            (-4, 0x53, 0x1234, 0x53),
            (-21, 0x64, 0x5678, 0x64),
            (-8, 0x75, 0x9abc, 0x75),
            (-12, 0x86, 7, 0x86),
            (-6, 0x97, 0x1357, 0x97),
        ] {
            set_args(&mut p, &[hwnd as u32, index as u32, value]);
            assert_eq!(set_long(&mut p), Ok(expected_previous));
            let window = &p.extra.win.windows[&hwnd];
            let current = match index {
                -16 => window.style as u64,
                -20 => window.ex_style as u64,
                -4 => window.wndproc,
                -21 => window.user_data,
                -8 => window.parent,
                -12 => window.control_id as u64,
                -6 => window.hinstance,
                _ => unreachable!(),
            };
            assert_eq!(current, expected_current, "index {index}");
        }
    }

    #[test]
    fn get_window_long_reads_all_modeled_standard_fields() {
        let hwnd = HWND_BASE + 0x40;
        let mut p = test_process_with_args(&[hwnd as u32, 0]);
        let mut window = test_window(false, 0x9abc);
        window.style = 0x10;
        window.ex_style = 0x20;
        window.wndproc = 0x1234;
        window.user_data = 0x5678;
        window.control_id = 7;
        window.hinstance = 0x1357;
        p.extra.win.windows.insert(hwnd, window);

        let get_long = lookup("GetWindowLongA").unwrap();
        for (index, expected) in [
            (-16, 0x10),
            (-20, 0x20),
            (-4, 0x1234),
            (-21, 0x5678),
            (-8, 0x9abc),
            (-12, 7),
            (-6, 0x1357),
        ] {
            set_args(&mut p, &[hwnd as u32, index as u32]);
            assert_eq!(get_long(&mut p), Ok(expected), "index {index}");
        }
    }

    #[test]
    fn window_long_reports_invalid_handles_and_stops_on_untranslated_offsets() {
        let hwnd = HWND_BASE + 0x40;
        let missing = hwnd + 0x100;
        let mut p = test_process_with_args(&[missing as u32, (-16_i32) as u32, 1]);
        assert!(p.mem.map(crate::process::TEB_ADDR, 0x1000, PROT_RW));
        let set_long = lookup("SetWindowLongA").unwrap();
        assert_eq!(set_long(&mut p), Ok(0));
        assert_eq!(p.rd32(crate::process::TEB_ADDR + 0x34).unwrap(), 1400);

        let get_long = lookup("GetWindowLongA").unwrap();
        set_args(&mut p, &[missing as u32, (-16_i32) as u32]);
        assert_eq!(get_long(&mut p), Ok(0));
        assert_eq!(p.rd32(crate::process::TEB_ADDR + 0x34).unwrap(), 1400);

        set_args(&mut p, &[hwnd as u32, (-16_i32) as u32]);
        p.extra.win.windows.insert(hwnd, test_window(false, 0));
        assert!(matches!(get_long(&mut p), Ok(0)));

        set_args(&mut p, &[hwnd as u32, 0]);
        assert!(matches!(
            get_long(&mut p),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("GetWindowLong") && name.contains("index 0")
        ));
        set_args(&mut p, &[hwnd as u32, 0, 1]);
        assert!(matches!(
            set_long(&mut p),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("SetWindowLong") && name.contains("index 0")
        ));
    }

    #[test]
    fn default_window_proc_reports_messages_outside_its_translated_set() {
        let mut p = Process::new(crate::vfs::Vfs::new());

        let result = def_window_proc(&mut p, 0, 0x1234, 0, 0);

        assert!(matches!(
            result,
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("0x1234")
        ));
    }

    #[test]
    fn default_window_proc_accepts_window_creation_and_preserves_the_window() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let hwnd = HWND_BASE + 0x40;
        p.extra.win.windows.insert(hwnd, test_window(false, 0));

        assert_eq!(def_window_proc(&mut p, hwnd, WM_CREATE, 0, 0), Ok(0));
        assert!(p.extra.win.windows.contains_key(&hwnd));
    }

    #[test]
    fn default_window_proc_accepts_size_notification_after_geometry_is_updated() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let hwnd = HWND_BASE + 0x40;
        let mut window = test_window(false, 0);
        window.w = 800;
        window.h = 600;
        window.client_w = 800;
        window.client_h = 600;
        p.extra.win.windows.insert(hwnd, window);

        assert_eq!(
            def_window_proc(&mut p, hwnd, WM_SIZE, 0, (600_u64 << 16) | 800),
            Ok(0)
        );
        let window = &p.extra.win.windows[&hwnd];
        assert_eq!((window.w, window.h), (800, 600));
        assert_eq!((window.client_w, window.client_h), (800, 600));
    }

    #[test]
    fn default_window_proc_handles_showwindow_with_its_documented_result() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let hwnd = HWND_BASE + 0x40;
        p.extra.win.windows.insert(hwnd, test_window(false, 0));

        assert_eq!(def_window_proc(&mut p, hwnd, WM_SHOWWINDOW, 1, 0), Ok(0));
        assert!(p.extra.win.windows[&hwnd].visible);
        assert!(p.extra.win.windows[&hwnd].needs_paint);

        assert_eq!(def_window_proc(&mut p, hwnd, WM_SHOWWINDOW, 0, 0), Ok(0));
        assert!(!p.extra.win.windows[&hwnd].visible);
    }

    fn test_window(visible: bool, parent: u64) -> Window {
        Window {
            class: "test".into(),
            wndproc: 0,
            title: String::new(),
            x: 0,
            y: 0,
            w: 640,
            h: 480,
            client_x: 0,
            client_y: 0,
            client_w: 640,
            client_h: 480,
            visible,
            style: 0,
            ex_style: 0,
            has_menu: false,
            menu: 0,
            hinstance: 0,
            user_data: 0,
            parent,
            control_id: 0,
            unicode: true,
            needs_paint: false,
            gpu_id: 0,
        }
    }

    fn test_process_with_args(args: &[u32]) -> Process {
        let mut p = Process::new(crate::vfs::Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        p.vmem_next = 0x2000_0000;
        set_args(&mut p, args);
        p
    }

    fn set_args(p: &mut Process, args: &[u32]) {
        let stack = p.alloc_virtual(0x1000, PROT_RW);
        p.cpu.gpr[RSP] = stack;
        p.wr32(stack, 0xfeed_face).unwrap();
        for (index, arg) in args.iter().copied().enumerate() {
            p.wr32(stack + 4 + index as u64 * 4, arg).unwrap();
        }
    }

    fn process_with_string_resource() -> (Process, u64) {
        const BASE: u64 = 0x0010_0000;
        const ROOT_RVA: u32 = 0x1000;
        const DATA_RVA: u32 = 0x2000;
        const BUFFER: u64 = BASE + 0x4000;
        let mut p = Process::new(crate::vfs::Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        p.vmem_next = 0x2000_0000;
        assert!(p.mem.map(BASE, 0x5000, PROT_RW));
        p.modules.push(Module {
            name: "game.exe".into(),
            path: r"C:\Game\game.exe".into(),
            base: BASE,
            size: 0x3000,
            entry: 0,
            is_dll: false,
            tls_callbacks: Vec::new(),
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: ROOT_RVA,
            resource_size: 0x1000,
            hle_group: None,
        });

        let root = BASE + ROOT_RVA as u64;
        p.wr16(root + 14, 1).unwrap();
        p.wr32(root + 16, 6).unwrap(); // RT_STRING
        p.wr32(root + 20, 0x8000_0018).unwrap();
        let names = root + 0x18;
        p.wr16(names + 14, 1).unwrap();
        p.wr32(names + 16, 2).unwrap(); // IDs 17–32 are in block 2.
        p.wr32(names + 20, 0x8000_0030).unwrap();
        let languages = root + 0x30;
        p.wr16(languages + 14, 1).unwrap();
        p.wr32(languages + 16, 0).unwrap(); // neutral language
        p.wr32(languages + 20, 0x48).unwrap();
        let data_entry = root + 0x48;

        let text: Vec<u16> = "Loading café".encode_utf16().collect();
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u16.to_le_bytes()); // string ID 16 (slot 0)
        payload.extend_from_slice(&0u16.to_le_bytes()); // string ID 17 (slot 1)
        payload.extend_from_slice(&(text.len() as u16).to_le_bytes()); // ID 18 (slot 2)
        for unit in text {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
        for _ in 3..16 {
            payload.extend_from_slice(&0u16.to_le_bytes());
        }
        p.wr32(data_entry, DATA_RVA).unwrap();
        p.wr32(data_entry + 4, payload.len() as u32).unwrap();
        p.write_bytes(BASE + DATA_RVA as u64, &payload).unwrap();
        (p, BUFFER)
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        set_args(p, args);
        lookup(name).unwrap_or_else(|| panic!("user32!{name} should resolve"))(p).unwrap()
    }

    #[test]
    fn enum_display_monitors_returns_false_when_callback_stops() {
        const CALLBACK: u64 = 0x0003_0000;
        const COUNT: u64 = 0x0003_1000;
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(CALLBACK, 0x1000, PROT_RWX));
        assert!(p.mem.map(COUNT, 0x1000, PROT_RW));
        p.wr32(COUNT, 0).unwrap();

        let mut code = vec![0xff, 0x05]; // inc dword ptr [COUNT]
        code.extend_from_slice(&(COUNT as u32).to_le_bytes());
        code.extend_from_slice(&[0xb8, 0, 0, 0, 0, 0xc2, 0x10, 0]); // return FALSE; stdcall 4 args
        p.write_bytes(CALLBACK, &code).unwrap();

        let stack = p.alloc_virtual(0x1000, PROT_RW);
        let sp = stack + 0x800;
        p.cpu.gpr[RSP] = sp;
        p.wr32(sp, 0xfeed_face).unwrap();
        for (index, argument) in [0, 0, CALLBACK as u32, 0].into_iter().enumerate() {
            p.wr32(sp + 4 + index as u64 * 4, argument).unwrap();
        }
        let result = lookup("EnumDisplayMonitors").unwrap()(&mut p).unwrap();

        assert_eq!(result, 0);
        assert_eq!(p.rd32(COUNT).unwrap(), 1);
    }

    #[test]
    fn get_monitor_info_a_initializes_the_extended_device_name_field() {
        const INFO: u64 = 0x0003_2000;
        const MONITORINFOEXA_SIZE: u32 = 72;
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(INFO, 0x1000, PROT_RW));
        p.write_bytes(INFO, &[0xa5; MONITORINFOEXA_SIZE as usize])
            .unwrap();
        p.wr32(INFO, MONITORINFOEXA_SIZE).unwrap();

        assert_eq!(call(&mut p, "GetMonitorInfoA", &[0x5000, INFO as u32]), 1);

        let actual = p.read_bytes(INFO + 40, 32).unwrap();
        let mut expected = [0; 32];
        expected[..12].copy_from_slice(b"\\\\.\\DISPLAY1");
        assert_eq!(actual, expected);
    }

    #[test]
    fn get_monitor_info_w_initializes_the_extended_device_name_field() {
        const INFO: u64 = 0x0003_3000;
        const MONITORINFOEXW_SIZE: u32 = 104;
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(INFO, 0x1000, PROT_RW));
        p.write_bytes(INFO, &[0xa5; MONITORINFOEXW_SIZE as usize])
            .unwrap();
        p.wr32(INFO, MONITORINFOEXW_SIZE).unwrap();

        assert_eq!(call(&mut p, "GetMonitorInfoW", &[0x5000, INFO as u32]), 1);

        let actual = p.read_bytes(INFO + 40, 64).unwrap();
        let mut expected = [0; 64];
        for (index, unit) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
            expected[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn set_window_pos_repaints_changed_visible_window_when_allowed() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        let mut p = test_process_with_args(&[]);
        p.extra
            .win
            .windows
            .insert(HWND as u64, test_window(true, 0));

        assert_eq!(
            call(
                &mut p,
                "SetWindowPos",
                &[HWND, 0, 10, 20, 700, 500, 0x14], // SWP_NOZORDER | SWP_NOACTIVATE
            ),
            1
        );

        let window = &p.extra.win.windows[&(HWND as u64)];
        assert_eq!((window.x, window.y, window.w, window.h), (10, 20, 700, 500));
        assert!(window.needs_paint);
    }

    #[test]
    fn set_window_pos_no_redraw_preserves_pending_paint_state() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        let mut p = test_process_with_args(&[]);
        p.extra
            .win
            .windows
            .insert(HWND as u64, test_window(true, 0));

        assert_eq!(
            call(
                &mut p,
                "SetWindowPos",
                &[HWND, 0, 10, 20, 700, 500, 0x1c], // NOZORDER | NOACTIVATE | NOREDRAW
            ),
            1
        );

        let window = &p.extra.win.windows[&(HWND as u64)];
        assert_eq!((window.x, window.y, window.w, window.h), (10, 20, 700, 500));
        assert!(!window.needs_paint);
    }

    #[test]
    fn set_window_pos_rejects_unmodeled_z_order_before_mutation() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        let mut p = test_process_with_args(&[]);
        p.extra
            .win
            .windows
            .insert(HWND as u64, test_window(true, 0));

        set_args(&mut p, &[HWND, 0, 10, 20, 700, 500, 0x10]); // lacks SWP_NOZORDER
        let result = lookup("SetWindowPos").unwrap()(&mut p);

        assert!(matches!(
            result,
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "user32" && name.contains("SetWindowPos") && name.contains("z-order")
        ));
        let window = &p.extra.win.windows[&(HWND as u64)];
        assert_eq!((window.x, window.y, window.w, window.h), (0, 0, 640, 480));
    }

    #[test]
    fn set_window_pos_frame_changed_sends_nccalcsize_and_uses_guest_client_rect() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        const WNDPROC: u64 = 0x0040_0000;
        const SEEN_NCCALCSIZE: u64 = 0x0040_1000;
        let mut p = test_process_with_args(&[]);
        p.trace_modules_only = true;
        assert!(p.mem.map(WNDPROC, 0x2000, PROT_RWX));
        assert!(p.mem.map(0x2000_f000, 0x1000, PROT_RW));

        // Record WM_NCCALCSIZE and return a client rectangle inset by (4, 5, 6, 5).
        let mut wndproc = vec![
            0x8b, 0x44, 0x24, 0x08, // mov eax, [esp+8] (message)
            0x3d, 0x83, 0, 0, 0, // cmp eax, WM_NCCALCSIZE
            0x75, 0x00, // jne return zero; displacement patched below
            0x8b, 0x44, 0x24, 0x10, // mov eax, [esp+16] (lParam)
            0xc7, 0x00, 4, 0, 0, 0, // mov dword ptr [eax], 4 (left)
            0xc7, 0x40, 0x04, 5, 0, 0, 0, // mov dword ptr [eax+4], 5 (top)
            0xc7, 0x40, 0x08, 0x7a, 0x02, 0, 0, // mov dword ptr [eax+8], 634 (right)
            0xc7, 0x40, 0x0c, 0xdb, 0x01, 0, 0, // mov dword ptr [eax+12], 475 (bottom)
            0xc7, 0x05, // mov dword ptr [SEEN_NCCALCSIZE], 1
        ];
        wndproc.extend_from_slice(&(SEEN_NCCALCSIZE as u32).to_le_bytes());
        wndproc.extend_from_slice(&1u32.to_le_bytes());
        let return_zero = wndproc.len();
        wndproc.extend_from_slice(&[0x31, 0xc0, 0xc2, 0x10, 0x00]); // xor eax,eax; ret 16
        wndproc[10] = (return_zero - 11) as u8;
        p.write_bytes(WNDPROC, &wndproc).unwrap();

        let mut window = test_window(true, 0);
        window.wndproc = WNDPROC;
        window.style = WS_POPUP;
        p.extra.win.windows.insert(HWND as u64, window);
        p.extra.win.focus = 0;

        set_args(
            &mut p,
            &[
                HWND,
                u32::MAX, // HWND_TOPMOST
                0,
                0,
                0,
                0,
                0x23, // FRAMECHANGED | NOMOVE | NOSIZE
            ],
        );
        let result = lookup("SetWindowPos").unwrap()(&mut p);

        assert_eq!(result, Ok(1));
        let trace = String::from_utf8_lossy(&p.stderr);
        for message in [
            "msg=0x46", "msg=0x83", "msg=0x47", "msg=0x86", "msg=0x6", "msg=0x7",
        ] {
            assert!(trace.contains(message), "missing {message} in {trace}");
        }
        assert_eq!(p.rd32(SEEN_NCCALCSIZE).unwrap(), 1);
        assert_eq!(p.extra.win.focus, HWND as u64);
        let window = &p.extra.win.windows[&(HWND as u64)];
        assert_eq!((window.x, window.y, window.w, window.h), (0, 0, 640, 480));
        assert_eq!((window.client_w, window.client_h), (630, 470));
        assert_ne!(window.ex_style & 0x0000_0008, 0); // WS_EX_TOPMOST
    }

    #[test]
    fn def_window_proc_nccalcsize_applies_the_current_nonclient_frame() {
        const HWND: u64 = (HWND_BASE + 0x40) as u64;
        let mut p = test_process_with_args(&[]);
        let mut window = test_window(true, 0);
        window.style = WS_POPUP | 0x0004_0000; // WS_THICKFRAME
        p.extra.win.windows.insert(HWND, window);
        let rect = kernel32::alloc_block(&mut p, 16, true);
        write_rect(&mut p, rect, 0, 0, 640, 480).unwrap();

        assert_eq!(def_window_proc(&mut p, HWND, WM_NCCALCSIZE, 0, rect), Ok(0));
        assert_eq!(read_rect(&p, rect).unwrap(), (8, 8, 632, 472));
    }

    #[test]
    fn set_window_pos_showwindow_updates_visibility_and_paint_state() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        let mut p = test_process_with_args(&[]);
        p.extra
            .win
            .windows
            .insert(HWND as u64, test_window(false, 0));

        assert_eq!(
            call(
                &mut p,
                "SetWindowPos",
                &[HWND, 0, 0, 0, 0, 0, 0x57], // NOZORDER | NOACTIVATE | NOSIZE | NOMOVE | SHOWWINDOW
            ),
            1
        );

        let window = &p.extra.win.windows[&(HWND as u64)];
        assert!(window.visible);
        assert!(window.needs_paint);
    }

    #[test]
    fn move_window_repaint_argument_controls_paint_invalidation() {
        const HWND: u32 = (HWND_BASE + 0x40) as u32;
        let mut p = test_process_with_args(&[]);
        p.extra
            .win
            .windows
            .insert(HWND as u64, test_window(true, 0));

        assert_eq!(call(&mut p, "MoveWindow", &[HWND, 10, 20, 700, 500, 0]), 1);
        assert!(!p.extra.win.windows[&(HWND as u64)].needs_paint);

        assert_eq!(call(&mut p, "MoveWindow", &[HWND, 20, 30, 710, 510, 1]), 1);
        let window = &p.extra.win.windows[&(HWND as u64)];
        assert_eq!((window.x, window.y, window.w, window.h), (20, 30, 710, 510));
        assert!(window.needs_paint);
    }

    fn write_test_msg(
        p: &mut Process,
        out: u64,
        hwnd: u32,
        message: u32,
        wparam: u32,
        lparam: u32,
    ) {
        p.wr32(out, hwnd).unwrap();
        p.wr32(out + 4, message).unwrap();
        p.wr32(out + 8, wparam).unwrap();
        p.wr32(out + 12, lparam).unwrap();
        p.wr32(out + 16, 0).unwrap();
        p.wr32(out + 20, 0).unwrap();
        p.wr32(out + 24, 0).unwrap();
    }

    #[test]
    fn system_parameters_info_cursor_shadow_requires_a_supplied_host_value() {
        let mut p = test_process_with_args(&[]);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(output, 0xaabb_ccdd).unwrap();
        let handler = lookup("SystemParametersInfoA")
            .expect("SPI_GETCURSORSHADOW must have an explicit translator");
        set_args(&mut p, &[0x101a, 0, output as u32, 0]);

        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("SystemParametersInfoA")
        ));
        assert_eq!(p.rd32(output).unwrap(), 0xaabb_ccdd);
    }

    #[test]
    fn system_parameters_info_a_and_w_return_the_supplied_cursor_shadow_value() {
        for (api, enabled) in [
            ("SystemParametersInfoA", true),
            ("SystemParametersInfoW", false),
        ] {
            let mut p = test_process_with_args(&[]);
            let output = p.alloc_virtual(0x1000, PROT_RW);
            p.system_parameters.cursor_shadow = Some(enabled);
            let handler = lookup(api).unwrap();
            set_args(&mut p, &[SPI_GETCURSORSHADOW, 0, output as u32, 0]);

            assert_eq!(handler(&mut p), Ok(1), "{api}");
            assert_eq!(p.rd32(output).unwrap(), enabled as u32, "{api}");
        }
    }

    #[test]
    fn system_parameters_info_set_cursor_shadow_rejects_null_and_exposes_untranslated_setting() {
        let mut p = test_process_with_args(&[]);
        let handler = lookup("SystemParametersInfoA").unwrap();
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        set_args(&mut p, &[0x101b, 0, 0, 0]);
        assert_eq!(handler(&mut p).unwrap(), 0);
        assert_eq!(
            crate::hle::kernel32::lookup("GetLastError").unwrap()(&mut p).unwrap(),
            87
        ); // ERROR_INVALID_PARAMETER: pvParam must point to BOOL.

        let input = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(input, 0).unwrap();
        set_args(&mut p, &[0x101b, 0, input as u32, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("SPI_SETCURSORSHADOW")
        ));
    }

    #[test]
    fn system_parameters_info_set_mouse_trails_only_succeeds_when_host_already_matches() {
        let mut p = test_process_with_args(&[]);
        let handler = lookup("SystemParametersInfoA").unwrap();
        p.system_parameters.mouse_trails = Some(0);
        set_args(&mut p, &[0x005d, 0, 0, 0]);
        assert_eq!(handler(&mut p).unwrap(), 1);

        set_args(&mut p, &[0x005d, 0, 0, 1]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("fWinIni")
        ));

        set_args(&mut p, &[0x005d, 4, 0, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("SPI_SETMOUSETRAILS")
        ));

        p.system_parameters.mouse_trails = None;
        set_args(&mut p, &[0x005d, 0, 0, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("host value unavailable")
        ));
    }

    #[test]
    fn system_parameters_info_returns_the_supplied_mouse_trail_count() {
        let mut p = test_process_with_args(&[]);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        p.system_parameters.mouse_trails = Some(4);
        let handler = lookup("SystemParametersInfoA").unwrap();
        set_args(&mut p, &[0x005e, 0, output as u32, 0]);

        assert_eq!(handler(&mut p), Ok(1));
        assert_eq!(p.rd32(output).unwrap(), 4);
    }

    #[test]
    fn system_parameters_info_returns_the_supplied_mouse_sonar_setting() {
        let mut p = test_process_with_args(&[]);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        p.system_parameters.mouse_sonar = Some(true);
        let handler = lookup("SystemParametersInfoA").unwrap();
        set_args(&mut p, &[0x101c, 0, output as u32, 0]);

        assert_eq!(handler(&mut p), Ok(1));
        assert_eq!(p.rd32(output).unwrap(), 1);
    }

    #[test]
    fn system_parameters_info_set_mouse_sonar_only_succeeds_when_host_already_matches() {
        let mut p = test_process_with_args(&[]);
        let handler = lookup("SystemParametersInfoA").unwrap();
        p.system_parameters.mouse_sonar = Some(false);

        // SPI_SETMOUSESONAR encodes its BOOL directly in pvParam; zero is a valid FALSE.
        set_args(&mut p, &[0x101d, 0, 0, 0]);
        assert_eq!(handler(&mut p).unwrap(), 1);

        set_args(&mut p, &[0x101d, 0, 0, 2]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("fWinIni")
        ));

        p.system_parameters.mouse_sonar = Some(true);
        set_args(&mut p, &[0x101d, 0, 1, 0]);
        assert_eq!(handler(&mut p).unwrap(), 1);

        set_args(&mut p, &[0x101d, 0, 0, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("SPI_SETMOUSESONAR")
        ));

        p.system_parameters.mouse_sonar = None;
        set_args(&mut p, &[0x101d, 0, 0, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("host value unavailable")
        ));
    }

    #[test]
    fn system_parameters_info_get_work_area_matches_the_virtual_desktop() {
        let mut p = test_process_with_args(&[]);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        let handler = lookup("SystemParametersInfoA").unwrap();
        set_args(&mut p, &[0x0030, 0, output as u32, 0]);

        assert_eq!(handler(&mut p), Ok(1));
        assert_eq!(
            [
                p.rd32(output).unwrap(),
                p.rd32(output + 4).unwrap(),
                p.rd32(output + 8).unwrap(),
                p.rd32(output + 12).unwrap(),
            ],
            [0, 0, 1920, 1080]
        );
    }

    #[test]
    fn system_parameters_info_returns_the_supplied_wheel_scroll_line_count() {
        let mut p = test_process_with_args(&[]);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        p.system_parameters.wheel_scroll_lines = Some(3);
        let handler = lookup("SystemParametersInfoA").unwrap();
        set_args(&mut p, &[0x0068, 0, output as u32, 0]);

        assert_eq!(handler(&mut p), Ok(1));
        assert_eq!(p.rd32(output).unwrap(), 3);
    }

    #[test]
    fn set_foreground_lock_timeout_rejects_the_games_null_pv_param() {
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        let handler = lookup("SystemParametersInfoA").unwrap();
        set_args(&mut p, &[SPI_SETFOREGROUNDLOCKTIMEOUT, 0, 0, 2]);

        assert_eq!(handler(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 87);
    }

    #[test]
    fn system_parameters_info_invalid_arguments_fail_and_other_actions_stay_visible() {
        let mut p = test_process_with_args(&[]);
        let handler = lookup("SystemParametersInfoA").unwrap();
        let output = p.alloc_virtual(0x1000, PROT_RW);
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        p.system_parameters.cursor_shadow = Some(true);

        set_args(&mut p, &[SPI_GETCURSORSHADOW, 0, 0, 0]);
        assert_eq!(handler(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 87);

        p.wr32(output, 0xaabb_ccdd).unwrap();
        set_args(&mut p, &[0x1004, 0, output as u32, 0]);
        assert!(matches!(
            handler(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("uiAction=0x1004")
        ));
        assert_eq!(p.rd32(output).unwrap(), 0xaabb_ccdd);
    }

    #[test]
    fn get_ancestor_follows_parent_and_owner_chains_for_each_ga_flag() {
        const OWNER: u64 = 0x10020;
        const POPUP: u64 = 0x10030;
        const CHILD: u64 = 0x10040;
        let mut p = test_process_with_args(&[]);
        p.extra.win.windows.insert(OWNER, test_window(false, 0));
        let mut popup = test_window(false, OWNER);
        popup.style = WS_POPUP;
        p.extra.win.windows.insert(POPUP, popup);
        let mut child = test_window(false, POPUP);
        child.style = WS_CHILD;
        p.extra.win.windows.insert(CHILD, child);

        assert_eq!(call(&mut p, "GetAncestor", &[CHILD as u32, 1]), POPUP);
        assert_eq!(call(&mut p, "GetAncestor", &[CHILD as u32, 2]), POPUP);
        assert_eq!(call(&mut p, "GetAncestor", &[CHILD as u32, 3]), OWNER);
        assert_eq!(call(&mut p, "GetAncestor", &[POPUP as u32, 1]), 0);
        assert_eq!(call(&mut p, "GetAncestor", &[POPUP as u32, 2]), POPUP);
        assert_eq!(call(&mut p, "GetAncestor", &[POPUP as u32, 3]), OWNER);
        assert_eq!(call(&mut p, "GetAncestor", &[HWND_DESKTOP as u32, 3]), 0);
        assert_eq!(call(&mut p, "GetAncestor", &[0xdead_beef, 3]), 0);
    }

    #[test]
    fn is_zoomed_tracks_window_maximize_and_restore_state() {
        const HWND: u64 = 0x10020;
        let mut p = test_process_with_args(&[]);
        p.extra.win.windows.insert(HWND, test_window(false, 0));

        assert_eq!(call(&mut p, "IsZoomed", &[HWND as u32]), 0);
        assert_eq!(call(&mut p, "ShowWindow", &[HWND as u32, 3]), 0);
        assert_eq!(call(&mut p, "IsZoomed", &[HWND as u32]), 1);
        assert_eq!(call(&mut p, "ShowWindow", &[HWND as u32, 9]), 1);
        assert_eq!(call(&mut p, "IsZoomed", &[HWND as u32]), 0);
    }

    #[test]
    fn is_iconic_tracks_window_minimize_and_normal_show_commands() {
        const HWND: u64 = 0x10020;
        let mut p = test_process_with_args(&[]);
        p.extra.win.windows.insert(HWND, test_window(false, 0));

        assert_eq!(call(&mut p, "IsIconic", &[HWND as u32]), 0);
        assert_eq!(call(&mut p, "ShowWindow", &[HWND as u32, 2]), 0);
        assert_eq!(call(&mut p, "IsIconic", &[HWND as u32]), 1);
        assert_eq!(call(&mut p, "ShowWindow", &[HWND as u32, 1]), 1);
        assert_eq!(call(&mut p, "IsIconic", &[HWND as u32]), 0);
    }

    #[test]
    fn get_menu_returns_top_level_menu_and_exposes_undefined_or_invalid_handles() {
        const WINDOW_WITH_MENU: u64 = 0x10020;
        const WINDOW_WITHOUT_MENU: u64 = 0x10030;
        const CHILD_WINDOW: u64 = 0x10040;
        let mut p = test_process_with_args(&[]);
        let mut with_menu = test_window(false, 0);
        with_menu.menu = 0x300;
        with_menu.has_menu = true;
        p.extra.win.windows.insert(WINDOW_WITH_MENU, with_menu);
        p.extra
            .win
            .windows
            .insert(WINDOW_WITHOUT_MENU, test_window(false, 0));
        let mut child = test_window(false, WINDOW_WITH_MENU);
        child.style = WS_CHILD;
        p.extra.win.windows.insert(CHILD_WINDOW, child);

        assert_eq!(call(&mut p, "GetMenu", &[WINDOW_WITH_MENU as u32]), 0x300);
        assert_eq!(call(&mut p, "GetMenu", &[WINDOW_WITHOUT_MENU as u32]), 0);
        assert_eq!(call(&mut p, "GetMenu", &[HWND_DESKTOP as u32]), 0);
        set_args(&mut p, &[CHILD_WINDOW as u32]);
        assert!(matches!(
            lookup("GetMenu").unwrap()(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("undefined")
        ));
        set_args(&mut p, &[0xdead_beef]);
        assert!(matches!(
            lookup("GetMenu").unwrap()(&mut p),
            Err(Stop::Unimplemented { dll, name })
                if dll == "user32" && name.contains("invalid HWND")
        ));
    }

    #[test]
    fn create_window_ex_retains_top_level_menu_but_treats_child_menu_as_control_id() {
        let mut p = test_process_with_args(&[]);
        let class_name = p.alloc_virtual(0x1000, PROT_RW);
        p.write_cstr(class_name, "TEST_WINDOW").unwrap();
        const WNDPROC: u64 = 0x0040_0000;
        const CAPTURED_MENU: u64 = 0x0040_1000;
        assert!(p.mem.map(WNDPROC, 0x2000, PROT_RWX));
        let mut wndproc = vec![
            0x8b, 0x44, 0x24, 0x08, // mov eax, [esp+8] (message)
            0x83, 0xf8, 0x01, // cmp eax, WM_CREATE
            0x74, 0x05, // je inspect CREATESTRUCT
            0x83, 0xf8, 0x81, // cmp eax, WM_NCCREATE
            0x75, 0x11, // jne return success
            0x8b, 0x44, 0x24, 0x10, // mov eax, [esp+16] (lParam)
            0x8b, 0x40, 0x08, // mov eax, [eax+8] (x86 CREATESTRUCT.hMenu)
            0xa3, // mov [CAPTURED_MENU], eax
        ];
        wndproc.extend_from_slice(&(CAPTURED_MENU as u32).to_le_bytes());
        wndproc.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc2, 0x10, 0]); // return TRUE, pop 4 args
        p.write_bytes(WNDPROC, &wndproc).unwrap();
        p.extra.win.classes.insert(
            "test_window".into(),
            Class {
                wndproc: WNDPROC,
                name: "TEST_WINDOW".into(),
                unicode: false,
            },
        );

        fn create_window_with_callback_stack(p: &mut Process, args: &[u32]) -> u64 {
            let stack = p.alloc_virtual(0x1000, PROT_RW);
            let sp = stack + 0x800;
            p.cpu.gpr[RSP] = sp;
            p.wr32(sp, 0xfeed_face).unwrap();
            for (index, arg) in args.iter().copied().enumerate() {
                p.wr32(sp + 4 + index as u64 * 4, arg).unwrap();
            }
            lookup("CreateWindowExA").unwrap()(p).unwrap()
        }

        let parent = create_window_with_callback_stack(
            &mut p,
            &[0, class_name as u32, 0, 0, 0, 0, 100, 100, 0, 0x300, 0, 0],
        );
        assert_ne!(parent, 0);
        assert_eq!(call(&mut p, "GetMenu", &[parent as u32]), 0x300);
        assert_eq!(p.rd32(CAPTURED_MENU).unwrap(), 0x300);

        let child = create_window_with_callback_stack(
            &mut p,
            &[
                0,
                class_name as u32,
                0,
                WS_CHILD,
                0,
                0,
                20,
                20,
                parent as u32,
                7,
                0,
                0,
            ],
        );
        assert_ne!(child, 0);
        let child_window = p.extra.win.windows.get(&child).unwrap();
        assert_eq!(child_window.control_id, 7);
        assert_eq!(child_window.menu, 0);
        assert!(!child_window.has_menu);
        assert_eq!(p.rd32(CAPTURED_MENU).unwrap(), 7);
    }

    #[test]
    fn multimedia_timer_period_requests_balance_per_resolution() {
        let mut p = test_process_with_args(&[]);
        let begin = lookup_winmm("timeBeginPeriod").expect("timeBeginPeriod should resolve");
        let end = lookup_winmm("timeEndPeriod").expect("timeEndPeriod should resolve");

        for expected_count in [1, 2] {
            set_args(&mut p, &[10]);
            assert_eq!(begin(&mut p), Ok(TIMERR_NOERROR as u64));
            assert_eq!(
                p.extra.win.timer_period_requests.get(&10),
                Some(&(expected_count as u64))
            );
        }
        for period in [TIME_PERIOD_MIN_MS, TIME_PERIOD_MAX_MS] {
            set_args(&mut p, &[period]);
            assert_eq!(begin(&mut p), Ok(TIMERR_NOERROR as u64));
        }

        for invalid_period in [0, TIME_PERIOD_MAX_MS + 1] {
            set_args(&mut p, &[invalid_period]);
            assert_eq!(begin(&mut p), Ok(TIMERR_NOCANDO as u64));
            assert_eq!(end(&mut p), Ok(TIMERR_NOCANDO as u64));
        }
        set_args(&mut p, &[11]);
        assert_eq!(end(&mut p), Ok(TIMERR_NOCANDO as u64));

        for expected_count in [1, 0] {
            set_args(&mut p, &[10]);
            assert_eq!(end(&mut p), Ok(TIMERR_NOERROR as u64));
            assert_eq!(
                p.extra
                    .win
                    .timer_period_requests
                    .get(&10)
                    .copied()
                    .unwrap_or(0),
                expected_count
            );
        }
        set_args(&mut p, &[10]);
        assert_eq!(end(&mut p), Ok(TIMERR_NOCANDO as u64));
        for period in [TIME_PERIOD_MIN_MS, TIME_PERIOD_MAX_MS] {
            set_args(&mut p, &[period]);
            assert_eq!(end(&mut p), Ok(TIMERR_NOERROR as u64));
        }
        assert!(p.extra.win.timer_period_requests.is_empty());
    }

    #[test]
    fn multimedia_timer_period_imports_use_generated_x86_stdcall_cleanup() {
        use anywasm_cpu::cpu::RAX;

        const CODE: u64 = 0x0004_0000;
        let mut p = Process::new(crate::vfs::Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(CODE, 0x1000, PROT_RWX));
        let stack = p.alloc_virtual(0x1000, PROT_RW);
        let initial_sp = stack + 0x800;
        let begin_thunk = p
            .thunk_for("winmm", "timeBeginPeriod", false)
            .expect("timeBeginPeriod import thunk");
        let end_thunk = p
            .thunk_for("winmm", "timeEndPeriod", false)
            .expect("timeEndPeriod import thunk");
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("winmm", "timeBeginPeriod"),
            Some(4)
        );
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("winmm", "timeEndPeriod"),
            Some(4)
        );

        let mut code = Vec::new();
        for thunk in [begin_thunk, end_thunk] {
            code.extend_from_slice(&[0x6a, 0x0a]); // push 10 ms
            code.push(0xb8); // mov eax, thunk
            code.extend_from_slice(&(thunk as u32).to_le_bytes());
            code.extend_from_slice(&[0xff, 0xd0]); // call eax
        }
        code.push(0xcc); // stop after both imports return
        p.write_bytes(CODE, &code).unwrap();
        p.cpu.rip = CODE;
        p.cpu.gpr[RSP] = initial_sp;

        assert!(matches!(p.run(32), Stop::Crash(_)));
        assert_eq!(p.cpu.gpr[RSP], initial_sp);
        assert_eq!(p.cpu.gpr[RAX] as u32, TIMERR_NOERROR);
        assert!(p.extra.win.timer_period_requests.is_empty());
    }

    #[test]
    fn cursor_apis_update_cursor_state_and_unsupported_stubs_are_absent() {
        assert!(lookup("IsIconic").is_some());
        assert!(lookup("IsZoomed").is_some());
        assert!(lookup("CallNextHookEx").is_none());
        assert!(lookup("DeleteDC").is_none());
        assert!(lookup("BringWindowToTop").is_none());
        assert!(lookup("EnableWindow").is_none());
        assert!(lookup("SetLayeredWindowAttributes").is_none());
        assert!(lookup("SetProcessDPIAware").is_none());
        assert!(lookup("SetProcessDpiAwarenessContext").is_none());
        assert!(lookup("CopyIcon").is_none());
        assert!(lookup("SetTimer").is_some());
        assert!(lookup("KillTimer").is_some());
        assert!(lookup_winmm("timeBeginPeriod").is_some());
        assert!(lookup_winmm("timeEndPeriod").is_some());

        let mut p = test_process_with_args(&[0x6000]);
        assert_eq!(
            call(&mut p, "LoadCursorW", &[0, 32512]),
            0x6000_0000 | 32512
        );
        assert_eq!(call(&mut p, "LoadCursorW", &[1, 32512]), 0);
        assert_eq!(call(&mut p, "SetCursor", &[0x6000]), 0);
        assert_eq!(call(&mut p, "SetCursor", &[0]), 0x6000);
        assert_eq!(call(&mut p, "ShowCursor", &[0]), u64::MAX);
        assert_eq!(call(&mut p, "ShowCursor", &[1]), 0);

        let clip = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(clip, 5).unwrap();
        p.wr32(clip + 4, 5).unwrap();
        p.wr32(clip + 8, 15).unwrap();
        p.wr32(clip + 12, 15).unwrap();
        assert_eq!(call(&mut p, "ClipCursor", &[clip as u32]), 1);
        assert_eq!(call(&mut p, "SetCursorPos", &[100, (-3i32) as u32]), 1);
        let point = p.alloc_virtual(0x1000, PROT_RW);
        assert_eq!(call(&mut p, "GetCursorPos", &[point as u32]), 1);
        assert_eq!(p.rd32(point).unwrap() as i32, 14);
        assert_eq!(p.rd32(point + 4).unwrap() as i32, 5);
        assert_eq!(call(&mut p, "ClipCursor", &[0]), 1);
        assert_eq!(call(&mut p, "SetCursorPos", &[100, (-3i32) as u32]), 1);
        assert_eq!(call(&mut p, "GetCursorPos", &[point as u32]), 1);
        assert_eq!(p.rd32(point).unwrap() as i32, 100);
        assert_eq!(p.rd32(point + 4).unwrap() as i32, -3);
    }

    #[test]
    fn load_string_w_copies_a_truncated_terminated_resource_string() {
        let (mut p, buffer) = process_with_string_resource();
        let load_string = lookup("LoadStringW").expect("LoadStringW must resolve");
        set_args(&mut p, &[0x0010_0000, 18, buffer as u32, 8]);
        assert_eq!(load_string(&mut p), Ok(7));
        assert_eq!(p.read_wstr(buffer).unwrap(), "Loading");
    }

    #[test]
    fn load_string_w_zero_capacity_returns_read_only_resource_text_pointer() {
        let (mut p, buffer) = process_with_string_resource();
        let load_string = lookup("LoadStringW").expect("LoadStringW must resolve");
        set_args(&mut p, &[0x0010_0000, 18, buffer as u32, 0]);
        let length = "Loading café".encode_utf16().count() as u64;
        assert_eq!(load_string(&mut p), Ok(length));
        let text = p.rdptr(buffer).unwrap();
        let units: Vec<u16> = (0..length)
            .map(|index| p.rd16(text + index * 2).unwrap())
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "Loading café");
    }

    #[test]
    fn missing_string_resource_returns_zero_without_touching_the_buffer() {
        let (mut p, buffer) = process_with_string_resource();
        let load_string = lookup("LoadStringW").expect("LoadStringW must resolve");
        p.write_wstr(buffer, "keep this").unwrap();
        let before = p.read_bytes(buffer, 20).unwrap();
        set_args(&mut p, &[0x0010_0000, 1000, buffer as u32, 12]);

        assert_eq!(load_string(&mut p), Ok(0));
        assert_eq!(p.read_bytes(buffer, 20).unwrap(), before);
    }

    #[test]
    fn translate_message_posts_character_messages_for_keydowns() {
        let mut p = test_process_with_args(&[]);
        assert!(lookup("TranslateMessage").is_some());
        p.extra.win.keys.resize(256, 0);
        let msg = p.alloc_virtual(0x1000, PROT_RW);

        write_test_msg(
            &mut p,
            msg,
            HWND_DESKTOP as u32,
            0x0100,
            b'A' as u32,
            0x001e_0001,
        );
        assert_eq!(call(&mut p, "TranslateMessage", &[msg as u32]), 1);
        let translated = p.extra.win.queue.pop_front().unwrap();
        assert_eq!(translated.hwnd, HWND_DESKTOP);
        assert_eq!(translated.msg, 0x0102);
        assert_eq!(translated.wparam, b'a' as u64);
        assert_eq!(translated.lparam, 0x001e_0001);

        // Alt system keydowns produce WM_SYSCHAR; the system-key context is already
        // reflected in WinState by the input path before this message is translated.
        p.extra.win.keys[0x12] = 1;
        write_test_msg(
            &mut p,
            msg,
            HWND_DESKTOP as u32,
            0x0104,
            b'B' as u32,
            0x2030_0001,
        );
        assert_eq!(call(&mut p, "TranslateMessage", &[msg as u32]), 1);
        let translated = p.extra.win.queue.pop_front().unwrap();
        assert_eq!(translated.hwnd, HWND_DESKTOP);
        assert_eq!(translated.msg, 0x0106);
        assert_eq!(translated.wparam, b'b' as u64);
        assert_eq!(translated.lparam, 0x2030_0001);
    }

    #[test]
    fn translate_message_ignores_non_key_and_non_character_messages() {
        let mut p = test_process_with_args(&[]);
        assert!(lookup("TranslateMessage").is_some());
        let msg = p.alloc_virtual(0x1000, PROT_RW);

        write_test_msg(&mut p, msg, HWND_DESKTOP as u32, WM_MOUSEMOVE as u32, 0, 0);
        assert_eq!(call(&mut p, "TranslateMessage", &[msg as u32]), 0);
        assert!(p.extra.win.queue.is_empty());

        write_test_msg(&mut p, msg, HWND_DESKTOP as u32, WM_KEYDOWN as u32, 0x70, 0);
        assert_eq!(call(&mut p, "TranslateMessage", &[msg as u32]), 0);
        assert!(p.extra.win.queue.is_empty());
    }

    #[test]
    fn window_geometry_and_dc_apis_report_real_state() {
        let mut p = test_process_with_args(&[]);
        let hwnd = HWND_BASE + 0x40;
        p.extra.win.windows.insert(hwnd, test_window(false, 0));
        p.extra.win.windows.get_mut(&hwnd).unwrap().x = 40;
        p.extra.win.windows.get_mut(&hwnd).unwrap().y = 20;
        assert_eq!(call(&mut p, "GetDpiForWindow", &[hwnd as u32]), 96);
        assert_eq!(call(&mut p, "GetDpiForWindow", &[0x1234]), 0);

        assert_eq!(call(&mut p, "GetDC", &[0x1234]), 0);
        let hdc = call(&mut p, "GetDC", &[hwnd as u32]);
        assert_eq!(hdc, 0x7000_0000 + hwnd);
        assert_eq!(call(&mut p, "ReleaseDC", &[hwnd as u32, hdc as u32]), 1);
        assert_eq!(call(&mut p, "ReleaseDC", &[hwnd as u32, hdc as u32]), 0);

        let paint = p.alloc_virtual(0x1000, PROT_RW);
        let hdc = call(&mut p, "BeginPaint", &[hwnd as u32, paint as u32]);
        assert_eq!(hdc, 0x7000_0000 + hwnd);
        assert_eq!(call(&mut p, "EndPaint", &[hwnd as u32, paint as u32]), 1);
        assert_eq!(call(&mut p, "EndPaint", &[hwnd as u32, paint as u32]), 0);

        let point = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(point, 5).unwrap();
        p.wr32(point + 4, 6).unwrap();
        assert_eq!(
            call(&mut p, "ClientToScreen", &[hwnd as u32, point as u32]),
            1
        );
        assert_eq!(
            (p.rd32(point).unwrap(), p.rd32(point + 4).unwrap()),
            (45, 26)
        );
        assert_eq!(
            call(&mut p, "ScreenToClient", &[hwnd as u32, point as u32]),
            1
        );
        assert_eq!((p.rd32(point).unwrap(), p.rd32(point + 4).unwrap()), (5, 6));

        assert_eq!(call(&mut p, "ShowWindow", &[hwnd as u32, 1]), 0);
        assert_eq!(call(&mut p, "ShowWindow", &[hwnd as u32, 0]), 1);
    }

    #[test]
    fn class_apis_report_real_lifetimes() {
        let mut p = test_process_with_args(&[]);
        p.extra.win.classes.insert(
            "test".into(),
            Class {
                wndproc: 0,
                name: "test".into(),
                unicode: false,
            },
        );
        let class_name = p.alloc_virtual(0x1000, PROT_RW);
        p.write_cstr(class_name, "TEST").unwrap();
        assert_eq!(call(&mut p, "UnregisterClassA", &[class_name as u32, 0]), 1);
        assert!(!p.extra.win.classes.contains_key("test"));
        assert_eq!(call(&mut p, "UnregisterClassA", &[class_name as u32, 0]), 0);
    }

    #[test]
    fn browser_input_targets_visible_game_window_after_hidden_glfw_helper() {
        let helper = HWND_BASE;
        let game = HWND_BASE + 0x10;
        let mut win = WinState::default();
        win.windows.insert(helper, test_window(false, 0));
        win.windows.insert(game, test_window(true, 0));
        win.main_window = helper;
        win.focus = helper;

        assert_eq!(input_target_window(&win), game);
    }

    #[test]
    fn fetch_writes_win32_msg_with_the_x86_layout() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        assert!(p.mem.map(0x10000, 0x1000, PROT_RW));
        p.extra.win.queue.push_back(Msg {
            hwnd: 0x1122_3344,
            msg: WM_LBUTTONDOWN,
            wparam: 0xaabb_ccdd,
            lparam: 0x5566_7788,
            time: 0x1020_3040,
            pt: (-2, 3),
        });

        assert!(fetch(&mut p, 0x10000, 0, true).unwrap());
        assert_eq!(p.rd32(0x10000).unwrap(), 0x1122_3344);
        assert_eq!(p.rd32(0x10004).unwrap(), WM_LBUTTONDOWN as u32);
        assert_eq!(p.rd32(0x10008).unwrap(), 0xaabb_ccdd);
        assert_eq!(p.rd32(0x1000c).unwrap(), 0x5566_7788);
        assert_eq!(p.rd32(0x10010).unwrap(), 0x1020_3040);
        assert_eq!(p.rd32(0x10014).unwrap(), (-2i32) as u32);
        assert_eq!(p.rd32(0x10018).unwrap(), 3);
        assert_eq!(read_message_type(&p, 0x10000).unwrap(), WM_LBUTTONDOWN);
        assert_eq!(
            read_message_fields(&p, 0x10000).unwrap(),
            (0x1122_3344, WM_LBUTTONDOWN, 0xaabb_ccdd, 0x5566_7788)
        );
    }

    #[test]
    fn set_timer_replaces_by_window_and_id_and_emits_periodic_wm_timer_messages() {
        let mut p = test_process_with_args(&[]);
        let hwnd = HWND_BASE + 0x40;
        p.extra.win.windows.insert(hwnd, test_window(true, 0));

        let generated_id = call(&mut p, "SetTimer", &[hwnd as u32, 0, 1, 0]);
        assert_ne!(generated_id, 0);
        assert_eq!(next_timer_delay_ms(&p), Some(10));

        let same_id = call(
            &mut p,
            "SetTimer",
            &[hwnd as u32, generated_id as u32, 500, 0],
        );
        assert_eq!(same_id, generated_id);
        assert_eq!(p.extra.win.timers.len(), 1);
        assert_eq!(next_timer_delay_ms(&p), Some(500));

        let out = p.alloc_virtual(0x1000, PROT_RW);
        p.advance_time_100ns(4_990_000); // 499 ms
        assert_eq!(next_timer_delay_ms(&p), Some(1));
        assert!(!fetch(&mut p, out, 0, true).unwrap());

        p.advance_time_100ns(10_000); // 1 ms
        assert!(fetch(&mut p, out, 0, true).unwrap());
        assert_eq!(
            read_message_fields(&p, out).unwrap(),
            (hwnd, WM_TIMER, generated_id, 0)
        );
        assert_eq!(read_message_time(&p, out).unwrap(), 500);
        assert_eq!(next_timer_delay_ms(&p), Some(500));

        p.advance_time_100ns(5_000_000); // next 500 ms period
        assert!(fetch(&mut p, out, hwnd, true).unwrap());
        assert_eq!(
            read_message_fields(&p, out).unwrap(),
            (hwnd, WM_TIMER, generated_id, 0)
        );
        assert_eq!(read_message_time(&p, out).unwrap(), 1000);
    }

    #[test]
    fn timer_peek_preserves_pending_message_and_kill_does_not_remove_it() {
        let mut p = test_process_with_args(&[]);
        let hwnd = HWND_BASE + 0x40;
        p.extra.win.windows.insert(hwnd, test_window(true, 0));
        let timer_id = call(&mut p, "SetTimer", &[hwnd as u32, 17, 10, 0]);
        assert_eq!(timer_id, 17);
        p.advance_time_100ns(100_000); // 10 ms
        let out = p.alloc_virtual(0x1000, PROT_RW);

        assert!(fetch(&mut p, out, hwnd, false).unwrap());
        assert_eq!(p.extra.win.queue.len(), 1);
        assert_eq!(
            call(&mut p, "KillTimer", &[hwnd as u32, timer_id as u32]),
            1
        );
        assert!(fetch(&mut p, out, hwnd, true).unwrap());
        assert_eq!(
            read_message_fields(&p, out).unwrap(),
            (hwnd, WM_TIMER, timer_id, 0)
        );
        assert_eq!(p.extra.win.queue.len(), 0);
        assert_eq!(next_timer_delay_ms(&p), None);
        assert!(!fetch(&mut p, out, hwnd, true).unwrap());
    }

    #[test]
    fn thread_timer_uses_a_generated_id_and_invalid_window_is_reported() {
        let mut p = test_process_with_args(&[]);
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        let thread_timer = call(&mut p, "SetTimer", &[0, 0x1234, 25, 0]);
        assert_ne!(thread_timer, 0);
        assert_ne!(thread_timer, 0x1234);
        assert_eq!(call(&mut p, "SetTimer", &[0x1234, 1, 25, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 1400);
        assert_eq!(call(&mut p, "KillTimer", &[0, thread_timer as u32]), 1);
        assert_eq!(next_timer_delay_ms(&p), None);
    }

    #[test]
    fn dispatch_message_calls_timerproc_with_current_get_tick_count() {
        let mut p = test_process_with_args(&[]);
        let callback = 0x0003_0000;
        let marker = 0x0003_1000;
        assert!(p.mem.map(callback, 0x2000, PROT_RWX));
        let mut code = vec![0x8b, 0x44, 0x24, 0x10, 0xa3]; // mov eax,[esp+16]; mov [marker],eax
        code.extend_from_slice(&(marker as u32).to_le_bytes());
        code.extend_from_slice(&[0xc2, 0x10, 0x00]); // ret 16 (TIMERPROC is CALLBACK/stdcall)
        p.write_bytes(callback, &code).unwrap();
        let message = p.alloc_virtual(0x1000, PROT_RW);
        write_test_msg(
            &mut p,
            message,
            HWND_BASE as u32,
            WM_TIMER as u32,
            23,
            callback as u32,
        );
        p.wr32(message + 16, 50).unwrap();
        p.advance_time_100ns(4_200_000); // GetTickCount is now 420 ms.

        let stack = p.alloc_virtual(0x1_0000, PROT_RW) + 0x8000;
        p.cpu.gpr[RSP] = stack;
        p.wr32(stack, 0xfeed_face).unwrap();
        p.wr32(stack + 4, message as u32).unwrap();
        assert_eq!(lookup("DispatchMessageA").unwrap()(&mut p), Ok(0));
        assert_eq!(p.rd32(marker).unwrap(), 420);
    }
}
