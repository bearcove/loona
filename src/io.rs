use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    net::TcpStream,
    BufResult,
};

pub trait ReadOwned {
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B>;
}

pub trait WriteOwned {
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B>;

    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>>;

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
