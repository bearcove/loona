use std::rc::Rc;

use fluke_buffet::{IntoHalves, Piece, ReadOwned, RollMut, WriteOwned};
use fluke_h2_parse::Frame;
use pretty_hex::PrettyHex;
use tracing::debug;

pub mod rfc9113;

pub struct TestGroup<IO> {
    pub name: String,
    pub tests: Vec<Box<dyn Test<IO>>>,
}

pub struct Conn<IO: IntoHalves + 'static> {
    w: <IO as IntoHalves>::Write,
    ev_rx: tokio::sync::mpsc::Receiver<Ev>,
}

pub enum Ev {
    Todo,
}

impl<IO: IntoHalves> Conn<IO> {
    pub fn new(io: IO) -> Self {
        let (mut r, w) = io.into_halves();

        let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<Ev>(1);
        let recv_fut = async move {
            let mut res_buf = RollMut::alloc()?;
            let mut buf = vec![0u8; 1024];
            loop {
                debug!("Reading a chunk");
                let res;
                (res, buf) = r.read_owned(buf).await;
                let n = res.unwrap();
                let chunk = &buf[..n];

                debug!("Got a chunk:\n{:?}", chunk.hex_dump());
                res_buf.put(&chunk[..]).unwrap();

                // try to parse a frame
                use fluke_h2_parse::Finish;
                let (rest, frame) = Frame::parse(res_buf.filled()).finish().unwrap();
                panic!("got frame {frame:#?}");

                todo!()
            }

            Ok::<_, eyre::Report>(())
        };
        fluke_buffet::spawn(async move { recv_fut.await.unwrap() });

        Self { w, ev_rx }
    }

    pub async fn send(&mut self, buf: impl Into<Piece>) -> eyre::Result<()> {
        self.w.write_all_owned(buf.into()).await?;
        Ok(())
    }

    pub async fn write_frame(&mut self, frame: Frame) -> eyre::Result<()> {
        let mut buf = Vec::new();
        frame.write_into(&mut buf)?;
        self.send(buf).await
    }
}

pub struct Config {}

pub trait Test<IO: IntoHalves + 'static> {
    fn name(&self) -> &'static str;
    fn run(
        &self,
        config: Rc<Config>,
        conn: Conn<IO>,
    ) -> futures_util::future::LocalBoxFuture<eyre::Result<()>>;
}

pub fn all_groups<IO: IntoHalves + 'static>() -> Vec<TestGroup<IO>> {
    vec![rfc9113::group()]
}

#[macro_export]
macro_rules! test_struct {
    ($name: expr, $fn: ident, $struct: ident) => {
        #[derive(Default)]
        pub struct $struct {}

        impl<IO: IntoHalves + 'static> Test<IO> for $struct {
            fn name(&self) -> &'static str {
                $name
            }

            fn run(
                &self,
                config: std::rc::Rc<Config>,
                conn: Conn<IO>,
            ) -> futures_util::future::LocalBoxFuture<eyre::Result<()>> {
                Box::pin($fn(config, conn))
            }
        }
    };
}

#[macro_export]
macro_rules! gen_tests {
    ($body: tt) => {
        #[cfg(test)]
        mod rfc9113 {
            use ::httpwg::rfc9113 as __rfc;

            #[test]
            fn test_3_4() {
                use __rfc::Test3_4 as Test;
                $body
            }

            #[test]
            fn test_4_1() {
                use __rfc::Test4_1 as Test;
                $body
            }
        }
    };
}
