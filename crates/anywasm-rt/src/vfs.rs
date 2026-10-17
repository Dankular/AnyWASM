//! In-memory virtual file system keyed by lower-cased, `\`-normalised paths.
use std::collections::{HashMap, HashSet};

const ASSET_HEADER_LEN: usize = 24;
const MAX_ASSET_READ_LEN: usize = 64 * 1024 * 1024 - 256 * 1024;

fn read_index<const N: usize>(bytes: &[u8], cursor: &mut usize) -> Option<[u8; N]> {
    let end = (*cursor).checked_add(N)?;
    let value = bytes.get(*cursor..end)?.try_into().ok()?;
    *cursor = end;
    Some(value)
}

fn read_index_u16(bytes: &[u8], cursor: &mut usize) -> Option<u16> {
    Some(u16::from_le_bytes(read_index(bytes, cursor)?))
}

fn read_index_u32(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    Some(u32::from_le_bytes(read_index(bytes, cursor)?))
}

fn read_index_i64(bytes: &[u8], cursor: &mut usize) -> Option<i64> {
    Some(i64::from_le_bytes(read_index(bytes, cursor)?))
}

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
    attributes: HashMap<String, u32>,
    file_metadata: HashMap<String, FileMetadata>,
    root_alias: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AssetFile {
    package_offset: u64,
    len: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileMetadata {
    pub accessed: Option<i64>,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub mode: Option<u32>,
    pub link_count: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub size: u64,
    pub is_directory: bool,
    pub accessed: Option<i64>,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    /// POSIX permission and file-type bits copied from the source filesystem.
    pub mode: Option<u32>,
    pub link_count: Option<u16>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveDirError {
    NotFound,
    NotEmpty,
    AccessDenied,
}

fn without_device_prefix(path: &str) -> String {
    let mut path = path.replace('/', "\\");
    for prefix in [r"\\?\UNC\", r"\??\UNC\"] {
        if path
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
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

fn trim_win32_component(component: &str) -> &str {
    let without_spaces = component.trim_end_matches(' ');
    match without_spaces {
        "." | ".." => without_spaces,
        _ => without_spaces.trim_end_matches('.'),
    }
}

/// Resolve a Windows path against an absolute current directory and return an absolute path.
pub fn absolute(path: &str, current_directory: &str) -> String {
    let path = without_device_prefix(path);
    if let Some(unc_path) = path.strip_prefix("\\\\") {
        let mut components = Vec::new();
        for component in unc_path.split('\\') {
            let component = trim_win32_component(component);
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
    let (cwd_root, cwd_tail, cwd_drive) = if let Some(unc) = current_directory.strip_prefix("\\\\")
    {
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
        let component = trim_win32_component(component);
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
    if let Some(relative) = absolute
        .strip_prefix("C:\\")
        .or_else(|| absolute.strip_prefix("c:\\"))
    {
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

    /// Mount a drive-qualified host installation path onto the VFS root.
    pub fn set_root_alias(&mut self, root: &str) -> Result<(), String> {
        let root = without_device_prefix(root).replace('/', "\\");
        if root.as_bytes().get(1) != Some(&b':') || root.as_bytes().get(2) != Some(&b'\\') {
            return Err(format!(
                "VFS root alias must be an absolute drive path: {root:?}"
            ));
        }
        let root = norm(&root);
        if root.is_empty() {
            return Err("VFS root alias cannot be a drive root".to_owned());
        }
        self.root_alias = Some(root);
        Ok(())
    }

    /// Normalize a path and map the mounted host installation prefix to the VFS root.
    pub fn normalize_path(&self, path: &str) -> String {
        let path = norm(path);
        let Some(root) = &self.root_alias else {
            return path;
        };
        if path == *root {
            return String::new();
        }
        path.strip_prefix(&format!("{root}\\"))
            .unwrap_or(&path)
            .to_owned()
    }

    pub fn insert(&mut self, path: &str, data: Vec<u8>) {
        self.files.insert(self.normalize_path(path), data);
    }
    pub fn get(&self, path: &str) -> Option<&Vec<u8>> {
        self.files.get(&self.normalize_path(path))
    }
    pub fn get_mut(&mut self, path: &str) -> Option<&mut Vec<u8>> {
        let path = self.normalize_path(path);
        self.files.get_mut(&path)
    }
    pub fn exists(&self, path: &str) -> bool {
        let path = self.normalize_path(path);
        self.files.contains_key(&path) || self.assets.contains_key(&path)
    }

    /// Whether a path already has mutable guest bytes in the local VFS overlay.
    pub fn has_local_file(&self, path: &str) -> bool {
        self.files.contains_key(&self.normalize_path(path))
    }

    /// Whether a directory contains files backed by streamed host assets rather than
    /// the mutable in-memory guest overlay.
    pub fn has_streamed_assets_below(&self, path: &str) -> bool {
        let path = self.normalize_path(path);
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}\\")
        };
        self.assets.keys().any(|asset| asset.starts_with(&prefix))
    }

    /// Return stored attributes or the VFS default for a file or directory.
    pub fn file_attributes(&self, path: &str) -> Option<u32> {
        let path = self.normalize_path(path);
        if let Some(attributes) = self.attributes.get(&path) {
            return Some(*attributes);
        }
        if self.is_dir(&path) {
            Some(0x10) // FILE_ATTRIBUTE_DIRECTORY
        } else if self.files.contains_key(&path) || self.assets.contains_key(&path) {
            Some(0x80) // FILE_ATTRIBUTE_NORMAL
        } else {
            None
        }
    }

    /// Store attributes for a path that the caller has already resolved as existing.
    pub fn set_file_attributes(&mut self, path: &str, attributes: u32) {
        let path = self.normalize_path(path);
        self.attributes.insert(path, attributes);
    }

    pub fn is_readonly(&self, path: &str) -> bool {
        !self.is_dir(path)
            && self
                .file_attributes(path)
                .is_some_and(|attributes| attributes & 0x1 != 0)
    }

    /// Register an `AGP1` or metadata-carrying `AGP2` range index.
    /// Payload bytes stay in the host and are read through the browser import.
    pub fn register_asset_index(&mut self, bytes: &[u8]) -> Result<(), String> {
        let bad = |why: &str| format!("corrupt asset package index: {why}");
        let with_metadata = match bytes.get(..4) {
            Some(b"AGP1") => false,
            Some(b"AGP2") => true,
            _ => return Err(bad("unsupported magic")),
        };
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
        let mut parsed_metadata = HashMap::with_capacity(entry_count);
        let mut parsed_directories = HashSet::new();
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
                    .get(
                        cursor
                            ..cursor
                                .checked_add(name_len)
                                .ok_or_else(|| bad("path length overflow"))?,
                    )
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
            let (metadata, is_directory) = if with_metadata {
                let accessed = read_index_i64(bytes, &mut cursor)
                    .ok_or_else(|| bad("truncated access time"))?;
                let modified = read_index_i64(bytes, &mut cursor)
                    .ok_or_else(|| bad("truncated write time"))?;
                let created = read_index_i64(bytes, &mut cursor)
                    .ok_or_else(|| bad("truncated creation time"))?;
                let mode =
                    read_index_u32(bytes, &mut cursor).ok_or_else(|| bad("truncated mode"))?;
                let link_count = read_index_u16(bytes, &mut cursor)
                    .ok_or_else(|| bad("truncated link count"))?;
                let kind = *bytes
                    .get(cursor)
                    .ok_or_else(|| bad("truncated entry kind"))?;
                cursor += 1;
                let metadata = FileMetadata {
                    accessed: (accessed != i64::MIN).then_some(accessed),
                    modified: (modified != i64::MIN).then_some(modified),
                    created: (created != i64::MIN).then_some(created),
                    mode: (mode != u32::MAX).then_some(mode),
                    link_count: (link_count != u16::MAX).then_some(link_count),
                };
                let is_directory = match kind {
                    0 => false,
                    1 => true,
                    _ => return Err(bad("unknown entry kind")),
                };
                (metadata, is_directory)
            } else {
                (FileMetadata::default(), false)
            };
            let end = package_offset
                .checked_add(len)
                .ok_or_else(|| bad("file range overflow"))?;
            if package_offset < index_end as u64 || end > package_len {
                return Err(bad("file range is outside the package data region"));
            }
            if is_directory {
                if len != 0 {
                    return Err(bad("directory entry has a nonzero length"));
                }
                parsed_directories.insert(path.clone());
            } else {
                parsed.insert(
                    path.clone(),
                    AssetFile {
                        package_offset,
                        len,
                    },
                );
            }
            parsed_metadata.insert(path, metadata);
        }
        if cursor != index_end {
            return Err(bad("entry count does not match index length"));
        }
        self.assets = parsed;
        self.file_metadata = parsed_metadata;
        self.directories.extend(parsed_directories);
        Ok(())
    }

    /// Return a file's logical size, including files whose bytes live in a remote package.
    pub fn file_len(&self, path: &str) -> Option<u64> {
        let path = self.normalize_path(path);
        self.files
            .get(&path)
            .map(|data| data.len() as u64)
            .or_else(|| self.assets.get(&path).map(|asset| asset.len))
    }

    /// Return file or directory metadata when the VFS knows the path exists.
    /// Time and permission fields remain absent when the source index did not provide them.
    pub fn file_status(&self, path: &str) -> Option<FileStatus> {
        let normalized = self.normalize_path(path);
        let file_len = self
            .files
            .get(&normalized)
            .map(|data| data.len() as u64)
            .or_else(|| self.assets.get(&normalized).map(|asset| asset.len));
        let is_directory = file_len.is_none() && self.is_dir(&normalized);
        if file_len.is_none() && !is_directory {
            return None;
        }
        let metadata = self
            .file_metadata
            .get(&normalized)
            .copied()
            .unwrap_or_default();
        Some(FileStatus {
            size: file_len.unwrap_or(0),
            is_directory,
            accessed: metadata.accessed,
            modified: metadata.modified,
            created: metadata.created,
            mode: metadata.mode,
            link_count: metadata.link_count,
        })
    }

    /// Attach host or guest filesystem metadata to an existing file or directory.
    pub fn set_file_metadata(&mut self, path: &str, metadata: FileMetadata) {
        let path = self.normalize_path(path);
        self.file_metadata.insert(path, metadata);
    }

    /// Translate a remote file-relative range into its absolute package range.
    pub fn asset_location(&self, path: &str, offset: u64, len: usize) -> Option<(u64, usize)> {
        let asset = self.assets.get(&self.normalize_path(path))?;
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
        let path = self.normalize_path(path);
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
        let dir = self.normalize_path(path);
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
        let dir = self.normalize_path(path);
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

    /// Remove an empty, non-root guest directory from the virtual namespace.
    pub fn remove_dir(&mut self, path: &str) -> Result<(), RemoveDirError> {
        let dir = self.normalize_path(path);
        if dir.is_empty() {
            return Err(RemoveDirError::AccessDenied);
        }
        if self.files.contains_key(&dir) || self.assets.contains_key(&dir) {
            return Err(RemoveDirError::NotFound);
        }
        if !self.is_dir(&dir) {
            return Err(RemoveDirError::NotFound);
        }
        if !self.directory_entries(&dir).is_empty() || self.has_streamed_assets_below(&dir) {
            return Err(RemoveDirError::NotEmpty);
        }
        if !self.directories.remove(&dir) {
            // An inferred directory cannot be empty; this protects against removing a namespace
            // that is still supplied by immutable package data.
            return Err(RemoveDirError::AccessDenied);
        }
        self.attributes.remove(&dir);
        self.file_metadata.remove(&dir);
        Ok(())
    }
    pub fn remove(&mut self, path: &str) -> bool {
        let path = self.normalize_path(path);
        if self.is_readonly(&path) {
            return false;
        }
        let local = self.files.remove(&path).is_some();
        let asset = self.assets.remove(&path).is_some();
        if local || asset {
            self.attributes.remove(&path);
            let mut parent = path.rsplit_once('\\').map(|(parent, _)| parent);
            while let Some(dir) = parent {
                if self.is_dir(dir) {
                    break;
                }
                self.attributes.remove(dir);
                parent = dir.rsplit_once('\\').map(|(parent, _)| parent);
            }
        }
        local || asset
    }
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys().chain(
            self.assets
                .keys()
                .filter(|path| !self.files.contains_key(*path)),
        )
    }

    /// List immediate file and directory children using the indexed VFS namespace.
    pub fn directory_entries(&self, path: &str) -> Vec<(String, bool, u64)> {
        let directory = self.normalize_path(path);
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}\\")
        };
        let mut entries: HashMap<String, (bool, u64)> = HashMap::new();

        for file in self.paths() {
            let Some(relative) = file.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            if let Some((child, _)) = relative.split_once('\\') {
                entries.entry(child.to_owned()).or_insert((true, 0));
            } else {
                entries.insert(
                    relative.to_owned(),
                    (false, self.file_len(file).unwrap_or(0)),
                );
            }
        }

        for explicit in &self.directories {
            let Some(relative) = explicit.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            let name = relative
                .split_once('\\')
                .map_or(relative, |(child, _)| child);
            entries.entry(name.to_owned()).or_insert((true, 0));
        }

        let mut entries: Vec<_> = entries
            .into_iter()
            .map(|(name, (is_directory, size))| (name, is_directory, size))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
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

    fn metadata_asset_package(entries: &[(&str, &[u8], bool, i64, i64, i64, u32, u16)]) -> Vec<u8> {
        let index_len: usize = entries
            .iter()
            .map(|(name, ..)| 2 + name.len() + 16 + 31)
            .sum();
        let data_start = 24 + index_len;
        let payload_len = entries
            .iter()
            .filter(|(_, _, is_directory, ..)| !*is_directory)
            .map(|(_, data, ..)| data.len())
            .sum::<usize>();
        let package_len = data_start + payload_len;
        let mut out = b"AGP2".to_vec();
        out.extend((entries.len() as u32).to_le_bytes());
        out.extend((index_len as u64).to_le_bytes());
        out.extend((package_len as u64).to_le_bytes());
        let mut offset = data_start;
        for (name, data, is_directory, accessed, modified, created, mode, link_count) in entries {
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(name.as_bytes());
            out.extend((offset as u64).to_le_bytes());
            out.extend((if *is_directory { 0 } else { data.len() } as u64).to_le_bytes());
            out.extend(accessed.to_le_bytes());
            out.extend(modified.to_le_bytes());
            out.extend(created.to_le_bytes());
            out.extend(mode.to_le_bytes());
            out.extend(link_count.to_le_bytes());
            out.push(u8::from(*is_directory));
            if !*is_directory {
                offset += data.len();
            }
        }
        for (_, data, is_directory, ..) in entries {
            if !*is_directory {
                out.extend_from_slice(data);
            }
        }
        out
    }

    #[test]
    fn resolves_windows_paths_against_a_current_directory() {
        assert_eq!(
            absolute("config.ini", "C:\\games\\demo"),
            "C:\\games\\demo\\config.ini"
        );
        assert_eq!(
            absolute("..\\shared\\save.dat", "C:\\games\\demo"),
            "C:\\games\\shared\\save.dat"
        );
        assert_eq!(
            absolute("\\Windows\\win.ini", "C:\\games"),
            "C:\\Windows\\win.ini"
        );
        assert_eq!(
            resolve("C:\\games\\demo\\..\\save.dat", "C:\\"),
            "games\\save.dat"
        );
        assert_eq!(norm("C:\\games\\save.dat"), "games\\save.dat");
        assert_eq!(norm("\\\\?\\C:\\games\\save.dat"), "games\\save.dat");
        assert_eq!(
            absolute("\\\\server\\share\\folder\\..\\save.dat", "C:\\"),
            "\\\\server\\share\\save.dat"
        );
        assert_eq!(
            norm("\\\\?\\UNC\\server\\share\\save.dat"),
            "\\\\server\\share\\save.dat"
        );
        assert_eq!(
            absolute("save.dat", "\\\\server\\share\\game"),
            "\\\\server\\share\\game\\save.dat"
        );
        assert_eq!(
            absolute("\\save.dat", "\\\\server\\share\\game"),
            "\\\\server\\share\\save.dat"
        );
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
        assert_eq!(vfs.create_dir(""), Err(super::CreateDirError::PathNotFound));
    }

    #[test]
    fn removes_only_empty_non_root_guest_directories() {
        use super::RemoveDirError;

        let mut vfs = Vfs::new();
        vfs.insert("games\\data\\level.bin", vec![1]);
        vfs.create_dir("games\\empty").unwrap();
        vfs.create_dir("games\\parent").unwrap();
        vfs.create_dir("games\\parent\\empty").unwrap();

        assert_eq!(vfs.remove_dir(""), Err(RemoveDirError::AccessDenied));
        assert_eq!(vfs.remove_dir("missing"), Err(RemoveDirError::NotFound));
        assert_eq!(vfs.remove_dir("games\\data"), Err(RemoveDirError::NotEmpty));
        assert_eq!(
            vfs.remove_dir("games\\data\\level.bin"),
            Err(RemoveDirError::NotFound)
        );
        assert_eq!(vfs.remove_dir("games\\empty"), Ok(()));
        assert!(!vfs.is_dir("games\\empty"));
        assert_eq!(vfs.remove_dir("games\\parent\\empty"), Ok(()));
        assert!(vfs.is_dir("games\\parent"));
    }

    #[test]
    fn indexed_assets_expose_paths_and_lengths_without_loading_payload() {
        let package = asset_package(&[
            ("data\\handling.cfg", &[4, 5]),
            ("models\\gta3.img", &[1, 2, 3]),
        ]);
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
        assert_eq!(
            vfs.asset_location("models/gta3.img", 1, 10),
            Some((data_start + 3, 2))
        );
    }

    #[test]
    fn metadata_index_preserves_file_and_directory_timestamps_and_modes() {
        let package = metadata_asset_package(&[
            (
                "GameData\\Shared",
                &[],
                true,
                1_600_000_000,
                1_700_000_000,
                1_500_000_000,
                0o040755,
                1,
            ),
            (
                "GameData\\Shared\\asset.bin",
                &[1, 2, 3],
                false,
                1_600_000_001,
                1_700_000_001,
                1_500_000_001,
                0o100444,
                2,
            ),
        ]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();
        vfs.set_root_alias(r"D:\The Sims 3").unwrap();

        assert_eq!(
            vfs.file_status(r"D:\The Sims 3\GameData\Shared\asset.bin"),
            Some(super::FileStatus {
                size: 3,
                is_directory: false,
                accessed: Some(1_600_000_001),
                modified: Some(1_700_000_001),
                created: Some(1_500_000_001),
                mode: Some(0o100444),
                link_count: Some(2),
            })
        );
        assert_eq!(
            vfs.file_status(r"D:\The Sims 3\GameData\Shared"),
            Some(super::FileStatus {
                size: 0,
                is_directory: true,
                accessed: Some(1_600_000_000),
                modified: Some(1_700_000_000),
                created: Some(1_500_000_000),
                mode: Some(0o040755),
                link_count: Some(1),
            })
        );
    }

    #[test]
    fn absolute_host_install_paths_resolve_through_the_direct_root_mount() {
        let package = asset_package(&[
            (r"Game\Bin\TS3W.exe", &[0x4d, 0x5a]),
            (r"GameData\Win32\Resource.cfg", b"Priority 500\n"),
        ]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();
        vfs.set_root_alias(r"D:\The Sims 3\").unwrap();

        let file = r"D:\The Sims 3\GameData\Win32\Resource.cfg";
        let directory = r"D:\The Sims 3\GameData\Win32";
        assert_eq!(vfs.normalize_path(file), r"gamedata\win32\resource.cfg");
        assert!(vfs.exists(file));
        assert_eq!(vfs.file_len(file), Some(b"Priority 500\n".len() as u64));
        assert!(vfs.is_dir(directory));
        assert_eq!(vfs.file_attributes(directory), Some(0x10));
        assert!(!vfs.exists(r"D:\The Sims 30\GameData\Win32\Resource.cfg"));
    }

    #[test]
    fn directory_entries_include_immediate_files_and_synthesized_subdirectories() {
        let package = asset_package(&[
            (r"GameData\Shared\Packages\FullBuild0.package", &[1, 2, 3]),
            (r"GameData\Shared\Resource.cfg", &[4]),
        ]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();
        vfs.set_root_alias(r"D:\The Sims 3").unwrap();
        vfs.create_dir(r"D:\The Sims 3\GameData\Shared\Empty")
            .unwrap();

        assert_eq!(
            vfs.directory_entries(r"D:\The Sims 3\GameData\Shared"),
            vec![
                ("empty".to_owned(), true, 0),
                ("packages".to_owned(), true, 0),
                ("resource.cfg".to_owned(), false, 1),
            ]
        );
    }

    #[test]
    fn direct_root_mount_requires_an_absolute_drive_path() {
        let mut vfs = Vfs::new();
        assert!(vfs.set_root_alias(r"Game\Bin").is_err());
        assert!(vfs.set_root_alias(r"\\server\share\game").is_err());
        assert_eq!(
            vfs.normalize_path(r"D:\The Sims 3\file.dat"),
            r"d:\the sims 3\file.dat"
        );
    }

    #[test]
    fn local_files_override_indexed_assets() {
        let package = asset_package(&[("data\\handling.cfg", &[1, 2])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();
        vfs.insert("data/handling.cfg", vec![9, 8, 7]);

        assert_eq!(vfs.file_len("data\\handling.cfg"), Some(3));
        assert_eq!(
            vfs.read_range("data/handling.cfg", 1, 8).unwrap(),
            vec![8, 7]
        );
        assert_eq!(vfs.paths().count(), 1);
    }

    #[test]
    fn rejects_duplicate_normalized_asset_paths() {
        let package = asset_package(&[("Data\\Handling.cfg", &[1]), ("data/handling.cfg", &[2])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        assert!(Vfs::new()
            .register_asset_index(&package[..index_end])
            .is_err());
    }

    #[test]
    fn rejects_asset_records_outside_the_package_data_region() {
        let mut package = asset_package(&[("data\\file.bin", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let record_offset = 24 + 2 + "data\\file.bin".len();
        package[record_offset..record_offset + 8].copy_from_slice(&0u64.to_le_bytes());
        assert!(Vfs::new()
            .register_asset_index(&package[..index_end])
            .is_err());

        let mut package = asset_package(&[("data\\file.bin", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let package_len = u64::from_le_bytes(package[16..24].try_into().unwrap());
        package[16..24].copy_from_slice(&(package_len - 1).to_le_bytes());
        assert!(Vfs::new()
            .register_asset_index(&package[..index_end])
            .is_err());
    }

    #[test]
    fn streamed_asset_coverage_is_detected_for_watched_directories() {
        let package = asset_package(&[(r"Game\Bin\TS3W.exe", &[1, 2, 3])]);
        let index_end = 24 + u64::from_le_bytes(package[8..16].try_into().unwrap()) as usize;
        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..index_end]).unwrap();

        assert!(vfs.has_streamed_assets_below(r"C:\Game\Bin"));
        assert!(!vfs.has_streamed_assets_below(r"C:\Game\BinOther"));
        assert!(vfs.has_streamed_assets_below(r"C:\"));
    }
}
