use std::{
    marker::PhantomData,
    ops::{self, Bound, RangeBounds},
};

mod privatepool;

pub type BufResult<T, B> = (std::io::Result<T>, B);

pub use privatepool::{initialize_allocator_with_num_bufs, num_free, Error, Result, BUF_SIZE};

/// Initialize the allocator. Must be called before any other
/// allocation function.
pub fn initialize_allocator() -> Result<()> {
    // 64 * 1024 * 4096 bytes = 256 MiB
    #[cfg(not(feature = "miri"))]
    let default_num_bufs = 64 * 1024;

    #[cfg(feature = "miri")]
    let default_num_bufs = 1024;

    initialize_allocator_with_num_bufs(default_num_bufs)
}

impl BufMut {
    #[inline(always)]
    pub fn alloc() -> Result<BufMut, Error> {
        privatepool::alloc()
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

    /// Dangerous: freeze a slice of this.
    ///
    /// # Safety
    /// Must only be used if you can guarantee this portion won't be written to
    /// anymore.
    #[inline]
    pub(crate) unsafe fn freeze_slice(&self, range: impl RangeBounds<usize>) -> Buf {
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
        privatepool::inc(left.index); // in fact, increase it by 1

        (left, right)
    }

    /// Skip over the first `n` bytes, panics if out of bound
    pub fn skip(&mut self, n: usize) {
        assert!(n <= self.len as usize);

        let u16_n: u16 = n.try_into().unwrap();
        self.off += u16_n;
        self.len -= u16_n;
    }
}

/// A mutable buffer. Cannot be cloned, but can be written to
pub struct BufMut {
    pub(crate) index: u32,
    pub(crate) off: u16,
    pub(crate) len: u16,

    // makes this type non-Send, which we do want
    _non_send: PhantomData<*mut ()>,
}

impl ops::Deref for BufMut {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                privatepool::base_ptr_with_offset(self.index, self.off as _),
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
                privatepool::base_ptr_with_offset(self.index, self.off as _),
                self.len as _,
            )
        }
    }
}

mod iobufmut {
    use crate::{ReadInto, RollMut};

    use super::BufMut;
    pub trait Sealed {}
    impl Sealed for BufMut {}
    impl Sealed for RollMut {}
    impl Sealed for ReadInto {}
    impl Sealed for Vec<u8> {}
}

/// The IoBufMut trait is implemented by buffer types that can be passed to
/// io-uring operations.
///
/// # Safety
///
/// If the address returned by `io_buf_mut_stable_mut_ptr` is not actually
/// stable and moves while an io_uring operation is in-flight, the kernel might
/// write to the wrong memory location.
pub unsafe trait IoBufMut: iobufmut::Sealed {
    /// Gets a pointer to the start of the buffer
    fn io_buf_mut_stable_mut_ptr(&mut self) -> *mut u8;

    /// Gets the capacity of the buffer
    fn io_buf_mut_capacity(&self) -> usize;

    /// Gets a mutable slice of the buffer
    ///
    /// # Safety
    ///
    /// An arbitrary implementor may return invalid pointers or lengths.
    unsafe fn slice_mut(&mut self) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.io_buf_mut_stable_mut_ptr(), self.io_buf_mut_capacity())
    }
}

unsafe impl IoBufMut for BufMut {
    fn io_buf_mut_stable_mut_ptr(&mut self) -> *mut u8 {
        unsafe { privatepool::base_ptr_with_offset(self.index, self.off as _) }
    }

    fn io_buf_mut_capacity(&self) -> usize {
        self.len as usize
    }
}

unsafe impl IoBufMut for Vec<u8> {
    fn io_buf_mut_stable_mut_ptr(&mut self) -> *mut u8 {
        self.as_mut_ptr()
    }

    fn io_buf_mut_capacity(&self) -> usize {
        self.capacity()
    }
}

impl Drop for BufMut {
    fn drop(&mut self) {
        privatepool::dec(self.index);
    }
}

/// A read-only buffer. Can be cloned, but cannot be written to.
pub struct Buf {
    pub(crate) index: u32,
    pub(crate) off: u16,
    pub(crate) len: u16,

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

    /// Take an owned slice of this
    pub fn slice(mut self, range: impl RangeBounds<usize>) -> Self {
        let mut new_start = 0;
        let mut new_end = self.len();

        match range.start_bound() {
            Bound::Included(&n) => new_start = n,
            Bound::Excluded(&n) => new_start = n + 1,
            Bound::Unbounded => {}
        }

        match range.end_bound() {
            Bound::Included(&n) => new_end = n + 1,
            Bound::Excluded(&n) => new_end = n,
            Bound::Unbounded => {}
        }

        assert!(new_start <= new_end);
        assert!(new_end <= self.len());

        self.off += new_start as u16;
        self.len = (new_end - new_start) as u16;
        self
    }

    /// Split this buffer in twain.
    /// Panics if `at` is out of bounds.
    #[inline]
    pub fn split_at(self, at: usize) -> (Self, Self) {
        assert!(at <= self.len as usize);

        let left = Buf {
            index: self.index,
            off: self.off,
            len: at as _,

            _non_send: PhantomData,
        };

        let right = Buf {
            index: self.index,
            off: self.off + at as u16,
            len: (self.len - at as u16),

            _non_send: PhantomData,
        };

        std::mem::forget(self); // don't decrease ref count
        privatepool::inc(left.index); // in fact, increase it by 1

        (left, right)
    }
}

impl ops::Deref for Buf {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                privatepool::base_ptr_with_offset(self.index, self.off as _),
                self.len as _,
            )
        }
    }
}

impl Clone for Buf {
    fn clone(&self) -> Self {
        privatepool::inc(self.index);
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
        privatepool::dec(self.index);
    }
}

#[cfg(test)]
mod tests {
    use crate::{num_free, Buf, BufMut};
    use std::rc::Rc;

    #[test]
    fn size_test() {
        crate::bufpool::initialize_allocator().unwrap();

        assert_eq!(8, std::mem::size_of::<BufMut>());
        assert_eq!(8, std::mem::size_of::<Buf>());
        assert_eq!(16, std::mem::size_of::<Box<[u8]>>());

        assert_eq!(16, std::mem::size_of::<&[u8]>());

        #[allow(dead_code)]
        enum BufOrBox {
            Buf(Buf),
            Box((Rc<Box<[u8]>>, u32, u32)),
        }
        assert_eq!(16, std::mem::size_of::<BufOrBox>());

        #[allow(dead_code)]
        enum Chunk {
            Buf(Buf),
            Box(Box<[u8]>),
            Static(&'static [u8]),
        }
        assert_eq!(24, std::mem::size_of::<Chunk>());
    }

    #[test]
    fn freeze_test() -> eyre::Result<()> {
        crate::bufpool::initialize_allocator().unwrap();

        let total_bufs = num_free();
        let mut bm = BufMut::alloc().unwrap();

        assert_eq!(total_bufs - 1, num_free());
        assert_eq!(bm.len(), 4096);

        bm[..11].copy_from_slice(b"hello world");
        assert_eq!(&bm[..11], b"hello world");

        let b = bm.freeze();
        assert_eq!(&b[..11], b"hello world");
        assert_eq!(total_bufs - 1, num_free());

        let b2 = b.clone();
        assert_eq!(&b[..11], b"hello world");
        assert_eq!(total_bufs - 1, num_free());

        drop(b);
        assert_eq!(total_bufs - 1, num_free());

        drop(b2);
        assert_eq!(total_bufs, num_free());

        Ok(())
    }

    #[test]
    fn split_test() -> eyre::Result<()> {
        crate::bufpool::initialize_allocator().unwrap();

        let total_bufs = num_free();
        let mut bm = BufMut::alloc().unwrap();

        bm[..12].copy_from_slice(b"yellowjacket");
        let (a, b) = bm.split_at(6);

        assert_eq!(total_bufs - 1, num_free());
        assert_eq!(&a[..], b"yellow");
        assert_eq!(&b[..6], b"jacket");

        drop((a, b));

        Ok(())
    }
}
