use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use axpoll::{IoEvents, PollRegistrar, SharedObserver};
use axpoll_set::PollSet;
use futures::future;
use tokio::sync::Barrier;

struct WaitFuture {
    poll_set: Arc<PollSet>,
    ready: Arc<AtomicBool>,
    registrar: Option<PollRegistrar<SharedObserver>>,
}

impl Future for WaitFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.ready.load(Ordering::SeqCst) {
            this.registrar = None;
            return Poll::Ready(());
        }

        let registrar = this
            .registrar
            .get_or_insert_with(|| PollRegistrar::new(cx.waker()));
        registrar.reset(cx.waker());
        unsafe { registrar.register(&this.poll_set, IoEvents::IN) };

        if this.ready.load(Ordering::SeqCst) {
            this.registrar = None;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl WaitFuture {
    fn new(poll_set: Arc<PollSet>, ready: Arc<AtomicBool>) -> Self {
        Self {
            poll_set,
            ready,
            registrar: None,
        }
    }
}

#[tokio::test]
async fn async_wake_single() {
    let poll_set = Arc::new(PollSet::new());
    let ready = Arc::new(AtomicBool::new(false));

    let future = WaitFuture::new(poll_set.clone(), ready.clone());

    let handle = tokio::spawn(async move {
        ready.store(true, Ordering::SeqCst);
        unsafe { poll_set.wake(IoEvents::IN) };
    });

    future.await;
    handle.await.unwrap();
}

#[tokio::test]
async fn async_wake_many() {
    let poll_set = Arc::new(PollSet::new());
    let mut flags = Vec::new();
    let mut handles = Vec::new();
    let barrier = Arc::new(Barrier::new(66));
    for _ in 0..65 {
        let flag = Arc::new(AtomicBool::new(false));
        let waiter_barrier = barrier.clone();
        let future = WaitFuture::new(poll_set.clone(), flag.clone());
        let handle = tokio::spawn(async move {
            waiter_barrier.wait().await;
            future.await;
        });
        flags.push(flag);
        handles.push(handle);
    }
    barrier.wait().await;

    let mut ready = Vec::new();
    let mut pending = Vec::new();
    for (index, handle) in handles.into_iter().enumerate() {
        if index % 2 == 0 {
            ready.push(handle);
            flags[index].store(true, Ordering::SeqCst);
        } else {
            pending.push(handle);
        }
    }
    unsafe { poll_set.wake(IoEvents::IN) };
    future::try_join_all(ready).await.unwrap();

    for (index, flag) in flags.iter().enumerate() {
        if index % 2 != 0 {
            flag.store(true, Ordering::SeqCst);
        }
    }
    unsafe { poll_set.wake(IoEvents::IN) };
    future::try_join_all(pending).await.unwrap();
}
