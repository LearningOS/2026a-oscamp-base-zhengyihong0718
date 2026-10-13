//! # Free-List Allocator
//!
//! Building on the bump allocator, implement a Free-List Allocator that supports memory reclamation.
//!
//! ## How It Works
//!
//! A Free-List Allocator uses a linked list to track all freed memory blocks.
//! On allocation, it first searches the list for a suitable block (first-fit strategy);
//! if none is found, it falls back to allocating from the unused region.
//! On deallocation, the block is inserted at the head of the list.
//!
//! ```text
//! free_list -> [block A: 64B] -> [block B: 128B] -> [block C: 32B] -> null
//! ```
//!
//! Each free block stores a `FreeBlock` struct at its head (containing block size and next pointer).
//!
//! ## Task
//!
//! Implement `FreeListAllocator`'s `alloc` and `dealloc` methods:
//!
//! ### alloc
//! 1. Traverse the free_list, find the first block with `size >= layout.size()` and proper alignment (first-fit)
//! 2. If found, remove it from the list and return it
//! 3. If not found, allocate from the `bump` region (same as bump allocator)
//!
//! ### dealloc
//! 1. Write `FreeBlock` header info at the freed block
//! 2. Insert it at the head of free_list
//!
//! ## Key Concepts
//!
//! - Intrusive linked list
//! - `*mut T` read/write: `ptr.write(val)` / `ptr.read()`
//! - Memory alignment checks

#![cfg_attr(not(test), no_std)]

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

/// Free block header, stored at the beginning of each free memory block
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

pub struct FreeListAllocator {
    heap_start: usize,
    heap_end: usize,
    /// Bump pointer: unallocated region starts here
    bump_next: core::sync::atomic::AtomicUsize,
    /// All intrusive list accesses are protected by this spin lock.
    list_locked: core::sync::atomic::AtomicBool,
    free_list: core::cell::UnsafeCell<*mut FreeBlock>,
}

#[cfg(test)]
unsafe impl Send for FreeListAllocator {}
#[cfg(test)]
unsafe impl Sync for FreeListAllocator {}
#[cfg(not(test))]
unsafe impl Send for FreeListAllocator {}
#[cfg(not(test))]
unsafe impl Sync for FreeListAllocator {}

impl FreeListAllocator {
    /// # Safety
    /// `heap_start..heap_end` must be a valid readable and writable memory region.
    pub unsafe fn new(heap_start: usize, heap_end: usize) -> Self {
        Self {
            heap_start,
            heap_end,
            bump_next: core::sync::atomic::AtomicUsize::new(heap_start),
            list_locked: core::sync::atomic::AtomicBool::new(false),
            free_list: core::cell::UnsafeCell::new(null_mut()),
        }
    }

    fn lock_list(&self) -> ListGuard<'_> {
        while self
            .list_locked
            .compare_exchange(
                false,
                true,
                core::sync::atomic::Ordering::Acquire,
                core::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {
            core::hint::spin_loop();
        }
        ListGuard(self)
    }
}

struct ListGuard<'a>(&'a FreeListAllocator);
impl Drop for ListGuard<'_> {
    fn drop(&mut self) {
        self.0
            .list_locked
            .store(false, core::sync::atomic::Ordering::Release);
    }
}

unsafe impl GlobalAlloc for FreeListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Ensure block is at least large enough to hold a FreeBlock header (for future dealloc)
        let size = layout.size().max(core::mem::size_of::<FreeBlock>());
        let align = layout.align().max(core::mem::align_of::<FreeBlock>());

        // TODO: Step 1 — traverse free_list, find a suitable block (first-fit)
        //
        // Hints:
        // - Use prev_ptr and curr to traverse the list
        // - Check if curr address satisfies align, and (*curr).size >= size
        // - If found, remove it from the list (update prev's next or the free_list head)
        // - Return curr as *mut u8

        // TODO: Step 2 — no suitable block in free_list, allocate from bump region
        //
        // Same logic as 02_bump_allocator's alloc
        {
            let _guard = self.lock_list();
            // The lock covers both traversal and unlinking, including header reads.
            let mut link = self.free_list.get();
            unsafe {
                while !(*link).is_null() {
                    let block = *link;
                    if block as usize % align == 0 && (*block).size >= size {
                        *link = (*block).next;
                        // Preserve a usable remainder rather than losing a larger block.
                        let address = block as usize;
                        if let Some(split) = address
                            .checked_add(size)
                            .and_then(|n| n.checked_add(core::mem::align_of::<FreeBlock>() - 1))
                            .map(|n| n & !(core::mem::align_of::<FreeBlock>() - 1))
                        {
                            let used = split - address;
                            if used <= (*block).size
                                && (*block).size - used >= core::mem::size_of::<FreeBlock>()
                            {
                                let remainder = split as *mut FreeBlock;
                                remainder.write(FreeBlock {
                                    size: (*block).size - used,
                                    next: *link,
                                });
                                *link = remainder;
                            }
                        }
                        return block as *mut u8;
                    }
                    link = core::ptr::addr_of_mut!((*block).next);
                }
            }
        }
        let mut current = self.bump_next.load(core::sync::atomic::Ordering::SeqCst);
        loop {
            let Some(aligned) = current.checked_add(align - 1).map(|n| n & !(align - 1)) else {
                return null_mut();
            };
            let Some(end) = aligned.checked_add(size) else {
                return null_mut();
            };
            if aligned < self.heap_start || end > self.heap_end {
                return null_mut();
            }
            match self.bump_next.compare_exchange(
                current,
                end,
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => return aligned as *mut u8,
                Err(actual) => current = actual,
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size().max(core::mem::size_of::<FreeBlock>());

        // TODO: Insert the freed block at the head of free_list
        //
        // Steps:
        // 1. Cast ptr to *mut FreeBlock
        // 2. Write FreeBlock { size, next: current list head }
        // 3. Update free_list head to ptr
        let _guard = self.lock_list();
        unsafe {
            let block = ptr.cast::<FreeBlock>();
            block.write(FreeBlock {
                size,
                next: *self.free_list.get(),
            });
            *self.free_list.get() = block;
        }
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    const HEAP_SIZE: usize = 4096;

    fn make_allocator() -> (FreeListAllocator, Vec<u8>) {
        let mut heap = vec![0u8; HEAP_SIZE];
        let start = heap.as_mut_ptr() as usize;
        let alloc = unsafe { FreeListAllocator::new(start, start + HEAP_SIZE) };
        (alloc, heap)
    }

    #[test]
    fn test_alloc_basic() {
        let (alloc, _heap) = make_allocator();
        let layout = Layout::from_size_align(32, 8).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_alloc_alignment() {
        let (alloc, _heap) = make_allocator();
        for align in [1, 2, 4, 8, 16] {
            let layout = Layout::from_size_align(8, align).unwrap();
            let ptr = unsafe { alloc.alloc(layout) };
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % align, 0, "align={align}");
        }
    }

    #[test]
    fn test_dealloc_and_reuse() {
        let (alloc, _heap) = make_allocator();
        let layout = Layout::from_size_align(64, 8).unwrap();

        let p1 = unsafe { alloc.alloc(layout) };
        assert!(!p1.is_null());

        // After freeing, the next allocation should reuse the same block
        unsafe { alloc.dealloc(p1, layout) };
        let p2 = unsafe { alloc.alloc(layout) };
        assert!(!p2.is_null());
        assert_eq!(p1, p2, "should reuse the freed block");
    }

    #[test]
    fn test_multiple_alloc_dealloc() {
        let (alloc, _heap) = make_allocator();
        let layout = Layout::from_size_align(128, 8).unwrap();

        let p1 = unsafe { alloc.alloc(layout) };
        let p2 = unsafe { alloc.alloc(layout) };
        let p3 = unsafe { alloc.alloc(layout) };
        assert!(!p1.is_null() && !p2.is_null() && !p3.is_null());

        unsafe { alloc.dealloc(p2, layout) };
        unsafe { alloc.dealloc(p1, layout) };

        let q1 = unsafe { alloc.alloc(layout) };
        let q2 = unsafe { alloc.alloc(layout) };
        assert!(!q1.is_null() && !q2.is_null());
    }

    #[test]
    fn test_oom() {
        let (alloc, _heap) = make_allocator();
        let layout = Layout::from_size_align(HEAP_SIZE + 1, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(ptr.is_null(), "should return null when exceeding heap");
    }
}
