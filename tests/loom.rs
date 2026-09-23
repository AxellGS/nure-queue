//! Model checking with Loom. Run with:
//!
//! ```powershell
//! cargo test --features loom --test loom
//! ```
//!
//! Scope is deliberately sequential: Vyukov's push/pop retry loops spin
//! until another thread makes progress, and loom also explores the
//! schedule where that thread never runs — an unbounded solo spin that
//! blows the branch budget at any bound (loom's documented spin-lock
//! limitation; observed as `vv` overflow, i.e. 65k+ events in one path).
//! This test still pins real value: construction (every loom `UnsafeCell`
//! must come from `::new`, never `assume_init`) plus the full op set
//! under loom's tracked atomics/cells. True MPMC interleavings are
//! covered by `regression_583` (100k-op stress), miri and fuzz.

#![cfg(feature = "loom")]

use nure_queue::Queue;

#[test]
fn loom_sequential_ops() {
    loom::model(|| {
        let q = Queue::<u32, 4>::new();
        assert!(q.is_empty());
        q.enqueue(10).unwrap();
        q.enqueue(20).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.dequeue(), Some(10));
        assert_eq!(q.force_push(30), None);
        assert_eq!(q.force_push(40), None);
        q.enqueue(50).unwrap();
        assert!(q.is_full());
        assert_eq!(q.enqueue(60), Err(60));
        assert_eq!(q.force_push(60), Some(20));
        assert_eq!(q.pop(), Some(30));
        assert_eq!(q.pop(), Some(40));
        assert_eq!(q.pop(), Some(50));
        assert_eq!(q.pop(), Some(60));
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    });
}
