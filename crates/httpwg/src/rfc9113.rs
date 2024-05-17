use fluke_buffet::IntoHalves;
use std::rc::Rc;

use crate::{test_struct, Config, Conn, Test, TestGroup};

test_struct!("3.4", test3_4, Test3_4);
async fn test3_4<IO: IntoHalves + 'static>(
    _config: Rc<Config>,
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    tracing::debug!("Writing http/2 preface");
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;
    tracing::debug!("Sleeping a bit!");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    tracing::debug!("Okay that's it");
    Ok(())
}

pub fn group<IO: IntoHalves + 'static>() -> TestGroup<IO> {
    fn t<IO: IntoHalves + 'static, T: Test<IO> + Default + 'static>() -> Box<dyn Test<IO>> {
        Box::new(T::default())
    }

    TestGroup {
        name: "RFC 9113".to_owned(),
        tests: vec![t::<IO, Test3_4>()],
    }
}
