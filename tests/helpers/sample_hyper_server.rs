use std::convert::Infallible;

use futures::Future;
use hyper::{service::Service, Body, Request, Response};

pub(crate) struct TestService;

impl Service<Request<Body>> for TestService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        async move {
            let (_parts, body) = req.into_parts();
            let res = Response::builder().body(body).unwrap();
            Ok(res)
        }
    }
}
