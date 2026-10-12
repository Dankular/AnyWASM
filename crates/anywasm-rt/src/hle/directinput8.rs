//! DirectInput 8 host interfaces backed by the runtime's browser input state.

use super::{com, kernel32, *};
use crate::process::{Process, Stop};

const S_OK: u64 = 0;
const S_FALSE: u64 = 1;
const E_NOTIMPL: u64 = 0x8000_4001;
const E_NOINTERFACE: u64 = 0x8000_4002;
const E_POINTER: u64 = 0x8000_4003;
const E_INVALIDARG: u64 = 0x8007_0057;
const DIERR_NOTACQUIRED: u64 = 0x8007_000c;
const DIERR_DEVICENOTREG: u64 = 0x8004_0154;
const DIERR_BETADIRECTINPUTVERSION: u64 = 0x8000_0300;

const DEVTYPE_MOUSE: u32 = 0x12;
const DEVTYPE_KEYBOARD: u32 = 0x13;
const GUID_SYS_MOUSE: &str = "6f1d2b60-d5a0-11cf-bfc7-444553540000";
const GUID_SYS_KEYBOARD: &str = "6f1d2b61-d5a0-11cf-bfc7-444553540000";
const IID_DIRECTINPUT8_A: &str = "bf798030-483a-4da2-aa99-5d64ed369700";
const IID_DIRECTINPUT8_W: &str = "bf798031-483a-4da2-aa99-5d64ed369700";
const IID_DIRECTINPUTDEVICE8_A: &str = "54d41080-dc15-4833-a41b-748f73a38179";
const IID_DIRECTINPUTDEVICE8_W: &str = "54d41081-dc15-4833-a41b-748f73a38179";

const DIRECTINPUT8_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "CreateDevice",
    "EnumDevices",
    "GetDeviceStatus",
    "RunControlPanel",
    "Initialize",
    "FindDevice",
    "EnumDevicesBySemantics",
    "ConfigureDevices",
];

const DIRECTINPUTDEVICE8_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "GetCapabilities",
    "EnumObjects",
    "GetProperty",
    "SetProperty",
    "Acquire",
    "Unacquire",
    "GetDeviceState",
    "GetDeviceData",
    "SetDataFormat",
    "SetEventNotification",
    "SetCooperativeLevel",
    "GetObjectInfo",
    "GetDeviceInfo",
    "RunControlPanel",
    "Initialize",
    "CreateEffect",
    "EnumEffects",
    "GetEffectInfo",
    "GetForceFeedbackState",
    "SendForceFeedbackCommand",
    "EnumCreatedEffectObjects",
    "Escape",
    "Poll",
    "SendDeviceData",
    "EnumEffectsInFile",
    "WriteEffectToFile",
    "BuildActionMap",
    "SetActionMap",
    "GetImageInfo",
];

pub fn lookup(name: &str) -> Option<HleFn> {
    (name == "DirectInput8Create").then_some(direct_input8_create)
}

pub fn methods(iface: &str) -> Option<&'static [&'static str]> {
    match iface {
        "IDirectInput8A" | "IDirectInput8W" => Some(DIRECTINPUT8_METHODS),
        "IDirectInputDevice8A" | "IDirectInputDevice8W" => Some(DIRECTINPUTDEVICE8_METHODS),
        _ => None,
    }
}

pub fn iid_name(iid: &str) -> Option<&'static str> {
    match iid {
        IID_DIRECTINPUT8_A => Some("IDirectInput8A"),
        IID_DIRECTINPUT8_W => Some("IDirectInput8W"),
        IID_DIRECTINPUTDEVICE8_A => Some("IDirectInputDevice8A"),
        IID_DIRECTINPUTDEVICE8_W => Some("IDirectInputDevice8W"),
        _ => None,
    }
}

/// COM methods use stdcall and include `this` in the x86 stack argument count.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    let (iface, method) = name.split_once("::")?;
    let args = match (iface, method) {
        ("IDirectInput8A" | "IDirectInput8W", "QueryInterface") => 3,
        ("IDirectInput8A" | "IDirectInput8W", "AddRef" | "Release") => 1,
        ("IDirectInput8A" | "IDirectInput8W", "CreateDevice") => 4,
        ("IDirectInput8A" | "IDirectInput8W", "EnumDevices") => 5,
        ("IDirectInput8A" | "IDirectInput8W", "GetDeviceStatus") => 2,
        ("IDirectInput8A" | "IDirectInput8W", "RunControlPanel") => 3,
        ("IDirectInput8A" | "IDirectInput8W", "Initialize") => 3,
        ("IDirectInput8A" | "IDirectInput8W", "FindDevice") => 4,
        ("IDirectInput8A" | "IDirectInput8W", "EnumDevicesBySemantics") => 6,
        ("IDirectInput8A" | "IDirectInput8W", "ConfigureDevices") => 5,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "QueryInterface") => 3,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "AddRef" | "Release" | "Acquire" | "Unacquire" | "Poll") => 1,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "GetCapabilities" | "SetDataFormat" | "SetEventNotification" | "GetDeviceInfo" | "GetForceFeedbackState" | "SendForceFeedbackCommand" | "Escape" | "GetImageInfo") => 2,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "GetProperty" | "SetProperty" | "GetDeviceState" | "SetCooperativeLevel" | "RunControlPanel" | "GetEffectInfo") => 3,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "EnumObjects" | "GetObjectInfo" | "EnumEffects" | "EnumCreatedEffectObjects" | "WriteEffectToFile" | "BuildActionMap" | "SetActionMap") => 4,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "GetDeviceData" | "CreateEffect" | "SendDeviceData" | "EnumEffectsInFile") => 5,
        ("IDirectInputDevice8A" | "IDirectInputDevice8W", "Initialize") => 4,
        _ => return None,
    };
    Some(args * 4)
}

pub fn method(iface: &str, method: &str) -> Option<HleFn> {
    let device_iface = matches!(iface, "IDirectInputDevice8A" | "IDirectInputDevice8W");
    if !device_iface && !matches!(iface, "IDirectInput8A" | "IDirectInput8W") {
        return None;
    }
    Some(match method {
        "CreateDevice" if !device_iface => create_device,
        "EnumDevices" if !device_iface => enum_devices,
        "GetDeviceStatus" if !device_iface => get_device_status,
        "RunControlPanel" => |_| Ok(S_OK),
        "Initialize" if !device_iface => initialize,
        "FindDevice" if !device_iface => find_device,
        "EnumDevicesBySemantics" | "ConfigureDevices" if !device_iface => |_| Ok(E_NOTIMPL),
        "GetCapabilities" if device_iface => get_capabilities,
        "EnumObjects" if device_iface => |_| Ok(S_OK),
        "GetProperty" if device_iface => get_property,
        "SetProperty" if device_iface => set_property,
        "Acquire" if device_iface => acquire,
        "Unacquire" if device_iface => unacquire,
        "GetDeviceState" if device_iface => get_device_state,
        "GetDeviceData" if device_iface => get_device_data,
        "SetDataFormat" if device_iface => set_data_format,
        "SetEventNotification" if device_iface => |_| Ok(S_OK),
        "SetCooperativeLevel" if device_iface => set_cooperative_level,
        "GetDeviceInfo" if device_iface => get_device_info,
        "GetForceFeedbackState" if device_iface => get_force_feedback_state,
        "SendForceFeedbackCommand" if device_iface => |_| Ok(S_OK),
        "Poll" if device_iface => poll,
        "SendDeviceData" if device_iface => send_device_data,
        "EnumEffects" | "EnumCreatedEffectObjects" | "EnumEffectsInFile" if device_iface => |_| Ok(S_OK),
        "BuildActionMap" | "SetActionMap" if device_iface => |_| Ok(S_OK),
        "GetImageInfo" if device_iface => |_| Ok(S_FALSE),
        "CreateEffect" | "GetEffectInfo" | "Escape" | "WriteEffectToFile" if device_iface => |_| Ok(E_NOTIMPL),
        _ => return None,
    })
}

fn guid(p: &Process, address: u64) -> Result<String, Stop> {
    let b = p.read_bytes(address, 16)?;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes(b[0..4].try_into().unwrap()),
        u16::from_le_bytes(b[4..6].try_into().unwrap()),
        u16::from_le_bytes(b[6..8].try_into().unwrap()),
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

fn direct_input8_create(p: &mut Process) -> R {
    let version = p.arg(1) as u32;
    let iid_ptr = p.arg(2);
    let out = p.arg(3);
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if version < 0x0800 {
        return Ok(DIERR_BETADIRECTINPUTVERSION);
    }
    let iid = guid(p, iid_ptr)?;
    let iface = match iid.as_str() {
        IID_DIRECTINPUT8_A => "IDirectInput8A",
        IID_DIRECTINPUT8_W => "IDirectInput8W",
        "00000000-0000-0000-c000-000000000046" => "IDirectInput8A",
        _ => return Ok(E_NOINTERFACE),
    };
    let object = com::create(p, iface, com::Kind::DirectInput8);
    p.wrptr(out, object)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!("[dinput8] DirectInput8Create version={version:#x} iid={iid} -> {object:#x}\n").as_bytes(),
        );
    }
    Ok(S_OK)
}

fn create_device(p: &mut Process) -> R {
    let (this, guid_ptr, out) = (p.arg(0), p.arg(1), p.arg(2));
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if !matches!(com::kind_of(p, this), Some(com::Kind::DirectInput8)) {
        return Ok(E_NOINTERFACE);
    }
    let requested = guid(p, guid_ptr)?;
    let device_type = match requested.as_str() {
        GUID_SYS_MOUSE => DEVTYPE_MOUSE,
        GUID_SYS_KEYBOARD => DEVTYPE_KEYBOARD,
        _ => {
            if p.trace_imports {
                p.stderr.extend_from_slice(
                    format!("[dinput8] CreateDevice unsupported GUID {requested}\n").as_bytes(),
                );
            }
            return Ok(DIERR_DEVICENOTREG);
        }
    };
    let iface = match p.extra.com.objs.get(&this).map(|o| o.iface) {
        Some("IDirectInput8W") => "IDirectInputDevice8W",
        _ => "IDirectInputDevice8A",
    };
    let object = com::create(
        p,
        iface,
        com::Kind::DirectInputDevice {
            device_type,
            acquired: false,
            data_size: if device_type == DEVTYPE_MOUSE { 16 } else { 256 },
        },
    );
    p.wrptr(out, object)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!("[dinput8] CreateDevice guid={requested} -> {object:#x}\n").as_bytes(),
        );
    }
    Ok(S_OK)
}

fn enum_devices(p: &mut Process) -> R {
    let (device_class, callback, context) =
        (p.arg(1) as u32, p.arg(2), p.arg(3));
    if callback == 0 {
        return Ok(E_INVALIDARG);
    }
    let instance = kernel32::alloc_block(p, 580, true);
    if instance == 0 {
        return Ok(com::E_OUTOFMEMORY);
    }
    let devices: &[u32] = match device_class {
        0 | 1 => &[DEVTYPE_MOUSE, DEVTYPE_KEYBOARD],
        2 => &[DEVTYPE_MOUSE],
        3 => &[DEVTYPE_KEYBOARD],
        _ => &[],
    };
    for &device_type in devices {
        write_device_instance(p, instance, device_type, false)?;
        if p.call_guest(callback, &[instance, context])? == 0 {
            break;
        }
    }
    kernel32::free_block(p, instance);
    Ok(S_OK)
}

fn get_device_status(p: &mut Process) -> R {
    match guid(p, p.arg(1))?.as_str() {
        GUID_SYS_MOUSE | GUID_SYS_KEYBOARD => Ok(S_OK),
        _ => Ok(DIERR_DEVICENOTREG),
    }
}

fn initialize(p: &mut Process) -> R {
    let version = p.arg(2) as u32;
    Ok(if version >= 0x0800 { S_OK } else { DIERR_BETADIRECTINPUTVERSION })
}

fn find_device(p: &mut Process) -> R {
    let requested = guid(p, p.arg(1))?;
    let guid_ptr = p.arg(1);
    let out = p.arg(3);
    if !matches!(requested.as_str(), GUID_SYS_MOUSE | GUID_SYS_KEYBOARD) {
        return Ok(DIERR_DEVICENOTREG);
    }
    if out != 0 {
        let bytes = p.read_bytes(guid_ptr, 16)?;
        p.write_bytes(out, &bytes)?;
        Ok(S_OK)
    } else {
        Ok(E_POINTER)
    }
}

fn device_properties(p: &Process, this: u64) -> Option<(u32, bool, u32)> {
    let o = p.extra.com.objs.get(&this)?;
    match &o.kind {
        com::Kind::DirectInputDevice { device_type, acquired, data_size } => {
            Some((*device_type, *acquired, *data_size))
        }
        _ => None,
    }
}

fn get_capabilities(p: &mut Process) -> R {
    let this = p.arg(0);
    let out = p.arg(1);
    let Some((device_type, _, _)) = device_properties(p, this) else {
        return Ok(E_NOINTERFACE);
    };
    if out == 0 {
        return Ok(E_POINTER);
    }
    let mut caps = [0u8; 44];
    caps[0..4].copy_from_slice(&44u32.to_le_bytes());
    caps[4..8].copy_from_slice(&1u32.to_le_bytes()); // attached
    caps[8..12].copy_from_slice(&device_type.to_le_bytes());
    let (axes, buttons) = if device_type == DEVTYPE_MOUSE { (3u32, 8u32) } else { (0, 256) };
    caps[12..16].copy_from_slice(&axes.to_le_bytes());
    caps[16..20].copy_from_slice(&buttons.to_le_bytes());
    p.write_bytes(out, &caps)?;
    Ok(S_OK)
}

fn set_property(p: &mut Process) -> R {
    let this = p.arg(0);
    Ok(if device_properties(p, this).is_some() { S_OK } else { E_NOINTERFACE })
}

fn get_property(p: &mut Process) -> R {
    let (this, property_guid, property) = (p.arg(0), p.arg(1), p.arg(2));
    if device_properties(p, this).is_none() {
        return Ok(E_NOINTERFACE);
    }
    if property == 0 {
        return Ok(E_POINTER);
    }
    let size = p.rd32(property)?;
    if size < 20 {
        return Ok(E_INVALIDARG);
    }
    let property_id = guid(p, property_guid)?;
    // DIPROP_AXISMODE is property ID 2; DirectInput defaults to relative axes.
    if property_id == "00000002-0000-0000-c000-000000000046" {
        p.wr32(property + 16, 1)?;
    } else if property_id == "00000001-0000-0000-c000-000000000046" {
        p.wr32(property + 16, 0)?;
    } else if property_id == "00000004-0000-0000-c000-000000000046" {
        p.wr32(property + 16, 1)?;
    }
    Ok(S_OK)
}

fn acquire(p: &mut Process) -> R {
    let this = p.arg(0);
    let Some(object) = p.extra.com.objs.get_mut(&this) else {
        return Ok(E_NOINTERFACE);
    };
    match &mut object.kind {
        com::Kind::DirectInputDevice { acquired, .. } => {
            *acquired = true;
            Ok(S_OK)
        }
        _ => Ok(E_NOINTERFACE),
    }
}

fn unacquire(p: &mut Process) -> R {
    let this = p.arg(0);
    let Some(object) = p.extra.com.objs.get_mut(&this) else {
        return Ok(E_NOINTERFACE);
    };
    match &mut object.kind {
        com::Kind::DirectInputDevice { acquired, .. } => {
            *acquired = false;
            Ok(S_OK)
        }
        _ => Ok(E_NOINTERFACE),
    }
}

fn set_data_format(p: &mut Process) -> R {
    let (this, format) = (p.arg(0), p.arg(1));
    if format == 0 {
        return Ok(E_POINTER);
    }
    let data_size = p.rd32(format + 12)?;
    if data_size == 0 || data_size > 4096 {
        return Ok(E_INVALIDARG);
    }
    let Some(object) = p.extra.com.objs.get_mut(&this) else {
        return Ok(E_NOINTERFACE);
    };
    match &mut object.kind {
        com::Kind::DirectInputDevice { data_size: current, .. } => {
            *current = data_size;
            Ok(S_OK)
        }
        _ => Ok(E_NOINTERFACE),
    }
}

fn get_device_state(p: &mut Process) -> R {
    let (this, requested_size, out) = (p.arg(0), p.arg(1) as usize, p.arg(2));
    let Some((device_type, acquired, data_size)) = device_properties(p, this) else {
        return Ok(E_NOINTERFACE);
    };
    if !acquired {
        return Ok(DIERR_NOTACQUIRED);
    }
    if out == 0 {
        return Ok(E_POINTER);
    }
    let required = data_size as usize;
    if requested_size < required {
        return Ok(E_INVALIDARG);
    }
    let mut state = vec![0u8; required];
    if device_type == DEVTYPE_MOUSE {
        let (dx, dy, wheel) = {
            let win = &mut p.extra.win;
            let delta = (win.mouse_dx, win.mouse_dy, win.mouse_wheel);
            win.mouse_dx = 0;
            win.mouse_dy = 0;
            win.mouse_wheel = 0;
            delta
        };
        state[0..4].copy_from_slice(&dx.to_le_bytes());
        state[4..8].copy_from_slice(&dy.to_le_bytes());
        state[8..12].copy_from_slice(&wheel.to_le_bytes());
        for (index, virtual_key) in [1usize, 2, 4, 5, 6, 7, 8, 9].into_iter().enumerate() {
            if 12 + index < state.len()
                && p.extra.win.keys.get(virtual_key).copied().unwrap_or(0) != 0
            {
                state[12 + index] = 0x80;
            }
        }
    } else {
        for virtual_key in 0..p.extra.win.keys.len().min(256) {
            if p.extra.win.keys[virtual_key] != 0 {
                if let Some(scan_code) = directinput_scan_code(virtual_key) {
                    if (scan_code as usize) < state.len() {
                        state[scan_code as usize] = 0x80;
                    }
                }
            }
        }
    }
    p.write_bytes(out, &state)?;
    Ok(S_OK)
}

fn get_device_data(p: &mut Process) -> R {
    let (this, count_out) = (p.arg(0), p.arg(3));
    let Some((_, acquired, _)) = device_properties(p, this) else {
        return Ok(E_NOINTERFACE);
    };
    if !acquired {
        return Ok(DIERR_NOTACQUIRED);
    }
    if count_out == 0 {
        return Ok(E_POINTER);
    }
    p.wr32(count_out, 0)?;
    Ok(S_OK)
}

fn set_cooperative_level(p: &mut Process) -> R {
    let this = p.arg(0);
    Ok(if device_properties(p, this).is_some() { S_OK } else { E_NOINTERFACE })
}

fn get_device_info(p: &mut Process) -> R {
    let (this, out) = (p.arg(0), p.arg(1));
    let Some((device_type, _, _)) = device_properties(p, this) else {
        return Ok(E_NOINTERFACE);
    };
    if out == 0 {
        return Ok(E_POINTER);
    }
    write_device_instance(p, out, device_type, false)?;
    Ok(S_OK)
}

fn get_force_feedback_state(p: &mut Process) -> R {
    let out = p.arg(1);
    if out == 0 {
        return Ok(E_POINTER);
    }
    p.wr32(out, 0)?;
    Ok(S_OK)
}

fn poll(p: &mut Process) -> R {
    Ok(if device_properties(p, p.arg(0)).is_some() { S_OK } else { E_NOINTERFACE })
}

fn send_device_data(p: &mut Process) -> R {
    let count_out = p.arg(3);
    if count_out != 0 {
        p.wr32(count_out, 0)?;
    }
    Ok(S_OK)
}

fn write_device_instance(p: &mut Process, address: u64, device_type: u32, wide: bool) -> Result<(), Stop> {
    let size = if wide { 1100u32 } else { 580u32 };
    let mut data = vec![0u8; size as usize];
    data[0..4].copy_from_slice(&size.to_le_bytes());
    let guid_bytes = if device_type == DEVTYPE_MOUSE {
        [0x60,0x2b,0x1d,0x6f,0xa0,0xd5,0xcf,0x11,0xbf,0xc7,0x44,0x45,0x53,0x54,0,0]
    } else {
        [0x61,0x2b,0x1d,0x6f,0xa0,0xd5,0xcf,0x11,0xbf,0xc7,0x44,0x45,0x53,0x54,0,0]
    };
    data[4..20].copy_from_slice(&guid_bytes);
    data[20..36].copy_from_slice(&guid_bytes);
    data[36..40].copy_from_slice(&device_type.to_le_bytes());
    let name = if device_type == DEVTYPE_MOUSE { "AnyWASM Browser Mouse" } else { "AnyWASM Browser Keyboard" };
    if wide {
        for (index, ch) in name.encode_utf16().enumerate() {
            let at = 40 + index * 2;
            data[at..at + 2].copy_from_slice(&ch.to_le_bytes());
            data[at + 520..at + 522].copy_from_slice(&ch.to_le_bytes());
        }
    } else {
        data[40..40 + name.len()].copy_from_slice(name.as_bytes());
        data[300..300 + name.len()].copy_from_slice(name.as_bytes());
    }
    p.write_bytes(address, &data)
}

fn directinput_scan_code(vk: usize) -> Option<u8> {
    let code = match vk {
        0x08 => 0x0e, 0x09 => 0x0f, 0x0d => 0x1c, 0x10 => 0x2a, 0x11 => 0x1d,
        0x12 => 0x38, 0x1b => 0x01, 0x20 => 0x39, 0x21 => 0xc9, 0x22 => 0xd1,
        0x23 => 0xcf, 0x24 => 0xcb, 0x25 => 0xcb, 0x26 => 0xc8, 0x27 => 0xcd,
        0x28 => 0xd0, 0x2d => 0xd2, 0x2e => 0xd3,
        0x30 => 0x0b, 0x31 => 0x02, 0x32 => 0x03, 0x33 => 0x04, 0x34 => 0x05,
        0x35 => 0x06, 0x36 => 0x07, 0x37 => 0x08, 0x38 => 0x09, 0x39 => 0x0a,
        0x41..=0x5a => {
            const LETTERS: [u8; 26] = [
                0x1e,0x30,0x2e,0x20,0x12,0x21,0x22,0x23,0x17,0x24,0x25,0x26,0x32,
                0x31,0x18,0x19,0x10,0x13,0x1f,0x14,0x16,0x2f,0x11,0x2d,0x15,0x2c,
            ];
            LETTERS[vk - 0x41]
        }
        0x60..=0x69 => [0x52,0x4f,0x50,0x51,0x4b,0x4c,0x4d,0x47,0x48,0x49][vk - 0x60],
        0x70..=0x7b => [0x3b,0x3c,0x3d,0x3e,0x3f,0x40,0x41,0x42,0x43,0x44,0x57,0x58][vk - 0x70],
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_property_rejects_an_invalid_device_handle() {
        let mut process = Process::new(crate::vfs::Vfs::new());
        let set_property = method("IDirectInputDevice8A", "SetProperty").unwrap();

        assert_eq!(set_property(&mut process).unwrap(), E_NOINTERFACE);
    }
}

