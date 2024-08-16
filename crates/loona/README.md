[![test pipeline](https://github.com/bearcove/loona/actions/workflows/test.yml/badge.svg)](https://github.com/bearcove/loona/actions/workflows/test.yml?query=branch%3Amain)
[![Coverage Status (codecov.io)](https://codecov.io/gh/bearcove/loona/branch/main/graph/badge.svg)](https://codecov.io/gh/bearcove/loona/)
[![MIT OR Apache-2.0 licensed](https://img.shields.io/badge/license-MIT+Apache_2.0-blue.svg)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/loona)](https://crates.io/crates/loona)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/bearcove/loona)

# loona

![The loona logo: a lunatic moon looking threatening and like it drank a beer it wasn't supposed to. Also pimples.](https://private-user-images.githubusercontent.com/7998310/358643098-409d548c-d642-4160-b529-5959a851d6b3.png?jwt=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJnaXRodWIuY29tIiwiYXVkIjoicmF3LmdpdGh1YnVzZXJjb250ZW50LmNvbSIsImtleSI6ImtleTUiLCJleHAiOjE3MjM4MjQ1MjUsIm5iZiI6MTcyMzgyNDIyNSwicGF0aCI6Ii83OTk4MzEwLzM1ODY0MzA5OC00MDlkNTQ4Yy1kNjQyLTQxNjAtYjUyOS01OTU5YTg1MWQ2YjMucG5nP1gtQW16LUFsZ29yaXRobT1BV1M0LUhNQUMtU0hBMjU2JlgtQW16LUNyZWRlbnRpYWw9QUtJQVZDT0RZTFNBNTNQUUs0WkElMkYyMDI0MDgxNiUyRnVzLWVhc3QtMSUyRnMzJTJGYXdzNF9yZXF1ZXN0JlgtQW16LURhdGU9MjAyNDA4MTZUMTYwMzQ1WiZYLUFtei1FeHBpcmVzPTMwMCZYLUFtei1TaWduYXR1cmU9NzA0ZjhkMTBjY2E5MmQ4MzNmZTUwMTRiNzljOWYzZDgzZDU5Y2RkOTE0ODA0ZGQ5NTY3YjI3NTY4YTI2NTkxOSZYLUFtei1TaWduZWRIZWFkZXJzPWhvc3QmYWN0b3JfaWQ9MCZrZXlfaWQ9MCZyZXBvX2lkPTAifQ.owizDuUCFNblhfVStHoLmz27zE5mcOIOQa1w8w8OwzU)

_Logo by [MisiasArt](https://www.deviantart.com/misiasart)_

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
