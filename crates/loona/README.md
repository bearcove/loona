[![test pipeline](https://github.com/bearcove/loona/actions/workflows/test.yml/badge.svg)](https://github.com/bearcove/loona/actions/workflows/test.yml?query=branch%3Amain)
[![Coverage Status (codecov.io)](https://codecov.io/gh/bearcove/loona/branch/main/graph/badge.svg)](https://codecov.io/gh/bearcove/loona/)
[![MIT OR Apache-2.0 licensed](https://img.shields.io/badge/license-MIT+Apache_2.0-blue.svg)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/loona)](https://crates.io/crates/loona)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/bearcove/loona)

# loona

![The loona logo: a lunatic moon looking threatening and like it drank a beer it wasn't supposed to. Also pimples.](https://github.com/user-attachments/assets/409d548c-d642-4160-b529-5959a851d6b3)

_Logo by [MisiasArt](https://misiasart.carrd.co)_

loona is an HTTP/1.1 and HTTP/2 implementation on top of Rust, using io_uring on Linux.

It is focused on correctness and performance.

At this stage, loona is still a research project, but you can check out the
rest of the loona cinematic universe:

  * [buffet](https://crates.io/crates/buffet), loona's buffering library
  * [luring](https://crates.io/crates/luring), loona's io_uring abstraction on top of tokio
  * [httpwg](https://crates.io/crates/httpwg), a Rust port of h2spec
  * [loona-h2](https://crates.io/crates/loona-h2), parsers for HTTP/2 frames
  * [loona-hpack](https://crates.io/crates/loona-hpack), HPACK decoder

## Example usage

To see how loona can be used to make HTTP/1 and HTTP/2 servers, you can check out:

  * [httpwg-loona](../httpwg-loona/README.md)

loona also integrates well with ktls, you can check the [tls example](./examples/tls) (Linux-only).
