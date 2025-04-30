# XMrs File format library

A no_std library to edit Sound Tracker data with pleasure.

Because "Representation is the Essence of Programming".

Suppported files:

- IT **Impulse Tracker**
- MOD **Amiga Modules**
- S3M **Scream Tracker III**
- SID **Rob Hubbard C64 files** (WIP).
- XM **FastTracker II**

To edit data, use `Module` struct.

You can serialize `Module` using serde or bincode2 (see `std` feature).

## How to load historical tracker files

Test with `cargo run --no-default-features --features=demo --example xmrs -- --help`, then read 50 lines `examples/xmrs` example.

## About no_std

[micromath](https://crates.io/crates/micromath) is used by default in [no_std](https://docs.rust-embedded.org/book/intro/no-std.html). If you prefer [libm](https://crates.io/crates/libm), use `cargo build --no-default-features --features=libm --release`.

If you want to use import, add `--features=import`.

If you want to optimize for memory or space in the embedded context, don't use the import feature, but prefer to prepare a serialized version with [bincode](https://crates.io/crates/bincode) with `alloc` feature and possibly [flate2-rs](https://crates.io/crates/flate2).

## About std

If you want to use [std](https://doc.rust-lang.org/std/) feature use `cargo build --no-default-features --features=std --release`

If you'd like to post-process in another language (but why use another language when you're lucky enough to have [rust]()!?) don't forget that, after loading, you can serialize in [json](https://crates.io/crates/serde_json) with [serde](https://crates.io/crates/serde).
