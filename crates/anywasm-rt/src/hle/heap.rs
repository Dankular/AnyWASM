//! Process heap for HeapAlloc / malloc: size-class free lists over chunks mapped on demand.
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct HeapArena {
    cur: u64,
    cur_end: u64,
    regions: Vec<(u64, u64)>,
    reserved_bytes: u64,
    maximum_size: u64,
    sizes: HashMap<u64, u64>,
    free: BTreeMap<u64, Vec<u64>>,
    bytes_in_use: u64,
}

impl HeapArena {
    fn alloc(
        &mut self,
        n: u64,
        chunk_next: &mut u64,
        map: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<u64> {
        let sz = ((n.max(1)) + 15) & !15;
        if let Some((&k, _)) = self.free.range(sz..).next() {
            let list = self.free.get_mut(&k).unwrap();
            let a = list.pop().unwrap();
            if list.is_empty() {
                self.free.remove(&k);
            }
            self.sizes.insert(a, k);
            self.bytes_in_use += k;
            return Some(a);
        }
        let reservation = BLOCK_PREFIX + sz;
        if self.cur + reservation > self.cur_end {
            let min_want = (reservation + 0xfff) & !0xfff;
            let want = if self.maximum_size == 0 {
                (reservation.max(CHUNK) + 0xfff) & !0xfff
            } else {
                let remaining = self.maximum_size.saturating_sub(self.reserved_bytes);
                let capacity = (remaining.min(CHUNK)) & !0xfff;
                if capacity < min_want {
                    return None;
                }
                capacity
            };
            let base = *chunk_next;
            if !map(base, want) {
                return None;
            }
            self.regions.push((base, want));
            self.reserved_bytes += want;
            self.cur = base;
            self.cur_end = base + want;
            *chunk_next = base + want + 0x1000;
        }
        let a = self.cur + BLOCK_PREFIX;
        self.cur += reservation;
        self.sizes.insert(a, sz);
        self.bytes_in_use += sz;
        Some(a)
    }

    fn size_of(&self, a: u64) -> Option<u64> {
        self.sizes.get(&a).copied()
    }

    fn free(&mut self, a: u64) -> Option<u64> {
        let sz = self.sizes.remove(&a)?;
        self.bytes_in_use -= sz;
        self.free.entry(sz).or_default().push(a);
        Some(sz)
    }
}

struct PrivateHeap {
    flags: u32,
    arena: HeapArena,
}

pub struct Heap {
    pub process_heap: u64,
    chunk_next: u64,
    cur: u64,
    cur_end: u64,
    sizes: HashMap<u64, u64>,
    free: BTreeMap<u64, Vec<u64>>,
    pub bytes_in_use: u64,
    private_heaps: HashMap<u64, PrivateHeap>,
}

pub const HEAP_REGION: u64 = 0x3000_0000;
const CHUNK: u64 = 4 << 20;
// Keep enough mapped headroom before every payload for Win32 guest code that uses a
// one-element sentinel at `buffer - 1`. Without block overhead, that access aliases the
// previous allocation because this heap otherwise packs guest payloads back-to-back.
const BLOCK_PREFIX: u64 = 0x30;

impl Heap {
    pub fn new() -> Self {
        Heap {
            process_heap: 0x7ffb_0000,
            chunk_next: HEAP_REGION,
            cur: 0,
            cur_end: 0,
            sizes: HashMap::new(),
            free: BTreeMap::new(),
            bytes_in_use: 0,
            private_heaps: HashMap::new(),
        }
    }

    /// Allocate `n` bytes (16-byte aligned, zeroed because freshly mapped frames are zero and freed
    /// blocks are cleared by the caller when needed). `map` maps a new chunk when needed.
    pub fn alloc(&mut self, n: u64, map: &mut dyn FnMut(u64, u64) -> bool) -> Option<u64> {
        let sz = ((n.max(1)) + 15) & !15;
        if let Some((&k, _)) = self.free.range(sz..).next() {
            let list = self.free.get_mut(&k).unwrap();
            let a = list.pop().unwrap();
            if list.is_empty() {
                self.free.remove(&k);
            }
            self.sizes.insert(a, k);
            self.bytes_in_use += k;
            return Some(a);
        }
        let reservation = BLOCK_PREFIX + sz;
        if self.cur + reservation > self.cur_end {
            let want = reservation.max(CHUNK);
            let want = (want + 0xfff) & !0xfff;
            if !map(self.chunk_next, want) {
                return None;
            }
            self.cur = self.chunk_next;
            self.cur_end = self.chunk_next + want;
            self.chunk_next += want + 0x1000; // guard gap
        }
        let a = self.cur + BLOCK_PREFIX;
        self.cur += reservation;
        self.sizes.insert(a, sz);
        self.bytes_in_use += sz;
        Some(a)
    }

    pub fn size_of(&self, a: u64) -> Option<u64> {
        self.sizes.get(&a).copied()
    }

    /// Release a block; returns its size (so the caller can scrub it) or `None` if unknown.
    pub fn free(&mut self, a: u64) -> Option<u64> {
        let sz = self.sizes.remove(&a)?;
        self.bytes_in_use -= sz;
        self.free.entry(sz).or_default().push(a);
        Some(sz)
    }

    pub fn create_private(
        &mut self,
        handle: u64,
        flags: u32,
        initial_size: u64,
        maximum_size: u64,
        map: &mut dyn FnMut(u64, u64) -> bool,
    ) -> bool {
        if handle == 0 || handle == self.process_heap || self.private_heaps.contains_key(&handle) {
            return false;
        }
        let initial = ((initial_size.max(1) + 0xfff) & !0xfff).max(0x1000);
        let initial = if maximum_size == 0 {
            initial
        } else {
            initial.min(maximum_size & !0xfff)
        };
        if initial < 0x1000 {
            return false;
        }
        let base = self.chunk_next;
        if !map(base, initial) {
            return false;
        }
        self.chunk_next += initial + 0x1000;
        let arena = HeapArena {
            cur: base,
            cur_end: base + initial,
            regions: vec![(base, initial)],
            reserved_bytes: initial,
            maximum_size,
            ..HeapArena::default()
        };
        self.private_heaps
            .insert(handle, PrivateHeap { flags, arena });
        true
    }

    pub fn has_heap(&self, handle: u64) -> bool {
        handle == self.process_heap || self.private_heaps.contains_key(&handle)
    }

    pub fn heap_flags(&self, handle: u64) -> Option<u32> {
        self.private_heaps.get(&handle).map(|heap| heap.flags)
    }

    pub fn alloc_for(
        &mut self,
        handle: u64,
        n: u64,
        map: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<u64> {
        if handle == self.process_heap {
            self.alloc(n, map)
        } else {
            let heap = self.private_heaps.get_mut(&handle)?;
            heap.arena.alloc(n, &mut self.chunk_next, map)
        }
    }

    pub fn size_of_for(&self, handle: u64, a: u64) -> Option<u64> {
        if handle == self.process_heap {
            self.size_of(a)
        } else {
            self.private_heaps.get(&handle)?.arena.size_of(a)
        }
    }

    pub fn free_for(&mut self, handle: u64, a: u64) -> Option<u64> {
        if handle == self.process_heap {
            self.free(a)
        } else {
            self.private_heaps.get_mut(&handle)?.arena.free(a)
        }
    }

    pub fn destroy_private(&mut self, handle: u64) -> Option<Vec<(u64, u64)>> {
        let heap = self.private_heaps.remove(&handle)?;
        Some(heap.arena.regions)
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}
