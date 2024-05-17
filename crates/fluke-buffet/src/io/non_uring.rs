use crate::{BufResult, IoBufMut, Piece, ReadOwned, WriteOwned};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

impl<T> ReadOwned for T
where
    T: AsyncRead + Unpin,
{
    async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        let buf_slice = unsafe { buf.slice_mut() };
        let res = tokio::io::AsyncReadExt::read(self, buf_slice).await;
        (res, buf)
    }
}

impl<T> WriteOwned for T
where
    T: AsyncWrite + Unpin,
{
    async fn write(&mut self, buf: Piece) -> BufResult<usize, Piece> {
        let res = tokio::io::AsyncWriteExt::write(self, &buf[..]).await;
        (res, buf)
    }

    // TODO: implement writev, for performance. this involves wrapping
    // everything in `IoSlice`, advancing correctly, etc. It's not fun, but it
    // should yield a boost for non-uring codepaths.

    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}
