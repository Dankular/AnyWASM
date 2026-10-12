//! OLE initialization and basic COM activation status.

use super::*;
use crate::process::Process;

const S_OK: u64 = 0;
const REGDB_E_CLASSNOTREG: u64 = 0x8004_0154;

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
    let out = p.arg(4);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(REGDB_E_CLASSNOTREG)
}

fn co_get_class_object(p: &mut Process) -> R {
    let out = p.arg(4);
    if out != 0 {
        p.wrptr(out, 0)?;
    }
    Ok(REGDB_E_CLASSNOTREG)
}
