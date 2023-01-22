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

pub trait WriteOwned {
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B>;

    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        let mut out_list = Vec::with_capacity(list.len());
        let mut list = list.into_iter();
        let mut total = 0;

        while let Some(buf) = list.next() {
            let (res, buf) = self.write(buf).await;
            out_list.push(buf);
            match res {
                Ok(n) => total += n,
                Err(e) => {
                    out_list.extend(list);
                    return (Err(e), out_list);
                }
            }
        }

        (Ok(total), out_list)
    }

    async fn write_all<B: IoBuf>(&self, mut buf: B) -> BufResult<(), B> {
        let mut written = 0;
        let len = buf.bytes_init();
        while written < len {
            let (res, slice) = self.write(buf.slice(written..len)).await;
            buf = slice.into_inner();
            let n = match res {
                Ok(n) => n,
                Err(e) => return (Err(e), buf),
            };
            written += n;
        }
        (Ok(()), buf)
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
