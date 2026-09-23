//! Differential fuzzing: `nure-queue` vs `VecDeque` oracle.
//!
//! Random op sequences (`push`/`pop`/`force_push`/`len`/`is_empty`/`is_full`)
//! must keep the queue observably identical to a `VecDeque` model.
//! Each sequence replays on several capacities (1, 3, 16: single-slot,
//! non-power-of-two wrap, standard) plus a drop-accounting variant with
//! a `Drop`-counting payload (~2M ops total).
//!
//! ```powershell
//! cargo test --test differential
//! ```

use nure_queue::Queue;
use proptest::prelude::*;
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Clone, Copy)]
enum Op {
    Push(u8),
    Pop,
    ForcePush(u8),
    Len,
    IsEmpty,
    IsFull,
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            any::<u8>().prop_map(Op::Push),
            Just(Op::Pop),
            any::<u8>().prop_map(Op::ForcePush),
            Just(Op::Len),
            Just(Op::IsEmpty),
            Just(Op::IsFull),
        ],
        1..2000,
    )
}

fn check_ops<const N: usize>(ops: &[Op]) {
    let q = Queue::<u8, N>::new();
    let mut model: VecDeque<u8> = VecDeque::new();
    for &op in ops {
        match op {
            Op::Push(v) => {
                let r = q.push(v);
                if model.len() < N {
                    model.push_back(v);
                    assert_eq!(r, Ok(()));
                } else {
                    assert_eq!(r, Err(v));
                }
            }
            Op::Pop => {
                assert_eq!(q.pop(), model.pop_front());
            }
            Op::ForcePush(v) => {
                let r = q.force_push(v);
                let expected = if model.len() == N {
                    model.pop_front()
                } else {
                    None
                };
                model.push_back(v);
                assert_eq!(r, expected);
            }
            Op::Len => assert_eq!(q.len(), model.len()),
            Op::IsEmpty => assert_eq!(q.is_empty(), model.is_empty()),
            Op::IsFull => assert_eq!(q.is_full(), model.len() == N),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, .. ProptestConfig::default() })]
    #[test]
    fn differential_vs_vecdeque(ops in ops_strategy()) {
        check_ops::<1>(&ops);
        check_ops::<3>(&ops);
        check_ops::<16>(&ops);
    }
}

#[derive(Debug)]
struct Counted {
    v: u8,
    drops: Arc<AtomicUsize>,
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn check_drops<const N: usize>(ops: &[Op]) {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut pushed_ok = 0usize;
    let mut failed = 0usize;
    {
        let q = Queue::<Counted, N>::new();
        let mut model: VecDeque<u8> = VecDeque::new();
        for &op in ops {
            match op {
                Op::Push(v) => {
                    let item = Counted {
                        v,
                        drops: Arc::clone(&drops),
                    };
                    match q.push(item) {
                        Ok(()) => {
                            assert!(model.len() < N);
                            model.push_back(v);
                            pushed_ok += 1;
                        }
                        Err(returned) => {
                            assert_eq!(model.len(), N);
                            drop(returned);
                            failed += 1;
                        }
                    }
                }
                Op::Pop => {
                    assert_eq!(q.pop().map(|c| c.v), model.pop_front());
                }
                Op::ForcePush(v) => {
                    let r = q.force_push(Counted {
                        v,
                        drops: Arc::clone(&drops),
                    });
                    if model.len() == N {
                        let old = model.pop_front().unwrap();
                        model.push_back(v);
                        assert_eq!(r.map(|c| c.v), Some(old));
                    } else {
                        assert!(r.is_none());
                        model.push_back(v);
                    }
                    pushed_ok += 1;
                }
                Op::Len => assert_eq!(q.len(), model.len()),
                Op::IsEmpty => assert_eq!(q.is_empty(), model.is_empty()),
                Op::IsFull => assert_eq!(q.is_full(), model.len() == N),
            }
        }
        while q.pop().is_some() {}
    }
    assert_eq!(drops.load(Ordering::SeqCst), pushed_ok + failed);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]
    #[test]
    fn drops_match_pushes(ops in ops_strategy()) {
        check_drops::<1>(&ops);
        check_drops::<8>(&ops);
    }
}
