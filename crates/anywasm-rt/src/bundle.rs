//! `.awb` application bundle: the executable, its DLLs and data files in one blob, plus a tiny
//! manifest. Layout (little endian): `"AWB1"`, u32 entry count, then per entry
//! `u16 name_len, name (UTF-8), u64 size`, followed by all file contents back to back.
//! The first entry is always `__manifest__` with `key=value` lines (`exe`, `cmdline`).

use crate::Vfs;

#[derive(Default, Clone)]
pub struct Bundle {
    pub exe: String,
    pub cmdline: Option<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

impl Bundle {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut manifest = format!("exe={}\n", self.exe);
        if let Some(c) = &self.cmdline {
            manifest.push_str(&format!("cmdline={}\n", c.replace('\n', " ")));
        }
        let mut entries: Vec<(&str, &[u8])> = vec![("__manifest__", manifest.as_bytes())];
        entries.extend(self.files.iter().map(|(n, d)| (n.as_str(), d.as_slice())));
        let mut out = b"AWB1".to_vec();
        out.extend((entries.len() as u32).to_le_bytes());
        for (n, d) in &entries {
            out.extend((n.len() as u16).to_le_bytes());
            out.extend(n.as_bytes());
            out.extend((d.len() as u64).to_le_bytes());
        }
        for (_, d) in &entries {
            out.extend_from_slice(d);
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Bundle, String> {
        let bad = |w: &str| format!("corrupt bundle: {w}");
        if b.get(..4) != Some(b"AWB1") {
            return Err(bad("bad magic"));
        }
        let n = u32::from_le_bytes(b.get(4..8).ok_or_else(|| bad("header"))?.try_into().unwrap()) as usize;
        let mut p = 8usize;
        let mut dir = Vec::with_capacity(n);
        for _ in 0..n {
            let nl = u16::from_le_bytes(b.get(p..p + 2).ok_or_else(|| bad("dir"))?.try_into().unwrap()) as usize;
            p += 2;
            let name = std::str::from_utf8(b.get(p..p + nl).ok_or_else(|| bad("name"))?).map_err(|_| bad("utf8"))?.to_string();
            p += nl;
            let sz = u64::from_le_bytes(b.get(p..p + 8).ok_or_else(|| bad("size"))?.try_into().unwrap()) as usize;
            p += 8;
            dir.push((name, sz));
        }
        let mut bundle = Bundle::default();
        for (name, sz) in dir {
            let data = b.get(p..p.checked_add(sz).ok_or_else(|| bad("overflow"))?).ok_or_else(|| bad("data"))?;
            p += sz;
            if name == "__manifest__" {
                for line in String::from_utf8_lossy(data).lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        match k {
                            "exe" => bundle.exe = v.to_string(),
                            "cmdline" => bundle.cmdline = Some(v.to_string()),
                            _ => {}
                        }
                    }
                }
            } else {
                bundle.files.push((name, data.to_vec()));
            }
        }
        if bundle.exe.is_empty() {
            return Err(bad("manifest has no exe"));
        }
        Ok(bundle)
    }

    pub fn into_vfs(self) -> (Vfs, String, Option<String>) {
        let mut v = Vfs::new();
        for (n, d) in self.files {
            v.insert(&n, d);
        }
        (v, self.exe, self.cmdline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let b = Bundle { exe: "a.exe".into(), cmdline: Some("\"a.exe\" x".into()), files: vec![("a.exe".into(), vec![1, 2, 3]), ("d\\x.dll".into(), vec![])] };
        let r = Bundle::from_bytes(&b.to_bytes()).unwrap();
        assert_eq!(r.exe, "a.exe");
        assert_eq!(r.cmdline.as_deref(), Some("\"a.exe\" x"));
        assert_eq!(r.files, b.files);
    }

    #[test]
    fn rejects_truncated_and_bad_magic() {
        let good = Bundle { exe: "a.exe".into(), cmdline: None, files: vec![("a.exe".into(), vec![9; 10])] }.to_bytes();
        assert!(Bundle::from_bytes(b"XXXX").is_err());
        for n in 0..good.len() {
            assert!(Bundle::from_bytes(&good[..n]).is_err() || n == good.len(), "prefix {n} accepted");
        }
    }
}
