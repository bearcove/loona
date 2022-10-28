//! Types for performing vectored I/O.

use std::ops::Deref;

use tokio_uring::buf::IoBuf;

use crate::Roll;

/// A piece of data (arbitrary bytes) with a stable address, suitable for
/// passing to the kernel (io_uring writes).
#[derive(Clone)]
pub enum Piece {
    Static(&'static [u8]),
    Vec(Vec<u8>),
    Roll(Roll),
}

impl From<&'static [u8]> for Piece {
    fn from(slice: &'static [u8]) -> Self {
        Piece::Static(slice)
    }
}

impl From<&'static str> for Piece {
    fn from(slice: &'static str) -> Self {
        Piece::Static(slice.as_bytes())
    }
}

impl From<Vec<u8>> for Piece {
    fn from(vec: Vec<u8>) -> Self {
        Piece::Vec(vec)
    }
}

impl From<Roll> for Piece {
    fn from(roll: Roll) -> Self {
        Piece::Roll(roll)
    }
}

impl Deref for Piece {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl AsRef<[u8]> for Piece {
    fn as_ref(&self) -> &[u8] {
        match self {
            Piece::Static(slice) => slice,
            Piece::Vec(vec) => vec.as_ref(),
            Piece::Roll(roll) => roll.as_ref(),
        }
    }
}

impl Piece {
    #[inline(always)]
    pub fn as_io_buf(&self) -> &dyn IoBuf {
        match self {
            Piece::Static(slice) => slice,
            Piece::Vec(vec) => vec,
            Piece::Roll(roll) => roll,
        }
    }
}

unsafe impl IoBuf for Piece {
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

impl Piece {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.as_ref().len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A list of [Piece], suitable for issuing vectored writes via io_uring.
#[derive(Default)]
pub struct PieceList {
    pieces: Vec<Piece>,
}

impl PieceList {
    /// Add a single chunk to the list
    pub fn push(&mut self, chunk: impl Into<Piece>) {
        self.pieces.push(chunk.into());
    }

    /// Returns total length
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|c| c.len()).sum()
    }

    pub fn num_pieces(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty() || self.len() == 0
    }

    pub fn clear(&mut self) {
        self.pieces.clear();
    }

    pub fn into_vec(self) -> Vec<Piece> {
        self.pieces
    }
}

impl From<Vec<Piece>> for PieceList {
    fn from(chunks: Vec<Piece>) -> Self {
        Self { pieces: chunks }
    }
}
