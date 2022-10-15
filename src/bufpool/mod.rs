mod agg;
pub use agg::*;

use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    marker::PhantomData,
    ops::{self, Range},
};

use memmap2::MmapMut;

pub const BUF_SIZE: u16 = 4096;

#[thread_local]
static BUF_POOL: BufPool = BufPool::new_empty(BUF_SIZE, 64 * 1024);

thread_local! {
    static BUF_POOL_DESTRUCTOR: RefCell<Option<MmapMut>> = RefCell::new(None);
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not mmap buffer")]
    Mmap(#[from] std::io::Error),

    #[error("out of memory")]
    OutOfMemory,
}

/// A buffer pool
pub(crate) struct BufPool {
    buf_size: u16,
    num_buf: u32,
    inner: RefCell<Option<BufPoolInner>>,
}

struct BufPoolInner {
    // this is tied to an [MmapMut] that gets deallocated at thread exit
    // thanks to [BUF_POOL_DESTRUCTOR]
    ptr: *mut u8,

    // index of free blocks
    free: VecDeque<u32>,

    // ref counts start as all zeroes, get incremented when a block is borrowed
    ref_counts: Vec<i16>,
}

impl BufPool {
    pub(crate) const fn new_empty(buf_size: u16, num_buf: u32) -> BufPool {
        BufPool {
            buf_size,
            num_buf,
            inner: RefCell::new(None),
        }
    }

    pub(crate) fn alloc(&self) -> Result<BufMut> {
        let mut inner = self.borrow_mut()?;

        if let Some(index) = inner.free.pop_front() {
            inner.ref_counts[index as usize] += 1;
            Ok(BufMut {
                index,
                off: 0,
                len: self.buf_size as _,
                _non_send: PhantomData,
            })
        } else {
            Err(Error::OutOfMemory)
        }
    }

    fn inc(&self, index: u32) {
        let mut inner = self.inner.borrow_mut();
        let inner = inner.as_mut().unwrap();

        inner.ref_counts[index as usize] += 1;
    }

    fn dec(&self, index: u32) {
        let mut inner = self.inner.borrow_mut();
        let inner = inner.as_mut().unwrap();

        inner.ref_counts[index as usize] -= 1;
        if inner.ref_counts[index as usize] == 0 {
            inner.free.push_back(index);
        }
    }

    #[cfg(test)]
    pub(crate) fn num_free(&self) -> Result<usize> {
        Ok(self.borrow_mut()?.free.len())
    }

    fn borrow_mut(&self) -> Result<RefMut<BufPoolInner>> {
        let mut inner = self.inner.borrow_mut();
        if inner.is_none() {
            let len = self.num_buf as usize * self.buf_size as usize;
            let mut map = memmap2::MmapOptions::new().len(len).map_anon()?;
            let ptr = map.as_mut_ptr();
            BUF_POOL_DESTRUCTOR.with(|destructor| {
                *destructor.borrow_mut() = Some(map);
            });

            let mut free = VecDeque::with_capacity(self.num_buf as usize);
            for i in 0..self.num_buf {
                free.push_back(i as u32);
            }
            let ref_counts = vec![0; self.num_buf as usize];

            *inner = Some(BufPoolInner {
                ptr,
                free,
                ref_counts,
            });
        }

        let r = RefMut::map(inner, |o| o.as_mut().unwrap());
        Ok(r)
    }

    /// Returns the base pointer for a block
    ///
    /// # Safety
    ///
    /// Borrow-checking is on you!
    #[inline(always)]
    unsafe fn base_ptr(&self, index: u32) -> *mut u8 {
        let start = index as usize * self.buf_size as usize;
        self.inner.borrow_mut().as_mut().unwrap().ptr.add(start)
    }
}

/// A mutable buffer. Cannot be cloned, but can be written to
pub struct BufMut {
    index: u32,
    off: u16,
    len: u16,

    // makes this type non-Send, which we do want
    _non_send: PhantomData<*mut ()>,
}

impl BufMut {
    /// Clone this buffer. This is only pub(crate) because it's used
    /// by `AggBuf`.
    pub(crate) fn dangerous_clone(&self) -> Self {
        BUF_POOL.inc(1); // in fact, increase it by 1
        BufMut {
            index: self.index,
            off: self.off,
            len: self.len,
            _non_send: PhantomData,
        }
    }

    #[inline(always)]
    pub fn alloc() -> Result<BufMut, Error> {
        BUF_POOL.alloc()
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len as _
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Turn this buffer immutable. The reference count doesn't change, but the
    /// immutable view can be cloned.
    #[inline]
    pub fn freeze(self) -> Buf {
        let b = Buf {
            index: self.index,
            off: self.off,
            len: self.len,

            _non_send: PhantomData,
        };

        std::mem::forget(self); // don't decrease ref count

        b
    }

    /// Dangerous: freeze a slice of this. Must only be used if you can
    /// guarantee this portion won't be written to anymore.
    pub(crate) fn freeze_slice(&self, range: Range<u16>) -> Buf {
        let b = Buf {
            index: self.index,
            off: self.off,
            len: self.len,

            _non_send: PhantomData,
        };

        b.slice(range)
    }

    /// Split this buffer in twain. Both parts can be written to.  Panics if
    /// `at` is out of bounds.
    #[inline]
    pub fn split_at(self, at: usize) -> (Self, Self) {
        assert!(at <= self.len as usize);

        let left = BufMut {
            index: self.index,
            off: self.off,
            len: at as _,

            _non_send: PhantomData,
        };

        let right = BufMut {
            index: self.index,
            off: self.off + at as u16,
            len: (self.len - at as u16),

            _non_send: PhantomData,
        };

        std::mem::forget(self); // don't decrease ref count
        BUF_POOL.inc(left.index); // in fact, increase it by 1

        (left, right)
    }
}

impl ops::Deref for BufMut {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                BUF_POOL.base_ptr(self.index).add(self.off as _),
                self.len as _,
            )
        }
    }
}

impl ops::DerefMut for BufMut {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            std::slice::from_raw_parts_mut(
                BUF_POOL.base_ptr(self.index).add(self.off as _),
                self.len as _,
            )
        }
    }
}

unsafe impl tokio_uring::buf::IoBuf for BufMut {
    fn stable_ptr(&self) -> *const u8 {
        unsafe { BUF_POOL.base_ptr(self.index).add(self.off as _) as *const u8 }
    }

    fn bytes_init(&self) -> usize {
        // FIXME: this is incompatible with `readv`, but it is compatible with
        // `read`, which doesn't take `bytes_init` into account. This is
        // confusing.

        // no-op: buffers are zero-initialized, and users should be careful
        // not to read bonus data
        self.len as _
    }

    fn bytes_total(&self) -> usize {
        self.len as _
    }
}

unsafe impl tokio_uring::buf::IoBufMut for BufMut {
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        unsafe { BUF_POOL.base_ptr(self.index).add(self.off as _) }
    }

    unsafe fn set_init(&mut self, _pos: usize) {
        // no-op: buffers are zero-initialized, and users should be careful
        // not to read bonus data
    }
}

impl Drop for BufMut {
    fn drop(&mut self) {
        BUF_POOL.dec(self.index);
    }
}

/// A read-only buffer. Can be cloned, but cannot be written to.
pub struct Buf {
    index: u32,
    off: u16,
    len: u16,

    // makes this type non-Send, which we do want
    _non_send: PhantomData<*mut ()>,
}

impl Buf {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len as _
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Slice this buffer
    pub fn slice(&self, range: Range<u16>) -> Self {
        assert!(range.end >= range.start);
        assert!(range.end <= self.len);

        BUF_POOL.inc(1);
        Buf {
            index: self.index,
            off: self.off + range.start,
            len: (range.end - range.start) as _,

            _non_send: PhantomData,
        }
    }
}

unsafe impl tokio_uring::buf::IoBuf for Buf {
    fn stable_ptr(&self) -> *const u8 {
        unsafe { BUF_POOL.base_ptr(self.index).add(self.off as _) as *const u8 }
    }

    fn bytes_init(&self) -> usize {
        self.len as _
    }

    fn bytes_total(&self) -> usize {
        self.len as _
    }
}

impl ops::Deref for Buf {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                BUF_POOL.base_ptr(self.index).add(self.off as _),
                self.len as _,
            )
        }
    }
}

impl Clone for Buf {
    fn clone(&self) -> Self {
        BUF_POOL.inc(self.index);
        Self {
            index: self.index,
            off: self.off,
            len: self.len,
            _non_send: PhantomData,
        }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        BUF_POOL.dec(self.index);
    }
}

#[cfg(test)]
mod tests {
    use crate::bufpool::{Buf, BUF_POOL};

    use super::BufMut;

    #[test]
    fn align_test() {
        assert_eq!(4, std::mem::align_of::<BufMut>());
        assert_eq!(4, std::mem::align_of::<Buf>());
    }

    #[test]
    fn freeze_test() -> eyre::Result<()> {
        let total_bufs = BUF_POOL.num_free()?;
        let mut bm = BufMut::alloc().unwrap();

        assert_eq!(total_bufs - 1, BUF_POOL.num_free()?);
        assert_eq!(bm.len(), 4096);

        bm[..11].copy_from_slice(b"hello world");
        assert_eq!(&bm[..11], b"hello world");

        let b = bm.freeze();
        assert_eq!(&b[..11], b"hello world");
        assert_eq!(total_bufs - 1, BUF_POOL.num_free()?);

        let b2 = b.clone();
        assert_eq!(&b[..11], b"hello world");
        assert_eq!(total_bufs - 1, BUF_POOL.num_free()?);

        drop(b);
        assert_eq!(total_bufs - 1, BUF_POOL.num_free()?);

        drop(b2);
        assert_eq!(total_bufs, BUF_POOL.num_free()?);

        Ok(())
    }

    #[test]
    fn split_test() -> eyre::Result<()> {
        let total_bufs = BUF_POOL.num_free()?;
        let mut bm = BufMut::alloc().unwrap();

        bm[..12].copy_from_slice(b"yellowjacket");
        let (a, b) = bm.split_at(6);

        assert_eq!(total_bufs - 1, BUF_POOL.num_free()?);
        assert_eq!(&a[..], b"yellow");
        assert_eq!(&b[..6], b"jacket");

        drop((a, b));

        Ok(())
    }
}
