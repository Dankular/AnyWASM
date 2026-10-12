//! Paged guest memory with per-page R/W/X permissions.
//!
//! Three-level direct page table over a 48-bit virtual address space (12 + 12 + 12 bits of page
//! number); nodes are allocated lazily so an almost-empty address space costs a few KiB. Frames
//! live in one growing `Vec<u8>`. A one-entry software TLB per access kind keeps the hot path to a
//! compare and an index.

use std::cell::Cell;

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;
pub const VA_BITS: u32 = 48;

pub const PROT_NONE: u8 = 0;
pub const PROT_R: u8 = 1;
pub const PROT_W: u8 = 2;
pub const PROT_X: u8 = 4;
pub const PROT_RW: u8 = PROT_R | PROT_W;
pub const PROT_RWX: u8 = PROT_R | PROT_W | PROT_X;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Exec,
}

/// A guest memory fault: access to an unmapped page or a permission violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub addr: u64,
    pub access: Access,
    /// `true` when the page exists but lacks the permission (as opposed to being unmapped).
    pub protection: bool,
}

pub type MemResult<T> = Result<T, Fault>;

const L_BITS: u32 = 12;
const L_SIZE: usize = 1 << L_BITS;

/// One leaf slot: bit 0 present, bits 1..4 prot (R/W/X), bits 32.. frame index.
type Slot = u64;
const PRESENT: u64 = 1;

/// Stable translation-cache entry shared with the wasm32 emitted-block fast path.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct TlbEntry {
    pub page: u64,
    pub offset: u64,
    pub prot: u8,
}

impl TlbEntry {
    const INVALID: Self = Self {
        page: u64::MAX,
        offset: 0,
        prot: 0,
    };
}

const _: [(); 24] = [(); std::mem::size_of::<TlbEntry>()];
const _: [(); 0] = [(); std::mem::offset_of!(TlbEntry, page)];
const _: [(); 8] = [(); std::mem::offset_of!(TlbEntry, offset)];
const _: [(); 16] = [(); std::mem::offset_of!(TlbEntry, prot)];

#[cfg(target_arch = "wasm32")]
pub(crate) struct JitMemoryView {
    pub frames: u32,
    pub frames_len: u32,
    pub tlb_read: u32,
    pub tlb_write: u32,
}

struct Leaf(Box<[Slot; L_SIZE]>);
struct Mid(Vec<Option<Leaf>>);

pub struct Memory {
    top: Vec<Option<Mid>>,
    frames: Vec<u8>,
    free_frames: Vec<u32>,
    pages_mapped: u64,
    /// Bumped on every write to a page that has ever been fetched from (self-modifying code
    /// detection for future translation caches) and on every protection change.
    pub code_epoch: u64,
    // One-entry software TLB per access kind; the entry layout is also read by emitted WASM.
    tlb_r: Cell<TlbEntry>,
    tlb_w: Cell<TlbEntry>,
    /// Number of mapped pages that currently have PROT_X (0 lets writes skip SMC tracking).
    exec_page_count: u64,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn split(page: u64) -> (usize, usize, usize) {
    (
        ((page >> (2 * L_BITS)) & (L_SIZE as u64 - 1)) as usize,
        ((page >> L_BITS) & (L_SIZE as u64 - 1)) as usize,
        (page & (L_SIZE as u64 - 1)) as usize,
    )
}

impl Memory {
    pub fn new() -> Self {
        let mut top = Vec::new();
        top.resize_with(L_SIZE, || None);
        Memory {
            top,
            frames: Vec::new(),
            free_frames: Vec::new(),
            pages_mapped: 0,
            code_epoch: 0,
            tlb_r: Cell::new(TlbEntry::INVALID),
            tlb_w: Cell::new(TlbEntry::INVALID),
            exec_page_count: 0,
        }
    }

    pub fn pages_mapped(&self) -> u64 {
        self.pages_mapped
    }

    fn flush_tlb(&self) {
        self.tlb_r.set(TlbEntry::INVALID);
        self.tlb_w.set(TlbEntry::INVALID);
    }

    fn slot(&self, page: u64) -> Option<Slot> {
        if page >> (3 * L_BITS) != 0 {
            return None;
        }
        let (a, b, c) = split(page);
        let s = self.top[a].as_ref()?.0[b].as_ref()?.0[c];
        (s & PRESENT != 0).then_some(s)
    }

    fn slot_mut(&mut self, page: u64, create: bool) -> Option<&mut Slot> {
        if page >> (3 * L_BITS) != 0 {
            return None;
        }
        let (a, b, c) = split(page);
        if self.top[a].is_none() {
            if !create {
                return None;
            }
            let mut v = Vec::new();
            v.resize_with(L_SIZE, || None);
            self.top[a] = Some(Mid(v));
        }
        let mid = self.top[a].as_mut().unwrap();
        if mid.0[b].is_none() {
            if !create {
                return None;
            }
            mid.0[b] = Some(Leaf(Box::new([0; L_SIZE])));
        }
        Some(&mut mid.0[b].as_mut().unwrap().0[c])
    }

    fn alloc_frame(&mut self) -> u32 {
        if let Some(f) = self.free_frames.pop() {
            let o = f as usize * PAGE_SIZE as usize;
            self.frames[o..o + PAGE_SIZE as usize].fill(0);
            return f;
        }
        let f = (self.frames.len() / PAGE_SIZE as usize) as u32;
        self.frames
            .resize(self.frames.len() + PAGE_SIZE as usize, 0);
        f
    }

    /// Map `[addr, addr+len)` (page-rounded) with `prot`, zero-filled. Existing pages in the
    /// range are replaced. Returns `false` if the range leaves the 48-bit address space.
    pub fn map(&mut self, addr: u64, len: u64, prot: u8) -> bool {
        let start = addr >> 12;
        let end = (addr.saturating_add(len).saturating_add(PAGE_MASK)) >> 12;
        if end > (1u64 << (3 * L_BITS)) {
            return false;
        }
        for p in start..end {
            self.unmap_page(p);
            let f = self.alloc_frame();
            let s = self.slot_mut(p, true).unwrap();
            *s = PRESENT | ((prot as u64 & 7) << 1) | ((f as u64) << 32);
            self.pages_mapped += 1;
            if prot & PROT_X != 0 {
                self.exec_page_count += 1;
            }
        }
        self.flush_tlb();
        self.code_epoch += 1;
        true
    }

    fn unmap_page(&mut self, p: u64) {
        let old = match self.slot_mut(p, false) {
            Some(s) if *s & PRESENT != 0 => std::mem::replace(s, 0),
            _ => return,
        };
        self.free_frames.push((old >> 32) as u32);
        self.pages_mapped -= 1;
        if (old >> 1) & PROT_X as u64 != 0 {
            self.exec_page_count -= 1;
        }
    }

    pub fn unmap(&mut self, addr: u64, len: u64) {
        let start = addr >> 12;
        let end = (addr.saturating_add(len).saturating_add(PAGE_MASK)) >> 12;
        for p in start..end.min(1u64 << (3 * L_BITS)) {
            self.unmap_page(p);
        }
        self.flush_tlb();
        self.code_epoch += 1;
    }

    /// Change protection; returns `false` if any page in the range is unmapped.
    pub fn protect(&mut self, addr: u64, len: u64, prot: u8) -> bool {
        let start = addr >> 12;
        let end = (addr.saturating_add(len).saturating_add(PAGE_MASK)) >> 12;
        for p in start..end {
            if self.slot(p).is_none() {
                return false;
            }
        }
        for p in start..end {
            let s = self.slot_mut(p, false).unwrap();
            let was_x = (*s >> 1) & PROT_X as u64 != 0;
            *s = (*s & !(7 << 1)) | ((prot as u64 & 7) << 1);
            let now_x = prot & PROT_X != 0;
            match (was_x, now_x) {
                (false, true) => self.exec_page_count += 1,
                (true, false) => self.exec_page_count -= 1,
                _ => {}
            }
        }
        self.flush_tlb();
        self.code_epoch += 1;
        true
    }

    pub fn is_mapped(&self, addr: u64) -> bool {
        self.slot(addr >> 12).is_some()
    }

    /// Protection of the page containing `addr`, or `None` if unmapped.
    pub fn prot_of(&self, addr: u64) -> Option<u8> {
        self.slot(addr >> 12).map(|s| ((s >> 1) & 7) as u8)
    }

    /// Find a free, page-aligned region of `len` bytes at or above `hint` (first fit).
    pub fn find_free(&self, hint: u64, len: u64) -> Option<u64> {
        let pages = (len + PAGE_MASK) >> 12;
        let mut p = hint >> 12;
        let limit = 1u64 << (3 * L_BITS);
        'outer: while p + pages <= limit {
            for i in 0..pages {
                if self.slot(p + i).is_some() {
                    p = p + i + 1;
                    continue 'outer;
                }
            }
            return Some(p << 12);
        }
        None
    }

    // ── translation ──────────────────────────────────────────────────────────

    #[inline]
    fn translate(&self, addr: u64, acc: Access) -> MemResult<usize> {
        let page = addr >> 12;
        let tlb = match acc {
            Access::Write => &self.tlb_w,
            _ => &self.tlb_r,
        };
        let cached = tlb.get();
        if cached.page == page && acc != Access::Exec {
            return Ok(cached.offset as usize + (addr & PAGE_MASK) as usize);
        }
        let s = self.slot(page).ok_or(Fault {
            addr,
            access: acc,
            protection: false,
        })?;
        let prot = ((s >> 1) & 7) as u8;
        let need = match acc {
            Access::Read => PROT_R,
            Access::Write => PROT_W,
            Access::Exec => PROT_X,
        };
        // Reads are allowed from executable pages (no execute-only pages in practice).
        let ok = prot & need != 0 || (acc == Access::Read && prot & PROT_X != 0);
        if !ok {
            return Err(Fault {
                addr,
                access: acc,
                protection: true,
            });
        }
        let off = (s >> 32) as usize * PAGE_SIZE as usize;
        if acc != Access::Exec {
            tlb.set(TlbEntry {
                page,
                offset: off as u64,
                prot,
            });
        }
        Ok(off + (addr & PAGE_MASK) as usize)
    }

    /// Stable test-only snapshot of every mapped guest page: guest page number (u64 LE),
    /// permissions (u8), then the full page contents. Pages are ordered by guest address.
    #[cfg(feature = "jit-test-api")]
    pub fn test_snapshot_mapped_pages(&self) -> Vec<u8> {
        const RECORD_SIZE: usize = 8 + 1 + PAGE_SIZE as usize;
        let mut snapshot = Vec::with_capacity(self.pages_mapped as usize * RECORD_SIZE);
        for (a, mid) in self.top.iter().enumerate() {
            let Some(mid) = mid else { continue };
            for (b, leaf) in mid.0.iter().enumerate() {
                let Some(leaf) = leaf else { continue };
                for (c, slot) in leaf.0.iter().copied().enumerate() {
                    if slot & PRESENT == 0 {
                        continue;
                    }
                    let page = ((a as u64) << (2 * L_BITS)) | ((b as u64) << L_BITS) | c as u64;
                    let frame = (slot >> 32) as usize * PAGE_SIZE as usize;
                    snapshot.extend_from_slice(&page.to_le_bytes());
                    snapshot.push(((slot >> 1) & 7) as u8);
                    snapshot.extend_from_slice(&self.frames[frame..frame + PAGE_SIZE as usize]);
                }
            }
        }
        debug_assert_eq!(snapshot.len(), self.pages_mapped as usize * RECORD_SIZE);
        snapshot
    }

    /// Pointers needed by emitted modules. The caller must keep this `Memory` borrowed for the
    /// complete synchronous module execution so the backing vector cannot be reallocated.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn jit_view(&mut self) -> JitMemoryView {
        JitMemoryView {
            frames: self.frames.as_mut_ptr() as usize as u32,
            frames_len: self.frames.len() as u32,
            tlb_read: self.tlb_r.as_ptr() as usize as u32,
            tlb_write: self.tlb_w.as_ptr() as usize as u32,
        }
    }

    // ── byte-granular access ─────────────────────────────────────────────────

    /// Copy `out.len()` bytes starting at `addr` into `out`, checking `acc` on every page.
    pub fn read_into(&self, addr: u64, out: &mut [u8], acc: Access) -> MemResult<()> {
        let mut a = addr;
        let mut done = 0;
        while done < out.len() {
            let off = self.translate(a, acc)?;
            let n = ((PAGE_SIZE - (a & PAGE_MASK)) as usize).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.frames[off..off + n]);
            done += n;
            a = a.wrapping_add(n as u64);
        }
        Ok(())
    }

    pub fn write_from(&mut self, addr: u64, src: &[u8]) -> MemResult<()> {
        let mut a = addr;
        let mut done = 0;
        while done < src.len() {
            let off = self.translate(a, Access::Write)?;
            let n = ((PAGE_SIZE - (a & PAGE_MASK)) as usize).min(src.len() - done);
            self.frames[off..off + n].copy_from_slice(&src[done..done + n]);
            if self.exec_page_count != 0
                && self
                    .slot(a >> 12)
                    .is_some_and(|s| (s >> 1) & PROT_X as u64 != 0)
            {
                self.code_epoch += 1;
            }
            done += n;
            a = a.wrapping_add(n as u64);
        }
        Ok(())
    }

    /// Write ignoring page permissions (loader / host-side use). The range must be mapped.
    pub fn write_force(&mut self, addr: u64, src: &[u8]) -> MemResult<()> {
        let mut a = addr;
        let mut done = 0;
        while done < src.len() {
            let s = self.slot(a >> 12).ok_or(Fault {
                addr: a,
                access: Access::Write,
                protection: false,
            })?;
            let off = (s >> 32) as usize * PAGE_SIZE as usize + (a & PAGE_MASK) as usize;
            let n = ((PAGE_SIZE - (a & PAGE_MASK)) as usize).min(src.len() - done);
            self.frames[off..off + n].copy_from_slice(&src[done..done + n]);
            done += n;
            a = a.wrapping_add(n as u64);
        }
        self.code_epoch += 1;
        Ok(())
    }

    pub fn read_vec(&self, addr: u64, len: usize) -> MemResult<Vec<u8>> {
        let mut v = vec![0u8; len];
        self.read_into(addr, &mut v, Access::Read)?;
        Ok(v)
    }

    /// Read a NUL-terminated byte string (at most `max` bytes, terminator excluded).
    pub fn read_cstr(&self, addr: u64, max: usize) -> MemResult<Vec<u8>> {
        let mut out = Vec::new();
        for i in 0..max as u64 {
            let b = self.read_u8(addr + i)?;
            if b == 0 {
                break;
            }
            out.push(b);
        }
        Ok(out)
    }

    /// Read a NUL-terminated UTF-16LE string (lossy), at most `max` code units.
    pub fn read_wstr(&self, addr: u64, max: usize) -> MemResult<String> {
        let mut u = Vec::new();
        for i in 0..max as u64 {
            let c = self.read_u16(addr + i * 2)?;
            if c == 0 {
                break;
            }
            u.push(c);
        }
        Ok(String::from_utf16_lossy(&u))
    }

    // ── fixed-width access (little endian) ───────────────────────────────────

    #[inline]
    pub fn read_u8(&self, a: u64) -> MemResult<u8> {
        let o = self.translate(a, Access::Read)?;
        Ok(self.frames[o])
    }
    #[inline]
    pub fn read_u16(&self, a: u64) -> MemResult<u16> {
        self.read_n::<2>(a).map(u16::from_le_bytes)
    }
    #[inline]
    pub fn read_u32(&self, a: u64) -> MemResult<u32> {
        self.read_n::<4>(a).map(u32::from_le_bytes)
    }
    #[inline]
    pub fn read_u64(&self, a: u64) -> MemResult<u64> {
        self.read_n::<8>(a).map(u64::from_le_bytes)
    }
    #[inline]
    pub fn read_u128(&self, a: u64) -> MemResult<u128> {
        self.read_n::<16>(a).map(u128::from_le_bytes)
    }
    #[inline]
    fn read_n<const N: usize>(&self, a: u64) -> MemResult<[u8; N]> {
        if (a & PAGE_MASK) as usize + N <= PAGE_SIZE as usize {
            let o = self.translate(a, Access::Read)?;
            Ok(self.frames[o..o + N].try_into().unwrap())
        } else {
            let mut b = [0u8; N];
            self.read_into(a, &mut b, Access::Read)?;
            Ok(b)
        }
    }

    #[inline]
    pub fn write_u8(&mut self, a: u64, v: u8) -> MemResult<()> {
        self.write_n(a, &[v])
    }
    #[inline]
    pub fn write_u16(&mut self, a: u64, v: u16) -> MemResult<()> {
        self.write_n(a, &v.to_le_bytes())
    }
    #[inline]
    pub fn write_u32(&mut self, a: u64, v: u32) -> MemResult<()> {
        self.write_n(a, &v.to_le_bytes())
    }
    #[inline]
    pub fn write_u64(&mut self, a: u64, v: u64) -> MemResult<()> {
        self.write_n(a, &v.to_le_bytes())
    }
    #[inline]
    pub fn write_u128(&mut self, a: u64, v: u128) -> MemResult<()> {
        self.write_n(a, &v.to_le_bytes())
    }
    #[inline]
    fn write_n(&mut self, a: u64, b: &[u8]) -> MemResult<()> {
        if (a & PAGE_MASK) as usize + b.len() <= PAGE_SIZE as usize {
            let o = self.translate(a, Access::Write)?;
            self.frames[o..o + b.len()].copy_from_slice(b);
            if self.exec_page_count != 0
                && self
                    .slot(a >> 12)
                    .is_some_and(|s| (s >> 1) & PROT_X as u64 != 0)
            {
                self.code_epoch += 1;
            }
            Ok(())
        } else {
            self.write_from(a, b)
        }
    }

    /// Fetch up to 15 instruction bytes at `addr` (fewer if the next page isn't executable).
    pub fn fetch(&self, addr: u64, buf: &mut [u8; 15]) -> MemResult<usize> {
        let first = ((PAGE_SIZE - (addr & PAGE_MASK)) as usize).min(15);
        let o = self.translate(addr, Access::Exec)?;
        buf[..first].copy_from_slice(&self.frames[o..o + first]);
        if first == 15 {
            return Ok(15);
        }
        match self.translate(addr.wrapping_add(first as u64), Access::Exec) {
            Ok(o2) => {
                buf[first..].copy_from_slice(&self.frames[o2..o2 + (15 - first)]);
                Ok(15)
            }
            Err(_) => Ok(first),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_rw_roundtrip_and_cross_page() {
        let mut m = Memory::new();
        assert!(m.map(0x1_4000_0000, 0x3000, PROT_RW));
        m.write_u64(0x1_4000_0ffc, 0x1122_3344_5566_7788).unwrap(); // crosses a page
        assert_eq!(m.read_u64(0x1_4000_0ffc).unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(m.read_u8(0x1_4000_1000).unwrap(), 0x44);
        assert_eq!(
            m.read_u128(0x1_4000_0ff8).unwrap() >> 32 & 0xffff_ffff_ffff_ffff,
            0x1122_3344_5566_7788
        );
    }

    #[test]
    fn faults() {
        let mut m = Memory::new();
        assert_eq!(
            m.read_u8(0x1000),
            Err(Fault {
                addr: 0x1000,
                access: Access::Read,
                protection: false
            })
        );
        m.map(0x1000, 0x1000, PROT_R);
        assert!(m.write_u8(0x1000, 1).unwrap_err().protection);
        let mut b = [0u8; 15];
        assert!(m.fetch(0x1000, &mut b).is_err());
        m.protect(0x1000, 0x1000, PROT_R | PROT_X);
        assert_eq!(m.fetch(0x1000, &mut b).unwrap(), 15.min(0x1000));
        // Straddling into an unmapped page: partial fetch, no fault.
        let mut m2 = Memory::new();
        m2.map(0x2000, 0x1000, PROT_RWX);
        assert_eq!(m2.fetch(0x2ffa, &mut b).unwrap(), 6);
    }

    #[test]
    fn unmap_recycles_and_zeroes() {
        let mut m = Memory::new();
        m.map(0x5000, 0x1000, PROT_RW);
        m.write_u32(0x5000, 0xdead_beef).unwrap();
        m.unmap(0x5000, 0x1000);
        assert!(!m.is_mapped(0x5000));
        m.map(0x9000, 0x1000, PROT_RW);
        assert_eq!(m.read_u32(0x9000).unwrap(), 0);
        assert_eq!(m.pages_mapped(), 1);
    }

    #[test]
    fn find_free_skips_mapped() {
        let mut m = Memory::new();
        m.map(0x10000, 0x2000, PROT_RW);
        assert_eq!(m.find_free(0x10000, 0x1000), Some(0x12000));
        assert_eq!(m.find_free(0x8000, 0x2000), Some(0x8000));
    }

    #[test]
    fn code_epoch_bumps_on_exec_page_write() {
        let mut m = Memory::new();
        m.map(0x1000, 0x1000, PROT_RWX);
        let e = m.code_epoch;
        m.write_u8(0x1000, 0x90).unwrap();
        assert!(m.code_epoch > e);
    }

    #[test]
    fn out_of_range_rejected() {
        let mut m = Memory::new();
        assert!(!m.map(1u64 << 48, 0x1000, PROT_RW));
    }

    #[cfg(feature = "jit-test-api")]
    #[test]
    fn test_snapshot_lists_mapped_pages_with_permissions_and_full_contents() {
        let mut m = Memory::new();
        assert!(m.map(0x3000, PAGE_SIZE, PROT_RW));
        assert!(m.map(0x1000, PAGE_SIZE, PROT_R));
        m.write_force(0x1000, &[0xa1, 0xa2]).unwrap();
        m.write_from(0x3000, &[0xb1, 0xb2]).unwrap();

        let snapshot = m.test_snapshot_mapped_pages();
        let record_size = 8 + 1 + PAGE_SIZE as usize;
        assert_eq!(snapshot.len(), 2 * record_size);
        assert_eq!(u64::from_le_bytes(snapshot[0..8].try_into().unwrap()), 1);
        assert_eq!(snapshot[8], PROT_R);
        assert_eq!(&snapshot[9..11], &[0xa1, 0xa2]);
        let second = record_size;
        assert_eq!(
            u64::from_le_bytes(snapshot[second..second + 8].try_into().unwrap()),
            3
        );
        assert_eq!(snapshot[second + 8], PROT_RW);
        assert_eq!(&snapshot[second + 9..second + 11], &[0xb1, 0xb2]);
    }
}
