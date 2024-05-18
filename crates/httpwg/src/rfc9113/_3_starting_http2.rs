use fluke_buffet::IntoHalves;
use tracing::debug;

use crate::Conn;

pub async fn starting_http2<IO: IntoHalves + 'static>(mut conn: Conn<IO>) -> eyre::Result<()> {
    debug!("Writing http/2 preface");
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;

    debug!("Okay that's it");
    Ok(())
}
