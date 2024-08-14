# luring

luring is originally a fork of <https://github.com/thomasbarrett/io-uring-async>

It's the io_uring engine for [loona](https://github.com/bearcove/loona)

The original README follows.

# IoUringAsync

IoUringAsync is a thin async compatability layer over the commonly used
[io-uring](https://github.com/tokio-rs/io-uring) library that makes it
easier to use with the [Tokio](https://github.com/tokio-rs/tokio) runtime.

## Similar Projects
This project is heavily inspired by the [tokio-uring](https://github.com/tokio-rs/tokio-uring)
project. Unlike tokio-uring, IoUringAsync is not its own runtime. Instead, it
is a lightweight collection of mostly runtime-agnostic future.

## Limitations
Currently, this project does not support multishot `io_uring` operations.

## Complete Control over SQE submission.
```rust
let sqe = opcode::Write(...).build();
let fut = uring.push(sqe);
uring.submit();
let cqe = fut.await;
```

## Example

```rust
use std::rc::Rc;
use io_uring::{squeue, cqueue, opcode};
use io_uring_async::IoUringAsync;
use send_wrapper::SendWrapper;

fn main() {
    let uring = IoUringAsync::new(8).unwrap();
    let uring = Rc::new(uring);

    // Create a new current_thread runtime that submits all outstanding submission queue
    // entries as soon as the executor goes idle.
    let uring_clone = SendWrapper::new(uring.clone());
    let runtime = tokio::runtime::Builder::new_current_thread().
        on_thread_park(move || { uring_clone.submit().unwrap(); }).
        enable_all().
        build().unwrap();

    runtime.block_on(async move {
        tokio::task::LocalSet::new().run_until(async {
            // Spawn a task that waits for the io_uring to become readable and handles completion
            // queue entries accordingly.
            tokio::task::spawn_local(IoUringAsync::listen(uring.clone()));

            let cqe = uring.push(Nop::new().build()).await;
            assert!(cqe.result() >= 0, "nop error: {}", cqe.result());
        }).await;
    });
}
```
