use std::rc::Rc;

use tracing::debug;

use crate::{
    h2::{
        parse::{self, Frame, FrameType, StreamId},
        read::H2ReadContext,
        types::{ConnState, H2ConnEvent},
    },
    util::read_and_parse,
    ServerDriver,
};
use fluke_buffet::RollMut;
use fluke_maybe_uring::io::{ReadOwned, WriteOwned};

/// HTTP/2 server configuration
pub struct ServerConf {
    pub max_streams: u32,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self { max_streams: 32 }
    }
}

pub async fn serve(
    (mut transport_r, mut transport_w): (impl ReadOwned, impl WriteOwned),
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: Rc<impl ServerDriver + 'static>,
) -> eyre::Result<()> {
    let mut state = ConnState::default();
    state.self_settings.max_concurrent_streams = conf.max_streams;

    (client_buf, _) = match read_and_parse(
        parse::preface,
        &mut transport_r,
        client_buf,
        parse::PREFACE.len(),
    )
    .await?
    {
        Some((client_buf, frame)) => (client_buf, frame),
        None => {
            debug!("h2 client closed connection before sending preface");
            return Ok(());
        }
    };
    debug!("read preface");

    let mut out_scratch = RollMut::alloc()?;

    // we have to send a settings frame
    {
        let payload_roll = state.self_settings.into_roll(&mut out_scratch)?;
        let frame_roll = Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(payload_roll.len().try_into().unwrap())
        .into_roll(&mut out_scratch)?;

        transport_w
            .writev_all(vec![frame_roll, payload_roll])
            .await?;
        debug!("sent settings frame");
    }

    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<H2ConnEvent>(32);

    let mut h2_read_cx = H2ReadContext::new(driver.clone(), ev_tx.clone(), state);

    enum Outcome {
        PeerGone,
        Ok,
    }

    let mut outcome = Outcome::Ok;

    {
        let mut read_task = std::pin::pin!(h2_read_cx.read_loop(client_buf, transport_r));
        let mut write_task =
            std::pin::pin!(super::write::h2_write_loop(ev_rx, transport_w, out_scratch));

        tokio::select! {
            res = &mut read_task => {
                match res {
                    Err(e) => {
                        return Err(e.wrap_err("h2 read (finished first)"));
                    }
                    Ok(()) => {
                        debug!("read task finished, waiting on write task now");
                        let res = write_task.await;
                        match res {
                            Err(e) => {
                                if is_peer_gone(&e) {
                                    outcome = Outcome::PeerGone;
                                } else {
                                    return Err(e.wrap_err("h2 write (finished second)"));
                                }
                            }
                            Ok(()) => {
                                debug!("write task finished okay");
                            }
                        }
                    }
                }
            },
            res = write_task.as_mut() => {
                match res {
                    Err(e) => {
                        if is_peer_gone(&e) {
                            outcome = Outcome::PeerGone;
                        } else {
                            return Err(e.wrap_err("h2 write (finished first)"));
                        }
                    }
                    Ok(()) => {
                        debug!("write task finished, giving up read task");
                    }
                }
            },
        };
    };

    match outcome {
        Outcome::PeerGone => {
            if h2_read_cx.goaway_sent {
                debug!("write task failed with broken pipe, but we already sent a goaway, so we're good");
            } else {
                return Err(eyre::eyre!(
                    "peer closed connection without sending a goaway"
                ));
            }
        }
        Outcome::Ok => {
            // all goodt hen!
        }
    }

    Ok(())
}

fn is_peer_gone(e: &eyre::Report) -> bool {
    if let Some(io_error) = e.root_cause().downcast_ref::<std::io::Error>() {
        matches!(
            io_error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        )
    } else {
        false
    }
}
