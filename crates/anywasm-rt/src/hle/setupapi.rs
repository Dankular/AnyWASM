//! SetupAPI device-interface enumeration against the runtime's empty virtual device catalogue.
//!
//! The guest receives a valid, process-local device-info-set handle for a valid query. Since this
//! runtime has no modeled physical devices or host device bridge, enumeration returns the real
//! `ERROR_NO_MORE_ITEMS` result rather than fabricating device interfaces.

use crate::process::{Handle, HleFn, Process, Stop};
use anywasm_cpu::cpu::CpuMode;

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_INVALID_FLAGS: u32 = 1004;
const ERROR_INVALID_USER_BUFFER: u32 = 1784;

const DIGCF_DEFAULT: u32 = 0x0001;
const DIGCF_PRESENT: u32 = 0x0002;
const DIGCF_ALLCLASSES: u32 = 0x0004;
const DIGCF_PROFILE: u32 = 0x0008;
const DIGCF_DEVICEINTERFACE: u32 = 0x0010;
const DIGCF_ALL_FLAGS: u32 =
    DIGCF_DEFAULT | DIGCF_PRESENT | DIGCF_ALLCLASSES | DIGCF_PROFILE | DIGCF_DEVICEINTERFACE;

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "SetupDiGetClassDevsA" => Some(setup_di_get_class_devs_a),
        "SetupDiDestroyDeviceInfoList" => Some(setup_di_destroy_device_info_list),
        "SetupDiEnumDeviceInterfaces" => Some(setup_di_enum_device_interfaces),
        "SetupDiGetDeviceInterfaceDetailA" => Some(setup_di_get_device_interface_detail_a),
        _ => None,
    }
}

fn invalid_handle_value(p: &Process) -> u64 {
    if p.cpu.mode == CpuMode::X86_32 {
        u32::MAX as u64
    } else {
        u64::MAX
    }
}

fn device_info_key(handle: u64) -> String {
    format!("setupapi:hdevinfo:{handle:016x}")
}

fn create_device_info_set(p: &mut Process) -> u64 {
    // Handle::Other is the process-wide opaque-handle representation. The unique marker in
    // data_cells distinguishes our HDEVINFO values from registry and GDI handles that also use
    // Handle::Other; it is removed by SetupDiDestroyDeviceInfoList.
    let handle = p.new_handle(Handle::Other);
    p.extra.data_cells.insert(device_info_key(handle), handle);
    handle
}

fn is_device_info_set(p: &mut Process, handle: u64) -> bool {
    let key = device_info_key(handle);
    let marked = p.extra.data_cells.get(&key) == Some(&handle);
    let is_opaque_handle = matches!(p.handles.get(&handle), Some(Handle::Other));
    if marked && !is_opaque_handle {
        // A generic CloseHandle may have removed the entry; do not leave a stale HDEVINFO marker.
        p.extra.data_cells.remove(&key);
    }
    marked && is_opaque_handle
}

fn setup_di_get_class_devs_a(p: &mut Process) -> Result<u64, Stop> {
    let class_guid = p.arg(0);
    let enumerator = p.arg(1);
    let flags = p.arg(3) as u32;

    if flags & !DIGCF_ALL_FLAGS != 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(invalid_handle_value(p));
    }
    if class_guid == 0 {
        if flags & DIGCF_ALLCLASSES == 0
            || (flags & DIGCF_DEFAULT != 0 && flags & DIGCF_DEVICEINTERFACE == 0)
        {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(invalid_handle_value(p));
        }
    } else {
        // Validate the guest pointer even though the virtual catalogue is empty.
        let _ = p.read_bytes(class_guid, 16)?;
        if flags & DIGCF_DEFAULT != 0 && flags & DIGCF_DEVICEINTERFACE == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(invalid_handle_value(p));
        }
    }
    if enumerator != 0 {
        // SetupAPI accepts a class name or device-instance ID here; either is a filter over the
        // empty virtual catalogue. Reading it validates guest memory and the required NUL form.
        let enumerator_name = p.read_cstr(enumerator)?;
        if enumerator_name.contains('\\') && flags & DIGCF_DEVICEINTERFACE == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(invalid_handle_value(p));
        }
    }

    Ok(create_device_info_set(p))
}

fn setup_di_destroy_device_info_list(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0);
    if !is_device_info_set(p, handle) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    p.extra.data_cells.remove(&device_info_key(handle));
    p.handles.remove(&handle);
    Ok(1)
}

fn expected_device_interface_data_size(p: &Process) -> u32 {
    if p.cpu.mode == CpuMode::X86_32 {
        28
    } else {
        32
    }
}

fn expected_device_info_data_size(p: &Process) -> u32 {
    if p.cpu.mode == CpuMode::X86_32 {
        28
    } else {
        32
    }
}

fn setup_di_enum_device_interfaces(p: &mut Process) -> Result<u64, Stop> {
    let (handle, device_info, class_guid, _index, interface_data) =
        (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as u32, p.arg(4));
    if !is_device_info_set(p, handle) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if class_guid == 0 || interface_data == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let _ = p.read_bytes(class_guid, 16)?;

    let interface_size = p.rd32(interface_data)?;
    if interface_size != expected_device_interface_data_size(p) {
        p.set_last_error(ERROR_INVALID_USER_BUFFER);
        return Ok(0);
    }
    if device_info != 0 {
        let device_info_size = p.rd32(device_info)?;
        if device_info_size != expected_device_info_data_size(p) {
            p.set_last_error(ERROR_INVALID_USER_BUFFER);
            return Ok(0);
        }
        // There is no device record in the virtual catalogue to associate with this structure.
        p.set_last_error(ERROR_NO_MORE_ITEMS);
        return Ok(0);
    }

    p.set_last_error(ERROR_NO_MORE_ITEMS);
    Ok(0)
}

fn setup_di_get_device_interface_detail_a(p: &mut Process) -> Result<u64, Stop> {
    let (handle, interface_data, _detail_data, _detail_size, _required_size, device_info) = (
        p.arg(0),
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32,
        p.arg(4),
        p.arg(5),
    );
    if !is_device_info_set(p, handle) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if interface_data == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.rd32(interface_data)? != expected_device_interface_data_size(p) {
        p.set_last_error(ERROR_INVALID_USER_BUFFER);
        return Ok(0);
    }
    if device_info != 0 && p.rd32(device_info)? != expected_device_info_data_size(p) {
        p.set_last_error(ERROR_INVALID_USER_BUFFER);
        return Ok(0);
    }

    // No SP_DEVICE_INTERFACE_DATA can be returned by this empty catalogue, so there is no valid
    // interface record whose path could be copied into the detail structure.
    p.set_last_error(ERROR_INVALID_PARAMETER);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::TEB_ADDR;
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x20000;
    const STACK: u64 = BASE + 0x800;
    const GUID: u64 = BASE + 0x100;
    const IF_DATA: u64 = BASE + 0x200;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call_x86(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).expect("SetupAPI import should resolve")(p).unwrap()
    }

    fn last_error(p: &Process) -> u32 {
        p.rd32(TEB_ADDR + 0x34).unwrap()
    }

    #[test]
    fn all_four_imports_resolve_with_x86_stdcall_cleanup_sizes() {
        for name in [
            "SetupDiGetClassDevsA",
            "SetupDiDestroyDeviceInfoList",
            "SetupDiEnumDeviceInterfaces",
            "SetupDiGetDeviceInterfaceDetailA",
        ] {
            assert!(lookup(name).is_some(), "{name}");
        }
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("setupapi", "SetupDiGetClassDevsA"),
            Some(16)
        );
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("setupapi", "SetupDiEnumDeviceInterfaces"),
            Some(20)
        );
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("setupapi", "SetupDiGetDeviceInterfaceDetailA"),
            Some(24)
        );
    }

    #[test]
    fn class_devs_returns_a_valid_empty_set_then_enumeration_reports_no_items() {
        let mut p = process();
        let all_interfaces = call_x86(
            &mut p,
            "SetupDiGetClassDevsA",
            &[0, 0, 0, DIGCF_ALLCLASSES | DIGCF_DEVICEINTERFACE],
        );
        assert_ne!(all_interfaces, u32::MAX as u64);
        assert_eq!(
            call_x86(
                &mut p,
                "SetupDiDestroyDeviceInfoList",
                &[all_interfaces as u32]
            ),
            1
        );

        let handle = call_x86(
            &mut p,
            "SetupDiGetClassDevsA",
            &[GUID as u32, 0, 0, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE],
        );
        assert_ne!(handle, u32::MAX as u64);
        assert!(is_device_info_set(&mut p, handle));

        p.wr32(IF_DATA, 28).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "SetupDiEnumDeviceInterfaces",
                &[handle as u32, 0, GUID as u32, 0, IF_DATA as u32],
            ),
            0
        );
        assert_eq!(last_error(&p), ERROR_NO_MORE_ITEMS);
    }

    #[test]
    fn class_devs_rejects_invalid_query_flags_and_missing_class_filter() {
        let mut p = process();
        assert_eq!(
            call_x86(&mut p, "SetupDiGetClassDevsA", &[0, 0, 0, 0]),
            u32::MAX as u64
        );
        assert_eq!(last_error(&p), ERROR_INVALID_PARAMETER);
        assert_eq!(
            call_x86(
                &mut p,
                "SetupDiGetClassDevsA",
                &[GUID as u32, 0, 0, 0x8000_0000]
            ),
            u32::MAX as u64
        );
        assert_eq!(last_error(&p), ERROR_INVALID_FLAGS);
    }

    #[test]
    fn destroy_releases_only_its_hdevinfo_and_invalid_handles_fail() {
        let mut p = process();
        let handle = call_x86(
            &mut p,
            "SetupDiGetClassDevsA",
            &[GUID as u32, 0, 0, DIGCF_PRESENT],
        );
        assert_eq!(
            call_x86(&mut p, "SetupDiDestroyDeviceInfoList", &[handle as u32]),
            1
        );
        assert!(!is_device_info_set(&mut p, handle));
        assert_eq!(
            call_x86(&mut p, "SetupDiDestroyDeviceInfoList", &[handle as u32]),
            0
        );
        assert_eq!(last_error(&p), ERROR_INVALID_HANDLE);
    }

    #[test]
    fn enum_and_detail_validate_structure_sizes_without_inventing_devices() {
        let mut p = process();
        let handle = call_x86(
            &mut p,
            "SetupDiGetClassDevsA",
            &[GUID as u32, 0, 0, DIGCF_PRESENT],
        );
        p.wr32(IF_DATA, 24).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "SetupDiEnumDeviceInterfaces",
                &[handle as u32, 0, GUID as u32, 0, IF_DATA as u32],
            ),
            0
        );
        assert_eq!(last_error(&p), ERROR_INVALID_USER_BUFFER);

        p.wr32(IF_DATA, 28).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "SetupDiGetDeviceInterfaceDetailA",
                &[handle as u32, IF_DATA as u32, 0, 0, 0, 0],
            ),
            0
        );
        assert_eq!(last_error(&p), ERROR_INVALID_PARAMETER);
    }
}
