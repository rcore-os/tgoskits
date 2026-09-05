use std::{
    boxed::Box,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    task::{Wake, Waker},
    thread,
    time::Duration,
};

use axpoll::{
    ExclusiveConsumer, IoEvents, PollRegistrar, PollRegistration, PollSource, RegistrationMode,
    SharedObserver,
};
use axpoll_set::PollSet;

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

fn shared_registrar(
    poll: &PollSet,
    counter: &Arc<Counter>,
    interests: IoEvents,
) -> PollRegistrar<SharedObserver> {
    let waker = Waker::from(counter.clone());
    let mut registrar = PollRegistrar::new(&waker);
    unsafe { registrar.register(poll, interests) };
    registrar
}

fn exclusive_registrar(
    poll: &PollSet,
    counter: &Arc<Counter>,
    interests: IoEvents,
) -> PollRegistrar<ExclusiveConsumer> {
    let waker = Waker::from(counter.clone());
    let mut registrar = PollRegistrar::new(&waker);
    unsafe { registrar.register_exclusive(poll, interests) };
    registrar
}

#[test]
fn linux_wake_notifies_all_shared_and_one_exclusive() {
    let poll = PollSet::new();
    let shared = [Counter::new(), Counter::new()];
    let exclusive = [Counter::new(), Counter::new()];
    let _shared_registrars = shared
        .iter()
        .map(|counter| shared_registrar(&poll, counter, IoEvents::IN))
        .collect::<Vec<_>>();
    let _exclusive_registrars = exclusive
        .iter()
        .map(|counter| exclusive_registrar(&poll, counter, IoEvents::IN))
        .collect::<Vec<_>>();

    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 3);
    assert!(shared.iter().all(|counter| counter.count() == 1));
    assert_eq!(
        exclusive
            .iter()
            .map(|counter| counter.count())
            .sum::<usize>(),
        1
    );

    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 1);
    assert!(exclusive.iter().all(|counter| counter.count() == 1));
}

#[test]
fn exclusive_registration_records_selection_before_wake() {
    let poll = PollSet::new();
    let first = Counter::new();
    let second = Counter::new();
    let first_registrar = exclusive_registrar(&poll, &first, IoEvents::IN);
    let second_registrar = exclusive_registrar(&poll, &second, IoEvents::IN);

    assert!(!first_registrar.was_exclusively_notified());
    assert!(!second_registrar.was_exclusively_notified());
    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 1);
    assert!(first_registrar.was_exclusively_notified());
    assert!(!second_registrar.was_exclusively_notified());
    assert_eq!(first.count(), 1);
    assert_eq!(second.count(), 0);
}

#[test]
fn dropping_registrar_cancels_the_exact_registration() {
    let poll = PollSet::new();
    let cancelled = Counter::new();
    let live = Counter::new();
    let cancelled_registrar = shared_registrar(&poll, &cancelled, IoEvents::IN);
    let _live_registrar = shared_registrar(&poll, &live, IoEvents::IN);

    drop(cancelled_registrar);
    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 1);
    assert_eq!(cancelled.count(), 0);
    assert_eq!(live.count(), 1);
}

#[test]
fn resetting_registrar_does_not_accumulate_stale_entries() {
    let poll = PollSet::new();
    let counter = Counter::new();
    let waker = Waker::from(counter.clone());
    let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);

    for _ in 0..256 {
        registrar.reset(&waker);
        unsafe { registrar.register(&poll, IoEvents::IN) };
    }

    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 1);
    assert_eq!(counter.count(), 1);
    assert!(unsafe { poll.wake(IoEvents::IN) } == 0);
}

struct CollidingSource(PollSet);

impl PollSource for CollidingSource {
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        unsafe { self.0.register(waker, interests, mode) }
    }
}

#[test]
fn distinct_sources_never_alias_registration_ownership() {
    let first = CollidingSource(PollSet::new());
    let second = CollidingSource(PollSet::new());
    let counter = Counter::new();
    let waker = Waker::from(counter.clone());
    let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);

    unsafe { registrar.register(&first, IoEvents::IN) };
    unsafe { registrar.register(&second, IoEvents::OUT) };

    assert_eq!(unsafe { first.0.wake(IoEvents::IN) }, 1);
    assert_eq!(unsafe { second.0.wake(IoEvents::OUT) }, 1);
    assert_eq!(counter.count(), 2);
}

#[test]
fn more_than_sixty_four_waiters_are_never_displaced() {
    const WAITERS: usize = 96;
    let poll = PollSet::new();
    let counters = (0..WAITERS).map(|_| Counter::new()).collect::<Vec<_>>();
    let _registrars = counters
        .iter()
        .map(|counter| shared_registrar(&poll, counter, IoEvents::IN))
        .collect::<Vec<_>>();

    assert!(counters.iter().all(|counter| counter.count() == 0));
    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, WAITERS);
    assert!(counters.iter().all(|counter| counter.count() == 1));
}

struct ReentrantRegister {
    poll: Arc<PollSet>,
    registrar: Mutex<Option<PollRegistrar<SharedObserver>>>,
    started: mpsc::Sender<()>,
    done: mpsc::Sender<()>,
}

impl ReentrantRegister {
    fn run(&self) {
        let _ = self.started.send(());
        let counter = Counter::new();
        let waker = Waker::from(counter);
        let mut registrar = PollRegistrar::new(&waker);
        unsafe { registrar.register(&self.poll, IoEvents::OUT) };
        *self.registrar.lock().unwrap() = Some(registrar);
        let _ = self.done.send(());
    }
}

impl Wake for ReentrantRegister {
    fn wake(self: Arc<Self>) {
        self.run();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.run();
    }
}

#[test]
fn reentrant_registration_is_not_consumed_by_the_current_wake() {
    let poll = Arc::new(PollSet::new());
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let reentrant = Arc::new(ReentrantRegister {
        poll: poll.clone(),
        registrar: Mutex::new(None),
        started: started_tx,
        done: done_tx,
    });
    let waker = Waker::from(reentrant);
    let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);
    unsafe { registrar.register(&poll, IoEvents::IN) };

    let wake_poll = poll.clone();
    let wake_thread =
        thread::spawn(move || unsafe { wake_poll.wake(IoEvents::IN | IoEvents::OUT) });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reentrant waker was not invoked");
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reentrant waker could not register back into the PollSet");
    assert_eq!(wake_thread.join().unwrap(), 1);
    assert_eq!(unsafe { poll.wake(IoEvents::OUT) }, 1);
}

#[test]
fn ordinary_wake_uses_shared_and_exclusive_selection() {
    let poll = PollSet::new();
    let shared = [Counter::new(), Counter::new()];
    let exclusive = [Counter::new(), Counter::new()];
    let _shared_registrars = shared
        .iter()
        .map(|counter| shared_registrar(&poll, counter, IoEvents::IN))
        .collect::<Vec<_>>();
    let _exclusive_registrars = exclusive
        .iter()
        .map(|counter| exclusive_registrar(&poll, counter, IoEvents::IN))
        .collect::<Vec<_>>();

    assert_eq!(unsafe { poll.wake(IoEvents::IN) }, 3);
    assert!(shared.iter().all(|counter| counter.count() == 1));
    assert_eq!(
        exclusive
            .iter()
            .map(|counter| counter.count())
            .sum::<usize>(),
        1
    );
}

#[test]
fn terminal_wake_all_notifies_every_exclusive_waiter() {
    let poll = PollSet::new();
    let counters = [Counter::new(), Counter::new(), Counter::new()];
    let _registrars = counters
        .iter()
        .map(|counter| exclusive_registrar(&poll, counter, IoEvents::HUP))
        .collect::<Vec<_>>();

    assert_eq!(unsafe { poll.wake_all(IoEvents::HUP) }, counters.len());
    assert!(counters.iter().all(|counter| counter.count() == 1));
    assert_eq!(unsafe { poll.wake_all(IoEvents::HUP) }, 0);
}

#[test]
fn dropping_pollset_wakes_once_and_late_registrar_drop_is_safe() {
    let poll = PollSet::new();
    let counter = Counter::new();
    let registrar = shared_registrar(&poll, &counter, IoEvents::IN);

    drop(poll);
    assert_eq!(counter.count(), 1);
    drop(registrar);
    assert_eq!(counter.count(), 1);
}
