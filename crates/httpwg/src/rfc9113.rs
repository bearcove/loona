use futures_util::future::LocalBoxFuture;
use std::sync::Arc;

use crate::{Config, Conn, Test, TestGroup};

#[derive(Default)]
pub struct Test3_4 {}

impl Test for Test3_4 {
    fn run(&self, _config: Arc<Config>, mut conn: Conn) -> LocalBoxFuture<Result<(), String>> {
        Box::pin(async move {
            conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await;
            Ok(())
        })
    }
}

pub fn group() -> TestGroup {
    fn t<T: Test + Default + 'static>() -> Box<dyn Test> {
        Box::new(T::default())
    }

    TestGroup {
        name: "RFC 9113".to_owned(),
        tests: vec![t::<Test3_4>()],
    }
}
