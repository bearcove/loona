use std::{cell::UnsafeCell, rc::Rc};

use tokio_uring::buf::{IoBuf, IoBufMut};

use crate::{Buf, BufMut, ReadOwned};

/// A "rolling buffer". Uses either one [BufMut] or a `Box<[u8]>` for storage.
/// This buffer never grows, but it can be split, and it can be reallocated so
/// it regains its initical capacity, minus the length of the filled part.
pub struct RollMut {
    storage: StorageMut,
    len: u32,
}

enum StorageMut {
    Buf(BufMut),
    Box(BoxStorage),
}

impl StorageMut {
    #[inline(always)]
    fn len(&self) -> u32 {
        match self {
            StorageMut::Buf(b) => b.len() as u32,
            StorageMut::Box(b) => b.len(),
        }
    }

    fn as_ptr(&self) -> *const u8 {
        match self {
            StorageMut::Buf(b) => b.as_ptr(),
            StorageMut::Box(b) => b.as_ptr(),
        }
    }

    unsafe fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            StorageMut::Buf(b) => b.as_mut_ptr(),
            StorageMut::Box(b) => b.as_mut_ptr(),
        }
    }
}

#[derive(Clone)]
struct BoxStorage {
    buf: Rc<UnsafeCell<Box<[u8]>>>,
    off: u32,
}

impl BoxStorage {
    #[inline(always)]
    fn len(&self) -> u32 {
        let buf = self.buf.get();
        let len = unsafe { (*buf).len() as u32 };
        len - self.off
    }

    fn as_ptr(&self) -> *const u8 {
        let buf = self.buf.get();
        unsafe { (*buf).as_ptr().add(self.off as usize) }
    }

    unsafe fn as_mut_ptr(&self) -> *mut u8 {
        let buf = self.buf.get();
        (*buf).as_mut_ptr().add(self.off as usize)
    }

    /// Returns a slice of bytes into this buffer, of the specified length
    /// Panics if the length is larger than the buffer.
    fn slice(&self, len: u32) -> &[u8] {
        let buf = self.buf.get();
        unsafe { &(*buf)[self.off as usize..][..len as usize] }
    }
}

impl RollMut {
    /// Allocate, using a single [BufMut] for storage.
    pub fn alloc() -> eyre::Result<Self> {
        Ok(Self {
            storage: StorageMut::Buf(BufMut::alloc()?),
            len: 0,
        })
    }

    /// The length (filled portion) of this buffer, that can be read
    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Returns true if this is empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The capacity of this buffer, that can be written to
    #[inline(always)]
    pub fn cap(&self) -> u32 {
        self.storage.len() - self.len
    }

    /// Read at most `limit` bytes from [ReadOwned] into this buffer.
    pub async fn read_into(self, limit: u32, r: &impl ReadOwned) -> (Self, std::io::Result<usize>) {
        let len = std::cmp::min(limit, self.cap());
        let read_into = ReadInto {
            buf: self,
            len,
            init: 0,
        };
        let (res, mut read_into) = r.read(read_into).await;
        read_into.buf.len += read_into.init;
        (read_into.buf, res)
    }

    /// Borrow the filled portion of this buffer as a [Roll]
    pub fn filled(&self) -> Roll {
        match &self.storage {
            StorageMut::Buf(b) => {
                let u16_len: u16 = self.len.try_into().unwrap();
                RollInner::Buf(b.freeze_slice(0..u16_len)).into()
            }
            StorageMut::Box(b) => RollInner::Box(RollBox {
                b: b.clone(),
                len: self.len,
            })
            .into(),
        }
    }
}

struct ReadInto {
    buf: RollMut,
    len: u32,
    init: u32,
}

unsafe impl IoBuf for ReadInto {
    #[inline(always)]
    fn stable_ptr(&self) -> *const u8 {
        self.buf.storage.as_ptr()
    }

    #[inline(always)]
    fn bytes_init(&self) -> usize {
        0
    }

    #[inline(always)]
    fn bytes_total(&self) -> usize {
        self.len as _
    }
}

unsafe impl IoBufMut for ReadInto {
    #[inline(always)]
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        unsafe { self.buf.storage.as_mut_ptr() }
    }

    #[inline(always)]
    unsafe fn set_init(&mut self, pos: usize) {
        self.init = pos.try_into().expect("reads should be < 4GiB");
    }
}

/// An immutable view into a [RollMut]
#[derive(Clone)]
pub struct Roll {
    inner: RollInner,
}

impl From<RollInner> for Roll {
    fn from(inner: RollInner) -> Self {
        Self { inner }
    }
}

#[derive(Clone)]
enum RollInner {
    Buf(Buf),
    Box(RollBox),
}

#[derive(Clone)]
struct RollBox {
    b: BoxStorage,
    len: u32,
}

impl RollBox {
    #[inline(always)]
    pub fn split_at(self, at: u32) -> (Self, Self) {
        assert!(at <= self.len);

        let left = Self {
            b: self.b.clone(),
            len: at,
        };

        let mut right = Self {
            b: self.b,
            len: self.len - at,
        };
        right.b.off += at;

        (left, right)
    }

    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.len
    }
}

impl AsRef<[u8]> for RollBox {
    #[inline(always)]
    fn as_ref(&self) -> &[u8] {
        self.b.slice(self.len)
    }
}

impl AsRef<[u8]> for Roll {
    #[inline(always)]
    fn as_ref(&self) -> &[u8] {
        match &self.inner {
            RollInner::Buf(b) => b.as_ref(),
            RollInner::Box(b) => b.as_ref(),
        }
    }
}

impl RollInner {
    #[inline(always)]
    fn split_at(self, at: u32) -> (Self, Self) {
        match self {
            RollInner::Buf(b) => {
                let (left, right) = b.split_at(at as usize);
                (RollInner::Buf(left), RollInner::Buf(right))
            }
            RollInner::Box(b) => {
                let (left, right) = b.split_at(at);
                (RollInner::Box(left), RollInner::Box(right))
            }
        }
    }
}

impl Roll {
    /// Returns the length of this roll
    #[inline(always)]
    pub fn len(&self) -> u32 {
        match &self.inner {
            RollInner::Buf(b) => b.len() as u32,
            RollInner::Box(b) => b.len() as u32,
        }
    }

    pub fn split_at(self, at: u32) -> (Roll, Roll) {
        let (left, right) = self.inner.split_at(at);
        (left.into(), right.into())
    }
}
