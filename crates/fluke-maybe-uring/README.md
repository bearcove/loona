# fluke-maybe-uring

fluke uses [tokio-uring](https://crates.io/crates/tokio-uring), but it also
tries to function on platforms where io_uring is not available: this crate
abstracts over "classic tokio" I/O types and tokio-uring types.
