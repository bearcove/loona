use std::{cell::RefCell, rc::Rc};

use pretty_hex::PrettyHex;
use tokio::sync::mpsc;
use tokio_uring::{
    buf::{IoBuf, IoBufMut},
    BufResult,
};
use tracing::trace;

use crate::WriteOwned;

use super::ReadOwned;

/// Allows sending `Vec<u8>` chunks, which can be read through its [ReadOwned]
/// implementation.
pub struct ChanRead {
    inner: Rc<ChanReadInner>,
}

pub struct ChanReadSend {
    inner: Rc<ChanReadInner>,
}

struct ChanReadInner {
    notify: tokio::sync::Notify,
    guarded: RefCell<ChanReadGuarded>,
}

struct ChanReadGuarded {
    state: ChanReadState,
    pos: usize,
    buf: Vec<u8>,
}

enum ChanReadState {
    // Data may still come in
    Live,

    // [ChanReaderSend] was dropped, no more data is coming
    Eof,

    // [ChanReaderSend::rest] was called
    Reset,
}

impl ChanRead {
    pub fn new() -> (ChanReadSend, Self) {
        let inner = Rc::new(ChanReadInner {
            notify: Default::default(),
            guarded: RefCell::new(ChanReadGuarded {
                state: ChanReadState::Live,
                pos: 0,
                buf: Vec::new(),
            }),
        });
        (
            ChanReadSend {
                inner: inner.clone(),
            },
            Self { inner },
        )
    }
}

impl ChanReadSend {
    /// Sever this connection abnormally - read will eventually return [std::io::ErrorKind::ConnectionReset]
    pub fn reset(self) {
        let mut guarded = self.inner.guarded.borrow_mut();
        guarded.state = ChanReadState::Reset;
        // let it drop, which will notify waiters
    }

    /// Send a chunk of data. Readers will not be able to read _more_ than the
    /// length of this chunk in a single call, but may read less (if their buffer
    /// is too small).
    pub async fn send(&self, next_buf: impl Into<Vec<u8>>) -> Result<(), std::io::Error> {
        let next_buf = next_buf.into();
        trace!("Sending {:?}", next_buf.hex_dump());

        loop {
            {
                let mut guarded = self.inner.guarded.borrow_mut();
                match guarded.state {
                    ChanReadState::Live => {
                        if guarded.pos == guarded.buf.len() {
                            trace!("Writing + notifying waiters {:?}", next_buf.hex_dump());
                            guarded.pos = 0;
                            guarded.buf = next_buf;
                            self.inner.notify.notify_waiters();
                            return Ok(());
                        } else {
                            // wait for read
                        }
                    }

                    // can't send after dropping
                    ChanReadState::Eof => unreachable!(),

                    // can't send after calling abort
                    ChanReadState::Reset => unreachable!(),
                }
            }
            self.inner.notify.notified().await
        }
    }
}

impl Drop for ChanReadSend {
    fn drop(&mut self) {
        let mut guarded = self.inner.guarded.borrow_mut();
        if let ChanReadState::Live = guarded.state {
            guarded.state = ChanReadState::Eof;
        }
        self.inner.notify.notify_waiters();
    }
}

impl ReadOwned for ChanRead {
    async fn read<B: IoBufMut>(&self, mut buf: B) -> BufResult<usize, B> {
        trace!("Reading {} bytes", buf.bytes_total());
        let out =
            unsafe { std::slice::from_raw_parts_mut(buf.stable_mut_ptr(), buf.bytes_total()) };

        loop {
            {
                let mut guarded = self.inner.guarded.borrow_mut();
                let remain = guarded.buf.len() - guarded.pos;

                if remain > 0 {
                    trace!("{remain} bytes remain");

                    let n = std::cmp::min(remain, out.len());
                    trace!("reading {n} bytes");

                    out[..n].copy_from_slice(&guarded.buf[guarded.pos..guarded.pos + n]);
                    guarded.pos += n;

                    trace!("notifying waiters and returning {}", out[..n].hex_dump());
                    self.inner.notify.notify_waiters();

                    unsafe {
                        buf.set_init(n);
                    }
                    return (Ok(n), buf);
                }

                match guarded.state {
                    ChanReadState::Live => {
                        // muffin
                    }
                    ChanReadState::Eof => {
                        return (Ok(0), buf);
                    }
                    ChanReadState::Reset => {
                        return (Err(std::io::ErrorKind::ConnectionReset.into()), buf);
                    }
                }
            }

            self.inner.notify.notified().await;
        }
    }
}

pub struct ChanWrite {
    tx: mpsc::Sender<Vec<u8>>,
}

impl ChanWrite {
    pub fn new() -> (mpsc::Receiver<Vec<u8>>, Self) {
        let (tx, rx) = mpsc::channel(1);
        (rx, Self { tx })
    }
}

impl WriteOwned for ChanWrite {
    async fn write<B: IoBuf>(&self, buf: B) -> BufResult<usize, B> {
        let slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), buf.bytes_init()) };
        match self.tx.send(slice.to_vec()).await {
            Ok(()) => (Ok(buf.bytes_init()), buf),
            Err(_) => (Err(std::io::ErrorKind::BrokenPipe.into()), buf),
        }
    }
}

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::{ChanRead, ReadOwned};
    use pretty_assertions::assert_eq;

    #[test]
    fn test_chan_reader() {
        tokio_uring::start(async move {
            let (send, cr) = ChanRead::new();
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

            let (send, cr) = ChanRead::new();

            tokio_uring::spawn({
                async move {
                    send.send("two-part").await.unwrap();
                    send.reset();
                }
            });

            for _ in 0..5 {
                tokio::task::yield_now().await;
            }

            {
                let buf = vec![0u8; 4];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"two-");
            }

            {
                let buf = vec![0u8; 4];
                let (res, buf) = cr.read(buf).await;
                let n = res.unwrap();
                assert_eq!(&buf[..n], b"part");
            }

            {
                let buf = vec![0u8; 0];
                let (res, _) = cr.read(buf).await;
                let err = res.unwrap_err();
                assert_eq!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionReset,
                    "reached EOF"
                );
            }
        })
    }
}
