use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
    thread,
};

use axpoll::{IoEvents, PollSet};

struct Counter(AtomicUsize);

impl Counter {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicUsize::new(0)))
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    fn add(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.add();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.add();
    }
}

#[test]
fn register_and_wake() {
    let ps = PollSet::new();
    let counter = Counter::new();
    let w = Waker::from(counter.clone());
    unsafe { ps.register(&w, IoEvents::IN) };
    assert_eq!(unsafe { ps.wake(IoEvents::IN) }, 1);
    assert_eq!(counter.count(), 1);
}

#[test]
fn empty_return() {
    let ps = PollSet::new();
    assert_eq!(unsafe { ps.wake(IoEvents::IN) }, 0);
}

#[test]
fn wake_only_matching_interests() {
    let ps = PollSet::new();
    let read_counter = Counter::new();
    let write_counter = Counter::new();
    let read_waker = Waker::from(read_counter.clone());
    let write_waker = Waker::from(write_counter.clone());

    unsafe {
        ps.register(&read_waker, IoEvents::IN);
        ps.register(&write_waker, IoEvents::OUT);
    }

    assert_eq!(unsafe { ps.wake(IoEvents::IN) }, 1);
    assert_eq!(read_counter.count(), 1);
    assert_eq!(write_counter.count(), 0);
    assert_eq!(unsafe { ps.wake(IoEvents::OUT) }, 1);
    assert_eq!(write_counter.count(), 1);
}

#[test]
fn concurrent_registers_preserve_interests() {
    const NUM_WAITERS: usize = 64;

    let ps = Arc::new(PollSet::new());
    let barrier = Arc::new(Barrier::new(NUM_WAITERS));
    let counters = (0..NUM_WAITERS).map(|_| Counter::new()).collect::<Vec<_>>();
    let mut handles = Vec::with_capacity(NUM_WAITERS);

    for (i, counter) in counters.iter().cloned().enumerate() {
        let ps = ps.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            let waker = Waker::from(counter);
            let interests = if i % 2 == 0 {
                IoEvents::IN
            } else {
                IoEvents::OUT
            };
            barrier.wait();
            unsafe { ps.register(&waker, interests) };
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(unsafe { ps.wake(IoEvents::IN) }, NUM_WAITERS / 2);
    assert_eq!(
        counters
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, counter)| counter.count())
            .sum::<usize>(),
        NUM_WAITERS / 2
    );
    assert_eq!(
        counters
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 != 0)
            .map(|(_, counter)| counter.count())
            .sum::<usize>(),
        0
    );

    assert_eq!(unsafe { ps.wake(IoEvents::OUT) }, NUM_WAITERS / 2);
    assert_eq!(
        counters
            .iter()
            .map(|counter| counter.count())
            .sum::<usize>(),
        NUM_WAITERS
    );
}

#[test]
fn concurrent_deferred_wakes_partition_by_mask() {
    const NUM_WAITERS: usize = 64;

    let ps = Arc::new(PollSet::new());
    let counters = (0..NUM_WAITERS).map(|_| Counter::new()).collect::<Vec<_>>();

    for (i, counter) in counters.iter().cloned().enumerate() {
        let waker = Waker::from(counter);
        let interests = if i % 2 == 0 {
            IoEvents::IN
        } else {
            IoEvents::OUT
        };
        unsafe { ps.register(&waker, interests) };
    }

    let barrier = Arc::new(Barrier::new(2));
    let read_waker = {
        let ps = ps.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            unsafe { ps.wake(IoEvents::IN) }
        })
    };
    let write_waker = {
        let ps = ps.clone();
        thread::spawn(move || {
            barrier.wait();
            unsafe { ps.wake(IoEvents::OUT) }
        })
    };

    assert_eq!(
        read_waker.join().unwrap() + write_waker.join().unwrap(),
        NUM_WAITERS
    );
    assert!(counters.iter().all(|counter| counter.count() == 1));
    assert_eq!(unsafe { ps.wake(IoEvents::IN | IoEvents::OUT) }, 0);
}

#[test]
fn full_capacity() {
    let ps = PollSet::new();
    let counter = Counter::new();
    for _ in 0..64 {
        let w = Waker::from(counter.clone());
        let cx = Context::from_waker(&w);
        unsafe { ps.register(cx.waker(), IoEvents::IN) };
    }
    let woke = unsafe { ps.wake(IoEvents::IN) };
    assert_eq!(woke, 64);
    assert_eq!(counter.count(), 64);
}

#[test]
fn overwrite() {
    let ps = PollSet::new();
    let counters = (0..65).map(|_| Counter::new()).collect::<Vec<_>>();
    for c in &counters {
        let w = Waker::from(c.clone());
        let cx = Context::from_waker(&w);
        unsafe { ps.register(cx.waker(), IoEvents::IN) };
    }
    assert_eq!(unsafe { ps.wake(IoEvents::IN) }, 64);
    let total: usize = counters.iter().map(|c| c.count()).sum();
    assert_eq!(total, 65);
}

#[test]
fn drop_wakes() {
    let ps = PollSet::new();
    let counters = Counter::new();
    for _ in 0..10 {
        let w = Waker::from(counters.clone());
        let cx = Context::from_waker(&w);
        unsafe { ps.register(cx.waker(), IoEvents::IN) };
    }
    drop(ps);
    assert_eq!(counters.count(), 10);
}
