//! Small path helpers from shlwapi used by Win32 applications.

use super::*;
use crate::vfs::absolute;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "PathFileExistsA" => Some(path_file_exists_a),
        "PathFileExistsW" => Some(path_file_exists_w),
        _ => None,
    }
}

fn path_file_exists_a(p: &mut Process) -> R {
    let path = p.read_cstr(p.arg(0))?;
    path_file_exists(p, &path)
}

fn path_file_exists_w(p: &mut Process) -> R {
    let path = p.read_wstr(p.arg(0))?;
    path_file_exists(p, &path)
}

fn path_file_exists(p: &mut Process, path: &str) -> R {
    if path.is_empty() || path.encode_utf16().count() >= 260 {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        if p.trace_imports {
            p.stderr.extend_from_slice(
                format!("[path] PathFileExists path={path:?} -> invalid\n").as_bytes(),
            );
        }
        return Ok(0);
    }
    let resolved = absolute(path, &p.current_directory);
    let exists = p.vfs.exists(&resolved) || p.vfs.is_dir(&resolved);
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!("[path] PathFileExists path={path:?} resolved={resolved:?} -> {exists}\n")
                .as_bytes(),
        );
    }
    if exists {
        Ok(1)
    } else {
        p.set_last_error(ERROR_FILE_NOT_FOUND);
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::TEB_ADDR;
    use crate::Vfs;
    use anywasm_cpu::mem::PROT_RW;

    #[test]
    fn path_file_exists_checks_vfs_files_and_inferred_directories() {
        let mut vfs = Vfs::new();
        vfs.insert("game\\data\\present.txt", vec![1]);
        let mut p = Process::new(vfs);
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        p.current_directory = "C:\\game".into();

        assert_eq!(path_file_exists(&mut p, "data\\present.txt").unwrap(), 1);
        assert_eq!(path_file_exists(&mut p, "C:\\game\\data").unwrap(), 1);
        assert_eq!(path_file_exists(&mut p, "data\\missing.txt").unwrap(), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x68).unwrap(), ERROR_FILE_NOT_FOUND);
    }
}
