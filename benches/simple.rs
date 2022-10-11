#![feature(test)]
extern crate test;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

static INPUT: &[u8] = b"This is some sample data";

fn bench_bufpool(c: &mut Criterion) {
    {
        let mut g = c.benchmark_group("alloc-one");

        g.bench_function("stack", |b| {
            b.iter(|| {
                let mut buf = [0u8; 4096];
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            })
        });
        g.bench_function("pool", |b| {
            b.iter(|| {
                let mut buf = alt_http::bufpool::Buf::alloc().unwrap();
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            })
        });
        g.bench_function("vec-zeroed", |b| {
            b.iter(|| {
                let mut buf: Vec<u8> = Vec::with_capacity(4096);
                buf.resize(INPUT.len(), 0);
                buf[..INPUT.len()].copy_from_slice(INPUT);
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            })
        });
        g.bench_function("vec-uninit", |b| {
            b.iter(|| {
                let mut buf: Vec<u8> = Vec::with_capacity(4096);
                let slice = buf.spare_capacity_mut();
                for i in 0..INPUT.len() {
                    slice[i].write(INPUT[i]);
                }
                unsafe {
                    buf.set_len(INPUT.len());
                }
                assert_eq!(&buf[..INPUT.len()], INPUT);
                black_box(&buf[..]);
            })
        });
    }

    {
        let mut g = c.benchmark_group("alloc-many");

        for count in [256, 512, 1024, 2048, 4096] {
            g.bench_with_input(BenchmarkId::new("pool", count), &count, |b, &s| {
                b.iter(|| {
                    let mut bufs = Vec::with_capacity(s);
                    for _ in 0..s {
                        let mut buf = alt_http::bufpool::Buf::alloc().unwrap();
                        buf[..INPUT.len()].copy_from_slice(INPUT);
                        assert_eq!(&buf[..INPUT.len()], INPUT);
                        bufs.push(buf);
                    }
                    black_box(bufs);
                })
            });
            g.bench_with_input(BenchmarkId::new("vec", count), &count, |b, &s| {
                b.iter(|| {
                    let mut bufs = Vec::with_capacity(s);
                    for _ in 0..s {
                        let mut buf: Vec<u8> = Vec::with_capacity(4096);
                        buf.resize(INPUT.len(), 0);
                        buf[..INPUT.len()].copy_from_slice(INPUT);
                        assert_eq!(&buf[..INPUT.len()], INPUT);
                        bufs.push(buf);
                    }
                    black_box(bufs);
                })
            });
        }
    }
}

criterion_group!(benches, bench_bufpool);
criterion_main!(benches);
