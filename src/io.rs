use std::{cell::RefCell, rc::Rc};

use tokio::sync::broadcast;
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

pub struct ChanReadOwned {
    tx: broadcast::Sender<()>,
    buffered: Rc<RefCell<Buffered>>,
}

pub struct ChanReadOwnedSender {
    tx: broadcast::Sender<()>,
    buffered: Rc<RefCell<Buffered>>,
}

#[derive(Default)]
struct Buffered {
    pos: usize,
    buf: Vec<u8>,
}

impl Buffered {
    fn is_empty(&self) -> bool {
        self.pos == self.buf.len()
    }
}

impl ChanReadOwned {
    pub fn new() -> (Self, ChanReadOwnedSender) {
        let (tx, _) = broadcast::channel(1);
        let buffered = Rc::new(RefCell::new(Default::default()));
        (
            Self {
                tx: tx.clone(),
                buffered: buffered.clone(),
            },
            ChanReadOwnedSender { tx, buffered },
        )
    }
}

impl ChanReadOwnedSender {
    pub async fn send(&mut self, buf: Vec<u8>) {
        loop {
            {
                let mut buffered = self.buffered.borrow_mut();
                if buffered.is_empty() {
                    buffered.pos = 0;
                    buffered.buf = buf;
                    _ = self.tx.send(());
                    return;
                }
            }

            let mut rx = self.tx.subscribe();
            if rx.recv().await.is_ok() {
                continue;
            }
        }
    }
}

impl ReadOwned for ChanReadOwned {
    async fn read<B: IoBufMut>(&self, mut buf: B) -> BufResult<usize, B> {
        let out =
            unsafe { std::slice::from_raw_parts_mut(buf.stable_mut_ptr(), buf.bytes_total()) };

        loop {
            {
                let mut current = self.buffered.borrow_mut();
                let current = &mut current;
                let remain = current.buf.len() - current.pos;

                if remain > 0 {
                    let n = std::cmp::min(remain, out.len());
                    out[..n].copy_from_slice(&current.buf[current.pos..current.pos + n]);
                    current.pos += n;

                    _ = self.tx.send(());
                    return (Ok(n), buf);
                }
            }

            let mut rx = self.tx.subscribe();
            match rx.recv().await {
                Ok(_) => continue,
                Err(_) => return (Ok(0), buf),
            }
        }
    }
}
