//! Types for performing vectored I/O.

use tokio_uring::buf::IoBuf;

use super::Buf;

pub enum IoChunk {
    Static(&'static [u8]),
    Slice(Buf),
}

impl From<&'static [u8]> for IoChunk {
    fn from(slice: &'static [u8]) -> Self {
        IoChunk::Static(slice)
    }
}

impl From<Buf> for IoChunk {
    fn from(buf: Buf) -> Self {
        IoChunk::Slice(buf)
    }
}

unsafe impl IoBuf for IoChunk {
    fn stable_ptr(&self) -> *const u8 {
        match self {
            IoChunk::Static(s) => IoBuf::stable_ptr(s),
            IoChunk::Slice(s) => IoBuf::stable_ptr(s),
        }
    }

    fn bytes_init(&self) -> usize {
        match self {
            IoChunk::Static(s) => IoBuf::bytes_init(s),
            IoChunk::Slice(s) => IoBuf::bytes_init(s),
        }
    }

    fn bytes_total(&self) -> usize {
        match self {
            IoChunk::Static(s) => IoBuf::bytes_total(s),
            IoChunk::Slice(s) => IoBuf::bytes_total(s),
        }
    }
}

pub trait IoChunkable {
    /// Returns next chunk that can be written
    fn next_chunk(&self, offset: u32) -> Option<IoChunk>;

    /// Append this chunkable to the given list
    fn append_to(&self, list: &mut Vec<IoChunk>) {
        let mut offset = 0;
        while let Some(chunk) = self.next_chunk(offset) {
            offset += chunk.len() as u32;
            list.push(chunk);
        }
    }
}

impl IoChunk {
    #[inline(always)]
    pub fn len(&self) -> usize {
        match self {
            IoChunk::Static(slice) => slice.len(),
            IoChunk::Slice(buf) => buf.len(),
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> IoChunkable for T
where
    T: AsRef<&'static [u8]>,
{
    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        let this = self.as_ref();
        assert_eq!(offset, 0);
        if this.is_empty() {
            None
        } else {
            Some(IoChunk::Static(this))
        }
    }
}

#[derive(Default)]
pub struct IoChunkList {
    chunks: Vec<IoChunk>,
}

impl IoChunkList {
    /// Add a chunkable to the list (this may result in multiple chunks)
    pub fn push(&mut self, chunkable: impl IoChunkable) {
        chunkable.append_to(&mut self.chunks);
    }

    /// Returns total length
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() || self.len() == 0
    }

    pub fn into_vec(self) -> Vec<IoChunk> {
        self.chunks
    }
}
