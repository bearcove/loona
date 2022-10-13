use std::{
    cell::{RefCell, RefMut},
    ops::Range,
    rc::Rc,
};

use smallvec::SmallVec;

use crate::bufpool::{self, BufMut};

/// An "aggregate buffer", uses one or more [BufMut]s for storage. Allows
/// writing to uninitialized data, and borrowing ref-counted [AggregateSlice] of
/// initialized data.
#[derive(Default, Clone)]
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
        // dbg!(start, end, self.block_size, block_index);
        debug_assert!(block_index < self.blocks.len());

        let block_offset = start % self.block_size;
        let avail = self.block_size - block_offset;
        let given = std::cmp::min(avail, wanted);

        let block_offset = block_offset as usize;
        let given = given as usize;

        (block_index, block_offset..block_offset + given)
    }

    /// If `len == capacity` (ie. the `unfilled_mut` slice would be empty), try
    /// to add a block to this aggregate buffer. This is fallible, as we might
    /// be out of memory.
    pub fn grow_if_needed(&mut self) -> Result<(), bufpool::Error> {
        let block = BufMut::alloc()?;
        self.blocks.push(block);
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
            pool: AggregateBuf {
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
}

/// A slice of an [AggregateBuf]. This is a read-only view, it's clonable,
/// it holds a reference to the underlying [AggregateBuf], so holding it
/// will keep the _whole_ [AggregateBuf] alive.
#[derive(Clone)]
pub struct AggregateSlice {
    pool: AggregateBuf,
    off: u32,
    len: u32,
}

impl nom::InputLength for AggregateSlice {
    fn input_len(&self) -> usize {
        self.len as _
    }
}

impl nom::InputTake for AggregateSlice {
    fn take(&self, count: usize) -> Self {
        let count: u32 = count.try_into().unwrap();
        if count > self.len {
            panic!("take: count > self.len");
        }

        Self {
            pool: self.pool.clone(),
            off: self.off,
            len: count,
        }
    }

    fn take_split(&self, count: usize) -> (Self, Self) {
        let count: u32 = count.try_into().unwrap();
        if count > self.len {
            panic!("take_split: count > self.len");
        }

        (
            Self {
                pool: self.pool.clone(),
                off: self.off,
                len: count,
            },
            Self {
                pool: self.pool.clone(),
                off: self.off + count,
                len: self.len - count,
            },
        )
    }
}

impl nom::Compare<&[u8]> for AggregateSlice {
    fn compare(&self, _t: &[u8]) -> nom::CompareResult {
        todo!()
    }

    fn compare_no_case(&self, _t: &[u8]) -> nom::CompareResult {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::{AggregateBuf, AggregateBufInner};

    #[test]
    fn agg_inner_size() {
        assert_eq!(std::mem::size_of::<AggregateBufInner>(), 64);
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
            let pool = buf.read();

            let slice = pool.filled(0..agg_len);
            assert_eq!(slice.len(), block_size as usize);
            for b in slice {
                assert_eq!(*b, 1)
            }

            let slice = pool.filled(15..agg_len);
            assert_eq!(slice.len(), block_size as usize - 15);
            for b in slice {
                assert_eq!(*b, 1)
            }

            let slice = pool.filled((agg_len / 2)..agg_len);
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
                let mut src = b"#".repeat(dst.len());
                src[..hello.len()].copy_from_slice(hello.as_bytes());
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

        assert_eq!(slice.compare(b"that's not it"), nom::CompareResult::Error);
        assert_eq!(slice.compare(b"hello"), nom::CompareResult::Ok);
        assert_eq!(slice.compare(b"hello world"), nom::CompareResult::Ok);
        assert_eq!(
            slice.compare(b"hello world woops"),
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
}
