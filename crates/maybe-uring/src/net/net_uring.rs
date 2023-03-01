use std::{net::SocketAddr, rc::Rc};

use crate::{
    buf::{tokio_uring_compat::BufCompat, IoBuf, IoBufMut},
    io::{IntoSplit, ReadOwned, WriteOwned},
    BufResult,
};
pub use tokio_uring::net::{TcpListener as TokListener, TcpStream as TokStream};

pub type TcpStream = TokStream;

pub struct TcpListener {
    tok: TokListener,
}

impl TcpListener {
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let tok = TokListener::bind(addr)?;
        Ok(Self { tok })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.tok.local_addr()
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        self.tok.accept().await
    }
}

pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        let (res, b) = self.0.read(BufCompat(buf)).await;
        (res, b.0)
    }
}

pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        let (res, b) = self.0.write(BufCompat(buf)).await;
        (res, b.0)
    }

    async fn writev<B: IoBuf>(&mut self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        // TODO: use https://docs.rs/bytemuck/latest/bytemuck/trait.TransparentWrapper.html instead

        // Safety: `BufCompat` is `#[repr(transparent)]` but this still might
        // blow up because I'm not sure the layout of `Vec<T>` and
        // `Vec<BufCompat<T>>` are guaranteed to be the same.
        let list: Vec<BufCompat<B>> = unsafe { std::mem::transmute(list) };
        let (res, list) = self.0.writev(list).await;

        // Safety: see above
        let list: Vec<B> = unsafe { std::mem::transmute(list) };
        (res, list)
    }
}

impl IntoSplit for TcpStream {
    type Read = TcpReadHalf;
    type Write = TcpWriteHalf;

    fn into_split(self) -> (Self::Read, Self::Write) {
        let self_rc = Rc::new(self);
        (TcpReadHalf(self_rc.clone()), TcpWriteHalf(self_rc))
    }
}
