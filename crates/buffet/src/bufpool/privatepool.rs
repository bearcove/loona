use std::{cell::UnsafeCell, collections::VecDeque, marker::PhantomData};

use memmap2::MmapMut;

use super::BufMut;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("could not mmap buffer")]
    Mmap(#[from] std::io::Error),

    #[error("out of memory")]
    OutOfMemory,

    #[error("slice does not fit into this RollMut")]
    DoesNotFit,
}

thread_local! {
    static POOL: Pool = const { Pool::new() };
}

/// A buffer pool
struct Pool {
    inner: UnsafeCell<Option<Inner>>,
}

struct Inner {
    // mmapped memory
    ptr: *mut u8,

    // The mmap object, if we're using an anonymous mapping.
    // This is only used for its `Drop` implementation.
    _mmap: Option<MmapMut>,

    // index of free blocks
    // there's several optimizations we could do here:
    //   - we could use bitmaps to track which slots are free
    //   - we could start by only pushing, say, 1024 free slots and then grow the free list as
    //     needed (when more than 1024 bufs are allocated, we'll grow the free list by another 1024
    //     slots, etc. until we run out of memory)
    free: VecDeque<u32>,

    // ref counts start as all zeroes, get incremented when a block is borrowed
    ref_counts: Vec<i16>,
}

impl Pool {
    const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }

    #[inline(always)]
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let inner = self.inner.get();
        let inner = unsafe { Option::as_mut(&mut *inner).unwrap() };
        f(inner)
    }
}

#[inline(always)]
fn with<T>(f: impl FnOnce(&mut Inner) -> T) -> T {
    POOL.with(|pool| pool.with(f))
}

/// The size of a buffer, in bytes (4 KiB)
pub const BUF_SIZE: u16 = 4096;

/// Initializes the allocator with the given number of buffers
pub fn initialize_allocator_with_num_bufs(num_bufs: u32) -> Result<()> {
    POOL.with(|pool| {
        if unsafe { (*pool.inner.get()).is_some() } {
            return Ok(());
        }

        let mut inner = Inner {
            ptr: std::ptr::null_mut(),
            _mmap: None,
            free: VecDeque::from_iter(0..num_bufs),
            ref_counts: vec![0; num_bufs as usize],
        };

        let alloc_len = num_bufs as usize * BUF_SIZE as usize;

        #[cfg(feature = "miri")]
        {
            let mut map = vec![0; alloc_len];
            inner.ptr = map.as_mut_ptr();
            std::mem::forget(map);
        }

        #[cfg(not(feature = "miri"))]
        {
            let mut map = memmap2::MmapOptions::new().len(alloc_len).map_anon()?;
            inner.ptr = map.as_mut_ptr();
            inner._mmap = Some(map);
        }

        unsafe {
            (*pool.inner.get()) = Some(inner);
        }

        Ok(())
    })
}

/// Returns the number of free buffers in the pool
pub fn num_free() -> usize {
    with(|inner| inner.free.len())
}

/// Allocate a buffer
pub fn alloc() -> Result<BufMut> {
    with(|inner| {
        if let Some(index) = inner.free.pop_front() {
            inner.ref_counts[index as usize] += 1;
            Ok(BufMut {
                index,
                off: 0,
                len: BUF_SIZE as _,
                _non_send: PhantomData,
            })
        } else {
            Err(Error::OutOfMemory)
        }
    })
}

/// Increment the reference count of the given buffer
pub fn inc(index: u32) {
    with(|inner| {
        inner.ref_counts[index as usize] += 1;
    })
}

/// Decrement the reference count of the given buffer.
/// If the reference count reaches zero, the buffer is freed.
pub fn dec(index: u32) {
    with(|inner| {
        let slot = &mut inner.ref_counts[index as usize];
        *slot -= 1;
        if *slot == 0 {
            inner.free.push_back(index);
        }
    })
}

/// Returns a pointer to the start of the buffer
#[inline(always)]
pub unsafe fn base_ptr_with_offset(index: u32, offset: isize) -> *mut u8 {
    with(|inner| {
        inner
            .ptr
            .byte_offset(offset + index as isize * BUF_SIZE as isize)
    })
}
