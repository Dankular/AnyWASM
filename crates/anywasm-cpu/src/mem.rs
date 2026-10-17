//! Paged guest memory with per-page R/W/X permissions.
//!
//! Three-level direct page table over a 48-bit virtual address space (12 + 12 + 12 bits of page
//! number); nodes are allocated lazily so an almost-empty address space costs a few KiB. Frames
//! live in one growing `Vec<u8>`. A one-entry software TLB per access kind keeps the hot path to a
//! compare and an index.

use crate::frame_store::FrameStore;
use anywasm_cpu_state::memory::GuestMemory;
use anywasm_cpu_state::shared::{MappingEntry, GUEST_PAGE_SIZE};
use std::{cell::Cell, collections::HashMap};

pub use anywasm_cpu_state::memory::{Access, Fault, FaultKind, MemResult};

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;
pub const VA_BITS: u32 = 48;

pub const PROT_NONE: u8 = 0;
pub const PROT_R: u8 = 1;
pub const PROT_W: u8 = 2;
pub const PROT_X: u8 = 4;
pub const PROT_RW: u8 = PROT_R | PROT_W;
pub const PROT_RWX: u8 = PROT_R | PROT_W | PROT_X;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedSnapshotError {
    BufferTooSmall,
    AddressOverflow,
    MappingMismatch,
}

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
    pub dirty_frames: u32,
    pub dirty_frame_indices: u32,
    pub dirty_frame_count: u32,
}

struct Leaf(Box<[Slot; L_SIZE]>);
struct Mid(Vec<Option<Leaf>>);

pub struct Memory {
    top: Vec<Option<Mid>>,
    frames: FrameStore,
    /// Guest page number currently backed by each frame (`u64::MAX` when the frame is free).
    frame_pages: Vec<u64>,
    /// One byte per allocated frame for coordinator-to-worker incremental publication.
    dirty_frames: Vec<u8>,
    /// Fixed-capacity list populated when a frame first becomes dirty after publication.
    dirty_frame_indices: Vec<u32>,
    dirty_frame_count: u32,
    /// Mapping-row caches are rebuilt only when the map or protections change.
    shared_row_frames: Vec<u32>,
    shared_row_guest_pages: Vec<u64>,
    frame_mapping_rows: Vec<u32>,
    shared_frame_base: Option<u32>,
    shared_mapping_generation: u32,
    shared_cache_generation: u32,
    free_frames: Vec<u32>,
    pages_mapped: u64,
    /// Per-page dirty state for MEM_WRITE_WATCH allocations.
    write_watch_pages: HashMap<u64, bool>,
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
            frames: FrameStore::new(),
            frame_pages: Vec::new(),
            dirty_frames: Vec::new(),
            dirty_frame_indices: Vec::new(),
            dirty_frame_count: 0,
            shared_row_frames: Vec::new(),
            shared_row_guest_pages: Vec::new(),
            frame_mapping_rows: Vec::new(),
            shared_frame_base: None,
            shared_mapping_generation: 1,
            shared_cache_generation: 0,
            free_frames: Vec::new(),
            pages_mapped: 0,
            write_watch_pages: HashMap::new(),
            code_epoch: 0,
            tlb_r: Cell::new(TlbEntry::INVALID),
            tlb_w: Cell::new(TlbEntry::INVALID),
            exec_page_count: 0,
        }
    }

    pub fn pages_mapped(&self) -> u64 {
        self.pages_mapped
    }

    pub fn shared_mapping_generation(&self) -> u32 {
        self.shared_mapping_generation
    }

    /// Copy the authoritative page table and frames into coordinator-owned buffers suitable for
    /// publication to a shared worker memory. `frame_base` is the shared-memory offset where the
    /// packed frame region begins; each mapping's frame offset is absolute from that base memory.
    pub fn export_shared_snapshot(
        &mut self,
        frame_base: u32,
        mappings: &mut [MappingEntry],
        frame_bytes: &mut [u8],
    ) -> Result<(), SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        let required_frame_bytes = page_count
            .checked_mul(PAGE_SIZE as usize)
            .ok_or(SharedSnapshotError::AddressOverflow)?;
        if mappings.len() < page_count || frame_bytes.len() < required_frame_bytes {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        self.export_shared_mappings(frame_base, mappings)?;
        let mut row_frames = Vec::with_capacity(page_count);
        let mut row_guest_pages = Vec::with_capacity(page_count);
        let mut frame_mapping_rows = vec![u32::MAX; self.dirty_frames.len()];
        for (output_index, mapping) in mappings[..page_count].iter().enumerate() {
            let Some(slot) = self.slot(mapping.guest_page) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            let frame_index = (slot >> 32) as usize;
            let source_start = frame_index
                .checked_mul(PAGE_SIZE as usize)
                .ok_or(SharedSnapshotError::AddressOverflow)?;
            let source_end = source_start + PAGE_SIZE as usize;
            let destination_start = output_index * PAGE_SIZE as usize;
            let destination_end = destination_start + PAGE_SIZE as usize;
            let Some(source) = self.frames.get(source_start..source_end) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            frame_bytes[destination_start..destination_end].copy_from_slice(source);
            let frame_index =
                u32::try_from(frame_index).map_err(|_| SharedSnapshotError::AddressOverflow)?;
            row_frames.push(frame_index);
            row_guest_pages.push(mapping.guest_page);
            let Some(mapping_row) = frame_mapping_rows.get_mut(frame_index as usize) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            *mapping_row = output_index as u32;
        }
        self.shared_row_frames = row_frames;
        self.shared_row_guest_pages = row_guest_pages;
        self.frame_mapping_rows = frame_mapping_rows;
        self.shared_cache_generation = self.shared_mapping_generation;
        self.shared_frame_base = Some(frame_base);
        Ok(())
    }

    /// Export only host-dirty pages using the mapping cache from the last full publication.
    /// Dirtiness is retained until the caller acknowledges a successful publication.
    pub fn export_shared_delta(
        &self,
        dirty_pages: &mut [u32],
        dirty_frames: &mut [u8],
    ) -> Result<usize, SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        let required_frame_bytes = page_count
            .checked_mul(PAGE_SIZE as usize)
            .ok_or(SharedSnapshotError::AddressOverflow)?;
        if dirty_pages.len() < page_count || dirty_frames.len() < required_frame_bytes {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        if self.shared_cache_generation != self.shared_mapping_generation
            || self.shared_row_frames.len() != page_count
        {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        let mut dirty_count = 0usize;
        if self.dirty_frame_count as usize > self.dirty_frame_indices.len() {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        for frame_index in self.dirty_frame_indices[..self.dirty_frame_count as usize]
            .iter()
            .copied()
        {
            if self
                .dirty_frames
                .get(frame_index as usize)
                .copied()
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let mapping_index = *self
                .frame_mapping_rows
                .get(frame_index as usize)
                .ok_or(SharedSnapshotError::MappingMismatch)?;
            if mapping_index == u32::MAX {
                continue;
            }
            dirty_pages[dirty_count] = mapping_index;
            dirty_count += 1;
        }
        dirty_pages[..dirty_count].sort_unstable();
        for (dirty_position, mapping_index) in
            dirty_pages[..dirty_count].iter().copied().enumerate()
        {
            let frame_index = *self
                .shared_row_frames
                .get(mapping_index as usize)
                .ok_or(SharedSnapshotError::MappingMismatch)?
                as usize;
            let source_start = frame_index
                .checked_mul(PAGE_SIZE as usize)
                .ok_or(SharedSnapshotError::AddressOverflow)?;
            let source_end = source_start + PAGE_SIZE as usize;
            let destination_start = dirty_position * PAGE_SIZE as usize;
            let destination_end = destination_start + PAGE_SIZE as usize;
            let Some(source) = self.frames.get(source_start..source_end) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            dirty_frames[destination_start..destination_end].copy_from_slice(source);
        }
        Ok(dirty_count)
    }

    /// Clear selected host-dirty bits after their packed mapping rows are published.
    pub fn clear_shared_dirty_pages(
        &mut self,
        dirty_pages: &[u32],
    ) -> Result<(), SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        if dirty_pages.iter().enumerate().any(|(index, page)| {
            *page as usize >= page_count || (index > 0 && dirty_pages[index - 1] >= *page)
        }) {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        if self.shared_cache_generation != self.shared_mapping_generation
            || self.shared_row_frames.len() != page_count
        {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        for mapping_index in dirty_pages.iter().copied() {
            let Some(&frame_index) = self.shared_row_frames.get(mapping_index as usize) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            let Some(dirty) = self.dirty_frames.get_mut(frame_index as usize) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            *dirty = 0;
        }
        self.compact_shared_dirty_frames();
        Ok(())
    }

    /// Clear all host-dirty bits after a full snapshot is published.
    pub fn clear_all_shared_dirty(&mut self) {
        self.dirty_frames.fill(0);
        self.dirty_frame_count = 0;
    }

    fn compact_shared_dirty_frames(&mut self) {
        let mut write_index = 0usize;
        for read_index in 0..self.dirty_frame_count as usize {
            let frame_index = self.dirty_frame_indices[read_index];
            if self.dirty_frames[frame_index as usize] != 0 {
                self.dirty_frame_indices[write_index] = frame_index;
                write_index += 1;
            }
        }
        self.dirty_frame_count = write_index as u32;
    }

    /// Import selected worker-written pages after validating their map rows against this process.
    /// The input page bytes are densely packed in `dirty_pages` order.
    pub fn import_shared_dirty_snapshot(
        &mut self,
        frame_base: u32,
        mapping_generation: u32,
        dirty_pages: &[u32],
        frame_bytes: &[u8],
    ) -> Result<(), SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        let required_bytes = dirty_pages
            .len()
            .checked_mul(PAGE_SIZE as usize)
            .ok_or(SharedSnapshotError::AddressOverflow)?;
        if frame_bytes.len() < required_bytes {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        if frame_base % GUEST_PAGE_SIZE != 0 {
            return Err(SharedSnapshotError::AddressOverflow);
        }
        if mapping_generation != self.shared_mapping_generation
            || self.shared_cache_generation != self.shared_mapping_generation
            || self.shared_frame_base != Some(frame_base)
            || self.shared_row_frames.len() != page_count
            || self.shared_row_guest_pages.len() != page_count
        {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        let mut previous = None;
        let mut executable_page_written = false;
        for (dirty_position, mapping_index) in dirty_pages.iter().copied().enumerate() {
            let mapping_index = mapping_index as usize;
            if mapping_index >= page_count || previous.is_some_and(|index| index >= mapping_index) {
                return Err(SharedSnapshotError::MappingMismatch);
            }
            previous = Some(mapping_index);
            let frame_index = self.shared_row_frames[mapping_index] as usize;
            let guest_page = self.shared_row_guest_pages[mapping_index];
            let Some(slot) = self.slot(guest_page) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            if (slot >> 32) as usize != frame_index {
                return Err(SharedSnapshotError::MappingMismatch);
            }
            let protection = ((slot >> 1) & 7) as u8;
            let destination_start = frame_index
                .checked_mul(PAGE_SIZE as usize)
                .ok_or(SharedSnapshotError::AddressOverflow)?;
            let destination_end = destination_start + PAGE_SIZE as usize;
            let source_start = dirty_position * PAGE_SIZE as usize;
            let source_end = source_start + PAGE_SIZE as usize;
            let Some(destination) = self.frames.get_mut(destination_start..destination_end) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            destination.copy_from_slice(&frame_bytes[source_start..source_end]);
            executable_page_written |= protection & PROT_X != 0;
        }
        if executable_page_written {
            self.code_epoch = self.code_epoch.wrapping_add(1);
        }
        self.flush_tlb();
        Ok(())
    }

    fn export_shared_mappings(
        &self,
        frame_base: u32,
        mappings: &mut [MappingEntry],
    ) -> Result<(), SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        if mappings.len() < page_count {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        if frame_base % GUEST_PAGE_SIZE != 0 {
            return Err(SharedSnapshotError::AddressOverflow);
        }
        let mut output_index = 0usize;
        for (top_index, mid) in self.top.iter().enumerate() {
            let Some(mid) = mid else { continue };
            for (mid_index, leaf) in mid.0.iter().enumerate() {
                let Some(leaf) = leaf else { continue };
                for (leaf_index, slot) in leaf.0.iter().copied().enumerate() {
                    if slot & PRESENT == 0 {
                        continue;
                    }
                    let frame_offset = frame_base
                        .checked_add(
                            u32::try_from(output_index)
                                .map_err(|_| SharedSnapshotError::AddressOverflow)?
                                .checked_mul(GUEST_PAGE_SIZE)
                                .ok_or(SharedSnapshotError::AddressOverflow)?,
                        )
                        .ok_or(SharedSnapshotError::AddressOverflow)?;
                    mappings[output_index] = MappingEntry {
                        guest_page: ((top_index as u64) << (2 * L_BITS))
                            | ((mid_index as u64) << L_BITS)
                            | leaf_index as u64,
                        frame_offset,
                        protection: ((slot >> 1) & 7) as u8,
                        reserved: [0; 3],
                    };
                    output_index += 1;
                }
            }
        }
        if output_index != page_count {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        Ok(())
    }

    /// Import worker-written frame bytes after every worker has reached a safe point. Mappings
    /// must match the current coordinator page table exactly; topology and permission changes are
    /// performed by the coordinator and require a newly published snapshot.
    pub fn import_shared_snapshot(
        &mut self,
        frame_base: u32,
        mappings: &[MappingEntry],
        frame_bytes: &[u8],
    ) -> Result<(), SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        let required_frame_bytes = page_count
            .checked_mul(PAGE_SIZE as usize)
            .ok_or(SharedSnapshotError::AddressOverflow)?;
        if mappings.len() != page_count || frame_bytes.len() < required_frame_bytes {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        if frame_base % GUEST_PAGE_SIZE != 0 {
            return Err(SharedSnapshotError::AddressOverflow);
        }

        let mut executable_page_written = false;
        let mut previous_page = None;
        for (index, mapping) in mappings.iter().enumerate() {
            let expected_offset = frame_base
                .checked_add(
                    u32::try_from(index)
                        .map_err(|_| SharedSnapshotError::AddressOverflow)?
                        .checked_mul(GUEST_PAGE_SIZE)
                        .ok_or(SharedSnapshotError::AddressOverflow)?,
                )
                .ok_or(SharedSnapshotError::AddressOverflow)?;
            if mapping.frame_offset != expected_offset
                || mapping.reserved != [0; 3]
                || mapping.guest_page >= (1u64 << (3 * L_BITS))
                || previous_page.is_some_and(|page| page >= mapping.guest_page)
            {
                return Err(SharedSnapshotError::MappingMismatch);
            }
            previous_page = Some(mapping.guest_page);
            let Some(slot) = self.slot(mapping.guest_page) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            let protection = ((slot >> 1) & 7) as u8;
            if protection != mapping.protection {
                return Err(SharedSnapshotError::MappingMismatch);
            }
            let frame_index = (slot >> 32) as usize;
            let destination_start = frame_index
                .checked_mul(PAGE_SIZE as usize)
                .ok_or(SharedSnapshotError::AddressOverflow)?;
            let destination_end = destination_start + PAGE_SIZE as usize;
            let source_start = index * PAGE_SIZE as usize;
            let source_end = source_start + PAGE_SIZE as usize;
            let Some(destination) = self.frames.get_mut(destination_start..destination_end) else {
                return Err(SharedSnapshotError::MappingMismatch);
            };
            destination.copy_from_slice(&frame_bytes[source_start..source_end]);
            executable_page_written |= protection & PROT_X != 0;
        }
        if executable_page_written {
            self.code_epoch = self.code_epoch.wrapping_add(1);
        }
        self.flush_tlb();
        Ok(())
    }

    fn flush_tlb(&self) {
        self.tlb_r.set(TlbEntry::INVALID);
        self.tlb_w.set(TlbEntry::INVALID);
    }

    #[inline]
    fn mark_frame_dirty(&mut self, frame_offset: usize) {
        let frame_index = frame_offset / PAGE_SIZE as usize;
        if let Some(page) = self.frame_pages.get(frame_index).copied() {
            if let Some(dirty) = self.write_watch_pages.get_mut(&page) {
                *dirty = true;
            }
        }
        if self.dirty_frames.get(frame_index).copied() == Some(0)
            && (self.dirty_frame_count as usize) < self.dirty_frame_indices.len()
        {
            self.dirty_frames[frame_index] = 1;
            self.dirty_frame_indices[self.dirty_frame_count as usize] = frame_index as u32;
            self.dirty_frame_count += 1;
        }
    }

    #[inline]
    fn bump_shared_mapping_generation(&mut self) {
        let next = self.shared_mapping_generation.wrapping_add(1);
        self.shared_mapping_generation = if next == u32::MAX { 0 } else { next };
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
            self.dirty_frames[f as usize] = 1;
            return f;
        }
        let f = (self.frames.len() / PAGE_SIZE as usize) as u32;
        let moves = self.frames.moves();
        self.frames
            .resize(self.frames.len() + PAGE_SIZE as usize, 0);
        if self.frames.moves() != moves {
            // Workers that address frames in place hold absolute offsets; relocation invalidates them.
            self.bump_shared_mapping_generation();
        }
        self.frame_pages.push(u64::MAX);
        self.dirty_frames.push(1);
        self.dirty_frame_indices.push(0);
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
            self.frame_pages[f as usize] = p;
            let s = self.slot_mut(p, true).unwrap();
            *s = PRESENT | ((prot as u64 & 7) << 1) | ((f as u64) << 32);
            self.pages_mapped += 1;
            if prot & PROT_X != 0 {
                self.exec_page_count += 1;
            }
        }
        if start < end {
            self.bump_shared_mapping_generation();
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
        self.frame_pages[(old >> 32) as usize] = u64::MAX;
        self.write_watch_pages.remove(&p);
        self.pages_mapped -= 1;
        if (old >> 1) & PROT_X as u64 != 0 {
            self.exec_page_count -= 1;
        }
    }

    pub fn unmap(&mut self, addr: u64, len: u64) {
        let previous_pages_mapped = self.pages_mapped;
        let start = addr >> 12;
        let end = (addr.saturating_add(len).saturating_add(PAGE_MASK)) >> 12;
        for p in start..end.min(1u64 << (3 * L_BITS)) {
            self.unmap_page(p);
        }
        if self.pages_mapped != previous_pages_mapped {
            self.bump_shared_mapping_generation();
        }
        self.flush_tlb();
        self.code_epoch += 1;
    }

    /// Start tracking writes to every mapped page in a region. The range is rounded to pages in
    /// the same way as VirtualAlloc's address-plus-size span.
    pub fn enable_write_watch(&mut self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        let Some(rounded_end) = end.checked_add(PAGE_MASK) else {
            return false;
        };
        let first_page = addr >> 12;
        let end_page = rounded_end >> 12;
        if len == 0
            || first_page >= end_page
            || (first_page..end_page).any(|page| self.slot(page).is_none())
        {
            return false;
        }
        for page in first_page..end_page {
            self.write_watch_pages.insert(page, false);
        }
        true
    }

    /// Return dirty page base addresses intersecting a region, or `None` if any requested page
    /// is not part of a live MEM_WRITE_WATCH allocation.
    pub fn write_watch_snapshot(&self, addr: u64, len: u64) -> Option<Vec<u64>> {
        let end = addr.checked_add(len)?;
        let rounded_end = end.checked_add(PAGE_MASK)?;
        let first_page = addr >> 12;
        let end_page = rounded_end >> 12;
        if len == 0
            || first_page >= end_page
            || (first_page..end_page).any(|page| !self.write_watch_pages.contains_key(&page))
        {
            return None;
        }
        Some(
            (first_page..end_page)
                .filter_map(|page| {
                    self.write_watch_pages
                        .get(&page)
                        .copied()
                        .unwrap_or(false)
                        .then_some(page << 12)
                })
                .collect(),
        )
    }

    /// Clear the tracked-write state for the supplied page base addresses.
    pub fn reset_write_watch_pages(&mut self, page_addresses: &[u64]) {
        for address in page_addresses {
            if let Some(dirty) = self.write_watch_pages.get_mut(&(address >> 12)) {
                *dirty = false;
            }
        }
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
        let mut changed = false;
        for p in start..end {
            let s = self.slot_mut(p, false).unwrap();
            let was_x = (*s >> 1) & PROT_X as u64 != 0;
            let new_slot = (*s & !(7 << 1)) | ((prot as u64 & 7) << 1);
            changed |= *s != new_slot;
            *s = new_slot;
            let now_x = prot & PROT_X != 0;
            match (was_x, now_x) {
                (false, true) => self.exec_page_count += 1,
                (true, false) => self.exec_page_count -= 1,
                _ => {}
            }
        }
        if changed {
            self.bump_shared_mapping_generation();
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
            kind: FaultKind::GuestMemory,
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
                kind: FaultKind::GuestMemory,
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

    /// Location of the guest frame store: `(base address, bytes in use, bytes allocated)`.
    /// With a runtime built over an imported shared `WebAssembly.Memory` the address is a byte
    /// offset every worker can use directly; it changes only when the store grows and moves,
    /// which also bumps [`Self::shared_mapping_generation`].
    pub fn frame_store_region(&self) -> (usize, usize, usize) {
        (
            self.frames.as_ptr() as usize,
            self.frames.len(),
            self.frames.capacity(),
        )
    }

    /// Write one sorted [`MappingEntry`] per mapped page whose `frame_offset` points straight at
    /// the page's frame in the live frame store, so workers need no copy. `region_base` is the
    /// address of the store's first byte in the workers' address space (the store's own pointer
    /// on wasm32; a caller-chosen base in native tests). Returns the number of entries written.
    pub fn export_shared_mappings_in_place(
        &self,
        region_base: u32,
        mappings: &mut [MappingEntry],
    ) -> Result<usize, SharedSnapshotError> {
        let page_count =
            usize::try_from(self.pages_mapped).map_err(|_| SharedSnapshotError::AddressOverflow)?;
        if mappings.len() < page_count {
            return Err(SharedSnapshotError::BufferTooSmall);
        }
        if region_base % GUEST_PAGE_SIZE != 0 {
            return Err(SharedSnapshotError::AddressOverflow);
        }
        let mut output_index = 0usize;
        for (top_index, mid) in self.top.iter().enumerate() {
            let Some(mid) = mid else { continue };
            for (mid_index, leaf) in mid.0.iter().enumerate() {
                let Some(leaf) = leaf else { continue };
                for (leaf_index, slot) in leaf.0.iter().copied().enumerate() {
                    if slot & PRESENT == 0 {
                        continue;
                    }
                    let frame = u32::try_from(slot >> 32)
                        .map_err(|_| SharedSnapshotError::AddressOverflow)?;
                    let frame_offset = frame
                        .checked_mul(GUEST_PAGE_SIZE)
                        .and_then(|bytes| region_base.checked_add(bytes))
                        .ok_or(SharedSnapshotError::AddressOverflow)?;
                    mappings[output_index] = MappingEntry {
                        guest_page: ((top_index as u64) << (2 * L_BITS))
                            | ((mid_index as u64) << L_BITS)
                            | leaf_index as u64,
                        frame_offset,
                        protection: ((slot >> 1) & 7) as u8,
                        reserved: [0; 3],
                    };
                    output_index += 1;
                }
            }
        }
        if output_index != page_count {
            return Err(SharedSnapshotError::MappingMismatch);
        }
        Ok(output_index)
    }

    #[cfg(test)]
    fn frame_store_bytes(&self) -> &[u8] {
        &self.frames
    }

    /// Record frames that CPU workers wrote in place. The bytes are already visible to the
    /// interpreter; this only invalidates translations when a written frame backs an executable
    /// page. Returns how many of the frames were executable.
    pub fn note_shared_worker_writes(&mut self, frames: &[u32]) -> usize {
        let mut executable = 0;
        for &frame in frames {
            let Some(&page) = self.frame_pages.get(frame as usize) else {
                continue;
            };
            if page == u64::MAX {
                continue;
            }
            if let Some(dirty) = self.write_watch_pages.get_mut(&page) {
                *dirty = true;
            }
            if self
                .slot(page)
                .is_some_and(|s| (s >> 1) & PROT_X as u64 != 0)
            {
                executable += 1;
            }
        }
        if executable != 0 {
            self.code_epoch += 1;
        }
        executable
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
            dirty_frames: self.dirty_frames.as_mut_ptr() as usize as u32,
            dirty_frame_indices: self.dirty_frame_indices.as_mut_ptr() as usize as u32,
            dirty_frame_count: &mut self.dirty_frame_count as *mut u32 as usize as u32,
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
            self.mark_frame_dirty(off);
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
                kind: FaultKind::GuestMemory,
            })?;
            let off = (s >> 32) as usize * PAGE_SIZE as usize + (a & PAGE_MASK) as usize;
            let n = ((PAGE_SIZE - (a & PAGE_MASK)) as usize).min(src.len() - done);
            self.frames[off..off + n].copy_from_slice(&src[done..done + n]);
            self.mark_frame_dirty(off);
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
            self.mark_frame_dirty(o);
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

impl GuestMemory for Memory {
    fn read_u8(&self, address: u64) -> MemResult<u8> {
        Memory::read_u8(self, address)
    }
    fn read_u16(&self, address: u64) -> MemResult<u16> {
        Memory::read_u16(self, address)
    }
    fn read_u32(&self, address: u64) -> MemResult<u32> {
        Memory::read_u32(self, address)
    }
    fn read_u64(&self, address: u64) -> MemResult<u64> {
        Memory::read_u64(self, address)
    }
    fn read_u128(&self, address: u64) -> MemResult<u128> {
        Memory::read_u128(self, address)
    }
    fn write_u8(&mut self, address: u64, value: u8) -> MemResult<()> {
        Memory::write_u8(self, address, value)
    }
    fn write_u16(&mut self, address: u64, value: u16) -> MemResult<()> {
        Memory::write_u16(self, address, value)
    }
    fn write_u32(&mut self, address: u64, value: u32) -> MemResult<()> {
        Memory::write_u32(self, address, value)
    }
    fn write_u64(&mut self, address: u64, value: u64) -> MemResult<()> {
        Memory::write_u64(self, address, value)
    }
    fn write_u128(&mut self, address: u64, value: u128) -> MemResult<()> {
        Memory::write_u128(self, address, value)
    }
    fn read_into(&self, address: u64, output: &mut [u8], access: Access) -> MemResult<()> {
        Memory::read_into(self, address, output, access)
    }
    fn write_from(&mut self, address: u64, input: &[u8]) -> MemResult<()> {
        Memory::write_from(self, address, input)
    }
    fn fetch(&self, address: u64, output: &mut [u8; 15]) -> MemResult<usize> {
        Memory::fetch(self, address, output)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn write_watch_tracks_guest_writes_resets_returned_pages_and_clears_unmapped_pages() {
        let mut memory = Memory::new();
        assert!(memory.map(0x20000, 0x2000, PROT_RW));
        assert!(memory.enable_write_watch(0x20000, 0x2000));
        assert_eq!(memory.write_watch_snapshot(0x20000, 0x2000), Some(vec![]));

        memory.write_u8(0x20010, 1).unwrap();
        memory.write_u8(0x21010, 2).unwrap();
        assert_eq!(
            memory.write_watch_snapshot(0x20000, 0x2000),
            Some(vec![0x20000, 0x21000])
        );

        // GetWriteWatch only resets pages it returned when the caller's output capacity is short.
        memory.reset_write_watch_pages(&[0x20000]);
        assert_eq!(
            memory.write_watch_snapshot(0x20000, 0x2000),
            Some(vec![0x21000])
        );

        memory.unmap(0x21000, 0x1000);
        assert_eq!(
            memory.write_watch_snapshot(0x20000, 0x2000),
            None,
            "a freed watched page no longer belongs to a live tracked region"
        );
    }

    #[test]
    fn write_watch_records_shared_worker_stores() {
        let mut memory = Memory::new();
        assert!(memory.map(0x30000, 0x1000, PROT_RW));
        assert!(memory.enable_write_watch(0x30000, 0x1000));
        let frame = (memory.slot(0x30).unwrap() >> 32) as u32;

        assert_eq!(memory.note_shared_worker_writes(&[frame]), 0);
        assert_eq!(
            memory.write_watch_snapshot(0x30000, 0x1000),
            Some(vec![0x30000])
        );
    }

    #[test]
    fn in_place_mappings_point_at_live_frames_and_worker_writes_invalidate_code() {
        let mut m = Memory::new();
        assert!(m.map(0x30000, 0x2000, PROT_RW)); // two data pages
        assert!(m.map(0x10000, 0x1000, PROT_R | PROT_X)); // one code page, lower guest address
        m.write_force(0x30000, &[0x5a; 16]).unwrap();
        let (region, used, capacity) = m.frame_store_region();
        assert_eq!(region % 4096, 0, "frame store base is page aligned");
        assert_eq!(used, 3 * 4096);
        assert!(capacity >= used);

        let mut mappings = [MappingEntry::default(); 3];
        assert_eq!(
            m.export_shared_mappings_in_place(0x4_0000, &mut mappings),
            Ok(3)
        );
        let pages: Vec<u64> = mappings.iter().map(|e| e.guest_page).collect();
        assert_eq!(pages, vec![0x10, 0x30, 0x31], "sorted by guest page");
        // The data page frame was allocated first (frame 0), the code page third (frame 2).
        assert_eq!(mappings[1].frame_offset, 0x4_0000);
        assert_eq!(mappings[2].frame_offset, 0x4_0000 + 4096);
        assert_eq!(mappings[0].frame_offset, 0x4_0000 + 2 * 4096);
        assert_eq!(mappings[0].protection, PROT_R | PROT_X);
        // The frame really holds the bytes written through the paged interface.
        assert_eq!(m.frame_store_bytes()[..16], [0x5a; 16]);

        let mut short = [MappingEntry::default(); 2];
        assert_eq!(
            m.export_shared_mappings_in_place(0x4_0000, &mut short),
            Err(SharedSnapshotError::BufferTooSmall)
        );
        assert_eq!(
            m.export_shared_mappings_in_place(0x4_0001, &mut mappings),
            Err(SharedSnapshotError::AddressOverflow)
        );

        let epoch = m.code_epoch;
        assert_eq!(
            m.note_shared_worker_writes(&[0, 1]),
            0,
            "data-only writes keep translations"
        );
        assert_eq!(m.code_epoch, epoch);
        assert_eq!(
            m.note_shared_worker_writes(&[2]),
            1,
            "frame 2 backs the code page"
        );
        assert_eq!(m.code_epoch, epoch + 1);
        assert_eq!(
            m.note_shared_worker_writes(&[99]),
            0,
            "unknown frames are ignored"
        );
    }

    #[test]
    fn growing_the_frame_store_bumps_the_mapping_generation_only_when_it_moves() {
        let mut m = Memory::new();
        let mut last = m.shared_mapping_generation();
        let mut last_region = m.frame_store_region().0;
        let mut relocated = 0;
        for page in 0..2000u64 {
            assert!(m.map(0x1_0000_0000 + page * 4096, 4096, PROT_RW));
            let region = m.frame_store_region().0;
            if region != last_region {
                relocated += 1;
                last_region = region;
            }
            last = m.shared_mapping_generation();
        }
        assert!(
            relocated >= 1,
            "2000 pages must outgrow the initial capacity"
        );
        assert!(last != 0);
    }

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
    fn alternating_hot_accesses_across_four_pages_preserve_each_tlb_entry() {
        let mut memory = Memory::new();
        assert!(memory.map(0x10_000, 4 * PAGE_SIZE, PROT_RW));

        for pass in 0..8u64 {
            for page in 0..4u64 {
                let address = 0x10_000 + page * PAGE_SIZE + 24;
                let value = (pass << 32) | page;
                memory.write_u64(address, value).unwrap();
                assert_eq!(memory.read_u64(address).unwrap(), value);
            }
        }
    }

    #[test]
    fn mapping_protection_and_unmapping_invalidate_cached_translations() {
        let mut memory = Memory::new();
        assert!(memory.map(0x20_000, PAGE_SIZE, PROT_RW));
        memory.write_u8(0x20_000, 0x5a).unwrap();
        assert_eq!(memory.read_u8(0x20_000).unwrap(), 0x5a);

        assert!(memory.protect(0x20_000, PAGE_SIZE, PROT_R));
        assert_eq!(memory.read_u8(0x20_000).unwrap(), 0x5a);
        assert!(memory.write_u8(0x20_000, 0x33).unwrap_err().protection);

        assert!(memory.protect(0x20_000, PAGE_SIZE, PROT_RW));
        memory.write_u8(0x20_000, 0x33).unwrap();
        memory.unmap(0x20_000, PAGE_SIZE);
        assert_eq!(memory.read_u8(0x20_000).unwrap_err().addr, 0x20_000);

        assert!(memory.map(0x20_000, PAGE_SIZE, PROT_R));
        assert_eq!(memory.read_u8(0x20_000).unwrap(), 0);
        assert!(memory.write_u8(0x20_000, 0x11).unwrap_err().protection);
    }

    #[test]
    fn cross_page_access_checks_both_pages() {
        let mut memory = Memory::new();
        assert!(memory.map(0x30_000, PAGE_SIZE, PROT_RW));
        memory.write_u64(0x30_ff8, 0x1122_3344_5566_7788).unwrap();

        let error = memory.read_u64(0x30_ffc).unwrap_err();
        assert_eq!(error.addr, 0x31_000);
        assert_eq!(error.access, Access::Read);
        assert!(!error.protection);
    }

    #[test]
    fn shared_delta_tracks_cross_page_host_writes_and_acknowledgement() {
        let mut memory = Memory::new();
        assert!(memory.map(0x1000, 3 * PAGE_SIZE, PROT_RW));
        let frame_base = 0x8000;
        let mut mappings = vec![MappingEntry::default(); 3];
        let mut full_frames = vec![0; 3 * PAGE_SIZE as usize];
        memory
            .export_shared_snapshot(frame_base, &mut mappings, &mut full_frames)
            .unwrap();
        memory.clear_all_shared_dirty();
        memory
            .write_from(0x2ffe, &[0xa1, 0xa2, 0xb1, 0xb2])
            .unwrap();

        let mut dirty_pages = [u32::MAX; 3];
        let mut dirty_frames = vec![0; 3 * PAGE_SIZE as usize];
        let count = memory
            .export_shared_delta(&mut dirty_pages, &mut dirty_frames)
            .unwrap();

        assert_eq!(count, 2);
        assert_eq!(&dirty_pages[..count], &[1, 2]);
        assert_eq!(
            &dirty_frames[PAGE_SIZE as usize - 2..PAGE_SIZE as usize],
            &[0xa1, 0xa2]
        );
        assert_eq!(
            &dirty_frames[PAGE_SIZE as usize..PAGE_SIZE as usize + 2],
            &[0xb1, 0xb2]
        );
        memory
            .clear_shared_dirty_pages(&dirty_pages[..count])
            .unwrap();
        assert_eq!(
            memory
                .export_shared_delta(&mut dirty_pages, &mut dirty_frames)
                .unwrap(),
            0
        );
    }

    #[test]
    fn shared_mapping_generation_invalidates_cached_delta_rows() {
        let mut memory = Memory::new();
        assert!(memory.map(0x4000, 2 * PAGE_SIZE, PROT_RW));
        let mut mappings = vec![MappingEntry::default(); 2];
        let mut full_frames = vec![0; 2 * PAGE_SIZE as usize];
        memory
            .export_shared_snapshot(0xc000, &mut mappings, &mut full_frames)
            .unwrap();
        memory.clear_all_shared_dirty();
        memory.write_u8(0x4000, 0x5a).unwrap();

        let mut dirty_pages = [u32::MAX; 2];
        let mut dirty_frames = vec![0; 2 * PAGE_SIZE as usize];
        assert_eq!(
            memory
                .export_shared_delta(&mut dirty_pages, &mut dirty_frames)
                .unwrap(),
            1
        );
        memory.clear_shared_dirty_pages(&dirty_pages[..1]).unwrap();

        let old_generation = memory.shared_mapping_generation();
        assert!(memory.protect(0x4000, PAGE_SIZE, PROT_R));
        assert_ne!(memory.shared_mapping_generation(), old_generation);
        assert_eq!(
            memory.export_shared_delta(&mut dirty_pages, &mut dirty_frames),
            Err(SharedSnapshotError::MappingMismatch)
        );

        memory
            .export_shared_snapshot(0xc000, &mut mappings, &mut full_frames)
            .unwrap();
        memory.clear_all_shared_dirty();
        assert_eq!(
            memory
                .export_shared_delta(&mut dirty_pages, &mut dirty_frames)
                .unwrap(),
            0
        );
    }

    #[test]
    fn shared_dirty_import_changes_only_selected_mapped_pages() {
        let mut memory = Memory::new();
        assert!(memory.map(0x1000, 3 * PAGE_SIZE, PROT_RW));
        let frame_base = 0x9000;
        let mut mappings = vec![MappingEntry::default(); 3];
        let mut full_frames = vec![0; 3 * PAGE_SIZE as usize];
        memory
            .export_shared_snapshot(frame_base, &mut mappings, &mut full_frames)
            .unwrap();
        memory.clear_all_shared_dirty();
        let worker_page = vec![0x5c; PAGE_SIZE as usize];
        let mapping_generation = memory.shared_mapping_generation();

        memory
            .import_shared_dirty_snapshot(frame_base, mapping_generation, &[1], &worker_page)
            .unwrap();

        assert_eq!(memory.read_u8(0x1000).unwrap(), 0);
        assert_eq!(memory.read_u8(0x2000).unwrap(), 0x5c);
        assert_eq!(memory.read_u8(0x3000).unwrap(), 0);
        assert_eq!(
            memory.import_shared_dirty_snapshot(
                frame_base,
                mapping_generation.wrapping_add(1),
                &[1],
                &worker_page,
            ),
            Err(SharedSnapshotError::MappingMismatch)
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
                protection: false,
                kind: FaultKind::GuestMemory,
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
