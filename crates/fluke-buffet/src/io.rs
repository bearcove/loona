use std::net::Shutdown;

use crate::{
    buf::{IoBuf, IoBufMut},
    BufResult, Piece, PieceList,
};

mod chan;
pub use chan::*;

mod non_uring;

#[allow(async_fn_in_trait)] // we never require Send
pub trait ReadOwned {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B>;
}

#[allow(async_fn_in_trait)] // we never require Send
pub trait WriteOwned {
    /// Write a single buffer, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::write_all]
    async fn write(&mut self, buf: Piece) -> BufResult<usize, Piece>;

    /// Write a single buffer, re-trying the write if the kernel does a partial write.
    async fn write_all(&mut self, mut buf: Piece) -> std::io::Result<()> {
        let mut written = 0;
        let len = buf.bytes_init();
        while written < len {
            let (res, slice) = self.write(buf).await;
            let n = res?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write zero",
                ));
            }
            (_, buf) = slice.split_at(n);
            written += n;
        }
        Ok(())
    }

    /// Write a list of buffers, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::writev_all]
    async fn writev(&mut self, list: &PieceList) -> std::io::Result<usize> {
        let mut total = 0;

        for buf in list.pieces.iter().cloned() {
            let buf_len = buf.bytes_init();
            let (res, _) = self.write(buf).await;

            match res {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "write zero",
                    ));
                }
                Ok(n) => {
                    total += n;
                    if n < buf_len {
                        // partial write, return the buffer list so the caller
                        // might choose to try the write again
                        return Ok(total);
                    }
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Ok(total)
    }

    /// Write a list of buffers, re-trying the write if the kernel does a partial write.
    async fn writev_all(&mut self, mut list: PieceList) -> std::io::Result<()> {
        while !list.is_empty() {
            let n = self.writev(&list).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write zero",
                ));
            }

            let mut n = n;
            while n > 0 {
                // pop and/or split items from the list
                let next_item = list.pieces.front_mut().unwrap();
                let next_item_len = next_item.len();

                if n < next_item_len {
                    // the number of bytes written falls in the middle of the buffer.
                    // split the buffer and push the remainder back to the list
                    let next_item = list.pieces.pop_front().unwrap();
                    let (l, _r) = next_item.split_at(n);
                    n -= l.len();
                    list.pieces.push_front(l);
                } else {
                    // the whole buffer was written, pop it from the list
                    list.pieces.pop_front();
                    n -= next_item_len;
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self, how: Shutdown) -> std::io::Result<()>;
}

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::{buf::IoBuf, io::WriteOwned, BufResult, Piece, PieceList};

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
            async fn write(&mut self, buf: Piece) -> BufResult<usize, Piece> {
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

            async fn shutdown(&mut self, _how: std::net::Shutdown) -> std::io::Result<()> {
                Ok(())
            }
        }

        crate::start(async move {
            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let res = writer.write_all(buf_a.into()).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            let res = writer
                .writev_all(PieceList::single(buf_a).followed_by(buf_b))
                .await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            writer.write_all(buf_a.into()).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5]);

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            writer
                .writev_all(PieceList::single(buf_a).followed_by(buf_b))
                .await
                .unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        });
    }
}

pub trait IntoHalves {
    type Read: ReadOwned;
    type Write: WriteOwned;

    /// Split this into an owned read half and an owned write half.
    fn into_halves(self) -> (Self::Read, Self::Write);
}
