use crate::{
    buf::{IoBuf, IoBufMut},
    BufResult,
};

mod buf_or_slice;
use buf_or_slice::*;

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
