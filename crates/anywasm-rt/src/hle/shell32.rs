//! Shell32 functions used by ordinary desktop windows.

use crate::process::*;
use crate::vfs::CreateDirError;

const CSIDL_PERSONAL: u32 = 5;
const CSIDL_MYDOCUMENTS: u32 = 0x000c;
const CSIDL_FLAG_DONT_VERIFY: u32 = 0x4000;
const CSIDL_FLAG_CREATE: u32 = 0x8000;
const SHGFP_TYPE_CURRENT: u32 = 0;
const SHGFP_TYPE_DEFAULT: u32 = 1;
const E_INVALIDARG: u64 = 0x8007_0057;
const ERROR_PATH_NOT_FOUND_HRESULT: u64 = 0x8007_0003;
const ERROR_FILENAME_EXCED_RANGE_HRESULT: u64 = 0x8007_00ce;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "SHGetFolderPathW" => sh_get_folder_path_w,
        _ => return None,
    })
}

fn sh_get_folder_path_w(p: &mut Process) -> Result<u64, Stop> {
    let csidl = p.arg(1) as u32;
    let folder_id = csidl & 0x3fff;
    let csidl_flags = csidl & 0xc000;
    let output = p.arg(4);

    if csidl & 0xffff_0000 != 0 || csidl_flags & !(CSIDL_FLAG_CREATE | CSIDL_FLAG_DONT_VERIFY) != 0
    {
        return Err(Stop::Crash(format!(
            "untranslated: shell32!SHGetFolderPathW CSIDL flags {csidl:#x}"
        )));
    }
    if !matches!(folder_id, CSIDL_PERSONAL | CSIDL_MYDOCUMENTS) {
        return Err(Stop::Crash(format!(
            "untranslated: shell32!SHGetFolderPathW CSIDL {folder_id:#x}"
        )));
    }
    if p.arg(2) != 0 {
        return Err(Stop::Crash(
            "untranslated: shell32!SHGetFolderPathW alternate user token".into(),
        ));
    }
    if !matches!(p.arg(3) as u32, SHGFP_TYPE_CURRENT | SHGFP_TYPE_DEFAULT) || output == 0 {
        if output != 0 {
            p.wr16(output, 0)?;
        }
        return Ok(E_INVALIDARG);
    }

    let environment = p.initial_env.as_ref().unwrap_or(&p.env);
    let profile = environment
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("USERPROFILE"))
        .map(|(_, value)| value.as_str())
        .unwrap_or(VIRTUAL_USER_PROFILE);
    let profile = crate::vfs::absolute(profile, &p.current_directory);
    let path = format!("{profile}\\Documents");

    if csidl_flags & CSIDL_FLAG_CREATE != 0 {
        if create_virtual_directory_tree(p, &path).is_err() {
            p.wr16(output, 0)?;
            return Ok(ERROR_PATH_NOT_FOUND_HRESULT);
        }
    } else if csidl_flags & CSIDL_FLAG_DONT_VERIFY == 0 && !p.vfs.is_dir(&path) {
        p.wr16(output, 0)?;
        return Ok(ERROR_PATH_NOT_FOUND_HRESULT);
    }

    if path.encode_utf16().count() >= 260 {
        p.wr16(output, 0)?;
        return Ok(ERROR_FILENAME_EXCED_RANGE_HRESULT);
    }
    p.write_wstr(output, &path)?;
    Ok(0) // S_OK: the path was written, and (unless DONT_VERIFY) verified in the VFS.
}

fn create_virtual_directory_tree(p: &mut Process, path: &str) -> Result<(), CreateDirError> {
    let absolute = crate::vfs::absolute(path, &p.current_directory);
    if absolute.as_bytes().get(1) != Some(&b':') {
        return Err(CreateDirError::PathNotFound);
    }

    let mut current = absolute[..2].to_string();
    for component in absolute[2..].split('\\').filter(|part| !part.is_empty()) {
        current.push('\\');
        current.push_str(component);
        if !p.vfs.is_dir(&current) {
            match p.vfs.create_dir(&current) {
                Ok(()) => p.notify_vfs_change(&current, 0x02 | 0x40), // DIR_NAME | CREATION
                Err(CreateDirError::AlreadyExists) if p.vfs.is_dir(&current) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod shell32_coverage_tests {
    use super::{lookup, ERROR_PATH_NOT_FOUND_HRESULT};
    use crate::process::{Process, Stop, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x20000;
    const STACK: u64 = BASE + 0x800;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call_x86(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, crate::process::Stop> {
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).expect("the tested shell32 import should resolve")(p)
    }

    #[test]
    fn sh_get_folder_path_w_returns_the_virtual_documents_directory() {
        let mut p = process();
        let output = BASE + 0x100;
        assert_eq!(
            call_x86(&mut p, "SHGetFolderPathW", &[0, 5, 0, 0, output as u32]),
            Ok(0)
        );
        assert_eq!(
            p.read_wstr(output).unwrap(),
            "C:\\Users\\AnyWASM\\Documents"
        );
        assert!(p.vfs.is_dir("C:\\Users\\AnyWASM\\Documents"));
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("shell32", "SHGetFolderPathW"),
            Some(20)
        );
    }

    #[test]
    fn sh_get_folder_path_w_create_flag_makes_missing_profile_directories_in_the_vfs() {
        let mut p = process();
        p.env
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("USERPROFILE"))
            .unwrap()
            .1 = "C:\\Profiles\\Player".into();
        let output = BASE + 0x100;
        assert_eq!(
            call_x86(
                &mut p,
                "SHGetFolderPathW",
                &[0, 5 | 0x8000, 0, 0, output as u32],
            ),
            Ok(0)
        );
        assert_eq!(
            p.read_wstr(output).unwrap(),
            "C:\\Profiles\\Player\\Documents"
        );
        assert!(p.vfs.is_dir("C:\\Profiles\\Player\\Documents"));
    }

    #[test]
    fn sh_get_folder_path_w_exposes_valid_but_untranslated_csidl_values() {
        let mut p = process();
        let output = BASE + 0x100;
        let result = call_x86(&mut p, "SHGetFolderPathW", &[0, 0x1c, 0, 0, output as u32]);
        assert!(matches!(
            result,
            Err(Stop::Crash(message)) if message.contains("untranslated") && message.contains("CSIDL")
        ));
    }

    #[test]
    fn sh_get_folder_path_w_only_skips_vfs_verification_when_requested() {
        let mut p = process();
        p.env
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("USERPROFILE"))
            .unwrap()
            .1 = "C:\\Profiles\\Player".into();
        let output = BASE + 0x100;
        p.write_wstr(output, "stale output").unwrap();

        assert_eq!(
            call_x86(&mut p, "SHGetFolderPathW", &[0, 5, 0, 0, output as u32]),
            Ok(ERROR_PATH_NOT_FOUND_HRESULT)
        );
        assert_eq!(p.rd16(output).unwrap(), 0);
        assert!(!p.vfs.is_dir("C:\\Profiles\\Player\\Documents"));

        assert_eq!(
            call_x86(
                &mut p,
                "SHGetFolderPathW",
                &[0, 5 | 0x4000, 0, 0, output as u32],
            ),
            Ok(0)
        );
        assert_eq!(
            p.read_wstr(output).unwrap(),
            "C:\\Profiles\\Player\\Documents"
        );
        assert!(!p.vfs.is_dir("C:\\Profiles\\Player\\Documents"));
    }

    #[test]
    fn file_drop_acceptance_is_unresolved_until_drop_messages_are_delivered() {
        assert!(lookup("DragAcceptFiles").is_none());
    }
}
