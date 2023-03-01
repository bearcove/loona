use crate::buf::IoBuf;

pub(crate) enum BufOrSlice<B: IoBuf> {
    Buf(B),
    Slice(crate::buf::Slice<B>),
}

unsafe impl<B: IoBuf> IoBuf for BufOrSlice<B> {
    fn stable_ptr(&self) -> *const u8 {
        match self {
            BufOrSlice::Buf(b) => b.stable_ptr(),
            BufOrSlice::Slice(s) => s.stable_ptr(),
        }
    }

    fn bytes_init(&self) -> usize {
        match self {
            BufOrSlice::Buf(b) => b.bytes_init(),
            BufOrSlice::Slice(s) => s.bytes_init(),
        }
    }

    fn bytes_total(&self) -> usize {
        match self {
            BufOrSlice::Buf(b) => b.bytes_total(),
            BufOrSlice::Slice(s) => s.bytes_total(),
        }
    }
}

impl<B: IoBuf> BufOrSlice<B> {
    pub(crate) fn len(&self) -> usize {
        match self {
            BufOrSlice::Buf(b) => b.bytes_init(),
            BufOrSlice::Slice(s) => s.len(),
        }
    }

    /// Consume the first `n` bytes of the buffer (assuming they've been written).
    /// This turns a `BufOrSlice::Buf` into a `BufOrSlice::Slice`
    pub(crate) fn consume(self, n: usize) -> Self {
        assert!(n <= self.len());

        match self {
            BufOrSlice::Buf(b) => BufOrSlice::Slice(b.slice(n..)),
            BufOrSlice::Slice(s) => {
                let n = s.begin() + n;
                BufOrSlice::Slice(s.into_inner().slice(n..))
            }
        }
    }
}
