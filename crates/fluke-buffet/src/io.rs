use crate::{BufResult, IoBufMut, Piece, PieceList};

mod pipe;
pub use pipe::*;

mod non_uring;

#[allow(async_fn_in_trait)] // we never require Send
pub trait ReadOwned {
    async fn read_owned<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B>;
}

#[allow(async_fn_in_trait)] // we never require Send
pub trait WriteOwned {
    /// Write a single buffer, taking ownership for the duration of the write.
    /// Might perform a partial write, see [WriteOwned::write_all]
    async fn write_owned(&mut self, buf: impl Into<Piece>) -> BufResult<usize, Piece>;

    /// Write a single buffer, re-trying the write if the kernel does a partial write.
    async fn write_all_owned(&mut self, buf: impl Into<Piece>) -> std::io::Result<()> {
        let mut buf = buf.into();
        let mut written = 0;
        let len = buf.len();
        while written < len {
            let (res, slice) = self.write_owned(buf).await;
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
    async fn writev_owned(&mut self, list: &PieceList) -> std::io::Result<usize> {
        let mut total = 0;

        for buf in list.pieces.iter().cloned() {
            let buf_len = buf.len();
            let (res, _) = self.write_owned(buf).await;

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
    async fn writev_all_owned(&mut self, mut list: PieceList) -> std::io::Result<()> {
        while !list.is_empty() {
            let n = self.writev_owned(&list).await?;
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
                    let (l, r) = next_item.split_at(n);
                    n -= l.len();
                    list.pieces.push_front(r);
                } else {
                    // the whole buffer was written, pop it from the list
                    list.pieces.pop_front();
                    n -= next_item_len;
                }
            }
        }

        Ok(())
    }

    /// Shuts down the write end of this socket. This flushes
    /// any data that may not have been send.
    async fn shutdown(&mut self) -> std::io::Result<()>;
}

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::{io::WriteOwned, BufResult, Piece, PieceList};

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
            async fn write_owned(&mut self, buf: impl Into<Piece>) -> BufResult<usize, Piece> {
                let buf = buf.into();
                assert!(!buf.is_empty(), "zero-length writes are forbidden");

                match self.mode {
                    Mode::WriteZero => (Ok(0), buf),
                    Mode::WritePartial => {
                        let n = match buf.len() {
                            1 => 1,
                            _ => buf.len() / 2,
                        };
                        self.bytes.borrow_mut().extend_from_slice(&buf[..n]);
                        (Ok(n), buf)
                    }
                }
            }

            async fn shutdown(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        crate::start(async move {
            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let res = writer.write_all_owned(buf_a).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            let res = writer
                .writev_all_owned(PieceList::single(buf_a).followed_by(buf_b))
                .await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            writer.write_all_owned(buf_a).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5]);

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            writer
                .writev_all_owned(PieceList::single(buf_a).followed_by(buf_b))
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
