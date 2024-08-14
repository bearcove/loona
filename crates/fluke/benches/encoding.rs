use std::{
    cell::{RefCell, UnsafeCell},
    collections::VecDeque,
    mem::MaybeUninit,
};

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

    c.bench_function("format_content_length/std_fmt/heap", |b| {
        b.iter_batched(
            || {
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), results)
            },
            |(lengths, mut results)| {
                for length in lengths {
                    let vec = length.to_string();
                    let piece: Piece = vec.into_bytes().into();
                    results.push(piece);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/std_fmt/buffet", |b| {
        b.iter_batched(
            || {
                fluke_buffet::bufpool::initialize_allocator_with_num_bufs(512 * 1024).unwrap();
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), RollMut::alloc().unwrap(), results)
            },
            |(lengths, mut roll, mut results)| {
                for length in lengths {
                    // that's the max length of an u64 formatted as decimal
                    roll.reserve_at_least(20).unwrap();

                    use std::io::Write;
                    std::write!(&mut roll, "{}", length).unwrap();

                    let piece: Piece = roll.take_all().into();
                    results.push(piece);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/itoa/heap", |b| {
        b.iter_batched(
            || {
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), results)
            },
            |(lengths, mut results)| {
                for length in &lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(*length).to_owned();
                    let piece: Piece = s.into_bytes().into();
                    results.push(piece);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/itoa/buffet", |b| {
        b.iter_batched(
            || {
                fluke_buffet::bufpool::initialize_allocator_with_num_bufs(512 * 1024).unwrap();
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), RollMut::alloc().unwrap(), results)
            },
            |(lengths, mut roll, mut results)| {
                for length in lengths {
                    use itoa::Buffer;
                    let mut buffer = Buffer::new();
                    let s = buffer.format(length);
                    // that's the max length of an u64 formatted as decimal
                    roll.reserve_at_least(20).unwrap();
                    roll.put(s.as_bytes()).unwrap();
                    let piece: Piece = roll.take_all().into();
                    results.push(piece);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/itoa/pool", |b| {
        const NUM_BUFFERS: usize = 16 * 1024;

        std::thread_local! {
            static BUFFER_POOL: [UnsafeCell<itoa::Buffer>; NUM_BUFFERS] = const { unsafe { MaybeUninit::uninit().assume_init() } };
            static BUFFER_POOL_FREE_LIST: RefCell<VecDeque<usize>> = RefCell::new(VecDeque::from_iter(0..NUM_BUFFERS));
        }

        struct GuardedBuffer {
            index: usize,
            buffer: *const UnsafeCell<itoa::Buffer>,
        }

        fn pop_buffer() -> Option<GuardedBuffer> {
            BUFFER_POOL_FREE_LIST.with(|fl| {
                let mut fl = fl.borrow_mut();
                fl.pop_front().map(|i| {
                    let buffer = unsafe { BUFFER_POOL.with(|p|
                        p.get_unchecked(i) as *const UnsafeCell<itoa::Buffer>
                    ) };
                    GuardedBuffer { index: i, buffer }
                })
            })
        }

        impl Drop for GuardedBuffer {
            fn drop(&mut self) {
                BUFFER_POOL_FREE_LIST.with(|fl| {
                    let mut fl = fl.borrow_mut();
                    fl.push_front(self.index);
                });
            }
        }

        b.iter_batched(
            || {
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), results)
            },
            |(lengths, mut results)| {
                for length in lengths {
                    // grab a buffer from the pool
                    let gb = pop_buffer().unwrap();
                    {
                        let buf = unsafe { (*gb.buffer).get().as_mut().unwrap() };
                        buf.format(length);
                    }
                    results.push(gb);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.bench_function("format_content_length/lut", |b| {
        type BufferType = Vec<u8>;
        type OffsetType = (u32, u32);
        type FormattedNumbersType = (BufferType, Vec<OffsetType>);

        static FORMATTED_NUMBERS: std::sync::LazyLock<FormattedNumbersType> =
            std::sync::LazyLock::new(|| {
                let mut buffer = BufferType::new();
                let mut offsets = Vec::new();
                for i in 0..=1024 * 1024 {
                    let start = buffer.len();
                    use std::io::Write;
                    write!(&mut buffer, "{}", i).unwrap();
                    let end = buffer.len();
                    offsets.push((start as u32, (end - start) as u32));
                }
                (buffer, offsets)
            });

        b.iter_batched(
            || {
                let results = Vec::with_capacity(content_lengths.len());
                (content_lengths.clone(), results)
            },
            |(lengths, mut results)| {
                let (buffer, offsets) = &*FORMATTED_NUMBERS;
                for length in lengths {
                    let (offset, len) = offsets[length as usize];
                    let slice = &buffer[offset as usize..(offset + len) as usize];
                    let piece: Piece = slice.into();
                    results.push(piece);
                }
                black_box(results);
            },
            codspeed_criterion_compat::BatchSize::SmallInput,
        )
    });

    c.finish()
}

criterion_group!(benches, format_status_code, format_content_length);
criterion_main!(benches);
