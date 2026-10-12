//! In-memory virtual file system keyed by lower-cased, `\`-normalised paths.
use std::collections::{HashMap, HashSet};

const ASSET_HEADER_LEN: usize = 24;
const MAX_ASSET_READ_LEN: usize = 64 * 1024 * 1024 - 256 * 1024;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    fn read_asset_range(package_offset: u64, len: u32, destination: *mut u8) -> u32;
}

#[derive(Default, Clone)]
pub struct Vfs {
    files: HashMap<String, Vec<u8>>,
    assets: HashMap<String, AssetFile>,
    directories: HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AssetFile {
    package_offset: u64,
    len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetReadError {
    NotFound,
    TooLarge,
    Unavailable,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateDirError {
    AlreadyExists,
    PathNotFound,
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
        let path = norm(path);
        self.files.contains_key(&path) || self.assets.contains_key(&path)
    }

    /// Register the fixed header and index prefix of an `AGP1` range package.
    /// Payload bytes stay in the host and are read through the browser import.
    pub fn register_asset_index(&mut self, bytes: &[u8]) -> Result<(), String> {
        let bad = |why: &str| format!("corrupt asset package index: {why}");
        if bytes.get(..4) != Some(b"AGP1") {
            return Err(bad("bad magic"));
        }
        if bytes.len() < ASSET_HEADER_LEN {
            return Err(bad("truncated header"));
        }
        let entry_count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let index_len = usize::try_from(u64::from_le_bytes(bytes[8..16].try_into().unwrap()))
            .map_err(|_| bad("index length overflows host address space"))?;
        let package_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let index_end = ASSET_HEADER_LEN
            .checked_add(index_len)
            .ok_or_else(|| bad("index length overflow"))?;
        if bytes.len() != index_end {
            return Err(bad("header/index byte count mismatch"));
        }
        if package_len < index_end as u64 {
            return Err(bad("package is shorter than its index"));
        }

        let mut cursor = ASSET_HEADER_LEN;
        if entry_count > index_len / 18 {
            return Err(bad("entry count cannot fit in index"));
        }
        let mut parsed = HashMap::with_capacity(entry_count);
        let mut seen = HashSet::with_capacity(entry_count);
        for _ in 0..entry_count {
            let name_len = u16::from_le_bytes(
                bytes
                    .get(cursor..cursor + 2)
                    .ok_or_else(|| bad("truncated path length"))?
                    .try_into()
                    .unwrap(),
            ) as usize;
            cursor += 2;
            let raw_name = std::str::from_utf8(
                bytes
                    .get(cursor..cursor.checked_add(name_len).ok_or_else(|| bad("path length overflow"))?)
                    .ok_or_else(|| bad("truncated path"))?,
            )
            .map_err(|_| bad("path is not UTF-8"))?;
            cursor += name_len;
            if raw_name.is_empty()
                || raw_name.starts_with('\\')
                || raw_name.starts_with('/')
                || raw_name.contains(':')
                || raw_name.contains('\0')
            {
                return Err(bad("path must be nonempty and relative"));
            }
            let path = norm(raw_name);
            if path.is_empty() || !seen.insert(path.clone()) {
                return Err(bad("empty or duplicate normalized path"));
            }

            let package_offset = u64::from_le_bytes(
                bytes
                    .get(cursor..cursor + 8)
                    .ok_or_else(|| bad("truncated package offset"))?
                    .try_into()
                    .unwrap(),
            );
            cursor += 8;
            let len = u64::from_le_bytes(
                bytes
                    .get(cursor..cursor + 8)
                    .ok_or_else(|| bad("truncated file length"))?
                    .try_into()
                    .unwrap(),
            );
            cursor += 8;
            let end = package_offset
                .checked_add(len)
                .ok_or_else(|| bad("file range overflow"))?;
            if package_offset < index_end as u64 || end > package_len {
                return Err(bad("file range is outside the package data region"));
            }
            parsed.insert(path, AssetFile { package_offset, len });
        }
        if cursor != index_end {
            return Err(bad("entry count does not match index length"));
        }
        self.assets = parsed;
        Ok(())
    }

    /// Return a file's logical size, including files whose bytes live in a remote package.
    pub fn file_len(&self, path: &str) -> Option<u64> {
        let path = norm(path);
        self.files
            .get(&path)
            .map(|data| data.len() as u64)
            .or_else(|| self.assets.get(&path).map(|asset| asset.len))
    }

    /// Translate a remote file-relative range into its absolute package range.
    pub fn asset_location(&self, path: &str, offset: u64, len: usize) -> Option<(u64, usize)> {
        let asset = self.assets.get(&norm(path))?;
        if offset >= asset.len {
            return Some((asset.package_offset.checked_add(asset.len)?, 0));
        }
        let count = (asset.len - offset).min(len as u64) as usize;
        Some((asset.package_offset.checked_add(offset)?, count))
    }

    /// Read a local or indexed file range. Remote bytes are supplied by the asyncified host.
    pub fn read_range(
        &self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, AssetReadError> {
        let path = norm(path);
        if let Some(data) = self.files.get(&path) {
            let Ok(start) = usize::try_from(offset) else {
                return Ok(Vec::new());
            };
            if start >= data.len() {
                return Ok(Vec::new());
            }
            let end = data.len().min(start.saturating_add(len));
            return Ok(data[start..end].to_vec());
        }
        let (package_offset, count) = self
            .asset_location(&path, offset, len)
            .ok_or(AssetReadError::NotFound)?;
        if count > MAX_ASSET_READ_LEN {
            return Err(AssetReadError::TooLarge);
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        #[allow(unused_mut)]
        let mut out = vec![0; count];
        #[cfg(target_arch = "wasm32")]
        {
            let copied = unsafe {
                read_asset_range(package_offset, count as u32, out.as_mut_ptr()) as usize
            };
            if copied == count {
                return Ok(out);
            }
            return Err(AssetReadError::Failed);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (package_offset, out);
            Err(AssetReadError::Unavailable)
        }
    }

    /// Directories are inferred from file prefixes and tracked when created explicitly.
    pub fn is_dir(&self, path: &str) -> bool {
        let dir = norm(path);
        if dir.is_empty() {
            return true;
        }
        if self.files.contains_key(&dir) || self.assets.contains_key(&dir) {
            return false;
        }
        if self.directories.contains(&dir) {
            return true;
        }
        let prefix = format!("{dir}\\");
        self.files.keys().any(|file| file.starts_with(&prefix))
            || self.assets.keys().any(|file| file.starts_with(&prefix))
    }
    /// Create one directory without creating missing parents.
    pub fn create_dir(&mut self, path: &str) -> Result<(), CreateDirError> {
        if path.is_empty() {
            return Err(CreateDirError::PathNotFound);
        }
        let dir = norm(path);
        if dir.is_empty() || self.exists(path) || self.is_dir(path) {
            return Err(CreateDirError::AlreadyExists);
        }
        let parent = dir.rsplit_once('\\').map_or("", |(parent, _)| parent);
        if !self.is_dir(parent) {
            return Err(CreateDirError::PathNotFound);
        }
        self.directories.insert(dir);
        Ok(())
    }
    pub fn remove(&mut self, path: &str) -> bool {
        let path = norm(path);
        self.files.remove(&path).is_some() || self.assets.remove(&path).is_some()
    }
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files
            .keys()
            .chain(self.assets.keys().filter(|path| !self.files.contains_key(*path)))
    }
    /// Find a file by bare file name anywhere in the VFS (DLL search fallback).
    pub fn find_by_name(&self, name: &str) -> Option<&String> {
        let n = norm(name);
        self.paths()
            .find(|k| k.rsplit('\\').next() == Some(n.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::{absolute, norm, resolve, Vfs};

    fn asset_package(files: &[(&str, &[u8])]) -> Vec<u8> {
        let index_len: usize = files.iter().map(|(name, _)| 2 + name.len() + 16).sum();
        let data_start = 24 + index_len;
        let package_len = data_start + files.iter().map(|(_, data)| data.len()).sum::<usize>();
        let mut out = b"AGP1".to_vec();
        out.extend((files.len() as u32).to_le_bytes());
        out.extend((index_len as u64).to_le_bytes());
        out.extend((package_len as u64).to_le_bytes());
        let mut offset = data_start;
        for (name, data) in files {
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(name.as_bytes());
            out.extend((offset as u64).to_le_bytes());
            out.extend((data.len() as u64).to_le_bytes());
            offset += data.len();
        }
        for (_, data) in files {
            out.extend_from_slice(data);
        }
        out
    }

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
    fn recognizes_file_backed_and_explicit_empty_directories() {
        let mut vfs = Vfs::new();
        vfs.insert("games\\data\\level.bin", vec![1]);

        assert!(vfs.is_dir("C:\\"));
        assert!(vfs.is_dir("games"));
        assert!(vfs.is_dir("C:\\games\\data"));
        assert!(!vfs.is_dir("games\\data\\level.bin"));
        assert!(!vfs.is_dir("games\\missing"));
        vfs.create_dir("games\\empty").unwrap();
        assert!(vfs.is_dir("C:\\games\\empty"));
        assert_eq!(
            vfs.create_dir("games\\empty"),
            Err(super::CreateDirError::AlreadyExists)
        );
        assert_eq!(
            vfs.create_dir("missing\\child"),
            Err(super::CreateDirError::PathNotFound)
        );
        assert_eq!(
            vfs.create_dir("games\\data\\level.bin"),
            Err(super::CreateDirError::AlreadyExists)
        );
        assert_eq!(
            vfs.create_dir("games\\data\\level.bin\\child"),
            Err(super::CreateDirError::PathNotFound)
        );
        assert_eq!(
            vfs.create_dir(""),
            Err(super::CreateDirError::PathNotFound)
        );
    }

    #[test]
    fn indexed_assets_expose_paths_and_lengths_without_loading_payload() {
        let package = asset_package(&[("data\\handling.cfg", &[4, 5]), ("models\\gta3.img", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();

        assert!(vfs.exists("MODELS/gta3.img"));
        assert!(vfs.is_dir("models"));
        assert_eq!(vfs.file_len("models\\gta3.img"), Some(3));
        assert_eq!(vfs.file_len("data/handling.cfg"), Some(2));
        assert!(vfs.get("models\\gta3.img").is_none());
        assert_eq!(vfs.paths().count(), 2);
        let data_start = index_end as u64;
        assert_eq!(vfs.asset_location("models/gta3.img", 1, 10), Some((data_start + 3, 2)));
    }

    #[test]
    fn local_files_override_indexed_assets() {
        let package = asset_package(&[("data\\handling.cfg", &[1, 2])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();
        vfs.insert("data/handling.cfg", vec![9, 8, 7]);

        assert_eq!(vfs.file_len("data\\handling.cfg"), Some(3));
        assert_eq!(vfs.read_range("data/handling.cfg", 1, 8).unwrap(), vec![8, 7]);
        assert_eq!(vfs.paths().count(), 1);
    }

    #[test]
    fn rejects_duplicate_normalized_asset_paths() {
        let package = asset_package(&[("Data\\Handling.cfg", &[1]), ("data/handling.cfg", &[2])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        assert!(Vfs::new().register_asset_index(&package[..index_end]).is_err());
    }

    #[test]
    fn rejects_asset_records_outside_the_package_data_region() {
        let mut package = asset_package(&[("data\\file.bin", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let record_offset = 24 + 2 + "data\\file.bin".len();
        package[record_offset..record_offset + 8].copy_from_slice(&0u64.to_le_bytes());
        assert!(Vfs::new().register_asset_index(&package[..index_end]).is_err());

        let mut package = asset_package(&[("data\\file.bin", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let package_len = u64::from_le_bytes(package[16..24].try_into().unwrap());
        package[16..24].copy_from_slice(&(package_len - 1).to_le_bytes());
        assert!(Vfs::new().register_asset_index(&package[..index_end]).is_err());
    }
}
