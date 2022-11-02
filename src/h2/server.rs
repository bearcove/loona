use std::rc::Rc;

use tracing::debug;

use crate::{
    h2::parse::{Frame, FrameType, SettingsFlags},
    util::read_and_parse,
    ReadWriteOwned, RollMut,
};

/// HTTP/2 server configuration
pub struct ServerConf {}

impl Default for ServerConf {
    fn default() -> Self {
        Self {}
    }
}

pub(crate) mod parse;

pub async fn serve(
    transport: impl ReadWriteOwned,
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
) -> eyre::Result<()> {
    const MAX_FRAME_LEN: usize = 64 * 1024;

    let transport = Rc::new(transport);

    (client_buf, _) = match read_and_parse(
        parse::preface,
        transport.as_ref(),
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

    loop {
        let frame;
        (client_buf, frame) =
            match read_and_parse(Frame::parse, transport.as_ref(), client_buf, 32 * 1024).await? {
                Some((client_buf, frame)) => (client_buf, frame),
                None => {
                    debug!("h2 client closed connection");
                    return Ok(());
                }
            };

        debug!(?frame, "received h2 frame");

        let mut remain = frame.len as usize;
        while remain > 0 {
            debug!("reading frame payload, {remain} remains");

            if client_buf.is_empty() {
                let res;
                (res, client_buf) = client_buf.read_into(remain, transport.as_ref()).await;
                let n = res?;
                debug!("read {n} bytes for frame payload");
            }
            let roll = client_buf.take_at_most(remain).unwrap();
            debug!("read {} of frame payload", roll.len());
            remain -= roll.len();
        }

        debug!("done reading frame payload");

        match frame.frame_type {
            FrameType::Data(_) => todo!(),
            FrameType::Headers(_h) => {
                debug!("received headers for stream {}", frame.stream_id);
            }
            FrameType::Priority => todo!(),
            FrameType::RstStream => todo!(),
            FrameType::Settings(s) => {
                if !s.contains(SettingsFlags::Ack) {
                    debug!("acknowledging new settings");
                    let res_frame = Frame::new(FrameType::Settings(SettingsFlags::Ack.into()), 0);
                    res_frame.write(transport.as_ref()).await?;
                }
            }
            FrameType::PushPromise => todo!("push promise not implemented"),
            FrameType::Ping => todo!(),
            FrameType::GoAway => todo!(),
            FrameType::WindowUpdate => {
                debug!("ignoring window update");
            }
            FrameType::Continuation => todo!(),
        }
    }
}
