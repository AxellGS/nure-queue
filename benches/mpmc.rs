//! Throughput duel (100k `u64` messages, capacity 1024):
//!
//! - `nure-queue` (Vyukov, `no_std`, static storage)
//! - `crossbeam-queue` `ArrayQueue` (Vyukov, heap on construction)
//! - `lfqueue` `AllocBoundedQueue` (SCQ paper, heap segments;
//!   vendored via `[patch]` + 1-line compile fix, zero behavior change)
//! - `Mutex<VecDeque>` (naive baseline)
//!
//! All backends are steady-state allocation-free; one-time construction
//! cost is outside the measured loop. Queues drain fully every iteration,
//! so instances are safely reused across iterations.
//!
//! ```powershell
//! cargo bench
//! ```

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::collections::VecDeque;
use std::sync::{
    Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crossbeam_queue::ArrayQueue;
use lfqueue::AllocBoundedQueue;
use nure_queue::Queue;

const CAP: usize = 1024;
const TOTAL: usize = 100_000;

static NURE_Q: Queue<u64, CAP> = Queue::new();

trait MpmcQueue: Sync {
    fn push(&self, v: u64) -> bool;
    fn pop(&self) -> Option<u64>;
}

struct Nure;
impl MpmcQueue for Nure {
    fn push(&self, v: u64) -> bool {
        NURE_Q.enqueue(v).is_ok()
    }
    fn pop(&self) -> Option<u64> {
        NURE_Q.dequeue()
    }
}

struct Cb(ArrayQueue<u64>);
impl MpmcQueue for Cb {
    fn push(&self, v: u64) -> bool {
        self.0.push(v).is_ok()
    }
    fn pop(&self) -> Option<u64> {
        self.0.pop()
    }
}

struct Lf(AllocBoundedQueue<u64>);
impl MpmcQueue for Lf {
    fn push(&self, v: u64) -> bool {
        self.0.enqueue(v).is_ok()
    }
    fn pop(&self) -> Option<u64> {
        self.0.dequeue()
    }
}

struct Mtx(Mutex<VecDeque<u64>>);
impl MpmcQueue for Mtx {
    fn push(&self, v: u64) -> bool {
        let mut g = self.0.lock().unwrap();
        if g.len() < CAP {
            g.push_back(v);
            true
        } else {
            false
        }
    }
    fn pop(&self) -> Option<u64> {
        self.0.lock().unwrap().pop_front()
    }
}

/// 1 producer + 1 consumer, FIFO order asserted on the consumer side.
fn run_spsc<Q: MpmcQueue>(q: &Q) {
    let barrier = Barrier::new(2);
    std::thread::scope(|s| {
        let b = &barrier;
        s.spawn(move || {
            b.wait();
            for i in 0..TOTAL as u64 {
                while !q.push(i) {}
            }
        });
        s.spawn(move || {
            b.wait();
            for expected in 0..TOTAL as u64 {
                loop {
                    if let Some(v) = q.pop() {
                        assert_eq!(v, expected);
                        break;
                    }
                }
            }
        });
    });
}

/// 4 producers + 4 consumers, disjoint value ranges, exact-count exit.
fn run_mpmc<Q: MpmcQueue>(q: &Q) {
    const P: usize = 4;
    const PER: usize = TOTAL / P;
    let consumed = AtomicUsize::new(0);
    let barrier = Barrier::new(P * 2);
    std::thread::scope(|s| {
        for p in 0..P {
            let b = &barrier;
            let c = &consumed;
            s.spawn(move || {
                b.wait();
                let base = (p * PER) as u64;
                for i in 0..PER as u64 {
                    while !q.push(base + i) {}
                }
            });
            s.spawn(move || {
                b.wait();
                loop {
                    if q.pop().is_some() {
                        if c.fetch_add(1, Ordering::Relaxed) + 1 >= TOTAL {
                            break;
                        }
                    } else if c.load(Ordering::Relaxed) >= TOTAL {
                        break;
                    }
                }
            });
        }
    });
    assert_eq!(consumed.load(Ordering::SeqCst), TOTAL);
}

fn bench_queues(c: &mut Criterion) {
    let mut spsc = c.benchmark_group("spsc_100k_fifo");
    spsc.throughput(Throughput::Elements(TOTAL as u64));
    spsc.bench_function("nure", |b| b.iter(|| run_spsc(&Nure)));
    spsc.bench_function("crossbeam", |b| {
        let q = Cb(ArrayQueue::new(CAP));
        b.iter(|| run_spsc(&q))
    });
    spsc.bench_function("lfqueue", |b| {
        let q = Lf(AllocBoundedQueue::new(CAP));
        b.iter(|| run_spsc(&q))
    });
    spsc.bench_function("mutex_vecdeque", |b| {
        let q = Mtx(Mutex::new(VecDeque::with_capacity(CAP)));
        b.iter(|| run_spsc(&q))
    });
    spsc.finish();

    let mut mpmc = c.benchmark_group("mpmc_4p4c_100k");
    mpmc.throughput(Throughput::Elements(TOTAL as u64));
    mpmc.bench_function("nure", |b| b.iter(|| run_mpmc(&Nure)));
    mpmc.bench_function("crossbeam", |b| {
        let q = Cb(ArrayQueue::new(CAP));
        b.iter(|| run_mpmc(&q))
    });
    mpmc.bench_function("lfqueue", |b| {
        let q = Lf(AllocBoundedQueue::new(CAP));
        b.iter(|| run_mpmc(&q))
    });
    mpmc.bench_function("mutex_vecdeque", |b| {
        let q = Mtx(Mutex::new(VecDeque::with_capacity(CAP)));
        b.iter(|| run_mpmc(&q))
    });
    mpmc.finish();
}

criterion_group!(benches, bench_queues);
criterion_main!(benches);
