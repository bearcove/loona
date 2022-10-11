#![feature(test)]
extern crate test;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

static INPUT: &[u8] = b"This is some sample data";

fn bench_bufpool(c: &mut Criterion) {
    c.bench_function("alloc bufpool", |b| {
        b.iter(|| {
            let mut buf = alt_http::bufpool::Buf::alloc().unwrap();
            buf[..INPUT.len()].copy_from_slice(INPUT);
            assert_eq!(&buf[..INPUT.len()], INPUT);
            black_box(&buf[..]);
        })
    });
    c.bench_function("alloc vec", |b| {
        b.iter(|| {
            let mut buf: Vec<u8> = Vec::with_capacity(1024);
            buf.resize(INPUT.len(), 0);
            buf[..INPUT.len()].copy_from_slice(INPUT);
            assert_eq!(&buf[..INPUT.len()], INPUT);
            black_box(&buf[..]);
        })
    });
}

criterion_group!(benches, bench_bufpool);
criterion_main!(benches);
