use codspeed_criterion_compat::{black_box, criterion_group, criterion_main, Criterion};
use fluke_buffet::RollMut;
use http::StatusCode;

pub fn format_status_code(c: &mut Criterion) {
    let status_codes = (100..=999)
        .map(|code| StatusCode::from_u16(code).unwrap())
        .collect::<Vec<StatusCode>>();

    let mut c = c.benchmark_group("format_status_code");

    c.bench_function("std::fmt", |b| {
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

    c.bench_function("itoa", |b| {
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

    c.bench_function("lookup_table", |b| {
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
    let content_lengths = (0..=16384).collect::<Vec<u64>>();

    let mut c = c.benchmark_group("format_content_length");

    c.bench_function("std::fmt", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                for length in &lengths {
                    black_box(length.to_string());
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("itoa (alloc)", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length);
                    black_box(s.to_string());
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("itoa (buffet)", |b| {
        b.iter_batched(
            || content_lengths.clone(),
            |lengths| {
                let mut roll = RollMut::alloc().unwrap();
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length);
                    let content_len = roll
                        .put_to_roll(s.len(), |slice| {
                            slice.copy_from_slice(s.as_bytes());
                            Ok(())
                        })
                        .unwrap();
                    black_box(content_len);
                }
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, format_status_code, format_content_length);
criterion_main!(benches);