//! Types for performing vectored I/O.

use tokio_uring::buf::IoBuf;

use crate::Roll;

pub enum IoChunk {
    Static(&'static [u8]),
    Vec(Vec<u8>),
    Roll(Roll),
}

impl From<&'static [u8]> for IoChunk {
    fn from(slice: &'static [u8]) -> Self {
        IoChunk::Static(slice)
    }
}

impl From<&'static str> for IoChunk {
    fn from(slice: &'static str) -> Self {
        IoChunk::Static(slice.as_bytes())
    }
}

impl From<Vec<u8>> for IoChunk {
    fn from(vec: Vec<u8>) -> Self {
        IoChunk::Vec(vec)
    }
}

impl From<Roll> for IoChunk {
    fn from(roll: Roll) -> Self {
        IoChunk::Roll(roll)
    }
}

impl AsRef<[u8]> for IoChunk {
    fn as_ref(&self) -> &[u8] {
        match self {
            IoChunk::Static(slice) => slice,
            IoChunk::Vec(vec) => vec.as_ref(),
            IoChunk::Roll(roll) => roll.as_ref(),
        }
    }
}

impl IoChunk {
    #[inline(always)]
    pub fn as_io_buf(&self) -> &dyn IoBuf {
        match self {
            IoChunk::Static(slice) => slice,
            IoChunk::Vec(vec) => vec,
            IoChunk::Roll(roll) => roll,
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

impl IoChunk {
    #[inline(always)]
    pub fn len(&self) -> usize {
        match self {
            IoChunk::Static(slice) => slice.len(),
            IoChunk::Vec(vec) => vec.len(),
            IoChunk::Roll(roll) => roll.len(),
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
pub struct IoChunkList {
    chunks: Vec<IoChunk>,
}

impl IoChunkList {
    /// Add a single chunk to the list
    pub fn push(&mut self, chunk: impl Into<IoChunk>) {
        self.chunks.push(chunk.into());
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
