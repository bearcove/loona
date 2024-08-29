use codspeed_criterion_compat::{criterion_group, criterion_main, Criterion};
use httpwg_loona::{Mode, Proto};

pub fn h2load(c: &mut Criterion) {
    c.bench_function("h2load", |b| {
        b.iter_with_setup(
            || {},
            |()| {
                buffet::bufpool::initialize_allocator_with_num_bufs(64 * 1024).unwrap();
                httpwg_loona::do_main("127.0.0.1".to_string(), 0, Proto::H2, Mode::H2Load);
            },
        )
    });
}

criterion_group!(benches, h2load);
criterion_main!(benches);
