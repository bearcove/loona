use std::{
    net::{Shutdown, SocketAddr},
    os::fd::{AsFd, AsRawFd, FromRawFd, RawFd},
    rc::Rc,
};

use fluke_io_uring_async::IoUringAsync;
use io_uring::opcode::Accept;
use nix::errno::Errno;

use crate::{
    buf::IoBufMut,
    io::{IntoHalves, ReadOwned, WriteOwned},
    BufResult, Piece, PieceList,
};

pub struct TcpStream {
    socket: socket2::Socket,
}

impl TcpStream {
    pub async fn connect(_addr: SocketAddr) -> std::io::Result<Self> {
        todo!()
    }
}

pub struct TcpListener {
    socket: socket2::Socket,
}

// have `uring`, of type `SendWrapper<Rc<IoUringAsync>>`, as a thread-local variable
thread_local! {
    static URING: Rc<IoUringAsync> = {
        // FIXME: magic values
        Rc::new(IoUringAsync::new(8).unwrap())
    };
}

pub fn get_uring() -> Rc<IoUringAsync> {
    let mut u = None;
    URING.with(|u_| u = Some(u_.clone()));
    u.unwrap()
}

impl TcpListener {
    // note: this is only async to match tokio's API
    // TODO: investigate why tokio's TcpListener::bind is async
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let addr: socket2::SockAddr = addr.into();
        let socket = socket2::Socket::new(addr.domain(), socket2::Type::STREAM, None)?;
        socket.bind(&addr)?;
        // FIXME: magic values
        socket.listen(16)?;

        Ok(Self { socket })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.socket.local_addr()?.as_socket().unwrap())
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        let fd = self.socket.as_fd();

        let u = get_uring();
        struct AcceptUserData {
            sockaddr_storage: libc::sockaddr_storage,
            sockaddr_len: libc::socklen_t,
        }
        let udata = Box::into_raw(Box::new(AcceptUserData {
            sockaddr_storage: unsafe { std::mem::zeroed() },
            sockaddr_len: std::mem::size_of::<libc::sockaddr>() as libc::socklen_t,
        }));

        let e = unsafe {
            Accept::new(
                io_uring::types::Fd(fd.as_raw_fd()),
                &mut (*udata).sockaddr_storage as *mut _ as *mut _,
                &mut (*udata).sockaddr_len,
            )
            .build()
        };
        let cqe = u.push(e).await;
        let raw_fd = cqe.error_for_errno()?;

        let udata = unsafe { Box::from_raw(udata) };
        let addr = unsafe { socket2::SockAddr::new(udata.sockaddr_storage, udata.sockaddr_len) };
        let peer_addr = addr.as_socket().unwrap();

        let socket = unsafe { socket2::Socket::from_raw_fd(raw_fd) };
        Ok((TcpStream { socket }, peer_addr))
    }
}

#[allow(dead_code)]
pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read<B: IoBufMut>(&mut self, _buf: B) -> BufResult<usize, B> {
        todo!()
    }
}

#[allow(dead_code)]
pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write(&mut self, _buf: Piece) -> BufResult<usize, Piece> {
        todo!()
    }

    async fn writev(&mut self, _list: &PieceList) -> std::io::Result<usize> {
        todo!()
    }

    async fn shutdown(&mut self, _how: Shutdown) -> std::io::Result<()> {
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

impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(_fd: RawFd) -> Self {
        todo!()
    }
}

trait CqueueExt {
    fn error_for_errno(&self) -> Result<i32, Errno>;
}

impl CqueueExt for io_uring::cqueue::Entry {
    fn error_for_errno(&self) -> Result<i32, Errno> {
        let res = self.result();
        if res < 0 {
            Err(Errno::from_raw(-res))
        } else {
            Ok(res as _)
        }
    }
}

#[cfg(test)]
mod tests {
    use fluke_io_uring_async::IoUringAsync;
    use send_wrapper::SendWrapper;

    use super::get_uring;

    #[test]
    fn test_accept() {
        color_eyre::install().unwrap();

        let u = SendWrapper::new(get_uring());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .on_thread_park(move || {
                u.submit().unwrap();
            })
            .build()
            .unwrap();

        async fn test_accept_inner() -> color_eyre::Result<()> {
            let listener = super::TcpListener::bind("127.0.0.1:0".parse().unwrap()).await?;
            let addr = listener.local_addr()?;
            println!("listening on {}", addr);

            std::thread::spawn(move || {
                use std::io::Write;

                let mut sock = std::net::TcpStream::connect(addr).unwrap();
                println!(
                    "connected! local={:?}, remote={:?}",
                    sock.local_addr(),
                    sock.peer_addr()
                );

                sock.write_all(b"hello").unwrap();
            });

            let _res = listener.accept().await?;
            println!("accepted one!");

            Ok(())
        }

        rt.block_on(async move {
            tokio::task::LocalSet::new()
                .run_until(async {
                    // Spawn a task that waits for the io_uring to become readable and handles completion
                    // queue entries accordingly.
                    tokio::task::spawn_local(IoUringAsync::listen(get_uring()));

                    test_accept_inner().await.unwrap();
                })
                .await;
        });
    }
}
