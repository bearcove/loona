# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/bearcove/loona/compare/buffet-v0.3.1...buffet-v0.3.2) - 2024-12-03

### Other

- updated the following local packages: b-x

## [0.3.1](https://github.com/bearcove/loona/compare/buffet-v0.3.0...buffet-v0.3.1) - 2024-09-05

### Other
- Remove needless timers
- allocate vec with the exact capacity
- Filter syscsalls better, introduce record mode for perfstat, remove misleading comment
- both hyper and loona httpwg clis use the harness now
- comment out writev_owned
- hacky writev implementation
- ignore some failures
- debug things
- well that's super weird
- Allow customizing number of buffers
- typo

## [0.3.0](https://github.com/bearcove/loona/compare/buffet-v0.2.1...buffet-v0.3.0) - 2024-08-21

### Added
- Run miri in CI

### Other
- 4
- 9 errors left
- more error-enum-ifiying
- Get rid of color-eyre dependency
- Fix httpwg-over-tcp

## [0.2.1](https://github.com/bearcove/loona/compare/buffet-v0.2.0...buffet-v0.2.1) - 2024-08-14

### Fixed
- Fix rustdoc errors
- Fix tls example

### Other
- Avoid unused warning

## [0.2.0](https://github.com/bearcove/fluke/compare/fluke-buffet-v0.1.0...fluke-buffet-v0.2.0) - 2024-05-27

### Added
- Upgrade dependencies

### Other
- Fix tests for Linux
- Make rfc9113 pass, still need to check codes
- progress
- Flesh out the handshake test
- Migrate more tests to pipe, deprecate ChanRead/ChanWrite
- Migrate ChanRead uses over to pipe
- Adjust shutdown implementation for TcpWriteHalf (io-uring codepath)
- Port test to ChanRead
- More assertions
- Finish up pipe implementation
- Simplify WriteOwned trait, introduce pipe
- Cancel listen
- Some cancellation?
- Remove listen cancellation
- Remove WrappedOp
- wip cancellation
- Only import io-uring on Linux, try closing fds on TcpStream drop
- make io-uring dep linux-only
- All tests pass!
- Some tests are starting to _almost_ pass with io-uring-async
- flesh out TcpListener API
- We can successfully accept a connection!
- A little print debugging
- Use nix for errno stuff
- Vendor io-uring-async and upgrade it to io-uring 0.6.x
- io-uring-async (cf. [#154](https://github.com/bearcove/fluke/pull/154))
- TcpWriteHalf fixes
- Fix buffer bug
- Remove BufOrSlice, closes [#153](https://github.com/bearcove/fluke/pull/153)
- Bring 'fluke-maybe-uring' back into 'fluke-buffet'
- Fix linux build
- Headers flow control wip
- wip header flow control
- wip Piece
- Finish propagating Piece/PieceCore split changes
- Piece::Slice
- drat, partial writes.
- hapsoc => fluke
- Fix http2/4.2/1
- Fix h2spec http2/6.5/3
- mh
- no continuation state
- Upgrade more deps
- Upgrade memmap2
- Upgrade pretty-hex
- Upgrade deps
- Switch to Rust stable, closes [#128](https://github.com/bearcove/fluke/pull/128)
- Bump some dependencies
- Bump dependencies
- Opt out of async_fn_in_trait warnings
- release

## [0.1.0](https://github.com/bearcove/fluke/releases/tag/fluke-buffet-v0.1.0) - 2023-10-03

### Other

- Add more READMEs
- Rebrand to fluke
