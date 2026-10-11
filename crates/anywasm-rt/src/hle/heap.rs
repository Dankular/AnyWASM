//! Process heap for HeapAlloc / malloc: size-class free lists over chunks mapped on demand.
use std::collections::{BTreeMap, HashMap};

pub struct Heap {
    pub process_heap: u64,
    chunk_next: u64,
    cur: u64,
    cur_end: u64,
    sizes: HashMap<u64, u64>,
    free: BTreeMap<u64, Vec<u64>>,
    pub bytes_in_use: u64,
}

pub const HEAP_REGION: u64 = 0x3_0000_0000;
const CHUNK: u64 = 4 << 20;

impl Heap {
    pub fn new() -> Self {
        Heap { process_heap: 0x7ffd_0004_0000, chunk_next: HEAP_REGION, cur: 0, cur_end: 0, sizes: HashMap::new(), free: BTreeMap::new(), bytes_in_use: 0 }
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
        if self.cur + sz > self.cur_end {
            let want = sz.max(CHUNK);
            let want = (want + 0xfff) & !0xfff;
            if !map(self.chunk_next, want) {
                return None;
            }
            self.cur = self.chunk_next;
            self.cur_end = self.chunk_next + want;
            self.chunk_next += want + 0x1000; // guard gap
        }
        let a = self.cur;
        self.cur += sz;
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
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}
