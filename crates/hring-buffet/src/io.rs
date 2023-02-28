use std::{cell::RefCell, rc::Rc};

use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    net::TcpStream,
    BufResult,
};
use tracing::trace;

mod chan;
pub use chan::*;

pub trait ReadOwned {
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B>;
}

impl<T> ReadOwned for Rc<T>
where
    T: ReadOwned,
{
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B> {
        self.as_ref().read(buf).await
    }
}

pub trait WriteOwned {
    /// Write a single buffer, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::write_all]
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B>;

    /// Write a single buffer, re-trying the write if the kernel does a partial write.
    async fn write_all<B: IoBuf>(&self, mut buf: B) -> std::io::Result<()> {
        let mut written = 0;
        let len = buf.bytes_init();
        while written < len {
            let (res, slice) = self.write(buf.slice(written..len)).await;
            buf = slice.into_inner();
            let n = res?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write zero",
                ));
            }
            written += n;
        }
        Ok(())
    }

    /// Write a list of buffers, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::writev_all]
    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        let mut out_list = Vec::with_capacity(list.len());
        let mut list = list.into_iter();
        let mut total = 0;

        while let Some(buf) = list.next() {
            let buf_len = buf.bytes_init();
            let (res, buf) = self.write(buf).await;
            out_list.push(buf);

            match res {
                Ok(0) => {
                    out_list.extend(list);
                    return (
                        Err(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "write zero",
                        )),
                        out_list,
                    );
                }
                Ok(n) => {
                    total += n;
                    if n < buf_len {
                        // partial write, return the buffer list so the caller
                        // might choose to try the write again
                        out_list.extend(list);
                        return (Ok(total), out_list);
                    }
                }
                Err(e) => {
                    out_list.extend(list);
                    return (Err(e), out_list);
                }
            }
        }

        (Ok(total), out_list)
    }

    /// Write a list of buffers, re-trying the write if the kernel does a partial write.
    async fn writev_all<B: IoBuf>(&self, list: impl Into<Vec<B>>) -> std::io::Result<()> {
        // FIXME: converting into a `Vec` and _then_ into an iterator is silly,
        // we can probably find a better function signature here.
        let mut list: Vec<_> = list.into().into_iter().map(BufOrSlice::Buf).collect();

        while !list.is_empty() {
            let res;
            (res, list) = self.writev(list).await;
            let n = res?;

            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write zero",
                ));
            }

            let mut n = n;
            list = list
                .into_iter()
                .filter_map(|item| {
                    if n == 0 {
                        Some(item)
                    } else {
                        let item_len = item.len();

                        if n >= item_len {
                            n -= item_len;
                            None
                        } else {
                            let item = item.consume(n);
                            n = 0;
                            Some(item)
                        }
                    }
                })
                .collect();
            assert_eq!(n, 0);
        }

        Ok(())
    }
}

impl<T> WriteOwned for Rc<T>
where
    T: WriteOwned,
{
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        self.as_ref().write(buf).await
    }

    async fn write_all<B: IoBuf>(&self, buf: B) -> std::io::Result<()> {
        self.as_ref().write_all(buf).await
    }

    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        self.as_ref().writev(list).await
    }

    async fn writev_all<B: IoBuf>(&self, list: impl Into<Vec<B>>) -> std::io::Result<()> {
        self.as_ref().writev_all(list).await
    }
}

enum BufOrSlice<B: IoBuf> {
    Buf(B),
    Slice(tokio_uring::buf::Slice<B>),
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
    fn len(&self) -> usize {
        match self {
            BufOrSlice::Buf(b) => b.bytes_init(),
            BufOrSlice::Slice(s) => s.len(),
        }
    }

    /// Consume the first `n` bytes of the buffer (assuming they've been written).
    /// This turns a `BufOrSlice::Buf` into a `BufOrSlice::Slice`
    fn consume(self, n: usize) -> Self {
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

pub trait ReadWriteOwned: ReadOwned + WriteOwned {}
impl<T> ReadWriteOwned for T where T: ReadOwned + WriteOwned {}

impl ReadOwned for TcpStream {
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B> {
        TcpStream::read(self, buf).await
    }
}

impl WriteOwned for TcpStream {
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        TcpStream::write(self, buf).await
    }

    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        TcpStream::writev(self, list).await
    }
}

pub trait SplitOwned {
    type Read: ReadOwned;
    type Write: WriteOwned;

    fn split_owned(self) -> (Self::Read, Self::Write);
}

impl SplitOwned for TcpStream {
    type Read = Rc<TcpStream>;
    type Write = Rc<TcpStream>;

    fn split_owned(self) -> (Self::Read, Self::Write) {
        let self_rc = Rc::new(self);
        (self_rc.clone(), self_rc)
    }
}

#[cfg(feature = "non-uring")]
impl SplitOwned for tokio::net::TcpStream {
    // we have to use a RefCell here since `ReadOwned` only takes `&self`
    type Read = RefCell<tokio::net::tcp::OwnedReadHalf>;
    type Write = RefCell<tokio::net::tcp::OwnedWriteHalf>;

    fn split_owned(self) -> (Self::Read, Self::Write) {
        let (r, w) = self.into_split();
        (r.into(), w.into())
    }
}

#[cfg(feature = "non-uring")]
impl ReadOwned for RefCell<tokio::net::tcp::OwnedReadHalf> {
    // we actually do want to panic if we try borrowing this mutably twice.
    // this is a wonky interface to start with, only to compare uring and
    // non-uring. the safer choice is to use an async mutex, but that might
    // impact performance.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn read<B: IoBufMut>(&self, mut buf: B) -> BufResult<usize, B> {
        let mut this = self.borrow_mut();
        let buf_slice =
            unsafe { std::slice::from_raw_parts_mut(buf.stable_mut_ptr(), buf.bytes_total()) };
        let res = tokio::io::AsyncReadExt::read(&mut *this, buf_slice).await;
        if let Ok(n) = &res {
            unsafe {
                buf.set_init(*n);
            }
        }
        (res, buf)
    }
}

#[cfg(feature = "non-uring")]
impl WriteOwned for RefCell<tokio::net::tcp::OwnedWriteHalf> {
    // see the `impl ReadOwned` above
    #[allow(clippy::await_holding_refcell_ref)]
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        let mut this = self.borrow_mut();
        let buf_slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), buf.bytes_init()) };
        let res = tokio::io::AsyncWriteExt::write(&mut *this, buf_slice).await;
        (res, buf)
    }

    // TODO: implement writev, etc.
}

/// Unites a [ReadOwned] and a [WriteOwned] into a single [ReadWriteOwned] type.
pub struct ReadWritePair<R, W>(pub R, pub W)
where
    R: ReadOwned,
    W: WriteOwned;

impl<R, W> ReadOwned for ReadWritePair<R, W>
where
    R: ReadOwned,
    W: WriteOwned,
{
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B> {
        trace!("pair, reading {} bytes", buf.bytes_total());
        self.0.read(buf).await
    }
}

impl<R, W> WriteOwned for ReadWritePair<R, W>
where
    R: ReadOwned,
    W: WriteOwned,
{
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        self.1.write(buf).await
    }
}

impl<R, W> SplitOwned for ReadWritePair<R, W>
where
    R: ReadOwned,
    W: WriteOwned,
{
    type Read = R;
    type Write = W;

    fn split_owned(self) -> (Self::Read, Self::Write) {
        (self.0, self.1)
    }
}

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::WriteOwned;

    #[test]
    fn test_write_all() {
        enum Mode {
            WriteZero,
            WritePartial,
        }

        struct Writer {
            mode: Mode,
            bytes: Rc<RefCell<Vec<u8>>>,
        }

        impl WriteOwned for Writer {
            async fn write<B: tokio_uring::buf::IoBuf>(
                &self,
                buf: B,
            ) -> tokio_uring::BufResult<usize, B> {
                assert!(buf.bytes_init() > 0, "zero-length writes are forbidden");

                match self.mode {
                    Mode::WriteZero => (Ok(0), buf),
                    Mode::WritePartial => {
                        let n = match buf.bytes_init() {
                            1 => 1,
                            _ => buf.bytes_init() / 2,
                        };
                        let slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), n) };
                        self.bytes.borrow_mut().extend_from_slice(slice);
                        (Ok(n), buf)
                    }
                }
            }
        }

        tokio_uring::start(async move {
            let writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let res = writer.write_all(buf_a).await;
            assert!(res.is_err());

            let writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            let res = writer.writev_all(vec![buf_a, buf_b]).await;
            assert!(res.is_err());

            let writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            writer.write_all(buf_a).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5]);

            let writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            writer.writev_all(vec![buf_a, buf_b]).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        });
    }
}
