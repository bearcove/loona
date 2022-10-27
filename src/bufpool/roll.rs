use std::{
    cell::UnsafeCell,
    fmt::{Debug, Formatter},
    ops::{Bound, Deref, RangeBounds},
    rc::Rc,
};

use tokio_uring::buf::{IoBuf, IoBufMut};

use crate::{Buf, BufMut, ReadOwned, BUF_SIZE};

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
    fn cap(&self) -> usize {
        match self {
            StorageMut::Buf(_) => BUF_SIZE as usize,
            StorageMut::Box(b) => b.cap(),
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            StorageMut::Buf(b) => b.len(),
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
    fn len(&self) -> usize {
        let buf = self.buf.get();
        let len = unsafe { (*buf).len() };
        len - self.off as usize
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

    /// Returns a mutable slice of bytes into this buffer, of the specified
    /// length Panics if the length is larger than the buffer.
    fn slice_mut(&mut self, len: u32) -> &mut [u8] {
        let buf = self.buf.get();
        unsafe { &mut (*buf)[self.off as usize..][..len as usize] }
    }

    fn cap(&self) -> usize {
        let buf = self.buf.get();
        unsafe { (*buf).len() }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("slice does not fit into this RollMut")]
pub struct DoesNotFit;

impl RollMut {
    /// Allocate, using a single [BufMut] for storage.
    pub fn alloc() -> eyre::Result<Self> {
        Ok(Self {
            storage: StorageMut::Buf(BufMut::alloc()?),
            len: 0,
        })
    }

    /// Double the capacity of this buffer by reallocating it, copying the
    /// filled part into the new buffer. This method always uses a `Box<[u8]>`
    /// for storage.
    ///
    /// This method is somewhat expensive.
    pub fn grow(&mut self) {
        let old_cap = self.storage.cap();
        let new_cap = old_cap * 2;
        let b = vec![0; new_cap].into_boxed_slice();
        let mut bs = BoxStorage {
            buf: Rc::new(UnsafeCell::new(b)),
            off: 0,
        };
        let src_slice = self.borrow_filled();
        let dst_slice = bs.slice_mut(src_slice.len() as u32);
        dst_slice.copy_from_slice(src_slice);
        let s = StorageMut::Box(bs);

        self.storage = s;
    }

    /// The length (filled portion) of this buffer, that can be read
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if this is empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The capacity of this buffer, that can be written to
    #[inline(always)]
    pub fn cap(&self) -> usize {
        self.storage.len() - self.len as usize
    }

    /// Read at most `limit` bytes from [ReadOwned] into this buffer.
    pub async fn read_into(
        self,
        limit: usize,
        r: &impl ReadOwned,
    ) -> (Self, std::io::Result<usize>) {
        let read_cap = std::cmp::min(limit, self.cap());
        let read_off = self.len;
        let read_into = ReadInto {
            buf: self,
            off: read_off,
            cap: read_cap.try_into().unwrap(),
            init: 0,
        };
        let (res, mut read_into) = r.read(read_into).await;
        read_into.buf.len += read_into.init;
        (read_into.buf, res)
    }

    /// Put a slice into this buffer, fails if the slice doesn't fit in the buffer's capacity
    pub fn put(&mut self, s: &[u8]) -> Result<(), DoesNotFit> {
        let len = s.len();
        if len > self.cap() {
            return Err(DoesNotFit);
        }
        unsafe {
            let ptr = self.storage.as_mut_ptr().add(self.len as usize);
            std::ptr::copy_nonoverlapping(s.as_ptr(), ptr, len);
        }
        let u32_len: u32 = len.try_into().unwrap();
        self.len += u32_len;
        Ok(())
    }

    /// Get a [Roll] corresponding to the filled portion of this buffer
    pub fn filled(&self) -> Roll {
        match &self.storage {
            StorageMut::Buf(b) => b.freeze_slice(0..self.len()).into(),
            StorageMut::Box(b) => RollBox {
                b: b.clone(),
                len: self.len,
            }
            .into(),
        }
    }

    /// Borrow the filled portion of this buffer as a slice
    pub fn borrow_filled(&self) -> &[u8] {
        match &self.storage {
            StorageMut::Buf(b) => &b[..self.len as usize],
            StorageMut::Box(b) => b.slice(self.len),
        }
    }

    /// Split this [RollMut] at the given index.
    /// Panics if `at > len()`. If `at < len()`, the filled portion will carry
    /// over in the new [RollMut].
    pub fn skip(&mut self, n: usize) {
        let u32_n: u32 = n.try_into().unwrap();
        assert!(u32_n <= self.len);

        match &mut self.storage {
            StorageMut::Buf(b) => b.skip(n),
            StorageMut::Box(b) => b.off += u32_n,
        }
        self.len -= u32_n;
    }

    /// Advance this buffer, keeping the filled part only starting with
    /// the given roll. (Useful after parsing something with nom)
    ///
    /// Panics if the roll is not from this buffer
    pub fn keep(&mut self, roll: Roll) {
        match (&mut self.storage, &roll.inner) {
            (StorageMut::Buf(ours), RollInner::Buf(theirs)) => {
                assert_eq!(ours.index, theirs.index, "roll must be from same buffer");
                assert!(theirs.off >= ours.off, "roll must be from same buffer");
                ours.off -= theirs.off;
            }
            (StorageMut::Box(ours), RollInner::Box(theirs)) => {
                assert_eq!(
                    ours.buf.get(),
                    theirs.b.buf.get(),
                    "roll must be from same buffer"
                );
                assert!(theirs.b.off >= ours.off);
                ours.off -= theirs.b.off;
            }
            _ => {
                panic!("cannot keep roll from different buffer");
            }
        }
    }
}

struct ReadInto {
    buf: RollMut,
    off: u32,
    cap: u32,
    init: u32,
}

unsafe impl IoBuf for ReadInto {
    #[inline(always)]
    fn stable_ptr(&self) -> *const u8 {
        unsafe { self.buf.storage.as_ptr().add(self.off as usize) }
    }

    #[inline(always)]
    fn bytes_init(&self) -> usize {
        0
    }

    #[inline(always)]
    fn bytes_total(&self) -> usize {
        self.cap as _
    }
}

unsafe impl IoBufMut for ReadInto {
    #[inline(always)]
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        unsafe { self.buf.storage.as_mut_ptr().add(self.off as usize) }
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

impl Debug for Roll {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self[..], f)
    }
}

impl<T> PartialEq<T> for Roll
where
    T: AsRef<[u8]>,
{
    fn eq(&self, other: &T) -> bool {
        &self[..] == other.as_ref()
    }
}

impl Eq for Roll {}

impl From<RollInner> for Roll {
    fn from(inner: RollInner) -> Self {
        Self { inner }
    }
}

impl From<RollBox> for Roll {
    fn from(b: RollBox) -> Self {
        RollInner::Box(b).into()
    }
}

impl From<Buf> for Roll {
    fn from(b: Buf) -> Self {
        RollInner::Buf(b).into()
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
    fn split_at(self, at: usize) -> (Self, Self) {
        let at: u32 = at.try_into().unwrap();
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
    fn len(&self) -> usize {
        self.len as usize
    }

    fn slice(&self, range: impl RangeBounds<usize>) -> Self {
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

        let mut new = self.clone();
        new.b.off += new_start as u32;
        new.len = (new_end - new_start) as u32;
        new
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

impl Deref for Roll {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl RollInner {
    #[inline(always)]
    fn split_at(self, at: usize) -> (Self, Self) {
        match self {
            RollInner::Buf(b) => {
                let (left, right) = b.split_at(at);
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
    pub fn len(&self) -> usize {
        match &self.inner {
            RollInner::Buf(b) => b.len(),
            RollInner::Box(b) => b.len(),
        }
    }

    /// Returns true if this roll is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn split_at(self, at: usize) -> (Roll, Roll) {
        let (left, right) = self.inner.split_at(at);
        (left.into(), right.into())
    }

    pub fn slice(&self, range: impl RangeBounds<usize>) -> Self {
        match &self.inner {
            RollInner::Buf(b) => b.slice(range).into(),
            RollInner::Box(b) => b.slice(range).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use nom::IResult;

    use crate::{ChanRead, Roll, RollMut, BUF_SIZE};

    #[test]
    fn test_roll_put() {
        fn test_roll_put_inner(mut rm: RollMut) {
            let initial_size = rm.cap();

            rm.put(b"hello").unwrap();
            assert_eq!(rm.cap(), initial_size - 5);

            let filled = rm.filled();
            assert_eq!(&filled[..], b"hello");

            rm.skip(5);
            assert_eq!(rm.len(), 0);
            assert_eq!(rm.cap(), initial_size - 5);
        }

        let rm = RollMut::alloc().unwrap();
        assert_eq!(rm.cap(), BUF_SIZE as usize);
        test_roll_put_inner(rm);

        let mut rm = RollMut::alloc().unwrap();
        rm.grow();
        test_roll_put_inner(rm);

        let mut rm = RollMut::alloc().unwrap();
        rm.grow();
        rm.grow();
        test_roll_put_inner(rm);
    }

    #[test]
    fn test_roll_put_then_grow() {
        let mut rm = RollMut::alloc().unwrap();
        assert_eq!(rm.cap(), BUF_SIZE as usize);

        let input = b"I am pretty long";

        rm.put(input).unwrap();
        assert_eq!(rm.len(), input.len());
        assert_eq!(rm.borrow_filled(), input);

        assert_eq!(rm.cap(), BUF_SIZE as usize - input.len());

        rm.grow();
        assert_eq!(rm.cap(), 2 * (BUF_SIZE as usize) - input.len());
        assert_eq!(rm.borrow_filled(), input);

        rm.skip(5);
        assert_eq!(rm.borrow_filled(), b"pretty long");
    }

    #[test]
    fn test_roll_readfrom_start() {
        tokio_uring::start(async move {
            let mut rm = RollMut::alloc().unwrap();

            let (send, read) = ChanRead::new();
            tokio_uring::spawn(async move {
                send.send("123456").await.unwrap();
            });

            let mut res;
            (rm, res) = rm.read_into(3, &read).await;
            res.unwrap();

            assert_eq!(rm.len(), 3);
            assert_eq!(rm.filled().as_ref(), b"123");

            (rm, res) = rm.read_into(3, &read).await;
            res.unwrap();

            assert_eq!(rm.len(), 6);
            assert_eq!(rm.filled().as_ref(), b"123456");
        });
    }

    #[test]
    fn test_roll_keep() {
        fn test_roll_keep_inner(mut rm: RollMut) {
            rm.put(b"helloworld").unwrap();
            assert_eq!(rm.borrow_filled(), b"helloworld");

            {
                let roll = rm.filled().slice(3..=5);
                assert_eq!(roll, b"low");
            }

            {
                let roll = rm.filled().slice(..5);
                assert_eq!(roll, b"hello");
            }

            let roll = rm.filled().slice(5..);
            assert_eq!(roll, b"world");

            rm.keep(roll);
            assert_eq!(rm.borrow_filled(), b"hello");
        }

        let rm = RollMut::alloc().unwrap();
        test_roll_keep_inner(rm);

        let mut rm = RollMut::alloc().unwrap();
        rm.grow();
        test_roll_keep_inner(rm);
    }

    // #[test]
    // fn test_roll_nom_sample() {
    //     fn parse(i: Roll) -> IResult<Roll, Roll> {
    //         nom::bytes::streaming::tag(&b"HTTP/1.1 200 OK"[..])(i)
    //     }

    //     let mut buf = RollMut::alloc().unwrap();

    //     let input = b"HTTP/1.1 200 OK".repeat(1000);
    //     let mut pending = &input[..];

    //     loop {
    //         let (rest, version) = match parse(buf.filled()) {
    //             Ok(t) => t,
    //             Err(e) => {
    //                 if e.is_incomplete() {
    //                     {
    //                         if pending.is_empty() {
    //                             println!("ran out of input");
    //                             break;
    //                         }

    //                         let n = std::cmp::min(buf.cap(), pending.len());
    //                         buf.put(&pending[..n]).unwrap();
    //                         pending = &pending[n..];

    //                         println!("advanced by {n}, {} remaining", pending.len());
    //                     }

    //                     continue;
    //                 }
    //                 panic!("parsing error: {e}");
    //             }
    //         };
    //         assert_eq!(version, b"HTTP/1.1 200 OK");

    //         buf.keep(rest);
    //     }
    // }
}
