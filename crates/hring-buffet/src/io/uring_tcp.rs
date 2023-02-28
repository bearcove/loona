use std::rc::Rc;

use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    net::TcpStream,
    BufResult,
};

use crate::{ReadOwned, WriteOwned};

pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        self.0.read(buf).await
    }
}

pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        self.0.write(buf).await
    }

    async fn writev<B: IoBuf>(&mut self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        self.0.writev(list).await
    }
}

pub trait IntoSplit {
    type Read: ReadOwned;
    type Write: WriteOwned;

    fn into_split(self) -> (Self::Read, Self::Write);
}

impl IntoSplit for TcpStream {
    type Read = TcpReadHalf;
    type Write = TcpWriteHalf;

    fn into_split(self) -> (Self::Read, Self::Write) {
        let self_rc = Rc::new(self);
        (TcpReadHalf(self_rc.clone()), TcpWriteHalf(self_rc))
    }
}
