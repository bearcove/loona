use enumflags2::BitFlags;
use eyre::Context;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::h2::{
    parse::{DataFlags, Frame, FrameType, HeadersFlags, PingFlags, SettingsFlags, StreamId},
    types::{H2ConnEvent, H2EventPayload},
};
use fluke_buffet::{PieceList, RollMut};
use fluke_maybe_uring::io::WriteOwned;

/// Write H2 frames to the transport, from a channel
pub(crate) async fn h2_write_loop(
    mut ev_rx: mpsc::Receiver<H2ConnEvent>,
    mut transport_w: impl WriteOwned,
    mut out_scratch: RollMut,
) -> eyre::Result<()> {
    let mut hpack_enc = fluke_hpack::Encoder::new();

    while let Some(ev) = ev_rx.recv().await {
        trace!("h2_write_loop: received H2 event");
        match ev {
            H2ConnEvent::AcknowledgeSettings {
                new_max_header_table_size,
            } => {
                debug!("Acknowledging new settings");
                hpack_enc.set_max_table_size(new_max_header_table_size.try_into().unwrap());

                let frame = Frame::new(
                    FrameType::Settings(SettingsFlags::Ack.into()),
                    StreamId::CONNECTION,
                );
                transport_w
                    .write_all(frame.into_roll(&mut out_scratch)?)
                    .await
                    .wrap_err("writing acknowledge settings")?;
            }
            H2ConnEvent::ServerEvent(ev) => {
                debug!(?ev, "Writing");

                match ev.payload {
                    H2EventPayload::Headers(res) => {
                        let flags = HeadersFlags::EndHeaders;
                        let mut frame = Frame::new(FrameType::Headers(flags.into()), ev.stream_id);

                        // TODO: don't allocate so much for headers. all `encode_into`
                        // wants is an `IntoIter`, we can definitely have a custom iterator
                        // that operates on all this instead of using a `Vec`.

                        // TODO: limit header size
                        let mut headers: Vec<(&[u8], &[u8])> = vec![];
                        headers.push((b":status", res.status.as_str().as_bytes()));
                        for (name, value) in res.headers.iter() {
                            if name == http::header::TRANSFER_ENCODING {
                                // do not set transfer-encoding: chunked when doing HTTP/2
                                continue;
                            }
                            headers.push((name.as_str().as_bytes(), value));
                        }

                        assert_eq!(out_scratch.len(), 0);
                        hpack_enc.encode_into(headers, &mut out_scratch)?;
                        let fragment_block = out_scratch.take_all();

                        frame.len = fragment_block.len() as u32;
                        let frame_roll = frame.into_roll(&mut out_scratch)?;

                        transport_w
                            .writev_all(PieceList::default().with(frame_roll).with(fragment_block))
                            .await
                            .wrap_err("writing headers")?;
                    }
                    H2EventPayload::BodyChunk(chunk) => {
                        let flags = BitFlags::<DataFlags>::default();
                        let frame = Frame::new(FrameType::Data(flags), ev.stream_id)
                            .with_len(chunk.len().try_into().unwrap());
                        let frame_roll = frame.into_roll(&mut out_scratch)?;
                        transport_w
                            .writev_all(PieceList::default().with(frame_roll).with(chunk))
                            .await
                            .wrap_err("writing bodychunk")?;
                    }
                    H2EventPayload::BodyEnd => {
                        // FIXME: this should transition the stream to `Closed`
                        // state (or at the very least `HalfClosedLocal`).
                        // Either way, whoever owns the stream state should know
                        // about it, cf. https://github.com/hapsoc/fluke/issues/123

                        let flags = DataFlags::EndStream;
                        let frame = Frame::new(FrameType::Data(flags.into()), ev.stream_id);
                        transport_w
                            .write_all(frame.into_roll(&mut out_scratch)?)
                            .await
                            .wrap_err("writing BodyEnd")?;
                    }
                }
            }
            H2ConnEvent::Ping(payload) => {
                // send pong frame
                let flags = PingFlags::Ack.into();
                let frame = Frame::new(FrameType::Ping(flags), StreamId::CONNECTION)
                    .with_len(payload.len() as u32);
                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(payload),
                    )
                    .await
                    .wrap_err("writing pong")?;
            }
            H2ConnEvent::GoAway {
                err,
                last_stream_id,
            } => {
                let error_code = err.as_known_error_code();
                warn!("connection error: {err} ({err:?}) (code {error_code:?})");

                // let's put something useful in debug data
                let additional_debug_data = format!("{err}").into_bytes();

                debug!(%last_stream_id, ?error_code, "Sending GoAway");
                let header = out_scratch.put_to_roll(8, |mut slice| {
                    use byteorder::{BigEndian, WriteBytesExt};
                    // TODO: do we ever need to write the reserved bit?
                    slice.write_u32::<BigEndian>(last_stream_id.0)?;
                    slice.write_u32::<BigEndian>(error_code.repr())?;

                    Ok(())
                })?;

                let frame = Frame::new(FrameType::GoAway, StreamId::CONNECTION).with_len(
                    (header.len() + additional_debug_data.len())
                        .try_into()
                        .unwrap(),
                );

                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(header)
                            .with(additional_debug_data),
                    )
                    .await
                    .wrap_err("writing goaway")?;
            }
            H2ConnEvent::RstStream {
                stream_id,
                error_code,
            } => {
                debug!(%stream_id, ?error_code, "Sending RstStream");
                let header = out_scratch.put_to_roll(4, |mut slice| {
                    use byteorder::{BigEndian, WriteBytesExt};
                    slice.write_u32::<BigEndian>(error_code.repr())?;
                    Ok(())
                })?;

                let frame = Frame::new(FrameType::RstStream, stream_id)
                    .with_len((header.len()).try_into().unwrap());

                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(header),
                    )
                    .await
                    .wrap_err("writing rststream")?;
            }
        }
    }

    Ok(())
}
