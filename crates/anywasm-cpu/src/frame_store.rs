//! Page-aligned, growable backing store for guest frames.
//!
//! A plain `Vec<u8>` has no alignment guarantee, but shared CPU workers address guest frames in
//! place and require every frame to start on a 4 KiB boundary. This buffer is allocated with
//! 4 KiB alignment and exposes its base address and capacity so the owner can publish them.
//! It only grows (`resize` may shorten the logical length but never gives memory back).

use std::alloc::{alloc_zeroed, dealloc, realloc, Layout};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

const ALIGN: usize = 4096;

pub struct FrameStore {
    ptr: Option<NonNull<u8>>,
    len: usize,
    capacity: usize,
    /// Incremented whenever the base address changes (growth moved the allocation).
    moves: u32,
}

// The store owns its allocation exclusively, like a `Vec<u8>`.
unsafe impl Send for FrameStore {}
unsafe impl Sync for FrameStore {}

impl FrameStore {
    pub const fn new() -> Self {
        FrameStore {
            ptr: None,
            len: 0,
            capacity: 0,
            moves: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Allocated bytes (always a multiple of 4 KiB).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of times growth relocated the base address.
    pub fn moves(&self) -> u32 {
        self.moves
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
            .map_or(ALIGN as *const u8, |p| p.as_ptr().cast_const())
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.map_or(ALIGN as *mut u8, |p| p.as_ptr())
    }

    /// Set the logical length to `new_len`, zero-filling any newly exposed bytes.
    pub fn resize(&mut self, new_len: usize, value: u8) {
        if new_len > self.capacity {
            self.grow_to(new_len);
        }
        if new_len > self.len {
            // SAFETY: new_len <= capacity and the allocation is valid for `capacity` bytes.
            unsafe {
                std::ptr::write_bytes(self.as_mut_ptr().add(self.len), value, new_len - self.len)
            };
        }
        self.len = new_len;
    }

    fn grow_to(&mut self, needed: usize) {
        let mut capacity = self.capacity.max(64 * ALIGN);
        while capacity < needed {
            capacity = capacity
                .checked_mul(2)
                .expect("frame store capacity overflow");
        }
        let new_layout = Layout::from_size_align(capacity, ALIGN).expect("frame store layout");
        // SAFETY: layouts are non-zero sized with a valid power-of-two alignment; `realloc` is
        // given the layout the block was allocated with.
        let new_ptr = unsafe {
            match self.ptr {
                None => alloc_zeroed(new_layout),
                Some(old) => {
                    let old_layout = Layout::from_size_align(self.capacity, ALIGN).unwrap();
                    let moved = realloc(old.as_ptr(), old_layout, capacity);
                    if !moved.is_null() {
                        std::ptr::write_bytes(
                            moved.add(self.capacity),
                            0,
                            capacity - self.capacity,
                        );
                    }
                    moved
                }
            }
        };
        let new_ptr =
            NonNull::new(new_ptr).unwrap_or_else(|| std::alloc::handle_alloc_error(new_layout));
        if self.ptr != Some(new_ptr) {
            self.moves = self.moves.wrapping_add(1);
        }
        self.ptr = Some(new_ptr);
        self.capacity = capacity;
    }
}

impl Default for FrameStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FrameStore {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr {
            // SAFETY: allocated with exactly this layout in `grow_to`.
            unsafe {
                dealloc(
                    ptr.as_ptr(),
                    Layout::from_size_align(self.capacity, ALIGN).unwrap(),
                )
            };
        }
    }
}

impl Deref for FrameStore {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len <= capacity` initialized bytes (or dangling with len 0).
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len) }
    }
}

impl DerefMut for FrameStore {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_page_aligned_zeroed_and_preserves_contents() {
        let mut store = FrameStore::new();
        assert!(store.is_empty());
        store.resize(4096, 0);
        assert_eq!(store.as_ptr() as usize % 4096, 0);
        store[17] = 0xab;
        let before_moves = store.moves();
        // Force several reallocations.
        for pages in 2..=600 {
            store.resize(pages * 4096, 0);
            assert_eq!(store.as_ptr() as usize % 4096, 0, "base stays page aligned");
        }
        assert_eq!(store[17], 0xab, "contents survive relocation");
        assert!(store[4096..].iter().all(|&b| b == 0), "new bytes are zero");
        assert!(store.capacity() >= 600 * 4096);
        assert!(store.moves() >= before_moves);
    }

    #[test]
    fn resize_shorter_keeps_capacity() {
        let mut store = FrameStore::new();
        store.resize(8192, 0);
        let capacity = store.capacity();
        store.resize(4096, 0);
        assert_eq!(store.len(), 4096);
        assert_eq!(store.capacity(), capacity);
    }
}
