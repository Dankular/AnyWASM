//! In-memory virtual file system keyed by lower-cased, `\`-normalised paths.
use std::collections::HashMap;

#[derive(Default, Clone)]
pub struct Vfs {
    files: HashMap<String, Vec<u8>>,
}

fn without_device_prefix(path: &str) -> String {
    let mut path = path.replace('/', "\\");
    for prefix in [r"\\?\UNC\", r"\??\UNC\"] {
        if path.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix)) {
            return format!("\\\\{}", &path[prefix.len()..]);
        }
    }
    for prefix in ["\\\\?\\", "\\??\\"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest.to_string();
            break;
        }
    }
    path
}

/// Resolve a Windows path against an absolute current directory and return an absolute path.
pub fn absolute(path: &str, current_directory: &str) -> String {
    let path = without_device_prefix(path);
    if let Some(unc_path) = path.strip_prefix("\\\\") {
        let mut components = Vec::new();
        for component in unc_path.split('\\') {
            match component {
                "" | "." => {}
                ".." if components.len() > 2 => {
                    components.pop();
                }
                ".." => {}
                other => components.push(other),
            }
        }
        return format!("\\\\{}", components.join("\\"));
    }
    let current_directory = without_device_prefix(current_directory);
    let (cwd_root, cwd_tail, cwd_drive) = if let Some(unc) = current_directory.strip_prefix("\\\\") {
        let parts: Vec<&str> = unc.split('\\').filter(|part| !part.is_empty()).collect();
        let root_parts = parts.len().min(2);
        let root = format!("\\\\{}", parts[..root_parts].join("\\"));
        (root, parts[root_parts..].join("\\"), "c:".to_string())
    } else {
        let drive = current_directory
            .as_bytes()
            .get(1)
            .filter(|&&ch| ch == b':')
            .map(|_| &current_directory[..2])
            .unwrap_or("c:");
        let tail = current_directory
            .strip_prefix(drive)
            .unwrap_or("\\")
            .trim_start_matches('\\')
            .to_string();
        (drive.to_string(), tail, drive.to_string())
    };

    let (root, tail) = if path.as_bytes().get(1) == Some(&b':') {
        let drive = &path[..2];
        let rest = &path[2..];
        if rest.starts_with('\\') {
            (drive.to_string(), rest.trim_start_matches('\\').to_string())
        } else if drive.eq_ignore_ascii_case(&cwd_drive) {
            let base = cwd_tail.trim_end_matches('\\');
            (drive.to_string(), join_path(base, rest))
        } else {
            (drive.to_string(), rest.to_string())
        }
    } else if path.starts_with('\\') {
        (cwd_root.clone(), path.trim_start_matches('\\').to_string())
    } else {
        (cwd_root, join_path(&cwd_tail, &path))
    };

    let mut components = Vec::new();
    for component in tail.split('\\') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    format!("{root}\\{}", components.join("\\"))
}

fn join_path(base: &str, path: &str) -> String {
    match (base.trim_matches('\\'), path.trim_matches('\\')) {
        ("", path) => path.to_string(),
        (base, "") => base.to_string(),
        (base, path) => format!("{base}\\{path}"),
    }
}

/// Resolve a Windows path to a root-relative VFS key.
pub fn resolve(path: &str, current_directory: &str) -> String {
    let absolute = absolute(path, current_directory);
    if let Some(relative) = absolute.strip_prefix("C:\\").or_else(|| absolute.strip_prefix("c:\\")) {
        relative.to_ascii_lowercase()
    } else {
        absolute.to_ascii_lowercase()
    }
}

/// Normalize a path as a root-relative VFS key, using `C:\` for relative inputs.
pub fn norm(path: &str) -> String {
    resolve(path, "C:\\")
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, path: &str, data: Vec<u8>) {
        self.files.insert(norm(path), data);
    }
    pub fn get(&self, path: &str) -> Option<&Vec<u8>> {
        self.files.get(&norm(path))
    }
    pub fn get_mut(&mut self, path: &str) -> Option<&mut Vec<u8>> {
        self.files.get_mut(&norm(path))
    }
    pub fn exists(&self, path: &str) -> bool {
        self.files.contains_key(&norm(path))
    }
    /// Directories are inferred from file prefixes; empty directories are not represented.
    pub fn is_dir(&self, path: &str) -> bool {
        let dir = norm(path);
        if dir.is_empty() {
            return true;
        }
        let prefix = format!("{dir}\\");
        self.files.keys().any(|file| file.starts_with(&prefix))
    }
    pub fn remove(&mut self, path: &str) -> bool {
        self.files.remove(&norm(path)).is_some()
    }
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }
    /// Find a file by bare file name anywhere in the VFS (DLL search fallback).
    pub fn find_by_name(&self, name: &str) -> Option<&String> {
        let n = norm(name);
        self.files.keys().find(|k| k.rsplit('\\').next() == Some(n.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::{absolute, norm, resolve, Vfs};

    #[test]
    fn resolves_windows_paths_against_a_current_directory() {
        assert_eq!(absolute("config.ini", "C:\\games\\demo"), "C:\\games\\demo\\config.ini");
        assert_eq!(absolute("..\\shared\\save.dat", "C:\\games\\demo"), "C:\\games\\shared\\save.dat");
        assert_eq!(absolute("\\Windows\\win.ini", "C:\\games"), "C:\\Windows\\win.ini");
        assert_eq!(resolve("C:\\games\\demo\\..\\save.dat", "C:\\"), "games\\save.dat");
        assert_eq!(norm("C:\\games\\save.dat"), "games\\save.dat");
        assert_eq!(norm("\\\\?\\C:\\games\\save.dat"), "games\\save.dat");
        assert_eq!(absolute("\\\\server\\share\\folder\\..\\save.dat", "C:\\"), "\\\\server\\share\\save.dat");
        assert_eq!(norm("\\\\?\\UNC\\server\\share\\save.dat"), "\\\\server\\share\\save.dat");
        assert_eq!(absolute("save.dat", "\\\\server\\share\\game"), "\\\\server\\share\\game\\save.dat");
        assert_eq!(absolute("\\save.dat", "\\\\server\\share\\game"), "\\\\server\\share\\save.dat");
    }

    #[test]
    fn recognizes_only_directories_with_vfs_children() {
        let mut vfs = Vfs::new();
        vfs.insert("games\\data\\level.bin", vec![1]);

        assert!(vfs.is_dir("C:\\"));
        assert!(vfs.is_dir("games"));
        assert!(vfs.is_dir("C:\\games\\data"));
        assert!(!vfs.is_dir("games\\missing"));
    }
}
