//! Exact reproduction of [`heapless#583`] against `nure-queue`.
//!
//! Original repro: 2 threads x 1M ops `dequeue`->`enqueue` on a full
//! `Queue<u8, 4>`. `heapless::mpmc` panics (`enqueue` fails on a
//! non-full queue under contention); `nure-queue` must not.
//! Ignored by default (takes seconds in debug); run explicitly:
//!
//! ```powershell
//! cargo test --release --test repro583 -- --ignored
//! ```
//!
//! [`heapless#583`]: https://github.com/rust-embedded/heapless/issues/583

use nure_queue::Queue;

/// Destructive drain for failure diagnostics only (mirrors the
/// original repro's `to_vec`).
fn snapshot(q: &Queue<u8, 4>) -> Vec<u8> {
    let mut ret = Vec::new();
    while let Some(v) = q.dequeue() {
        ret.push(v);
    }
    ret
}

#[test]
#[ignore]
fn repro_583_exact_one_million_ops() {
    const N: usize = 4;
    let q0 = Queue::<u8, N>::new();
    for i in 0..N {
        q0.enqueue(i as u8).expect("initial fill");
    }

    std::thread::scope(|sc| {
        for _ in 0..2 {
            sc.spawn(|| {
                for k in 0..1_000_000 {
                    if let Some(v) = q0.dequeue() {
                        q0.enqueue(v).unwrap_or_else(|v| {
                            panic!("{}: q0 -> q0: {}, {:?}", k, v, snapshot(&q0))
                        });
                    }
                }
            });
        }
    });
}

/// Second half of #583 (posted as `issue_583_dequeue`): enqueue-then-
/// dequeue must never observe an empty queue. By counting (every dequeue
/// is preceded by its own enqueue, max 2 items in flight < N), a `None`
/// here would be a linearizability violation, not just a panic.
#[test]
#[ignore]
fn repro_583_dequeue_direction() {
    const N: usize = 4;
    let q0 = Queue::<u8, N>::new();
    std::thread::scope(|sc| {
        for _ in 0..2 {
            sc.spawn(|| {
                for k in 0..1_000_000u32 {
                    q0.enqueue(k as u8).unwrap();
                    if q0.dequeue().is_none() {
                        panic!("{k}: dequeue returned None right after enqueue");
                    }
                }
            });
        }
    });
}
