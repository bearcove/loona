#![feature(test)]
extern crate test;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

static INPUT: &[u8] = b"This is some sample data";

fn bench_bufpool(c: &mut Criterion) {
    let mut g = c.benchmark_group("alloc");

    g.bench_function("noop", |b| b.iter_with_setup(|| {}, |_| {}));
    g.bench_function("buf-unchecked", |b| {
        b.iter_with_setup(
            || {
                alt_http::bufpool::init().unwrap();
            },
            |_| {
                let mut buf = alt_http::bufpool::Buf::alloc_unchecked().unwrap();
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            },
        )
    });
    g.bench_function("buf-checked", |b| {
        b.iter_with_setup(
            || {},
            |_| {
                let mut buf = alt_http::bufpool::Buf::alloc().unwrap();
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            },
        )
    });
    g.bench_function("vec", |b| {
        b.iter_with_setup(
            || {},
            |_| {
                let mut buf: Vec<u8> = Vec::with_capacity(4096);
                buf.resize(INPUT.len(), 0);
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            },
        )
    });
}

criterion_group!(benches, bench_bufpool);
criterion_main!(benches);
