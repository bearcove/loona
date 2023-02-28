use std::rc::Rc;

use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    net::TcpStream,
    BufResult,
};

mod buf_or_slice;
use buf_or_slice::*;

mod chan;
pub use chan::*;

pub trait ReadOwned {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B>;
}

pub trait WriteOwned {
    /// Write a single buffer, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::write_all]
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B>;

    /// Write a single buffer, re-trying the write if the kernel does a partial write.
    async fn write_all<B: IoBuf>(&mut self, mut buf: B) -> std::io::Result<()> {
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
    async fn writev<B: IoBuf>(&mut self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
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
    async fn writev_all<B: IoBuf>(&mut self, list: impl Into<Vec<B>>) -> std::io::Result<()> {
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

pub trait SplitOwned {
    type Read: ReadOwned;
    type Write: WriteOwned;

    // TODO: rename to `into_split_owned`? it consumes `self`
    fn split_owned(self) -> (Self::Read, Self::Write);
}

pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        self.0.read(buf).await
    }
}

pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        self.0.write(buf).await
    }

    async fn writev<B: IoBuf>(&mut self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        self.0.writev(list).await
    }
}

impl SplitOwned for TcpStream {
    type Read = TcpReadHalf;
    type Write = TcpWriteHalf;

    fn split_owned(self) -> (Self::Read, Self::Write) {
        let self_rc = Rc::new(self);
        (TcpReadHalf(self_rc.clone()), TcpWriteHalf(self_rc))
    }
}

#[cfg(feature = "non-uring")]
impl SplitOwned for tokio::net::TcpStream {
    type Read = tokio::net::tcp::OwnedReadHalf;
    type Write = tokio::net::tcp::OwnedWriteHalf;

    fn split_owned(self) -> (Self::Read, Self::Write) {
        self.into_split()
    }
}

#[cfg(feature = "non-uring")]
impl ReadOwned for tokio::net::tcp::OwnedReadHalf {
    async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        let buf_slice =
            unsafe { std::slice::from_raw_parts_mut(buf.stable_mut_ptr(), buf.bytes_total()) };
        let res = tokio::io::AsyncReadExt::read(self, buf_slice).await;
        if let Ok(n) = &res {
            unsafe {
                buf.set_init(*n);
            }
        }
        (res, buf)
    }
}

#[cfg(feature = "non-uring")]
impl WriteOwned for tokio::net::tcp::OwnedWriteHalf {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        let buf_slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), buf.bytes_init()) };
        let res = tokio::io::AsyncWriteExt::write(self, buf_slice).await;
        (res, buf)
    }

    // TODO: implement writev, etc.
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
                &mut self,
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
            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let res = writer.write_all(buf_a).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            let res = writer.writev_all(vec![buf_a, buf_b]).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            writer.write_all(buf_a).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5]);

            let mut writer = Writer {
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
