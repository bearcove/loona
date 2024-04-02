use std::{
    net::{Shutdown, SocketAddr},
    os::fd::{AsFd, AsRawFd},
    rc::Rc,
};

use fluke_io_uring_async::IoUringAsync;
use io_uring::opcode::Accept;

use crate::{
    buf::IoBufMut,
    io::{IntoHalves, ReadOwned, WriteOwned},
    BufResult, Piece, PieceList,
};

pub struct TcpStream {
    socket: socket2::Socket,
}

pub struct TcpListener {
    socket: socket2::Socket,
}

// have `uring`, of type `SendWrapper<Rc<IoUringAsync>>`, as a thread-local variable
thread_local! {
    static uring: Rc<IoUringAsync> = {{
        let u = Rc::new(IoUringAsync::new(8).unwrap());
        tokio::task::spawn_local(IoUringAsync::listen(u.clone()));
        u
    }};
}

fn get_uring() -> Rc<IoUringAsync> {
    let mut u = None;
    uring.with(|u_| u = Some(u_.clone()));
    u.unwrap()
}

impl TcpListener {
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let addr: socket2::SockAddr = addr.into();
        let socket = socket2::Socket::new(addr.domain(), socket2::Type::STREAM, None)?;
        socket.bind(&addr.into())?;

        Ok(Self { socket })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        todo!()
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        let fd = self.socket.as_fd();

        let u = get_uring();
        struct AcceptUserData {
            sockaddr_storage: libc::sockaddr,
            sockaddr_len: libc::socklen_t,
        }
        let mut udata = Box::into_raw(Box::new(AcceptUserData {
            sockaddr_storage: unsafe { std::mem::zeroed() },
            sockaddr_len: 0,
        }));

        let e = unsafe {
            Accept::new(
                io_uring::types::Fd(fd.as_raw_fd()),
                &mut (*udata).sockaddr_storage,
                &mut (*udata).sockaddr_len,
            )
            .build()
        };
        let res = u.push(e).await;
        todo!()
    }
}

pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        todo!()
    }
}

pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write(&mut self, buf: Piece) -> BufResult<usize, Piece> {
        todo!()
    }

    async fn writev(&mut self, list: &PieceList) -> std::io::Result<usize> {
        todo!()
    }

    async fn shutdown(&mut self, how: Shutdown) -> std::io::Result<()> {
        todo!()
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
