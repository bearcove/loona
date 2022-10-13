use std::{
    cell::{RefCell, RefMut},
    fmt,
    iter::Enumerate,
    ops::Range,
    rc::Rc,
};

use nom::{
    Compare, CompareResult, FindSubstring, InputIter, InputLength, InputTake, InputTakeAtPosition,
};
use smallvec::SmallVec;

use crate::bufpool::{self, BufMut};

macro_rules! dbg2 {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            // 🐉 uncomment to debug tests:
            // dbg!($($arg)*);
        }
    };
}

/// An "aggregate buffer", uses one or more [BufMut]s for storage. Allows
/// writing to uninitialized data, and borrowing ref-counted [AggregateSlice] of
/// initialized data.
pub struct AggregateBuf {
    inner: Rc<RefCell<AggregateBufInner>>,
}

/// The inner representation of an [AggregateBuf].
#[derive(Default)]
pub struct AggregateBufInner {
    /// storage
    blocks: SmallVec<[BufMut; 5]>,

    /// size of each block - typically a compile-time constant but let's
    /// have a field anyway for now.
    block_size: u32,

    /// global offset: how many bytes to ignore from the first block, in case
    /// this aggregate buffer is re-used from a previous operation.
    off: u32,

    /// length: number of bytes that are initialized (were we read into, and can
    /// be sliced).
    len: u32,
}

pub struct AggregateBufRead<'a> {
    handle: &'a Rc<RefCell<AggregateBufInner>>,
    borrow: std::cell::Ref<'a, AggregateBufInner>,
}

impl AggregateBuf {
    /// Create a new aggregate buffer backed by one buffer.
    pub fn new() -> Result<Self, bufpool::Error> {
        let inner = AggregateBufInner::new()?;
        Ok(Self {
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    pub fn read(&self) -> AggregateBufRead<'_> {
        let handle = &self.inner;
        let borrow = handle.borrow();
        AggregateBufRead { handle, borrow }
    }

    pub fn write(&self) -> RefMut<AggregateBufInner> {
        self.inner.borrow_mut()
    }

    /// Split off a new [AggregateBuf] from this one, re-using any unfilled
    /// space.
    pub fn split(self) -> Result<Self, bufpool::Error> {
        // Safety: we might have a bunch of [AggregateSlice] out there. They're
        // immutable references to the _filled_ part of `self`, so it's okay.

        let inner = self.inner.borrow();
        let reusable = inner.capacity() - inner.len;
        if reusable == 0 {
            return Self::new();
        }

        dbg2!(reusable, inner.block_size);

        // we can re-use the last block
        let global_off = inner.block_size - reusable;
        let reused_block = inner.blocks.iter().last().unwrap().dangerous_clone();

        let mut new_inner = AggregateBufInner {
            blocks: Default::default(),
            block_size: inner.block_size,
            off: global_off,
            len: 0,
        };
        new_inner.blocks.push(reused_block);
        Ok(Self {
            inner: Rc::new(RefCell::new(new_inner)),
        })
    }

    /// Return a write slice appropriate for a io_uring read
    pub fn write_slice(self) -> AggregateWriteSlice {
        let ptr;
        let len;

        {
            let mut inner = self.inner.borrow_mut();
            let (block_index, block_range) = inner.contiguous_range(inner.len..inner.capacity());
            ptr = unsafe {
                inner.blocks[block_index]
                    .as_mut_ptr()
                    .add(block_range.start)
            };
            len = block_range.len();
        }

        AggregateWriteSlice {
            buf: self,
            ptr,
            len,
            pos: 0,
        }
    }
}

/// A write slice of an [AggregateBuf] suitable for an io_uring read/write
pub struct AggregateWriteSlice {
    buf: AggregateBuf,
    ptr: *mut u8,
    pos: usize,
    len: usize,
}

impl AggregateWriteSlice {
    pub fn into_inner(self) -> AggregateBuf {
        self.buf.inner.borrow_mut().len += self.pos as u32;
        self.buf
    }
}

unsafe impl tokio_uring::buf::IoBuf for AggregateWriteSlice {
    fn stable_ptr(&self) -> *const u8 {
        self.ptr
    }

    fn bytes_init(&self) -> usize {
        self.pos
    }

    fn bytes_total(&self) -> usize {
        self.len
    }
}

unsafe impl tokio_uring::buf::IoBufMut for AggregateWriteSlice {
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    unsafe fn set_init(&mut self, pos: usize) {
        self.pos = pos
    }
}

impl AggregateBufInner {
    fn new() -> Result<Self, bufpool::Error> {
        let mut s = Self {
            blocks: Default::default(),
            block_size: crate::bufpool::BUF_SIZE as _,
            off: 0,
            len: 0,
        };
        s.blocks.push(BufMut::alloc()?);
        Ok(s)
    }

    /// Returns the size of each buffer in the pool
    #[inline(always)]
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Returns the total capacity of the buffer
    #[inline(always)]
    pub fn capacity(&self) -> u32 {
        self.blocks.len() as u32 * self.block_size - self.off
    }

    /// Returns the (filled) length of the buffer
    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Return true if this aggregate buf is empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns a block index and a slice into it, given a slice into the
    /// aggregate buffer. Doesn't check for the `filled` region
    fn contiguous_range(&self, wanted: Range<u32>) -> (usize, Range<usize>) {
        let (start, end) = (wanted.start, wanted.end);
        assert!(start <= end);

        if wanted.start == wanted.end {
            // special case: empty slice
            return (0, 0..0);
        }

        let wanted = end - start;

        // take the global offset into account when indexing into bufs
        let start = start + self.off;
        let block_index = (start / self.block_size) as usize;

        debug_assert!(block_index < self.blocks.len());

        let block_offset = start % self.block_size;
        let avail = self.block_size - block_offset;
        let given = std::cmp::min(avail, wanted);

        let block_offset = block_offset as usize;
        let given = given as usize;

        dbg2!(
            "contiguous_range",
            start,
            end,
            self.block_size,
            block_index,
            block_offset,
            avail,
            given
        );

        (block_index, block_offset..block_offset + given)
    }

    /// If `len == capacity` (ie. the `unfilled_mut` slice would be empty), try
    /// to add a block to this aggregate buffer. This is fallible, as we might
    /// be out of memory.
    pub fn grow_if_needed(&mut self) -> Result<(), bufpool::Error> {
        if self.len < self.capacity() {
            return Ok(());
        }

        let block = BufMut::alloc()?;
        self.blocks.push(block);
        Ok(())
    }

    pub fn put(&mut self, mut s: &[u8]) -> Result<(), bufpool::Error> {
        while !s.is_empty() {
            self.grow_if_needed()?;

            {
                let unfilled = self.unfilled_mut();
                let unfilled_len = unfilled.len();
                let to_copy = std::cmp::min(unfilled_len, s.len());
                unfilled[..to_copy].copy_from_slice(&s[..to_copy]);
                self.advance(to_copy as u32);
                s = &s[to_copy..];
            }
        }
        Ok(())
    }

    /// Gives a mutable slice that can be written to.
    /// Must call `advance` after writing to the returned slice.
    pub fn unfilled_mut(&mut self) -> &mut [u8] {
        let (block_index, range) = self.contiguous_range(self.len()..self.capacity());
        &mut self.blocks[block_index][range]
    }

    /// Called after writing to `unfilled_mut`. Panics if adding `n` brings the
    /// buffer over capacity.
    ///
    /// This isn't unsafe because [BufMut] are zeroed. But you will get
    /// incorrect results if you advance past the end of what's been filled.
    pub fn advance(&mut self, n: u32) {
        assert!(self.len + n <= self.capacity());
        self.len += n;
    }
}

impl AggregateBufRead<'_> {
    /// Take a slice out of the filled portion of this buffer. Panics if
    /// it is outside the filled portion.
    pub fn slice(&self, range: Range<u32>) -> AggregateSlice {
        assert!(range.start <= range.end);
        assert!(range.end <= self.borrow.len());

        AggregateSlice {
            parent: AggregateBuf {
                inner: self.handle.clone(),
            },
            off: range.start as _,
            len: (range.end - range.start) as _,
        }
    }

    /// Returns the biggest continuous slice we can get a the given offset.
    /// Panics if `wanted` is out of bounds. If the requested range spans
    /// multiple buffers, the returned slice will be smaller than requested.
    pub fn filled(&self, wanted: Range<u32>) -> &[u8] {
        // FIXME: this probably shouldn't be pub

        assert!(wanted.end <= self.borrow.len);

        let (block_index, given) = self.borrow.contiguous_range(wanted);
        &self.borrow.blocks[block_index][given]
    }

    /// Returns the (filled) length of the buffer
    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.borrow.len()
    }

    /// Return true if this aggregate buf is empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.borrow.is_empty()
    }
}

/// A slice of an [AggregateBuf]. This is a read-only view, it's clonable,
/// it holds a reference to the underlying [AggregateBuf], so holding it
/// will keep the _whole_ [AggregateBuf] alive.
pub struct AggregateSlice {
    parent: AggregateBuf,
    off: u32,
    len: u32,
}

impl Clone for AggregateSlice {
    fn clone(&self) -> Self {
        Self {
            parent: AggregateBuf {
                inner: self.parent.inner.clone(),
            },
            off: self.off,
            len: self.len,
        }
    }
}

impl fmt::Debug for AggregateSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AggregateSlice")
            .field("off", &self.off)
            .field("len", &self.len)
            .finish()
    }
}

impl AggregateSlice {
    /// Returns an iterator over the bytes in this slice
    #[inline]
    pub fn iter(&self) -> AggregateSliceIter {
        AggregateSliceIter {
            slice: self.clone(),
            pos: 0,
        }
    }

    /// Returns as a vector. This allocates a lot.
    pub fn to_vec(&self) -> Vec<u8> {
        self.iter().collect()
    }

    /// Returns as a string. This allocates a lot.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.to_vec()).to_string()
    }

    /// Returns the length of this slice
    pub fn len(&self) -> usize {
        self.len as _
    }

    /// Returns true if this is empty
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl InputLength for AggregateSlice {
    fn input_len(&self) -> usize {
        self.len as _
    }
}

impl InputTake for AggregateSlice {
    fn take(&self, count: usize) -> Self {
        let count: u32 = count.try_into().unwrap();
        if count > self.len {
            panic!("take: count > self.len");
        }

        Self {
            parent: AggregateBuf {
                inner: self.parent.inner.clone(),
            },
            off: self.off,
            len: count,
        }
    }

    fn take_split(&self, count: usize) -> (Self, Self) {
        let count: u32 = count.try_into().unwrap();
        if count > self.len {
            panic!("take_split: count > self.len");
        }

        let prefix = Self {
            parent: AggregateBuf {
                inner: self.parent.inner.clone(),
            },
            off: self.off,
            len: count,
        };
        let suffix = Self {
            parent: AggregateBuf {
                inner: self.parent.inner.clone(),
            },
            off: self.off + count,
            len: self.len - count,
        };
        (suffix, prefix)
    }
}

impl Compare<&[u8]> for AggregateSlice {
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

impl InputTakeAtPosition for AggregateSlice {
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

impl FindSubstring<&[u8]> for AggregateSlice {
    fn find_substring(&self, substr: &[u8]) -> Option<usize> {
        let mut offset = None;
        let mut curr_substr = substr;
        for (i, c) in self.iter().enumerate() {
            if c == curr_substr[0] {
                if offset.is_none() {
                    offset = Some(i);
                }
                curr_substr = &curr_substr[1..];
                if curr_substr.is_empty() {
                    return offset;
                }
            } else {
                curr_substr = substr;
            }
        }

        None
    }
}

impl InputIter for AggregateSlice {
    type Item = u8;
    type Iter = Enumerate<AggregateSliceIter>;
    type IterElem = AggregateSliceIter;

    #[inline]
    fn iter_indices(&self) -> Self::Iter {
        self.iter().enumerate()
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
    fn slice_index(&self, count: usize) -> Result<usize, nom::Needed> {
        if self.len() >= count {
            Ok(count)
        } else {
            Err(nom::Needed::new(count - self.len()))
        }
    }
}

fn lowercase_byte(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A' + b'a',
        _ => c,
    }
}

pub struct AggregateSliceIter {
    slice: AggregateSlice,
    pos: u32,
}

impl Iterator for AggregateSliceIter {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.slice.len {
            return None;
        }

        dbg2!("AggregateSliceIter::next", self.pos, self.slice.len);

        // FIXME: this implementation is extremely naive and not efficient at
        // all. we shouldn't have to borrow here or do block math on every
        // iteration: just until we run out of the current block.
        let inner = self.slice.parent.inner.borrow();
        let global_off = self.pos + self.slice.off;
        let (block_index, range) = inner.contiguous_range(global_off..global_off + 1);
        self.pos += 1;
        Some(inner.blocks[block_index][range][0])
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.slice.len - self.pos;
        (remaining as _, Some(remaining as _))
    }
}

#[cfg(test)]
mod tests {
    use crate::parse::aggregate::AggregateSlice;

    use super::{AggregateBuf, AggregateBufInner};
    use nom::IResult;
    use pretty_assertions::assert_eq;

    #[test]
    fn agg_inner_size() {
        assert_eq!(std::mem::size_of::<AggregateBufInner>(), 64);
    }

    #[test]
    fn agg_slice_size() {
        assert_eq!(std::mem::size_of::<AggregateBufInner>(), 16);
    }

    #[test]
    fn agg_fill() {
        let buf = AggregateBuf::new().unwrap();

        let block_size;
        let agg_len;
        {
            let mut buf = buf.write();
            block_size = buf.block_size();

            {
                println!("filling first block");
                let slice = buf.unfilled_mut();
                let len = slice.len();
                assert_eq!(len, block_size as usize);
                slice.fill(1);
                buf.advance(len as _);
                assert_eq!(buf.len(), block_size as _);
            }

            {
                println!("unfilled should be empty");
                let slice = buf.unfilled_mut();
                assert_eq!(slice.len(), 0);
            }

            {
                println!("allocating second block");
                buf.grow_if_needed().unwrap();
            }

            {
                println!("filling second block");
                let slice = buf.unfilled_mut();
                let len = slice.len();
                assert_eq!(len, block_size as usize);
                slice.fill(2);
                buf.advance(len as _);
                assert_eq!(buf.len(), block_size as u32 * 2);
            }

            {
                println!("unfilled should be empty again");
                let slice = buf.unfilled_mut();
                assert_eq!(slice.len(), 0);
            }

            agg_len = buf.len();
        }

        {
            let buf = buf.read();

            let slice = buf.filled(0..agg_len);
            assert_eq!(slice.len(), block_size as usize);
            for b in slice {
                assert_eq!(*b, 1)
            }

            let slice = buf.filled(15..agg_len);
            assert_eq!(slice.len(), block_size as usize - 15);
            for b in slice {
                assert_eq!(*b, 1)
            }

            let slice = buf.filled((agg_len / 2)..agg_len);
            assert_eq!(slice.len(), block_size as usize);
            for b in slice {
                assert_eq!(*b, 2)
            }
        }
    }

    #[test]
    fn agg_nom_traits() {
        use nom::{Compare, InputLength, InputTake};

        let buf = AggregateBuf::new().unwrap();
        let hello = "hello";
        let world = "world";

        {
            let mut buf = buf.write();

            {
                let dst = buf.unfilled_mut();
                let dst_len = dst.len();
                let mut src = b"#".repeat(dst_len);
                src[(dst_len - hello.len())..].copy_from_slice(hello.as_bytes());
                dst.copy_from_slice(&src);

                buf.advance(dst_len as _);
            }

            buf.grow_if_needed().unwrap();

            {
                let dst = buf.unfilled_mut();
                dst[..world.len()].copy_from_slice(world.as_bytes());
                buf.advance(world.len() as _);
            }
        }

        let block_size = buf.write().block_size;
        let start = block_size - hello.len() as u32;
        let end = start + hello.len() as u32 + world.len() as u32;
        let slice = buf.read().slice(start..end);
        assert_eq!(slice.input_len(), hello.len() + world.len());

        eprintln!("to_vec + compare owned");
        let owned = slice.to_vec();
        assert_eq!(String::from_utf8_lossy(&owned[..]), "helloworld");

        assert_eq!(slice.compare(b"that's not it"), nom::CompareResult::Error);
        assert_eq!(slice.compare(b"hello"), nom::CompareResult::Ok);
        eprintln!("helloworld nom::Compare");
        assert_eq!(slice.compare(b"helloworld"), nom::CompareResult::Ok);
        assert_eq!(
            slice.compare(b"helloworldwoops"),
            nom::CompareResult::Incomplete
        );

        {
            let hello_slice = slice.take(5);
            assert_eq!(hello_slice.compare(b"hello"), nom::CompareResult::Ok);
        }

        {
            let (hello_slice, world_slice) = slice.take_split(5);
            assert_eq!(hello_slice.compare(b"hello"), nom::CompareResult::Ok);
            assert_eq!(world_slice.compare(b"world"), nom::CompareResult::Ok);
        }

        // TODO: test `compare_no_case`
    }

    #[test]
    fn agg_nom_sample() {
        fn parse(i: AggregateSlice) -> IResult<AggregateSlice, AggregateSlice> {
            nom::bytes::streaming::tag(&b"HTTP/1.1"[..])(i)
        }

        let mut buf = AggregateBuf::new().unwrap();

        for _ in 0..300 {
            let input = "HTTP/1.1 200 OK";
            buf.write().put(input.as_bytes()).unwrap();

            let slice = buf.read().slice(0..input.len() as u32);
            let (version, _rest) = parse(slice).unwrap();
            assert_eq!(std::str::from_utf8(&version.to_vec()).unwrap(), "HTTP/1.1");

            buf = buf.split().unwrap();
        }
    }
}
