#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nure_queue::Queue;
use std::collections::VecDeque;

const CAP: usize = 16;

#[derive(Arbitrary, Debug)]
enum Op {
    Push(u8),
    Pop,
    ForcePush(u8),
}

fuzz_target!(|ops: Vec<Op>| {
    let q = Queue::<u8, CAP>::new();
    let mut model: VecDeque<u8> = VecDeque::with_capacity(CAP);
    // Bound per-input work: libFuzzer can emit megabyte inputs.
    for op in ops.iter().take(4096) {
        match *op {
            Op::Push(v) => {
                let r = q.push(v);
                if model.len() < CAP {
                    model.push_back(v);
                    assert_eq!(r, Ok(()));
                } else {
                    assert_eq!(r, Err(v));
                }
            }
            Op::Pop => assert_eq!(q.pop(), model.pop_front()),
            Op::ForcePush(v) => {
                let r = q.force_push(v);
                let expected = if model.len() == CAP {
                    model.pop_front()
                } else {
                    None
                };
                model.push_back(v);
                assert_eq!(r, expected);
            }
        }
    }
});
