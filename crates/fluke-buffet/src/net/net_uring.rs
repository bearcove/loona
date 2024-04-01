use std::{
    net::{Shutdown, SocketAddr},
    rc::Rc,
};

use crate::{
    buf::{tokio_uring_compat::BufCompat, IoBufMut},
    io::{IntoHalves, ReadOwned, WriteOwned},
    BufResult, Piece, PieceList,
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
        self.tok.accept().await.map(|tuple| {
            tuple.0.set_nodelay(true).unwrap();
            tuple
        })
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
    async fn write(&mut self, buf: Piece) -> BufResult<usize, Piece> {
        let (res, b) = self.0.write(BufCompat(buf)).submit().await;
        (res, b.0)
    }

    async fn writev(&mut self, list: &PieceList) -> std::io::Result<usize> {
        let list: Vec<BufCompat<Piece>> = list.pieces.iter().cloned().map(BufCompat).collect();
        let (res, _list) = self.0.writev(list).await;
        res
    }

    async fn shutdown(&mut self, how: Shutdown) -> std::io::Result<()> {
        self.0.shutdown(how)
    }
}

impl IntoHalves for TcpStream {
    type Read = TcpReadHalf;
    type Write = TcpWriteHalf;

    fn into_halves(self) -> (Self::Read, Self::Write) {
        let self_rc = Rc::new(self);
        (TcpReadHalf(self_rc.clone()), TcpWriteHalf(self_rc))
    }
}
