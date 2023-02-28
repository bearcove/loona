use tokio::io::{AsyncRead, AsyncWrite};
use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    BufResult,
};

use crate::{ReadOwned, WriteOwned};

impl<T> ReadOwned for T
where
    T: AsyncRead + Unpin,
{
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

impl<T> WriteOwned for T
where
    T: AsyncWrite + Unpin,
{
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        let buf_slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), buf.bytes_init()) };
        let res = tokio::io::AsyncWriteExt::write(self, buf_slice).await;
        (res, buf)
    }

    // TODO: implement writev, for performance. this involves wrapping
    // everything in `IoSlice`, advancing correctly, etc. It's not fun, but it
    // should yield a boost for non-uring codepaths.
}
