use std::{cell::RefCell, rc::Rc};

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

    async fn writev<B: IoBuf>(&self, list: Vec<B>) -> BufResult<usize, Vec<B>> {
        let mut out_list = Vec::with_capacity(list.len());
        let mut list = list.into_iter();
        let mut total = 0;

        while let Some(buf) = list.next() {
            let (res, buf) = self.write(buf).await;
            out_list.push(buf);
            match res {
                Ok(n) => total += n,
                Err(e) => {
                    out_list.extend(list);
                    return (Err(e), out_list);
                }
            }
        }

        (Ok(total), out_list)
    }

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

/// Allows sending `Vec<u8>` chunks, which can be read through its [ReadOwned]
/// implementation.
pub struct ChanReader {
    inner: Rc<ChanReaderInner>,
}

pub struct ChanReaderSend {
    inner: Rc<ChanReaderInner>,
}

struct ChanReaderInner {
    notify: tokio::sync::Notify,
    guarded: RefCell<ChanReaderGuarded>,
}

struct ChanReaderGuarded {
    state: ChanReaderState,
    pos: usize,
    buf: Vec<u8>,
}

enum ChanReaderState {
    // Data may still come in
    Live,

    // [ChanReaderSend] was dropped, no more data is coming
    Eof,

    // [ChanReaderSend::rest] was called
    Reset,
}

impl ChanReader {
    pub fn new() -> (ChanReaderSend, Self) {
        let inner = Rc::new(ChanReaderInner {
            notify: Default::default(),
            guarded: RefCell::new(ChanReaderGuarded {
                state: ChanReaderState::Live,
                pos: 0,
                buf: Vec::new(),
            }),
        });
        (
            ChanReaderSend {
                inner: inner.clone(),
            },
            Self { inner },
        )
    }
}

impl ChanReaderSend {
    /// Sever this connection abnormally - read will eventually return [std::io::ErrorKind::ConnectionReset]
    pub fn reset(self) {
        let mut guarded = self.inner.guarded.borrow_mut();
        guarded.state = ChanReaderState::Reset;
        // let it drop, which will notify waiters
    }

    /// Send a chunk of data. Readers will not be able to read _more_ than the
    /// length of this chunk in a single call, but may read less (if their buffer
    /// is too small).
    pub async fn send(&self, next_buf: impl Into<Vec<u8>>) -> Result<(), std::io::Error> {
        let next_buf = next_buf.into();

        loop {
            {
                let mut guarded = self.inner.guarded.borrow_mut();
                match guarded.state {
                    ChanReaderState::Live => {
                        if guarded.pos == guarded.buf.len() {
                            guarded.pos = 0;
                            guarded.buf = next_buf;
                            self.inner.notify.notify_waiters();
                            return Ok(());
                        } else {
                            // wait for read
                        }
                    }

                    // can't send after dropping
                    ChanReaderState::Eof => unreachable!(),

                    // can't send after calling abort
                    ChanReaderState::Reset => unreachable!(),
                }
            }
            self.inner.notify.notified().await
        }
    }
}

impl Drop for ChanReaderSend {
    fn drop(&mut self) {
        let mut guarded = self.inner.guarded.borrow_mut();
        if let ChanReaderState::Live = guarded.state {
            guarded.state = ChanReaderState::Eof;
        }
        self.inner.notify.notify_waiters();
    }
}

impl ReadOwned for ChanReader {
    async fn read<B: IoBufMut>(&self, mut buf: B) -> BufResult<usize, B> {
        let out =
            unsafe { std::slice::from_raw_parts_mut(buf.stable_mut_ptr(), buf.bytes_total()) };

        loop {
            {
                let mut guarded = self.inner.guarded.borrow_mut();
                let remain = guarded.buf.len() - guarded.pos;

                if remain > 0 {
                    let n = std::cmp::min(remain, out.len());
                    out[..n].copy_from_slice(&guarded.buf[guarded.pos..guarded.pos + n]);
                    guarded.pos += n;

                    self.inner.notify.notify_waiters();
                    return (Ok(n), buf);
                }

                if guarded.closed {
                    return (Ok(0), buf);
                }
            }

            self.inner.notify.notified().await;
        }
    }
}

/// Unites a [ReadOwned] and a [WriteOwned] into a single [ReadWriteOwned] type.
pub struct ReadWritePair<R, W>(R, W)
where
    R: ReadOwned,
    W: WriteOwned;

impl ReadOwned for ReadWritePair<TcpStream, TcpStream> {
    async fn read<B: IoBufMut>(&self, buf: B) -> BufResult<usize, B> {
        self.0.read(buf).await
    }
}

impl WriteOwned for ReadWritePair<TcpStream, TcpStream> {
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        self.1.write(buf).await
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::{ChanReader, ReadOwned};
    use pretty_assertions::assert_eq;

    #[test]
    fn test_chan_reader() {
        tokio_uring::start(async move {
            let (send, cr) = ChanReader::new();
            let wrote_three = Rc::new(RefCell::new(false));

            tokio_uring::spawn({
                let wrote_three = wrote_three.clone();
                async move {
                    send.send("one").await.unwrap();
                    send.send("two").await.unwrap();
                    send.send("three").await.unwrap();
                    *wrote_three.borrow_mut() = true;
                    send.send("splitread").await.unwrap();
                }
            });

            {
                let buf = vec![0u8; 256];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"one");
            }

            assert!(!*wrote_three.borrow());

            {
                let buf = vec![0u8; 256];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"two");
            }

            tokio::task::yield_now().await;
            assert!(*wrote_three.borrow());

            {
                let buf = vec![0u8; 256];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"three");
            }

            {
                let buf = vec![0u8; 5];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"split");

                let buf = vec![0u8; 256];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"read");
            }

            {
                let buf = vec![0u8; 0];
                let (res, _) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(n, 0, "reached EOF");
            }
        })
    }
}
