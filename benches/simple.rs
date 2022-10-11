#![feature(test)]
extern crate test;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_bufpool(c: &mut Criterion) {
    c.bench_function("alloc bufpool", |b| {
        b.iter(|| {
            for _ in 0..100_000 {
                let buf = alt_http::bufpool::Buf::alloc().unwrap();
                black_box(&buf[..]);
            }
        })
    });
    c.bench_function("alloc vec", |b| {
        b.iter(|| {
            for _ in 0..100_000 {
                let buf: Vec<u8> = Vec::with_capacity(1024);
                black_box(&buf[..]);
            }
        })
    });
}

criterion_group!(benches, bench_bufpool);
criterion_main!(benches);
