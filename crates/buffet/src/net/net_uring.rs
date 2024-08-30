use std::{
    mem::ManuallyDrop,
    net::SocketAddr,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    rc::Rc,
};

use io_uring::opcode::{Accept, Read, Write, Writev};
use libc::iovec;
use nix::errno::Errno;

use crate::{
    get_ring,
    io::{IntoHalves, ReadOwned, WriteOwned},
    BufResult, IoBufMut, Piece, PieceList,
};

pub struct TcpStream {
    fd: i32,
}

impl TcpStream {
    // TODO: nodelay
    pub async fn connect(addr: SocketAddr) -> std::io::Result<Self> {
        let addr: socket2::SockAddr = addr.into();
        let socket = ManuallyDrop::new(socket2::Socket::new(
            addr.domain(),
            socket2::Type::STREAM,
            None,
        )?);
        socket.set_nodelay(true)?;
        let fd = socket.as_raw_fd();

        let u = get_ring();

        let addr = Box::into_raw(Box::new(addr));
        let sqe = unsafe {
            io_uring::opcode::Connect::new(io_uring::types::Fd(fd), addr as *const _, (*addr).len())
        }
        .build();
        let cqe = u.push(sqe).await;
        cqe.error_for_errno()?;
        Ok(Self { fd })
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        // TODO: rethink this.
        // what about all the in-flight operations?
        unsafe {
            libc::close(self.fd);
        }
    }
}

pub struct TcpListener {
    fd: i32,
}

impl TcpListener {
    // note: this is only async to match tokio's API
    // TODO: investigate why tokio's TcpListener::bind is async
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let addr: socket2::SockAddr = addr.into();
        let socket = socket2::Socket::new(addr.domain(), socket2::Type::STREAM, None)?;

        socket.set_nodelay(true)?;

        // FIXME: don't hardcode, but we get test failures on Linux otherwise for some
        // reason
        socket.set_reuse_port(true)?;
        socket.set_reuse_address(true)?;
        socket.bind(&addr)?;

        // FIXME: magic values
        socket.listen(256)?;

        let fd = socket.as_raw_fd();
        std::mem::forget(socket);

        Ok(Self { fd })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        let socket = ManuallyDrop::new(unsafe { socket2::Socket::from_raw_fd(self.fd) });
        let addr = socket.local_addr()?;
        Ok(addr.as_socket().unwrap())
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        let u = get_ring();
        struct AcceptUserData {
            sockaddr_storage: libc::sockaddr_storage,
            sockaddr_len: libc::socklen_t,
        }
        // FIXME: this currently leaks if the future is dropped
        let udata = Box::into_raw(Box::new(AcceptUserData {
            sockaddr_storage: unsafe { std::mem::zeroed() },
            sockaddr_len: std::mem::size_of::<libc::sockaddr>() as libc::socklen_t,
        }));

        let sqe = unsafe {
            Accept::new(
                io_uring::types::Fd(self.fd),
                &mut (*udata).sockaddr_storage as *mut _ as *mut _,
                &mut (*udata).sockaddr_len,
            )
            .build()
        };
        let cqe = u.push(sqe).await;
        let fd = cqe.error_for_errno()?;

        let udata = unsafe { Box::from_raw(udata) };
        let addr = unsafe { socket2::SockAddr::new(udata.sockaddr_storage, udata.sockaddr_len) };
        let peer_addr = addr.as_socket().unwrap();

        Ok((TcpStream { fd }, peer_addr))
    }
}

// TODO: fix about the lifetime of TcpStream, closing
// the underlying fd, in-flight operations etc.
pub struct TcpReadHalf(Rc<TcpStream>);

impl ReadOwned for TcpReadHalf {
    async fn read_owned<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        let sqe = Read::new(
            io_uring::types::Fd(self.0.fd),
            buf.io_buf_mut_stable_mut_ptr(),
            buf.io_buf_mut_capacity() as u32,
        )
        .build();
        tracing::trace!(
            "submitting read_owned, reading from fd {} to {:p} with capacity {}",
            self.0.fd,
            buf.io_buf_mut_stable_mut_ptr(),
            buf.io_buf_mut_capacity()
        );
        let cqe = get_ring().push(sqe).await;
        let ret = match cqe.error_for_errno() {
            Ok(ret) => ret,
            Err(e) => return (Err(std::io::Error::from(e)), buf),
        };
        (Ok(ret as usize), buf)
    }
}

pub struct TcpWriteHalf(Rc<TcpStream>);

impl WriteOwned for TcpWriteHalf {
    async fn write_owned(&mut self, buf: impl Into<Piece>) -> BufResult<usize, Piece> {
        let buf = buf.into();
        let sqe = Write::new(
            io_uring::types::Fd(self.0.fd),
            buf.as_ref().as_ptr(),
            buf.len().try_into().expect("usize -> u32"),
        )
        .build();

        let cqe = get_ring().push(sqe).await;
        let ret = match cqe.error_for_errno() {
            Ok(ret) => ret,
            Err(e) => return (Err(std::io::Error::from(e)), buf),
        };
        (Ok(ret as usize), buf)
    }

    // TODO: implement writev

    // async fn writev_owned(&mut self, list: &PieceList) -> std::io::Result<usize>
    // {     let mut iovecs: Vec<iovec> = vec![];
    //     for piece in list.pieces.iter() {
    //         let iov = iovec {
    //             iov_base: piece.as_ref().as_ptr() as *mut libc::c_void,
    //             iov_len: piece.len(),
    //         };
    //         iovecs.push(iov);
    //     }
    //     let iovecs = iovecs.into_boxed_slice();
    //     let iov_cnt = iovecs.len();
    //     // FIXME: don't leak, duh
    //     let iovecs = Box::leak(iovecs);

    //     let sqe = Writev::new(
    //         io_uring::types::Fd(self.0.fd),
    //         iovecs.as_ptr() as *const _,
    //         iov_cnt as u32,
    //     )
    //     .build();

    //     let cqe = get_ring().push(sqe).await;
    //     let ret = match cqe.error_for_errno() {
    //         Ok(ret) => ret,
    //         Err(e) => return Err(std::io::Error::from(e)),
    //     };
    //     Ok(ret as usize)
    // }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        tracing::debug!("requesting shutdown");
        let sqe =
            io_uring::opcode::Shutdown::new(io_uring::types::Fd(self.0.fd), libc::SHUT_WR).build();
        let cqe = get_ring().push(sqe).await;
        cqe.error_for_errno()?;
        Ok(())
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
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self { fd }
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

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use crate::io::{IntoHalves, ReadOwned, WriteOwned};

    #[test]
    fn test_accept() {
        async fn test_accept_inner() {
            let listener = super::TcpListener::bind("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            println!("listening on {}", addr);

            std::thread::spawn(move || {
                use std::io::{Read, Write};

                let mut sock = std::net::TcpStream::connect(addr).unwrap();
                println!(
                    "[client] connected! local={:?}, remote={:?}",
                    sock.local_addr(),
                    sock.peer_addr()
                );

                let mut buf = [0u8; 5];
                sock.read_exact(&mut buf).unwrap();
                println!("[client] read: {:?}", std::str::from_utf8(&buf).unwrap());

                sock.write_all(b"hello").unwrap();
                println!("[client] wrote: hello");
            });

            let (stream, addr) = listener.accept().await.unwrap();
            println!("accepted connection!, addr={addr:?}");

            let (mut r, mut w) = stream.into_halves();
            w.write_all_owned("howdy").await.unwrap();

            let buf = vec![0u8; 1024];
            let (res, buf) = r.read_owned(buf).await;
            let n = res.unwrap();
            let slice = &buf[..n];
            println!(
                "read {} bytes: {:?}, as string: {:?}",
                n,
                slice,
                std::str::from_utf8(slice).unwrap()
            );
        }
        crate::start(async move { test_accept_inner().await });
    }
}
