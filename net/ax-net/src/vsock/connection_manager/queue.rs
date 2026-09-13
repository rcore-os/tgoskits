//! Bounded listener and accept queues.

use alloc::sync::Arc;

use axpoll::IoEvents;
use axpoll_set::PollSet;
use ringbuf::{HeapRb, traits::*};

use super::{VsockAddr, VsockConnId};
use crate::{NetError, NetResult};

pub(super) const VSOCK_ACCEPT_QUEUE_SIZE: usize = 128; // accept queue size

/// A fixed-size accept queue
pub struct AcceptQueue {
    producer: ringbuf::HeapProd<VsockConnId>,
    consumer: ringbuf::HeapCons<VsockConnId>,
}

impl AcceptQueue {
    pub fn new() -> Self {
        let rb = HeapRb::<VsockConnId>::new(VSOCK_ACCEPT_QUEUE_SIZE);
        let (producer, consumer) = rb.split();
        Self { producer, consumer }
    }

    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    pub fn push(&mut self, conn_id: VsockConnId) -> NetResult<()> {
        match self.producer.try_push(conn_id) {
            Ok(_) => Ok(()),
            Err(_) => Err(NetError::ResourceBusy),
        }
    }

    pub fn pop(&mut self) -> Option<VsockConnId> {
        self.consumer.try_pop()
    }

    pub(super) fn remove(&mut self, conn_id: VsockConnId) -> bool {
        let queued = self.consumer.occupied_len();
        let mut removed = false;
        for _ in 0..queued {
            let current = self
                .consumer
                .try_pop()
                .expect("queued count came from the same accept queue");
            if !removed && current == conn_id {
                removed = true;
            } else {
                self.producer
                    .try_push(current)
                    .expect("popping one entry reserves space for reinsertion");
            }
        }
        removed
    }
}

/// listen queue
pub struct ListenQueue {
    pub accept_queue: AcceptQueue,
    pub wakers: Arc<PollSet>,
    pub local_addr: VsockAddr,
}

impl ListenQueue {
    pub fn new(local_addr: VsockAddr) -> Self {
        Self {
            accept_queue: AcceptQueue::new(),
            wakers: Arc::new(PollSet::new()),
            local_addr,
        }
    }

    pub fn wake(&mut self) {
        // Accept queue state is published before waking listeners.
        unsafe { self.wakers.wake(IoEvents::IN) };
    }
}
