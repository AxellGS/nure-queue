use nure_queue::Queue;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn fifo_order_and_full_empty() {
    let q = Queue::<u32, 4>::new();
    assert!(q.is_empty());
    assert_eq!(q.len(), 0);
    assert_eq!(q.capacity(), 4);
    for i in 0..4 {
        q.enqueue(i).unwrap();
    }
    assert!(q.is_full());
    assert_eq!(q.enqueue(99), Err(99));
    assert_eq!(q.len(), 4);
    for i in 0..4 {
        assert_eq!(q.dequeue(), Some(i));
    }
    assert_eq!(q.dequeue(), None);
    assert!(q.is_empty());
}

#[test]
#[cfg(not(feature = "loom"))]
fn static_construction_no_alloc() {
    static Q: Queue<u32, 8> = Queue::new();
    Q.enqueue(7).unwrap();
    assert_eq!(Q.dequeue(), Some(7));
}

#[test]
fn force_push_ring_mode() {
    let q = Queue::<u32, 2>::new();
    assert_eq!(q.force_push(1), None);
    assert_eq!(q.force_push(2), None);
    assert_eq!(q.force_push(3), Some(1));
    assert_eq!(q.pop(), Some(2));
    assert_eq!(q.pop(), Some(3));
}

/// heapless#583 regression: concurrent dequeue->enqueue on a full queue
/// must never panic, no matter the contention.
/// Skipped under miri (interpreter is ~1000x slower; use
/// `contention_sum_conserved` there instead).
#[test]
#[cfg_attr(miri, ignore)]
fn regression_583_no_panic_under_contention() {
    const N: usize = 4;
    let q = Queue::<u8, N>::new();
    for i in 0..N {
        q.enqueue(i as u8).unwrap();
    }
    std::thread::scope(|sc| {
        for _ in 0..2 {
            sc.spawn(|| {
                for _ in 0..100_000 {
                    if let Some(v) = q.dequeue() {
                        q.enqueue(v)
                            .unwrap_or_else(|v| panic!("enqueue failed for {v}"));
                    }
                }
            });
        }
    });
    // All N items survive.
    let mut seen = vec![];
    while let Some(v) = q.dequeue() {
        seen.push(v);
    }
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1, 2, 3]);
}

#[derive(Debug)]
struct Counted {
    drops: Arc<AtomicUsize>,
    _id: u32,
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn drops_remaining_items() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let q = Queue::<Counted, 8>::new();
        for i in 0..5 {
            q.push(Counted {
                drops: Arc::clone(&drops),
                _id: i,
            })
            .unwrap();
        }
        // Pop moves one out; the temporary drops at end of statement.
        assert!(q.pop().is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        // 4 remain: queue `Drop` must release them.
    }
    assert_eq!(drops.load(Ordering::SeqCst), 5);
}

/// Small concurrent workload with an order-free invariant (sum conserved).
/// Fast natively; under miri it gets UB/race checking with scheduler
/// exploration: `cargo miri test --test smoke contention_sum`.
#[test]
fn contention_sum_conserved() {
    const PER: u32 = 250;
    let q = Queue::<u32, 8>::new();
    let q = &q;
    let sum = AtomicUsize::new(0);
    let count = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for p in 0..2 {
            sc.spawn(move || {
                let base = p * PER;
                for i in 0..PER {
                    while q.enqueue(base + i).is_err() {}
                }
            });
        }
        sc.spawn(|| {
            loop {
                if let Some(v) = q.dequeue() {
                    sum.fetch_add(v as usize, Ordering::Relaxed);
                    if count.fetch_add(1, Ordering::Relaxed) + 1 == 2 * PER as usize {
                        break;
                    }
                }
            }
        });
    });
    assert_eq!(count.load(Ordering::SeqCst), 500);
    assert_eq!(sum.load(Ordering::SeqCst), (0..500usize).sum::<usize>());
}
