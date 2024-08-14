use codspeed_criterion_compat::{black_box, criterion_group, criterion_main, Criterion};
use fluke_buffet::{Piece, RollMut};
use http::StatusCode;

pub fn format_status_code(c: &mut Criterion) {
    let status_codes = (100..=999)
        .map(|code| StatusCode::from_u16(code).unwrap())
        .collect::<Vec<StatusCode>>();

    let mut c = c.benchmark_group("format_status_code");

    c.bench_function("format_status_code/std_fmt", |b| {
        b.iter_batched(
            || status_codes.clone(),
            |codes| {
                for code in &codes {
                    black_box(code.as_u16().to_string());
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_status_code/itoa/heap", |b| {
        b.iter_batched(
            || status_codes.clone(),
            |codes| {
                for code in &codes {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    black_box(buffer.format(code.as_u16()).to_string());
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_status_code/itoa/stack", |b| {
        b.iter_batched(
            || status_codes.clone(),
            |codes| {
                for code in &codes {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(code.as_u16());
                    // The `s` is borrowed from `buffer`, which is stack-allocated.
                    black_box(s);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_status_code/lookup_table", |b| {
        b.iter_batched(
            || status_codes.clone(),
            |codes| {
                for code in &codes {
                    let code = black_box(*code);
                    black_box(fluke::h1::encode::encode_status_code(code));
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.finish()
}

pub fn format_content_length(c: &mut Criterion) {
    let content_lengths = (0..=1024)
        .chain((1024..=16384).step_by(127))
        .chain((16384..=1024 * 1024).step_by(1023))
        .collect::<Vec<u64>>();
    assert_eq!(content_lengths.len(), 2155);

    let mut c = c.benchmark_group("format_content_length");

    c.bench_function("format_content_length/itoa/buffet", |b| {
        b.iter_batched(
            || {
                fluke_buffet::bufpool::initialize_allocator_with_num_bufs(512 * 1024).unwrap();
                (content_lengths.clone(), RollMut::alloc().unwrap())
            },
            |(lengths, mut roll)| {
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length);
                    // that's the max length of an u64 formatted as decimal
                    roll.reserve_at_least(20).unwrap();
                    roll.put(s.as_bytes()).unwrap();
                    let piece: Piece = roll.take_all().into();
                    black_box(piece);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/itoa/heap", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length).to_owned();
                    let piece: Piece = s.into_bytes().into();
                    black_box(piece);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    // Note: we cannot use this variant as-is, because it's not pinned, so we can't
    // pass it to the kernel for writes. We _could_ have a pool of those Buffers
    // I suppose? And return them to the pool when done.
    c.bench_function("format_content_length/itoa/stack", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length);
                    // the `s` is borrowed from `buffer`, which is stack-allocated.
                    black_box(s);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/std_fmt/heap", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                for length in &lengths {
                    let vec = length.to_string();
                    let piece: Piece = vec.into_bytes().into();
                    black_box(piece);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/std_fmt/buffet", |b| {
        b.iter_batched(
            || {
                fluke_buffet::bufpool::initialize_allocator_with_num_bufs(512 * 1024).unwrap();
                (content_lengths.clone(), RollMut::alloc().unwrap())
            },
            |(lengths, mut roll)| {
                for length in &lengths {
                    // that's the max length of an u64 formatted as decimal
                    roll.reserve_at_least(20).unwrap();

                    use std::io::Write;
                    std::write!(&mut roll, "{}", length).unwrap();

                    let piece: Piece = roll.take_all().into();
                    black_box(piece);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, format_status_code, format_content_length);
criterion_main!(benches);
