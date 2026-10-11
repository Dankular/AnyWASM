//! In-memory virtual file system keyed by lower-cased, `\`-normalised paths.
use std::collections::HashMap;

#[derive(Default, Clone)]
pub struct Vfs {
    files: HashMap<String, Vec<u8>>,
}

pub fn norm(path: &str) -> String {
    let mut p = path.replace('/', "\\").to_ascii_lowercase();
    // Strip the \\?\ and \??\ prefixes.
    for pre in ["\\\\?\\", "\\??\\"] {
        if let Some(r) = p.strip_prefix(pre) {
            p = r.to_string();
        }
    }
    p
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
