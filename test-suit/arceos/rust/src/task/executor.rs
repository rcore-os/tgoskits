use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use std::{
    os::arceos::task::executor::{BlockOnError, block_on, block_on_timeout},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct PendingUntilCancelled(Arc<AtomicBool>);

impl Future for PendingUntilCancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingUntilCancelled {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct RemoteCompletion(Arc<Mutex<(bool, Option<Waker>)>>);

impl Future for RemoteCompletion {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock();
        if state.0 {
            Poll::Ready(42)
        } else {
            state.1 = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

pub fn run() -> crate::TestResult {
    // Completion wins when the first poll is ready at an elapsed deadline.
    assert_eq!(
        block_on_timeout(Duration::ZERO, core::future::ready(7)),
        Ok(7)
    );

    let dropped = Arc::new(AtomicBool::new(false));
    assert_eq!(
        block_on_timeout(
            Duration::from_millis(2),
            PendingUntilCancelled(Arc::clone(&dropped))
        ),
        Err(BlockOnError::TimedOut),
    );
    assert!(
        dropped.load(Ordering::Acquire),
        "timeout must destroy the scoped future"
    );

    let state = Arc::new(Mutex::new((false, None::<Waker>)));
    let producer_state = Arc::clone(&state);
    let producer = thread::spawn(move || {
        loop {
            let wake = {
                let mut state = producer_state.lock();
                let wake = state.1.take();
                if wake.is_some() {
                    state.0 = true;
                }
                wake
            };
            if let Some(wake) = wake {
                wake.wake();
                break;
            }
            thread::yield_now();
        }
    });
    assert_eq!(block_on(RemoteCompletion(state)), 42);
    producer.join().unwrap();
    Ok(())
}
