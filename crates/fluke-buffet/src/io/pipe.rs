use tokio::sync::mpsc;

use crate::{Piece, ReadOwned, WriteOwned};

/// Create a new pipe.
pub fn pipe() -> (PipeRead, PipeWrite) {
    let (tx, rx) = mpsc::channel(1);
    (
        PipeRead {
            rx,
            state: Default::default(),
            remain: None,
        },
        PipeWrite { tx },
    )
}

enum PipeEvent {
    Piece(Piece),
    Reset,
    // close is just dropping the channel
}

#[derive(Clone, Copy, Default)]
enum ReadState {
    #[default]
    Live,
    Reset,
    Eof,
}

pub struct PipeRead {
    rx: mpsc::Receiver<PipeEvent>,
    remain: Option<Piece>,
    state: ReadState,
}

impl ReadOwned for PipeRead {
    async fn read<B: crate::IoBufMut>(&mut self, mut buf: B) -> crate::BufResult<usize, B> {
        match self.state {
            ReadState::Live => {
                // good, continue
            }
            ReadState::Reset => {
                let err = std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "simulated connection reset",
                );
                return (Err(err), buf);
            }
            ReadState::Eof => return (Ok(0), buf),
        }

        if self.remain.is_none() {
            match self.rx.recv().await {
                Some(PipeEvent::Piece(piece)) => {
                    self.remain = Some(piece);
                }
                Some(PipeEvent::Reset) => {
                    self.state = ReadState::Reset;
                    return self.read(buf).await;
                }
                None => {
                    self.state = ReadState::Eof;
                    return self.read(buf).await;
                }
            }
        }

        let remain = self.remain.take().unwrap();
        let avail = buf.io_buf_mut_capacity();
        let read_size = avail.min(remain.len());
        let (copied, remain) = remain.split_at(read_size);
        assert_eq!(copied.len(), read_size);
        unsafe {
            buf.slice_mut()[..read_size].copy_from_slice(&copied[..]);
        }

        if !remain.is_empty() {
            self.remain = Some(remain);
        }
        (Ok(read_size), buf)
    }
}

pub struct PipeWrite {
    tx: mpsc::Sender<PipeEvent>,
}

impl PipeWrite {
    /// Simulate a connection reset
    pub async fn reset(self) {
        self.tx.send(PipeEvent::Reset).await.unwrap();
    }
}

impl WriteOwned for PipeWrite {
    async fn write(&mut self, buf: Piece) -> crate::BufResult<usize, Piece> {
        if let Err(_) = self.tx.send(PipeEvent::Piece(buf.clone())).await {
            let err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "simulated broken pipe");
            return (Err(err), buf);
        }

        (Ok(buf.len()), buf)
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
