use std::net::SocketAddr;
use tokio::net::{TcpListener as TokListener, TcpStream as TokStream};

pub type TcpStream = TokStream;

pub type TcpReadHalf = tokio::net::tcp::OwnedReadHalf;
pub type TcpWriteHalf = tokio::net::tcp::OwnedWriteHalf;

pub struct TcpListener {
    tok: TokListener,
}

impl TcpListener {
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let tok = TokListener::bind(addr).await?;
        Ok(Self { tok })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.tok.local_addr()
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        self.tok.accept().await
    }
}

impl IntoSplit for TcpStream {
    type Read = TcpReadHalf;
    type Write = TcpWriteHalf;

    fn into_split(self) -> (Self::Read, Self::Write) {
        TcpStream::into_split(self)
    }
}
