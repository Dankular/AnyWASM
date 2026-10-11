//! Dependency-free PE32 / PE32+ parser.
//!
//! Scope: everything the AnyWASM loader and analyzer need — headers, sections,
//! import / delay-import tables, exports (including forwarders), base
//! relocations and the TLS directory. All parsing is bounds-checked; malformed
//! input yields an [`Error`], never a panic.

use std::fmt;

pub const MACHINE_I386: u16 = 0x014c;
pub const MACHINE_AMD64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xaa64;

pub const SUBSYSTEM_NATIVE: u16 = 1;
pub const SUBSYSTEM_WINDOWS_GUI: u16 = 2;
pub const SUBSYSTEM_WINDOWS_CUI: u16 = 3;

pub const DIR_EXPORT: usize = 0;
pub const DIR_IMPORT: usize = 1;
pub const DIR_RESOURCE: usize = 2;
pub const DIR_BASERELOC: usize = 5;
pub const DIR_EXCEPTION: usize = 3;
pub const DIR_TLS: usize = 9;
pub const DIR_DELAY_IMPORT: usize = 13;
pub const DIR_COM_DESCRIPTOR: usize = 14;

pub const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const SCN_MEM_READ: u32 = 0x4000_0000;
pub const SCN_MEM_WRITE: u32 = 0x8000_0000;

pub const REL_BASED_ABSOLUTE: u8 = 0;
pub const REL_BASED_HIGH: u8 = 1;
pub const REL_BASED_LOW: u8 = 2;
pub const REL_BASED_HIGHLOW: u8 = 3;
pub const REL_BASED_DIR64: u8 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated(&'static str),
    BadMagic(&'static str),
    Unsupported(&'static str),
    BadRva(u32),
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated(w) => write!(f, "truncated PE while reading {w}"),
            Error::BadMagic(w) => write!(f, "bad magic: {w}"),
            Error::Unsupported(w) => write!(f, "unsupported PE feature: {w}"),
            Error::BadRva(r) => write!(f, "RVA {r:#x} is outside the image"),
            Error::Malformed(w) => write!(f, "malformed PE: {w}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, Default)]
pub struct DataDir {
    pub rva: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn executable(&self) -> bool {
        self.characteristics & SCN_MEM_EXECUTE != 0
    }
    pub fn writable(&self) -> bool {
        self.characteristics & SCN_MEM_WRITE != 0
    }
    pub fn readable(&self) -> bool {
        self.characteristics & SCN_MEM_READ != 0
    }
    /// Size the section occupies once mapped (virtual size, falling back to raw size).
    pub fn mapped_size(&self) -> u32 {
        if self.virtual_size != 0 {
            self.virtual_size
        } else {
            self.raw_size
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportName {
    Name { hint: u16, name: String },
    Ordinal(u16),
}

impl fmt::Display for ImportName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportName::Name { name, .. } => f.write_str(name),
            ImportName::Ordinal(o) => write!(f, "#{o}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    pub name: ImportName,
    /// RVA of the IAT slot the loader must fill.
    pub iat_rva: u32,
}

#[derive(Debug, Clone)]
pub struct ImportDll {
    /// DLL name exactly as written in the file.
    pub dll: String,
    pub entries: Vec<ImportEntry>,
    pub delayed: bool,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub name: Option<String>,
    pub ordinal: u16,
    pub rva: u32,
    /// `Some("DLL.Func")` when the export is a forwarder.
    pub forwarder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloc {
    pub rva: u32,
    pub kind: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub begin_address: u32,
    pub end_address: u32,
    pub unwind_info_address: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Tls {
    pub start_raw_data: u64,
    pub end_raw_data: u64,
    pub index_addr: u64,
    pub callbacks_addr: u64,
    pub zero_fill: u32,
}

#[derive(Debug, Clone)]
pub struct Pe<'a> {
    data: &'a [u8],
    pub machine: u16,
    pub is_64: bool,
    pub characteristics: u16,
    pub image_base: u64,
    pub entry_rva: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub section_alignment: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub stack_reserve: u64,
    pub stack_commit: u64,
    pub major_os_version: u16,
    pub dirs: [DataDir; 16],
    pub sections: Vec<Section>,
}

fn rd_u16(d: &[u8], o: usize, what: &'static str) -> Result<u16> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(Error::Truncated(what))
}
fn rd_u32(d: &[u8], o: usize, what: &'static str) -> Result<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Error::Truncated(what))
}
fn rd_u64(d: &[u8], o: usize, what: &'static str) -> Result<u64> {
    d.get(o..o + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or(Error::Truncated(what))
}

impl<'a> Pe<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 0x40 || &data[0..2] != b"MZ" {
            return Err(Error::BadMagic("missing MZ header"));
        }
        let e_lfanew = rd_u32(data, 0x3c, "e_lfanew")? as usize;
        if data.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0") {
            return Err(Error::BadMagic("missing PE signature"));
        }
        let coff = e_lfanew + 4;
        let machine = rd_u16(data, coff, "machine")?;
        let nsections = rd_u16(data, coff + 2, "section count")? as usize;
        let opt_size = rd_u16(data, coff + 16, "optional header size")? as usize;
        let characteristics = rd_u16(data, coff + 18, "characteristics")?;
        let opt = coff + 20;
        if opt_size < 96 {
            return Err(Error::Malformed("optional header too small"));
        }
        let magic = rd_u16(data, opt, "optional magic")?;
        let is_64 = match magic {
            0x10b => false,
            0x20b => true,
            _ => return Err(Error::BadMagic("optional header magic")),
        };
        let entry_rva = rd_u32(data, opt + 16, "entry point")?;
        let (image_base, ndirs_off, stack_reserve, stack_commit) = if is_64 {
            (
                rd_u64(data, opt + 24, "image base")?,
                opt + 108,
                rd_u64(data, opt + 72, "stack reserve")?,
                rd_u64(data, opt + 80, "stack commit")?,
            )
        } else {
            (
                rd_u32(data, opt + 28, "image base")? as u64,
                opt + 92,
                rd_u32(data, opt + 72, "stack reserve")? as u64,
                rd_u32(data, opt + 76, "stack commit")? as u64,
            )
        };
        let section_alignment = rd_u32(data, opt + 32, "section alignment")?;
        let major_os_version = rd_u16(data, opt + 40, "os version")?;
        let size_of_image = rd_u32(data, opt + 56, "size of image")?;
        let size_of_headers = rd_u32(data, opt + 60, "size of headers")?;
        let subsystem = rd_u16(data, opt + 68, "subsystem")?;
        let dll_characteristics = rd_u16(data, opt + 70, "dll characteristics")?;
        let ndirs = (rd_u32(data, ndirs_off, "dir count")? as usize).min(16);
        let mut dirs = [DataDir::default(); 16];
        for (i, d) in dirs.iter_mut().enumerate().take(ndirs) {
            let o = ndirs_off + 4 + i * 8;
            if o + 8 > opt + opt_size {
                break;
            }
            d.rva = rd_u32(data, o, "data dir rva")?;
            d.size = rd_u32(data, o + 4, "data dir size")?;
        }

        let sec_off = opt + opt_size;
        let mut sections = Vec::with_capacity(nsections);
        for i in 0..nsections {
            let o = sec_off + i * 40;
            let raw = data
                .get(o..o + 40)
                .ok_or(Error::Truncated("section header"))?;
            let end = raw[..8].iter().position(|&b| b == 0).unwrap_or(8);
            sections.push(Section {
                name: String::from_utf8_lossy(&raw[..end]).into_owned(),
                virtual_size: rd_u32(data, o + 8, "vsize")?,
                virtual_address: rd_u32(data, o + 12, "vaddr")?,
                raw_size: rd_u32(data, o + 16, "rawsize")?,
                raw_offset: rd_u32(data, o + 20, "rawoff")?,
                characteristics: rd_u32(data, o + 36, "scn flags")?,
            });
        }

        Ok(Pe {
            data,
            machine,
            is_64,
            characteristics,
            image_base,
            entry_rva,
            size_of_image,
            size_of_headers,
            section_alignment,
            subsystem,
            dll_characteristics,
            stack_reserve,
            stack_commit,
            major_os_version,
            dirs,
            sections,
        })
    }

    pub fn raw(&self) -> &'a [u8] {
        self.data
    }

    pub fn is_dll(&self) -> bool {
        self.characteristics & 0x2000 != 0
    }

    pub fn is_dotnet(&self) -> bool {
        self.dirs[DIR_COM_DESCRIPTOR].rva != 0
    }

    pub fn has_relocs(&self) -> bool {
        self.dirs[DIR_BASERELOC].size != 0
    }

    /// DYNAMIC_BASE flag: image may be loaded at any address.
    pub fn dynamic_base(&self) -> bool {
        self.dll_characteristics & 0x0040 != 0
    }

    /// Translate an RVA to a file offset (only valid for RVAs backed by raw data).
    pub fn rva_to_offset(&self, rva: u32) -> Result<usize> {
        if rva < self.size_of_headers {
            return Ok(rva as usize);
        }
        for s in &self.sections {
            let span = s.raw_size.min(s.mapped_size());
            if rva >= s.virtual_address && rva - s.virtual_address < span {
                return Ok(s.raw_offset as usize + (rva - s.virtual_address) as usize);
            }
        }
        Err(Error::BadRva(rva))
    }

    /// Slice of file bytes starting at `rva`, at most `len` long and never crossing the
    /// end of the backing section.
    pub fn slice_at_rva(&self, rva: u32, len: usize) -> Result<&'a [u8]> {
        let off = self.rva_to_offset(rva)?;
        let end = off.checked_add(len).ok_or(Error::BadRva(rva))?;
        self.data.get(off..end).ok_or(Error::Truncated("rva slice"))
    }

    pub fn u16_at_rva(&self, rva: u32) -> Result<u16> {
        rd_u16(self.data, self.rva_to_offset(rva)?, "u16@rva")
    }
    pub fn u32_at_rva(&self, rva: u32) -> Result<u32> {
        rd_u32(self.data, self.rva_to_offset(rva)?, "u32@rva")
    }
    pub fn u64_at_rva(&self, rva: u32) -> Result<u64> {
        rd_u64(self.data, self.rva_to_offset(rva)?, "u64@rva")
    }

    pub fn cstr_at_rva(&self, rva: u32) -> Result<String> {
        let off = self.rva_to_offset(rva)?;
        let tail = self.data.get(off..).ok_or(Error::BadRva(rva))?;
        let end = tail
            .iter()
            .take(4096)
            .position(|&b| b == 0)
            .ok_or(Error::Malformed("unterminated string"))?;
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    }

    /// Normal (non-delay) import table.
    pub fn imports(&self) -> Result<Vec<ImportDll>> {
        let dir = self.dirs[DIR_IMPORT];
        let mut out = Vec::new();
        if dir.rva == 0 {
            return Ok(out);
        }
        let mut desc = dir.rva;
        loop {
            let oft = self.u32_at_rva(desc)?;
            let name_rva = self.u32_at_rva(desc + 12)?;
            let first_thunk = self.u32_at_rva(desc + 16)?;
            if oft == 0 && name_rva == 0 && first_thunk == 0 {
                break;
            }
            let dll = self.cstr_at_rva(name_rva)?;
            let lookup = if oft != 0 { oft } else { first_thunk };
            let entries = self.read_thunks(lookup, first_thunk)?;
            out.push(ImportDll {
                dll,
                entries,
                delayed: false,
            });
            desc += 20;
            if out.len() > 4096 {
                return Err(Error::Malformed("import table not terminated"));
            }
        }
        Ok(out)
    }

    /// Delay-load import table (RVA-based descriptors only).
    pub fn delay_imports(&self) -> Result<Vec<ImportDll>> {
        let dir = self.dirs[DIR_DELAY_IMPORT];
        let mut out = Vec::new();
        if dir.rva == 0 {
            return Ok(out);
        }
        let mut desc = dir.rva;
        loop {
            let attrs = self.u32_at_rva(desc)?;
            let name_rva = self.u32_at_rva(desc + 4)?;
            let iat = self.u32_at_rva(desc + 12)?;
            let int = self.u32_at_rva(desc + 16)?;
            if attrs == 0 && name_rva == 0 && iat == 0 && int == 0 {
                break;
            }
            if attrs & 1 == 0 {
                return Err(Error::Unsupported("VA-based delay-import descriptors"));
            }
            let dll = self.cstr_at_rva(name_rva)?;
            let entries = self.read_thunks(int, iat)?;
            out.push(ImportDll {
                dll,
                entries,
                delayed: true,
            });
            desc += 32;
            if out.len() > 4096 {
                return Err(Error::Malformed("delay-import table not terminated"));
            }
        }
        Ok(out)
    }

    /// All imports, normal followed by delay-loaded.
    pub fn all_imports(&self) -> Result<Vec<ImportDll>> {
        let mut v = self.imports()?;
        v.extend(self.delay_imports()?);
        Ok(v)
    }

    fn read_thunks(&self, lookup_rva: u32, iat_rva: u32) -> Result<Vec<ImportEntry>> {
        let stride: u32 = if self.is_64 { 8 } else { 4 };
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let at = lookup_rva + i * stride;
            let raw = if self.is_64 {
                self.u64_at_rva(at)?
            } else {
                self.u32_at_rva(at)? as u64
            };
            if raw == 0 {
                break;
            }
            let ord_flag = if self.is_64 { 1u64 << 63 } else { 1u64 << 31 };
            let name = if raw & ord_flag != 0 {
                ImportName::Ordinal((raw & 0xffff) as u16)
            } else {
                let hn = (raw & 0x7fff_ffff) as u32;
                ImportName::Name {
                    hint: self.u16_at_rva(hn)?,
                    name: self.cstr_at_rva(hn + 2)?,
                }
            };
            out.push(ImportEntry {
                name,
                iat_rva: iat_rva + i * stride,
            });
            i += 1;
            if i > 1_000_000 {
                return Err(Error::Malformed("import thunk list not terminated"));
            }
        }
        Ok(out)
    }

    /// Export directory.
    pub fn exports(&self) -> Result<Vec<Export>> {
        let dir = self.dirs[DIR_EXPORT];
        if dir.rva == 0 {
            return Ok(Vec::new());
        }
        let ord_base = self.u32_at_rva(dir.rva + 16)?;
        let nfuncs = self.u32_at_rva(dir.rva + 20)? as usize;
        let nnames = self.u32_at_rva(dir.rva + 24)? as usize;
        let funcs = self.u32_at_rva(dir.rva + 28)?;
        let names = self.u32_at_rva(dir.rva + 32)?;
        let ords = self.u32_at_rva(dir.rva + 36)?;
        if nfuncs > 0x10_0000 || nnames > 0x10_0000 {
            return Err(Error::Malformed("absurd export count"));
        }
        let mut by_index: Vec<Option<String>> = vec![None; nfuncs];
        for i in 0..nnames {
            let name_rva = self.u32_at_rva(names + (i as u32) * 4)?;
            let idx = self.u16_at_rva(ords + (i as u32) * 2)? as usize;
            if idx < nfuncs {
                by_index[idx] = Some(self.cstr_at_rva(name_rva)?);
            }
        }
        let mut out = Vec::new();
        for (i, name) in by_index.into_iter().enumerate() {
            let rva = self.u32_at_rva(funcs + (i as u32) * 4)?;
            if rva == 0 {
                continue;
            }
            let forwarder = if rva >= dir.rva && rva < dir.rva + dir.size {
                Some(self.cstr_at_rva(rva)?)
            } else {
                None
            };
            out.push(Export {
                name,
                ordinal: (ord_base as usize + i) as u16,
                rva,
                forwarder,
            });
        }
        Ok(out)
    }

    /// Base relocation entries (ABSOLUTE padding entries are skipped).
    pub fn relocations(&self) -> Result<Vec<Reloc>> {
        let dir = self.dirs[DIR_BASERELOC];
        let mut out = Vec::new();
        if dir.rva == 0 || dir.size == 0 {
            return Ok(out);
        }
        let mut pos = 0u32;
        while pos + 8 <= dir.size {
            let page = self.u32_at_rva(dir.rva + pos)?;
            let blk = self.u32_at_rva(dir.rva + pos + 4)?;
            if blk < 8 || pos + blk > dir.size {
                return Err(Error::Malformed("bad relocation block size"));
            }
            for i in 0..((blk - 8) / 2) {
                let e = self.u16_at_rva(dir.rva + pos + 8 + i * 2)?;
                let kind = (e >> 12) as u8;
                if kind == REL_BASED_ABSOLUTE {
                    continue;
                }
                out.push(Reloc {
                    rva: page + (e & 0x0fff) as u32,
                    kind,
                });
            }
            pos += blk;
        }
        Ok(out)
    }

    /// TLS directory (addresses are VAs relative to `image_base`).
    pub fn tls(&self) -> Result<Option<Tls>> {
        let dir = self.dirs[DIR_TLS];
        if dir.rva == 0 {
            return Ok(None);
        }
        let t = if self.is_64 {
            Tls {
                start_raw_data: self.u64_at_rva(dir.rva)?,
                end_raw_data: self.u64_at_rva(dir.rva + 8)?,
                index_addr: self.u64_at_rva(dir.rva + 16)?,
                callbacks_addr: self.u64_at_rva(dir.rva + 24)?,
                zero_fill: self.u32_at_rva(dir.rva + 32)?,
            }
        } else {
            Tls {
                start_raw_data: self.u32_at_rva(dir.rva)? as u64,
                end_raw_data: self.u32_at_rva(dir.rva + 4)? as u64,
                index_addr: self.u32_at_rva(dir.rva + 8)? as u64,
                callbacks_addr: self.u32_at_rva(dir.rva + 12)? as u64,
                zero_fill: self.u32_at_rva(dir.rva + 16)?,
            }
        };
        Ok(Some(t))
    }

    /// TLS callback VAs (null-terminated array in the image).
    pub fn tls_callbacks(&self) -> Result<Vec<u64>> {
        let Some(t) = self.tls()? else {
            return Ok(Vec::new());
        };
        if t.callbacks_addr == 0 {
            return Ok(Vec::new());
        }
        let rva = (t.callbacks_addr - self.image_base) as u32;
        let mut out = Vec::new();
        for i in 0..256u32 {
            let v = if self.is_64 {
                self.u64_at_rva(rva + i * 8)?
            } else {
                self.u32_at_rva(rva + i * 4)? as u64
            };
            if v == 0 {
                break;
            }
            out.push(v);
        }
        Ok(out)
    }

    /// Win64 `.pdata` function entries from the PE exception directory.
    pub fn runtime_functions(&self) -> Result<Vec<RuntimeFunction>> {
        if !self.is_64 {
            return Ok(Vec::new());
        }
        let dir = self.dirs[DIR_EXCEPTION];
        if dir.rva == 0 || dir.size == 0 {
            return Ok(Vec::new());
        }
        if dir.size % 12 != 0 {
            return Err(Error::Malformed(
                "exception directory size is not a multiple of 12",
            ));
        }
        let bytes = self.slice_at_rva(dir.rva, dir.size as usize)?;
        let mut functions = Vec::with_capacity((dir.size / 12) as usize);
        let mut previous_begin = None;
        for entry in bytes.chunks_exact(12) {
            let function = RuntimeFunction {
                begin_address: u32::from_le_bytes(entry[0..4].try_into().unwrap()),
                end_address: u32::from_le_bytes(entry[4..8].try_into().unwrap()),
                unwind_info_address: u32::from_le_bytes(entry[8..12].try_into().unwrap()),
            };
            if function.begin_address >= function.end_address
                || function.unwind_info_address == 0
                || previous_begin.is_some_and(|previous| function.begin_address <= previous)
            {
                return Err(Error::Malformed(
                    "invalid or unsorted runtime function table",
                ));
            }
            previous_begin = Some(function.begin_address);
            functions.push(function);
        }
        Ok(functions)
    }
}

/// Lower-case a DLL name and strip a trailing `.dll` — the canonical key used for lookups.
pub fn canonical_dll(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    l.strip_suffix(".dll").map(str::to_owned).unwrap_or(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe_with_runtime_functions(entries: &[(u32, u32, u32)], dir_size: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x400];
        let put16 = |bytes: &mut [u8], offset: usize, value: u16| {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put32 = |bytes: &mut [u8], offset: usize, value: u32| {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        bytes[0..2].copy_from_slice(b"MZ");
        put32(&mut bytes, 0x3c, 0x80);
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        put16(&mut bytes, 0x84, MACHINE_AMD64);
        put16(&mut bytes, 0x86, 1);
        put16(&mut bytes, 0x94, 0xf0);
        let opt = 0x98;
        put16(&mut bytes, opt, 0x20b);
        put64(&mut bytes, opt + 24, 0x1400_0000_0);
        put32(&mut bytes, opt + 32, 0x1000);
        put32(&mut bytes, opt + 36, 0x200);
        put32(&mut bytes, opt + 56, 0x2000);
        put32(&mut bytes, opt + 60, 0x200);
        put16(&mut bytes, opt + 68, SUBSYSTEM_WINDOWS_CUI);
        put32(&mut bytes, opt + 108, 16);
        put32(&mut bytes, opt + 112 + DIR_EXCEPTION * 8, 0x1000);
        put32(&mut bytes, opt + 116 + DIR_EXCEPTION * 8, dir_size);
        let sec = opt + 0xf0;
        bytes[sec..sec + 6].copy_from_slice(b".pdata");
        put32(&mut bytes, sec + 8, 0x1000);
        put32(&mut bytes, sec + 12, 0x1000);
        put32(&mut bytes, sec + 16, 0x200);
        put32(&mut bytes, sec + 20, 0x200);
        put32(&mut bytes, sec + 36, SCN_MEM_READ);
        for (index, (begin, end, unwind)) in entries.iter().enumerate() {
            let offset = 0x200 + index * 12;
            put32(&mut bytes, offset, *begin);
            put32(&mut bytes, offset + 4, *end);
            put32(&mut bytes, offset + 8, *unwind);
        }
        bytes
    }

    fn put64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn parses_sorted_pe64_runtime_function_entries() {
        let bytes = pe_with_runtime_functions(&[(0x100, 0x120, 0x300), (0x120, 0x140, 0x310)], 24);
        let pe = Pe::parse(&bytes).unwrap();

        assert_eq!(
            pe.runtime_functions().unwrap(),
            vec![
                RuntimeFunction {
                    begin_address: 0x100,
                    end_address: 0x120,
                    unwind_info_address: 0x300,
                },
                RuntimeFunction {
                    begin_address: 0x120,
                    end_address: 0x140,
                    unwind_info_address: 0x310,
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_and_unsorted_runtime_function_tables() {
        let malformed_bytes = pe_with_runtime_functions(&[], 13);
        let malformed = Pe::parse(&malformed_bytes).unwrap();
        assert_eq!(
            malformed.runtime_functions().unwrap_err(),
            Error::Malformed("exception directory size is not a multiple of 12")
        );

        let unsorted_bytes =
            pe_with_runtime_functions(&[(0x120, 0x140, 0x310), (0x100, 0x120, 0x300)], 24);
        let unsorted = Pe::parse(&unsorted_bytes).unwrap();
        assert_eq!(
            unsorted.runtime_functions().unwrap_err(),
            Error::Malformed("invalid or unsorted runtime function table")
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(Pe::parse(b"").is_err());
        assert!(Pe::parse(&[0u8; 64]).is_err());
        let mut mz = vec![0u8; 0x100];
        mz[0] = b'M';
        mz[1] = b'Z';
        mz[0x3c] = 0x80;
        assert_eq!(
            Pe::parse(&mz).unwrap_err(),
            Error::BadMagic("missing PE signature")
        );
    }

    #[test]
    fn truncated_never_panics() {
        // Walk every prefix of a real fixture (if present) — must return Err, not panic.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/hello.exe");
        if let Ok(bytes) = std::fs::read(path) {
            for n in 0..bytes.len().min(2048) {
                let _ = Pe::parse(&bytes[..n]).and_then(|p| {
                    let _ = p.all_imports();
                    let _ = p.exports();
                    let _ = p.relocations();
                    let _ = p.tls();
                    Ok(())
                });
            }
        }
    }

    #[test]
    fn canonical_names() {
        assert_eq!(canonical_dll("KERNEL32.dll"), "kernel32");
        assert_eq!(
            canonical_dll("api-ms-win-crt-runtime-l1-1-0.dll"),
            "api-ms-win-crt-runtime-l1-1-0"
        );
        assert_eq!(canonical_dll("d3d11"), "d3d11");
    }
}
