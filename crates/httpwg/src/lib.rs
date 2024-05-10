use futures_util::future::BoxFuture;

pub mod rfc9113;

struct TestGroup {
    name: String,
    tests: Vec<Box<dyn Test>>,
}

pub trait Conn {}

pub struct Config {}

pub trait Test {
    fn run(&self, config: &Config, conn: Box<dyn Conn>) -> BoxFuture<Result<(), String>>;
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
