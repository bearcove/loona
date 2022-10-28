use std::{
    borrow::Cow,
    cell::UnsafeCell,
    fmt::{Debug, Formatter},
    iter::Enumerate,
    ops::{Bound, Deref, RangeBounds},
    rc::Rc,
};

use nom::{
    Compare, CompareResult, FindSubstring, InputIter, InputLength, InputTake, InputTakeAtPosition,
    Needed,
};
use tokio_uring::buf::{IoBuf, IoBufMut};

use crate::{Buf, BufMut, IoChunk, IoChunkable, ReadOwned, BUF_SIZE};

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

    /// Reallocates the backing storage for this buffer, copying the filled
    /// portion into it. Panics if buf.len() == buf.cap(), in which case
    /// reallocating won't do much good
    pub fn realloc(&mut self) {
        todo!("this is maybe a bad API, should it just grow and accept a bound?");
    }

    /// Reserve more capacity for this buffer if this buffer is full.
    /// If this buffer's size matches the underlying storage size,
    /// this is equivalent to `grow`. Otherwise, it's equivalent
    /// to `realloc`.
    pub fn reserve(&mut self) {
        if self.len() < self.cap() {
            return;
        }

        if self.len() < self.storage_size() {
            // we don't need to go up a buffer size
            self.realloc()
        } else {
            self.grow()
        }
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

    /// The size of the underlying storage (to know whether
    /// to reallocate or grow)
    pub fn storage_size(&self) -> usize {
        self.storage.cap()
    }

    /// Read at most `limit` bytes from [ReadOwned] into this buffer.
    ///
    /// This method takes ownership of `self` because it submits an io_uring
    /// operation, where the kernel owns the read buffer - the only way to
    /// gain ownership of `self` again is to complete the read operation.
    pub async fn read_into(
        self,
        limit: usize,
        r: &impl ReadOwned,
    ) -> (std::io::Result<usize>, Self) {
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
        (res, read_into.buf)
    }

    /// Put a slice into this buffer, fails if the slice doesn't fit in the buffer's capacity
    pub fn put(&mut self, s: impl AsRef<[u8]>) -> Result<(), DoesNotFit> {
        let s = s.as_ref();

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

    /// Takes the first `n` bytes (up to `len`) as a `Roll`, and advances
    /// this buffer. Returns `None` if `len` is zero.
    pub fn take_at_most(&mut self, n: usize) -> Option<Roll> {
        if self.len == 0 {
            return None;
        }

        let n = std::cmp::min(n, self.len as usize);
        let roll = self.filled().slice(..n);
        self.skip(n);
        Some(roll)
    }

    /// Takes the whole `filled` part. Panics if empty, since this is probably
    /// a misuse.
    pub fn take_all(&mut self) -> Roll {
        let roll = self.filled();
        assert!(
            !roll.is_empty(),
            "take_all is pointless if the filled part is empty, check len first"
        );
        self.skip(roll.len());
        roll
    }

    /// Advance this buffer, keeping the filled part only starting with
    /// the given roll. (Useful after parsing something with nom)
    ///
    /// Panics if the roll is not from this buffer
    pub fn keep(&mut self, roll: Roll) {
        match (&mut self.storage, &roll.inner) {
            (StorageMut::Buf(ours), RollInner::Buf(theirs)) => {
                assert_eq!(ours.index, theirs.index, "roll must be from same buffer");
                assert!(theirs.off >= ours.off, "roll must start within buffer");
                let skipped = theirs.off - ours.off;
                self.len -= skipped as u32;
                ours.off = theirs.off;
            }
            (StorageMut::Box(ours), RollInner::Box(theirs)) => {
                assert_eq!(
                    ours.buf.get(),
                    theirs.b.buf.get(),
                    "roll must be from same buffer"
                );
                assert!(theirs.b.off >= ours.off, "roll must start within buffer");
                let skipped = theirs.b.off - ours.off;
                self.len -= skipped;
                ours.off = theirs.b.off;
            }
            _ => {
                panic!("roll must be from same buffer");
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
        unreachable!("ReadInto should never be used as a write buffer")
    }

    #[inline(always)]
    fn bytes_init(&self) -> usize {
        unreachable!("ReadInto should never be used as a write buffer")
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

impl IoChunkable for Roll {
    fn len(&self) -> usize {
        Roll::len(self)
    }

    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        if offset == 0 {
            Some(IoChunk::Roll(self.clone()))
        } else {
            None
        }
    }
}

unsafe impl IoBuf for Roll {
    #[inline(always)]
    fn stable_ptr(&self) -> *const u8 {
        match &self.inner {
            RollInner::Buf(b) => b.stable_ptr(),
            RollInner::Box(b) => b.b.as_ptr(),
        }
    }

    #[inline(always)]
    fn bytes_init(&self) -> usize {
        self.len()
    }

    #[inline(always)]
    fn bytes_total(&self) -> usize {
        self.len()
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

    fn slice(mut self, range: impl RangeBounds<usize>) -> Self {
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

        self.b.off += new_start as u32;
        self.len = (new_end - new_start) as u32;
        self
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

    pub fn slice(self, range: impl RangeBounds<usize>) -> Self {
        match self.inner {
            RollInner::Buf(b) => b.slice(range).into(),
            RollInner::Box(b) => b.slice(range).into(),
        }
    }

    pub fn iter(&self) -> RollIter {
        RollIter {
            roll: self.clone(),
            pos: 0,
        }
    }

    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(self)
    }
}

impl InputIter for Roll {
    type Item = u8;
    type Iter = Enumerate<Self::IterElem>;
    type IterElem = RollIter;

    #[inline]
    fn iter_indices(&self) -> Self::Iter {
        self.iter_elements().enumerate()
    }
    #[inline]
    fn iter_elements(&self) -> Self::IterElem {
        self.iter()
    }
    #[inline]
    fn position<P>(&self, predicate: P) -> Option<usize>
    where
        P: Fn(Self::Item) -> bool,
    {
        self.iter().position(predicate)
    }
    #[inline]
    fn slice_index(&self, count: usize) -> Result<usize, Needed> {
        if self.len() >= count {
            Ok(count)
        } else {
            Err(Needed::new(count - self.len()))
        }
    }
}

/// An iterator over [Roll]
pub struct RollIter {
    roll: Roll,
    pos: usize,
}

impl Iterator for RollIter {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.roll.len() {
            return None;
        }

        let c = self.roll[self.pos];
        self.pos += 1;
        Some(c)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.roll.len() - self.pos;
        (remaining, Some(remaining))
    }
}

impl InputTake for Roll {
    #[inline]
    fn take(&self, count: usize) -> Self {
        self.clone().slice(..count)
    }
    #[inline]
    fn take_split(&self, count: usize) -> (Self, Self) {
        let (prefix, suffix) = self.clone().split_at(count);
        (suffix, prefix)
    }
}

impl InputTakeAtPosition for Roll {
    type Item = u8;

    fn split_at_position<P, E: nom::error::ParseError<Self>>(
        &self,
        predicate: P,
    ) -> nom::IResult<Self, Self, E>
    where
        P: Fn(Self::Item) -> bool,
    {
        match self.iter().position(predicate) {
            Some(i) => Ok(self.clone().take_split(i)),
            None => Err(nom::Err::Incomplete(nom::Needed::new(1))),
        }
    }

    fn split_at_position1<P, E: nom::error::ParseError<Self>>(
        &self,
        predicate: P,
        e: nom::error::ErrorKind,
    ) -> nom::IResult<Self, Self, E>
    where
        P: Fn(Self::Item) -> bool,
    {
        match self.iter().position(predicate) {
            Some(0) => Err(nom::Err::Error(E::from_error_kind(self.clone(), e))),
            Some(i) => Ok(self.take_split(i)),
            None => Err(nom::Err::Incomplete(nom::Needed::new(1))),
        }
    }

    fn split_at_position_complete<P, E: nom::error::ParseError<Self>>(
        &self,
        predicate: P,
    ) -> nom::IResult<Self, Self, E>
    where
        P: Fn(Self::Item) -> bool,
    {
        match self.iter().position(predicate) {
            Some(i) => Ok(self.take_split(i)),
            None => Ok(self.take_split(self.input_len())),
        }
    }

    fn split_at_position1_complete<P, E: nom::error::ParseError<Self>>(
        &self,
        predicate: P,
        e: nom::error::ErrorKind,
    ) -> nom::IResult<Self, Self, E>
    where
        P: Fn(Self::Item) -> bool,
    {
        match self.iter().position(predicate) {
            Some(0) => Err(nom::Err::Error(E::from_error_kind(self.clone(), e))),
            Some(i) => Ok(self.take_split(i)),
            None => {
                if self.is_empty() {
                    Err(nom::Err::Error(E::from_error_kind(self.clone(), e)))
                } else {
                    Ok(self.take_split(self.input_len()))
                }
            }
        }
    }
}

impl FindSubstring<&[u8]> for Roll {
    fn find_substring(&self, substr: &[u8]) -> Option<usize> {
        if substr.len() > self.len() {
            return None;
        }

        let (&substr_first, substr_rest) = match substr.split_first() {
            Some(split) => split,
            // an empty substring is found at position 0
            // This matches the behavior of str.find("").
            None => return Some(0),
        };

        if substr_rest.is_empty() {
            return memchr::memchr(substr_first, self);
        }

        let mut offset = 0;
        let haystack = &self[..self.len() - substr_rest.len()];

        while let Some(position) = memchr::memchr(substr_first, &haystack[offset..]) {
            offset += position;
            let next_offset = offset + 1;
            if &self[next_offset..][..substr_rest.len()] == substr_rest {
                return Some(offset);
            }

            offset = next_offset;
        }

        None
    }
}

impl Compare<&[u8]> for Roll {
    #[inline(always)]
    fn compare(&self, t: &[u8]) -> CompareResult {
        let pos = self.iter().zip(t.iter()).position(|(a, b)| a != *b);

        match pos {
            Some(_) => CompareResult::Error,
            None => {
                if self.len() >= t.len() {
                    CompareResult::Ok
                } else {
                    CompareResult::Incomplete
                }
            }
        }
    }

    #[inline(always)]
    fn compare_no_case(&self, t: &[u8]) -> CompareResult {
        if self
            .iter()
            .zip(t)
            .any(|(a, b)| lowercase_byte(a) != lowercase_byte(*b))
        {
            CompareResult::Error
        } else if self.len() < t.len() {
            CompareResult::Incomplete
        } else {
            CompareResult::Ok
        }
    }
}

fn lowercase_byte(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A' + b'a',
        _ => c,
    }
}

impl InputLength for Roll {
    #[inline]
    fn input_len(&self) -> usize {
        self.len()
    }
}

#[cfg(test)]
mod tests {
    use nom::IResult;
    use tokio_uring::net::{TcpListener, TcpStream};

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
            (res, rm) = rm.read_into(3, &read).await;
            res.unwrap();

            assert_eq!(rm.len(), 3);
            assert_eq!(rm.filled().as_ref(), b"123");

            (res, rm) = rm.read_into(3, &read).await;
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
            assert_eq!(rm.borrow_filled(), b"world");
        }

        let rm = RollMut::alloc().unwrap();
        test_roll_keep_inner(rm);

        let mut rm = RollMut::alloc().unwrap();
        rm.grow();
        test_roll_keep_inner(rm);
    }

    #[test]
    #[should_panic(expected = "roll must be from same buffer")]
    fn test_roll_keep_different_buf() {
        let mut rm1 = RollMut::alloc().unwrap();
        rm1.put("hello").unwrap();

        let mut rm2 = RollMut::alloc().unwrap();
        rm2.put("hello").unwrap();
        let roll2 = rm2.take_all();

        rm1.keep(roll2);
    }

    #[test]
    #[should_panic(expected = "roll must be from same buffer")]
    fn test_roll_keep_different_box() {
        let mut rm1 = RollMut::alloc().unwrap();
        rm1.grow();
        rm1.put("hello").unwrap();

        let mut rm2 = RollMut::alloc().unwrap();
        rm2.grow();
        rm2.put("hello").unwrap();
        let roll2 = rm2.take_all();

        rm1.keep(roll2);
    }

    #[test]
    #[should_panic(expected = "roll must be from same buffer")]
    fn test_roll_keep_different_type() {
        let mut rm1 = RollMut::alloc().unwrap();
        rm1.grow();
        rm1.put("hello").unwrap();

        let mut rm2 = RollMut::alloc().unwrap();
        rm2.put("hello").unwrap();
        let roll2 = rm2.take_all();

        rm1.keep(roll2);
    }

    #[test]
    #[should_panic(expected = "roll must start within buffer")]
    fn test_roll_keep_before_buf() {
        let mut rm1 = RollMut::alloc().unwrap();
        rm1.put("hello").unwrap();
        let roll = rm1.filled();
        rm1.skip(5);
        rm1.keep(roll);
    }

    #[test]
    #[should_panic(expected = "roll must start within buffer")]
    fn test_roll_keep_before_box() {
        let mut rm1 = RollMut::alloc().unwrap();
        rm1.grow();
        rm1.put("hello").unwrap();
        let roll = rm1.filled();
        rm1.skip(5);
        rm1.keep(roll);
    }

    #[test]
    fn test_roll_iter() {
        let mut rm = RollMut::alloc().unwrap();
        rm.put(b"hello").unwrap();
        let roll = rm.filled();
        let v = roll.iter().collect::<Vec<_>>();
        assert_eq!(v, b"hello");

        assert_eq!(roll.to_string_lossy(), "hello");
    }

    #[test]
    fn test_roll_iobuf() {
        async fn test_roll_iobuf_inner(mut rm: RollMut) -> eyre::Result<()> {
            rm.put(b"hello").unwrap();
            let roll = rm.take_all();

            let ln = TcpListener::bind("[::]:0".parse()?)?;
            let local_addr = ln.local_addr()?;

            let send_fut = async move {
                let stream = TcpStream::connect(local_addr).await?;
                let res;
                (res, _) = stream.write_all(roll).await;
                res?;
                Ok::<_, eyre::Report>(())
            };

            let recv_fut = async move {
                let (stream, addr) = ln.accept().await?;
                println!("Accepted connection from {addr}");

                let mut buf = Vec::with_capacity(1024);
                let res;
                (res, buf) = stream.read(buf).await;
                res?;

                assert_eq!(buf, b"hello");

                Ok::<_, eyre::Report>(())
            };

            tokio::try_join!(send_fut, recv_fut)?;
            Ok(())
        }

        tokio_uring::start(async move {
            let rm = RollMut::alloc().unwrap();
            test_roll_iobuf_inner(rm).await.unwrap();

            let mut rm = RollMut::alloc().unwrap();
            rm.grow();
            test_roll_iobuf_inner(rm).await.unwrap();
        });
    }

    #[test]
    fn test_roll_nom_sample() {
        fn parse(i: Roll) -> IResult<Roll, Roll> {
            nom::bytes::streaming::tag(&b"HTTP/1.1 200 OK"[..])(i)
        }

        let mut buf = RollMut::alloc().unwrap();

        let input = b"HTTP/1.1 200 OK".repeat(1000);
        let mut pending = &input[..];

        loop {
            let (rest, version) = match parse(buf.filled()) {
                Ok(t) => t,
                Err(e) => {
                    if e.is_incomplete() {
                        {
                            if pending.is_empty() {
                                println!("ran out of input");
                                break;
                            }

                            let n = std::cmp::min(buf.cap(), pending.len());
                            buf.put(&pending[..n]).unwrap();
                            pending = &pending[n..];

                            println!("advanced by {n}, {} remaining", pending.len());
                        }

                        continue;
                    }
                    panic!("parsing error: {e}");
                }
            };
            assert_eq!(version, b"HTTP/1.1 200 OK");

            buf.keep(rest);
        }
    }
}
