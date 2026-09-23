//! `nure-queue`: a `no_std`, allocator-free, lock-free bounded MPMC queue.
//!
//! A port of Crossbeam's [`ArrayQueue`](https://docs.rs/crossbeam-queue) (Dmitry
//! Vyukov's bounded MPMC algorithm) to static storage: no heap, no allocator,
//! constructible in `const` context so it can live in a `static`.
//!
//! ```rust
//! use nure_queue::Queue;
//!
//! let q = Queue::<u32, 8>::new();
//! q.enqueue(1).unwrap();
//! q.enqueue(2).unwrap();
//! assert_eq!(q.dequeue(), Some(1));
//! assert_eq!(q.dequeue(), Some(2));
//! ```
//!
//! Static, allocator-free construction (default build only,
//! demonstrated by `static_construction_no_alloc` in `tests/smoke.rs`):
//! ```ignore
//! use nure_queue::Queue;
//!
//! static Q: Queue<u32, 8> = Queue::new();
//! ```
//!
//! # Why this exists
//!
//! `heapless::mpmc` is deprecated (not truly lock-free — see
//! [heapless#583](https://github.com/rust-embedded/heapless/issues/583)).
//! Crossbeam's `ArrayQueue` is correct but heap-allocates on construction.
//! `embassy-sync`'s channel is static but mutex-based. This crate is the
//! intersection: static + truly lock-free.
//!
//! # Memory ordering
//!
//! Same protocol as Crossbeam: slot hand-off via acquire/release stamps,
//! head/tail CAS with `SeqCst` on success. See `push`/`pop` for details.

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![warn(missing_docs)]

use core::{fmt, mem::MaybeUninit};

// ---------------------------------------------------------------------------
// Atomics shim: core by default, loom via the `loom` cargo feature
// ---------------------------------------------------------------------------
#[cfg(feature = "loom")]
mod atom {
    pub use loom::cell::UnsafeCell;
    pub use loom::sync::atomic::{AtomicUsize, Ordering, fence};
    pub type AtomicIndex = AtomicUsize;
    pub type Index = usize;
}

#[cfg(not(feature = "loom"))]
mod atom {
    #![allow(unused_imports)]
    pub use core::cell::UnsafeCell;
    #[cfg(target_has_atomic = "64")]
    pub use core::sync::atomic::AtomicU64;
    pub use core::sync::atomic::{AtomicUsize, Ordering, fence};
    #[cfg(target_has_atomic = "64")]
    pub type AtomicIndex = AtomicU64;
    #[cfg(target_has_atomic = "64")]
    pub type Index = u64;
    #[cfg(not(target_has_atomic = "64"))]
    pub type AtomicIndex = AtomicUsize;
    #[cfg(not(target_has_atomic = "64"))]
    pub type Index = usize;
}

#[allow(unused_imports)]
use atom::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Cache-line padded wrapper so `head` and `tail` don't share a line.
#[repr(align(64))]
struct Padded<T>(T);

/// Backoff: exponential spinning; with `std` it yields past a threshold
/// (pure spin on `no_std`, where yielding does not exist).
struct Backoff {
    spins: u32,
}

impl Backoff {
    #[inline]
    fn new() -> Self {
        Self { spins: 0 }
    }

    #[inline]
    fn spin(&mut self) {
        #[cfg(feature = "std")]
        if self.spins > 6 {
            std::thread::yield_now();
            return;
        }
        let n = 1u32 << self.spins.min(6);
        for _ in 0..n {
            core::hint::spin_loop();
        }
        if self.spins < 8 {
            self.spins += 1;
        }
    }

    #[inline]
    fn snooze(&mut self) {
        #[cfg(feature = "std")]
        if self.spins > 8 {
            std::thread::yield_now();
            return;
        }
        let n = 1u32 << self.spins.min(8);
        for _ in 0..n {
            core::hint::spin_loop();
        }
        if self.spins < 10 {
            self.spins += 1;
        }
    }
}

/// Smallest power of two strictly greater than `cap` (`cap >= 1`, no overflow
/// for realistic capacities; construction asserts a sane bound).
const fn one_lap_for(cap: Index) -> Index {
    let need = cap + 1;
    let mut lap: Index = 2;
    while lap < need {
        lap *= 2;
    }
    lap
}

// ---------------------------------------------------------------------------
// Slot
// ---------------------------------------------------------------------------

/// One queue slot: a stamp for the Vyukov protocol + uninitialized payload.
struct Slot<T> {
    // Loom atomics must NOT sit inside UnsafeCell (double tracking).
    #[cfg(not(feature = "loom"))]
    stamp: UnsafeCell<AtomicIndex>,
    #[cfg(feature = "loom")]
    stamp: AtomicIndex,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    #[cfg(not(feature = "loom"))]
    /// SAFETY: caller owns this slot per the Vyukov protocol (unique index
    /// acquired via tail CAS), so exclusive access holds.
    #[inline]
    pub(crate) fn write_value(&self, value: T) {
        unsafe { self.value.get().write(MaybeUninit::new(value)) }
    }

    #[cfg(not(feature = "loom"))]
    #[inline]
    pub(crate) fn read_value(&self) -> T {
        unsafe { self.value.get().read().assume_init() }
    }

    #[cfg(not(feature = "loom"))]
    #[inline]
    pub(crate) fn drop_value(&mut self) {
        unsafe { self.value.get_mut().assume_init_drop() }
    }

    #[cfg(not(feature = "loom"))]
    /// Replaces the inner value, returning the old one.
    /// SAFETY: caller owns this slot exclusively.
    #[inline]
    pub(crate) fn replace_value(&self, value: T) -> T {
        unsafe {
            let old = core::mem::replace(&mut *self.value.get(), MaybeUninit::new(value));
            old.assume_init()
        }
    }

    #[cfg(not(feature = "loom"))]
    #[inline]
    pub(crate) fn stamp_load(&self) -> Index {
        unsafe { (*self.stamp.get()).load(Ordering::Acquire) }
    }

    #[cfg(not(feature = "loom"))]
    #[inline]
    pub(crate) fn stamp_store(&self, val: Index) {
        unsafe { (*self.stamp.get()).store(val, Ordering::Release) }
    }

    #[cfg(feature = "loom")]
    pub(crate) fn write_value(&self, value: T) {
        // with_mut gives *mut MaybeUninit<T>; (&mut *p) is &mut MaybeUninit<T>.
        // write returns &mut T; discard it.
        self.value.with_mut(|p| unsafe {
            let _ = (&mut *p).write(value);
        })
    }

    #[cfg(feature = "loom")]
    pub(crate) fn read_value(&self) -> T {
        // with gives *const MaybeUninit<T>. ptr::read copies T out without moving.
        self.value
            .with(|p| unsafe { core::ptr::read(p).assume_init() })
    }

    #[cfg(feature = "loom")]
    pub(crate) fn drop_value(&mut self) {
        self.value
            .with_mut(|p| unsafe { (&mut *p).assume_init_drop() })
    }

    #[cfg(feature = "loom")]
    pub(crate) fn replace_value(&self, value: T) -> T {
        self.value.with_mut(|p| unsafe {
            core::ptr::replace(&mut *p, MaybeUninit::new(value)).assume_init()
        })
    }

    #[cfg(feature = "loom")]
    pub(crate) fn stamp_load(&self) -> Index {
        self.stamp.load(Ordering::Acquire)
    }

    #[cfg(feature = "loom")]
    pub(crate) fn stamp_store(&self, val: Index) {
        self.stamp.store(val, Ordering::Release)
    }
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// A bounded lock-free multi-producer multi-consumer queue with static storage.
///
/// Capacity `N` is fixed at compile time; the whole queue (including all
/// slots) lives inline, so `Queue::new()` is `const` and the queue can be
/// placed in a `static` with no allocator involved (requires the default
/// build; with `loom` feature, use `Queue::new()` at runtime — see below).
///
/// `push` returns `Result<(), T>` on full push, `pop` returns `Option<T>` —
/// plus [`Queue::enqueue`]/[`Queue::dequeue`] aliases matching the old
/// `heapless::mpmc` API for drop-in migration.
///
/// # Static initialization
///
/// ```ignore
/// // Default build: `Queue::new()` is `const`.
/// static Q: Queue<u32, 8> = Queue::new();
/// ```
///
/// With `--features loom`, `new()` is a regular function: construct at runtime.
pub struct Queue<T, const N: usize> {
    head: Padded<AtomicIndex>,
    tail: Padded<AtomicIndex>,
    // Array-of-MaybeUninit: `assume_init()` on `[Slot; N]` would transiently
    // create uninitialized atomics (UB); each slot is written in `new()`.
    buffer: [MaybeUninit<Slot<T>>; N],
    one_lap: Index,
}

// SAFETY: synchronization via atomics; `T: Send` may cross threads.
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}
unsafe impl<T: Send, const N: usize> Send for Queue<T, N> {}

impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Queue<T, N> {
    /// Creates an empty queue. `const`-compatible on the default build:
    /// `static Q: Queue<u32, 8> = Queue::new();`.
    ///
    /// With the `loom` feature this is a regular (non-const) function
    /// because loom's atomics are runtime-only; use it at runtime there.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        assert!(N > 0, "capacity must be non-zero");
        assert!(N <= (isize::MAX as usize) / 2, "capacity is too large");

        let mut buffer: [MaybeUninit<Slot<T>>; N] = unsafe { MaybeUninit::uninit().assume_init() };
        let mut i = 0;
        while i < N {
            buffer[i] = MaybeUninit::new(Slot {
                stamp: UnsafeCell::new(AtomicIndex::new(i as Index)),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            });
            i += 1;
        }
        Self {
            head: Padded(AtomicIndex::new(0)),
            tail: Padded(AtomicIndex::new(0)),
            buffer,
            one_lap: one_lap_for(N as Index),
        }
    }

    /// Creates an empty queue (loom build: runtime-only, loom atomics
    /// are not `const`-constructible).
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        assert!(N > 0, "capacity must be non-zero");
        assert!(N <= (isize::MAX as usize) / 2, "capacity is too large");

        let mut buffer: [MaybeUninit<Slot<T>>; N] = unsafe { MaybeUninit::uninit().assume_init() };
        let mut i = 0;
        while i < N {
            buffer[i] = MaybeUninit::new(Slot {
                stamp: AtomicIndex::new(i as Index),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            });
            i += 1;
        }
        Self {
            head: Padded(AtomicIndex::new(0)),
            tail: Padded(AtomicIndex::new(0)),
            buffer,
            one_lap: one_lap_for(N as Index),
        }
    }

    /// Capacity in elements. Always `N`.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    fn push_or_else<F>(&self, mut value: T, f: F) -> Result<(), T>
    where
        F: Fn(T, Index, Index, &Slot<T>) -> Result<T, T>,
    {
        let mut backoff = Backoff::new();
        let mut tail = self.tail.0.load(Ordering::Relaxed);

        loop {
            let index = (tail & (self.one_lap - 1)) as usize;
            let lap = tail & !(self.one_lap - 1);

            let new_tail = if index + 1 < N {
                tail + 1
            } else {
                lap.wrapping_add(self.one_lap)
            };

            // SAFETY: `index < N` by masking invariant.
            let slot = unsafe { self.buffer.get_unchecked(index).assume_init_ref() };
            let stamp = slot.stamp_load();

            if tail == stamp {
                match self.tail.0.compare_exchange_weak(
                    tail,
                    new_tail,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // We own the slot: publish value, then stamp (release).
                        slot.write_value(value);
                        slot.stamp_store(tail + 1);
                        return Ok(());
                    }
                    Err(t) => {
                        tail = t;
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(self.one_lap) == tail + 1 {
                fence(Ordering::SeqCst);
                value = f(value, tail, new_tail, slot)?;
                backoff.spin();
                tail = self.tail.0.load(Ordering::Relaxed);
            } else {
                backoff.snooze();
                tail = self.tail.0.load(Ordering::Relaxed);
            }
        }
    }

    /// Attempts to push. On full queue, returns the value back as `Err`.
    pub fn push(&self, value: T) -> Result<(), T> {
        self.push_or_else(value, |v, tail, _, _| {
            let head = self.head.0.load(Ordering::Relaxed);
            if head.wrapping_add(self.one_lap) == tail {
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Alias of [`Queue::push`] matching the `heapless::mpmc` API.
    #[inline]
    pub fn enqueue(&self, value: T) -> Result<(), T> {
        self.push(value)
    }

    /// Pushes, replacing the oldest element if full (returns it, else `None`).
    /// Ring-buffer mode.
    pub fn force_push(&self, value: T) -> Option<T> {
        self.push_or_else(value, |v, tail, new_tail, slot| {
            let head = tail.wrapping_sub(self.one_lap);
            let new_head = new_tail.wrapping_sub(self.one_lap);

            if self
                .head
                .0
                .compare_exchange_weak(head, new_head, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                self.tail.0.store(new_tail, Ordering::SeqCst);
                // SAFETY: head moved, slot uniquely ours; swap payload.
                let old = slot.replace_value(v);
                slot.stamp_store(tail + 1);
                Err(old)
            } else {
                Ok(v)
            }
        })
        .err()
    }

    /// Attempts to pop. `None` if empty.
    pub fn pop(&self) -> Option<T> {
        let mut backoff = Backoff::new();
        let mut head = self.head.0.load(Ordering::Relaxed);

        loop {
            let index = (head & (self.one_lap - 1)) as usize;
            let lap = head & !(self.one_lap - 1);

            // SAFETY: `index < N` by masking invariant.
            let slot = unsafe { self.buffer.get_unchecked(index).assume_init_ref() };
            let stamp = slot.stamp_load();

            if head + 1 == stamp {
                let new = if index + 1 < N {
                    head + 1
                } else {
                    lap.wrapping_add(self.one_lap)
                };

                match self.head.0.compare_exchange_weak(
                    head,
                    new,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let msg = slot.read_value();
                        slot.stamp_store(head.wrapping_add(self.one_lap));
                        return Some(msg);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                fence(Ordering::SeqCst);
                let tail = self.tail.0.load(Ordering::Relaxed);
                if tail == head {
                    return None;
                }
                backoff.spin();
                head = self.head.0.load(Ordering::Relaxed);
            } else {
                backoff.snooze();
                head = self.head.0.load(Ordering::Relaxed);
            }
        }
    }

    /// Alias of [`Queue::pop`] matching the `heapless::mpmc` API.
    #[inline]
    pub fn dequeue(&self) -> Option<T> {
        self.pop()
    }

    /// `true` if no elements are stored.
    pub fn is_empty(&self) -> bool {
        let head = self.head.0.load(Ordering::SeqCst);
        let tail = self.tail.0.load(Ordering::SeqCst);
        tail == head
    }

    /// `true` if no push can succeed without [`Queue::force_push`].
    pub fn is_full(&self) -> bool {
        let tail = self.tail.0.load(Ordering::SeqCst);
        let head = self.head.0.load(Ordering::SeqCst);
        head.wrapping_add(self.one_lap) == tail
    }

    /// Number of elements currently stored.
    pub fn len(&self) -> usize {
        loop {
            let tail = self.tail.0.load(Ordering::SeqCst);
            let head = self.head.0.load(Ordering::SeqCst);

            if self.tail.0.load(Ordering::SeqCst) == tail {
                let hix = (head & (self.one_lap - 1)) as usize;
                let tix = (tail & (self.one_lap - 1)) as usize;

                return if hix < tix {
                    tix - hix
                } else if hix > tix {
                    N - hix + tix
                } else if tail == head {
                    0
                } else {
                    N
                };
            }
        }
    }
}

impl<T, const N: usize> Drop for Queue<T, N> {
    fn drop(&mut self) {
        if core::mem::needs_drop::<T>() {
            // Use `load` (not `get_mut`) to read head/tail — works for
            // both core and loom atomics.
            let head = self.head.0.load(Ordering::SeqCst);
            let tail = self.tail.0.load(Ordering::SeqCst);

            let hix = (head & (self.one_lap - 1)) as usize;
            let tix = (tail & (self.one_lap - 1)) as usize;

            let len = if hix < tix {
                tix - hix
            } else if hix > tix {
                N - hix + tix
            } else if tail == head {
                0
            } else {
                N
            };

            for i in 0..len {
                let index = if hix + i < N { hix + i } else { hix + i - N };
                // SAFETY: these slots hold live values per head/tail accounting.
                unsafe {
                    self.buffer
                        .get_unchecked_mut(index)
                        .assume_init_mut()
                        .drop_value();
                }
            }
        }
    }
}

impl<T, const N: usize> fmt::Debug for Queue<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Queue { .. }")
    }
}
