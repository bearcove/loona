use std::sync::Arc;

use fluke_buffet::{Piece, ReadOwned, WriteOwned};
use futures_util::future::LocalBoxFuture;

pub mod rfc9113;

pub struct TestGroup {
    pub name: String,
    pub tests: Vec<Box<dyn Test>>,
}

pub struct Conn {
    r: fluke_buffet::net::TcpStream,
    w: fluke_buffet::net::TcpStream,
}

impl Conn {
    async fn send(&mut self, buf: impl Into<Piece>) -> std::io::Result<usize> {
        self.w.write_all(buf.into()).await?;
        Ok(0)
    }
}

pub struct Config {}

pub trait Test {
    fn run(&self, config: Arc<Config>, conn: Conn) -> LocalBoxFuture<Result<(), String>>;
}

fn all_groups() -> Vec<TestGroup> {
    fn t<T: Test + Default + 'static>() -> Box<dyn Test> {
        Box::new(T::default())
    }

    vec![TestGroup {
        name: "RFC 9113".to_owned(),
        tests: vec![t::<rfc9113::Test3_4>()],
    }]
}
