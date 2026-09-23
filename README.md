[![CI](https://github.com/AxellGS/nure-queue/actions/workflows/ci.yml/badge.svg)](https://github.com/AxellGS/nure-queue/actions)

# nure-queue

`no_std`, allocator-free, lock-free bounded MPMC queue (Vyukov) with `const` construction for static storage.

`heapless::mpmc` is deprecated (not truly lock-free — [heapless#583](https://github.com/rust-embedded/heapless/issues/583)). Crossbeam's `ArrayQueue` is correct but heap-allocates on construction. `embassy-sync`'s channel is static but mutex-based. This crate is the intersection: static + truly lock-free.

## Use

```rust
use nure_queue::Queue;

static Q: Queue<u32, 8> = Queue::new();

Q.enqueue(1).unwrap();
assert_eq!(Q.dequeue(), Some(1));
```

API: `new` (`const` on the default build), `push`/`pop`, `enqueue`/`dequeue` (heapless-compatible aliases), `force_push` (ring overwrite), `len`/`is_empty`/`is_full`/`capacity`. Optional `std` feature adds thread yield to the backoff (pure spin on `no_std`); optional `loom` feature swaps atomics for model checking.

## Verification

- `tests/smoke.rs`: FIFO, full/empty, ring mode, `static` construction, drop accounting, 100k-op contention stress, 2-thread sum conservation.
- `tests/repro583.rs`: exact #583 reproducers in both directions (ignored by default; run in release with `-- --ignored`).
- `tests/differential.rs`: proptest model check vs `VecDeque` across capacities 1/3/16 plus drop accounting.
- `tests/loom.rs`: sequential model check under loom (`--features loom`). Full MPMC interleavings exceed loom's branch budget for any CAS-retry queue (same for crossbeam); contention is covered by stress + miri instead.
- miri: `cargo miri test --test smoke` (the 100k stress is gated out under miri; contention coverage comes from the sum test).
- libFuzzer differential harness in `fuzz/` against a `VecDeque` model.
- MSRV 1.85 (`rust-version` declared and tested).

## Benchmarks

100k `u64` messages, capacity 1024, Criterion, release, Linux numbers (Windows runs consistent within run-to-run noise).

| bench | nure-queue | crossbeam-queue | lfqueue | Mutex<VecDeque> |
|---|---|---|---|---|
| spsc FIFO | 1.334 ms · 75.0 Melem/s | 1.308 ms · 76.5 Melem/s | 7.930 ms · 12.6 Melem/s | 8.676 ms · 11.5 Melem/s |
| mpmc 4p4c | 11.382 ms · 8.79 Melem/s | 13.631 ms · 7.34 Melem/s | 10.666 ms · 9.38 Melem/s | 17.821 ms · 5.61 Melem/s |

Notes: SPSC is parity within noise (same algorithm). In MPMC, SCQ (`lfqueue`) scales better under contention; both Vyukov queues beat the mutex baseline clearly. `lfqueue` 0.5.1/0.8.1 does not compile on current toolchains (dead const-generic alias); benches use a vendored copy with a 1-line compile fix under `third-party/`, wired via `[patch.crates-io]` — algorithm code untouched.

## Check

```sh
cargo test
cargo test --features loom --test loom
```

Extended: `cargo miri test --test smoke`, `cargo bench`, and from `fuzz/`: `cargo fuzz run queue_differential -- -max_total_time=600` (needs nightly + libFuzzer toolchain).

## License

MIT OR Apache-2.0 (see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE))
