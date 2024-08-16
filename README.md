[![test pipeline](https://github.com/bearcove/loona/actions/workflows/test.yml/badge.svg)](https://github.com/bearcove/loona/actions/workflows/test.yml?query=branch%3Amain)
[![Coverage Status (codecov.io)](https://codecov.io/gh/bearcove/loona/branch/main/graph/badge.svg)](https://codecov.io/gh/bearcove/loona/)
[![MIT OR Apache-2.0 licensed](https://img.shields.io/badge/license-MIT+Apache_2.0-blue.svg)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/loona)](https://crates.io/crates/loona)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/bearcove/loona)

# loona

![The loona logo: a lunatic moon looking threatening and like it drank a beer it wasn't supposed to. Also pimples.](https://private-user-images.githubusercontent.com/7998310/358643098-409d548c-d642-4160-b529-5959a851d6b3.png?jwt=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJnaXRodWIuY29tIiwiYXVkIjoicmF3LmdpdGh1YnVzZXJjb250ZW50LmNvbSIsImtleSI6ImtleTUiLCJleHAiOjE3MjM4MjQ1MjUsIm5iZiI6MTcyMzgyNDIyNSwicGF0aCI6Ii83OTk4MzEwLzM1ODY0MzA5OC00MDlkNTQ4Yy1kNjQyLTQxNjAtYjUyOS01OTU5YTg1MWQ2YjMucG5nP1gtQW16LUFsZ29yaXRobT1BV1M0LUhNQUMtU0hBMjU2JlgtQW16LUNyZWRlbnRpYWw9QUtJQVZDT0RZTFNBNTNQUUs0WkElMkYyMDI0MDgxNiUyRnVzLWVhc3QtMSUyRnMzJTJGYXdzNF9yZXF1ZXN0JlgtQW16LURhdGU9MjAyNDA4MTZUMTYwMzQ1WiZYLUFtei1FeHBpcmVzPTMwMCZYLUFtei1TaWduYXR1cmU9NzA0ZjhkMTBjY2E5MmQ4MzNmZTUwMTRiNzljOWYzZDgzZDU5Y2RkOTE0ODA0ZGQ5NTY3YjI3NTY4YTI2NTkxOSZYLUFtei1TaWduZWRIZWFkZXJzPWhvc3QmYWN0b3JfaWQ9MCZrZXlfaWQ9MCZyZXBvX2lkPTAifQ.owizDuUCFNblhfVStHoLmz27zE5mcOIOQa1w8w8OwzU)

_Logo by [MisiasArt](https://www.deviantart.com/misiasart)_

An experimental, HTTP/1.1 and HTTP/2 implementation in Rust on top of io-uring.

This repository serves as a hope for several important projects:

  * [loona](crates/loona/README.md) itself
  * [buffet](crates/buffet/README.md), its buffer management library
  * [luring](crates/buffet/README.md), its io_uring abstraction on top of tokio
  * [httpwg](crates/httpwg/README.md), an HTTP conformance suite (replacing h2spec)

### Funding

Thanks to Namespace for providing fast GitHub Actions workers:

<a href="https://namespace.so"><img src="./static/namespace-d.svg" height="40"></a>

Thanks to all my <a href="https://fasterthanli.me/donate">individual sponsors</a>.

Thanks to Shopify and fly for their past funding:

<a href="https://shopify.github.io/"><img src="./static/shopify-d.svg" height="40"></a>
<a href="https://fly.io/docs/about/open-source/"><img src="./static/flyio-d.svg" height="40"></a>

## License

This project is primarily distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.
