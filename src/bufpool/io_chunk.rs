//! Types for performing vectored I/O.

use tokio_uring::buf::IoBuf;

use super::Buf;

pub enum IoChunk {
    Static(&'static [u8]),
    Vec(Vec<u8>),
    Buf(Buf),
}

impl From<&'static [u8]> for IoChunk {
    fn from(slice: &'static [u8]) -> Self {
        IoChunk::Static(slice)
    }
}

impl From<Vec<u8>> for IoChunk {
    fn from(vec: Vec<u8>) -> Self {
        IoChunk::Vec(vec)
    }
}

impl From<Buf> for IoChunk {
    fn from(buf: Buf) -> Self {
        IoChunk::Buf(buf)
    }
}

impl IoChunk {
    #[inline(always)]
    pub fn as_io_buf(&self) -> &dyn IoBuf {
        match self {
            IoChunk::Static(slice) => slice,
            IoChunk::Vec(vec) => vec,
            IoChunk::Buf(buf) => buf,
        }
    }
}

unsafe impl IoBuf for IoChunk {
    #[inline(always)]
    fn stable_ptr(&self) -> *const u8 {
        IoBuf::stable_ptr(self.as_io_buf())
    }

    fn bytes_init(&self) -> usize {
        IoBuf::bytes_init(self.as_io_buf())
    }

    fn bytes_total(&self) -> usize {
        IoBuf::bytes_total(self.as_io_buf())
    }
}

pub trait IoChunkable {
    /// Returns the total length of all chunks
    fn len(&self) -> usize;

    /// Returns true if the chunkable is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

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
            IoChunk::Vec(vec) => vec.len(),
            IoChunk::Buf(buf) => buf.len(),
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl IoChunkable for &'static [u8] {
    fn len(&self) -> usize {
        (self as &'static [u8]).len()
    }

    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        if offset == 0 {
            Some(IoChunk::Static(self))
        } else {
            None
        }
    }
}

impl IoChunkable for &'static str {
    fn len(&self) -> usize {
        (self as &'static str).len()
    }

    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        IoChunkable::next_chunk(&self.as_bytes(), offset)
    }
}

impl IoChunkable for Vec<u8> {
    fn len(&self) -> usize {
        self.len()
    }

    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        if offset == 0 {
            // FIXME: ooh that's bad, shouldn't need to clone here
            Some(IoChunk::Vec(self.clone()))
        } else {
            None
        }
    }
}

impl IoChunkable for Buf {
    fn len(&self) -> usize {
        self.len()
    }

    fn next_chunk(&self, offset: u32) -> Option<IoChunk> {
        if offset == 0 {
            Some(IoChunk::Buf(self.clone()))
        } else {
            None
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

    /// Add a single chunk to the list
    pub fn push_chunk(&mut self, chunk: IoChunk) {
        self.chunks.push(chunk);
    }

    /// Returns total length
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.len()).sum()
    }

    pub fn num_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() || self.len() == 0
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn into_vec(self) -> Vec<IoChunk> {
        self.chunks
    }
}

impl From<Vec<IoChunk>> for IoChunkList {
    fn from(chunks: Vec<IoChunk>) -> Self {
        Self { chunks }
    }
}
