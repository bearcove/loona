use codspeed_criterion_compat::{black_box, criterion_group, criterion_main, Criterion};
use httpwg_loona::{Proto, Ready};

pub fn h2load(c: &mut Criterion) {
    c.bench_function("h2load", |b| {
        b.iter(|| {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Ready>();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

            let server_thread = std::thread::spawn(|| {
                httpwg_loona::do_main(ready_tx, cancel_rx, 0, Proto::H2);
            });

            let ready = ready_rx.blocking_recv().unwrap();

            let mut child = std::process::Command::new("h2load")
                .arg("-n")
                .arg("100")
                .arg("-c")
                .arg("10")
                .arg(format!("http://127.0.0.1:{}", ready.port))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap();
            child.wait().unwrap();

            cancel_tx.send(()).unwrap();

            server_thread.join().unwrap();
        })
    });
}

criterion_group!(benches, h2load);
criterion_main!(benches);
