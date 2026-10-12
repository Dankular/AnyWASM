//! OLE initialization and basic COM activation status.

use super::*;
use crate::process::Process;

const S_OK: u64 = 0;
const REGDB_E_CLASSNOTREG: u64 = 0x8004_0154;
const CLASS_E_NOAGGREGATION: u64 = 0x8004_0110;
const E_NOINTERFACE: u64 = 0x8000_4002;
const E_POINTER: u64 = 0x8000_4003;
const CLSCTX_INPROC_SERVER: u32 = 0x1;

const CLSID_DIRECTSOUND8: [u8; 16] = [
    0x3f, 0xcc, 0x01, 0x39, 0xb5, 0x84, 0xa4, 0x4f, 0xba, 0x35, 0xaa, 0x81, 0x72, 0xb8, 0xa0, 0x9b,
];
const IID_IDIRECTSOUND8: [u8; 16] = [
    0x93, 0x7e, 0x0a, 0xc5, 0x95, 0xf3, 0x34, 0x48, 0x9e, 0xf6, 0x7f, 0xa9, 0x9d, 0xe5, 0x09, 0x66,
];

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "CoInitialize" | "CoInitializeEx" => |_| Ok(S_OK),
        "CoUninitialize" => |_| Ok(0),
        "CoCreateInstance" => co_create_instance,
        "CoGetClassObject" => co_get_class_object,
        _ => return None,
    })
}

fn co_create_instance(p: &mut Process) -> R {
    let (class_id, outer, context, iid, out) =
        (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(3), p.arg(4));
    if out == 0 || class_id == 0 || iid == 0 {
        return Ok(E_POINTER);
    }
    p.wrptr(out, 0)?;
    if outer != 0 {
        return Ok(CLASS_E_NOAGGREGATION);
    }
    if context & CLSCTX_INPROC_SERVER == 0 {
        return Ok(REGDB_E_CLASSNOTREG);
    }

    if p.read_bytes(class_id, 16)?.as_slice() != CLSID_DIRECTSOUND8 {
        return Ok(REGDB_E_CLASSNOTREG);
    }
    if p.read_bytes(iid, 16)?.as_slice() != IID_IDIRECTSOUND8 {
        return Ok(E_NOINTERFACE);
    }

    let object = com::create(p, "IDirectSound8", com::Kind::DirectSound);
    p.wrptr(out, object)?;
    Ok(S_OK)
}

fn co_get_class_object(p: &mut Process) -> R {
    let out = p.arg(4);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(REGDB_E_CLASSNOTREG)
}
