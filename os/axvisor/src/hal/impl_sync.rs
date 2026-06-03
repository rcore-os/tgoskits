use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::SpinNoIrq;
use axvisor_api::sync::SyncIf;
use std::os::arceos::api::task::{
    AxWaitQueueHandle, ax_wait_queue_wait, ax_wait_queue_wait_until, ax_wait_queue_wake,
};

static WAIT_QUEUE_IDS: AtomicUsize = AtomicUsize::new(1);
static WAIT_QUEUES: SpinNoIrq<BTreeMap<usize, Arc<AxWaitQueueHandle>>> =
    SpinNoIrq::new(BTreeMap::new());

fn get_wait_queue(queue: usize) -> Arc<AxWaitQueueHandle> {
    WAIT_QUEUES
        .lock()
        .get(&queue)
        .cloned()
        .expect("wait queue not found")
}

struct SyncImpl;

#[axvisor_api::api_impl]
impl SyncIf for SyncImpl {
    fn create_wait_queue() -> usize {
        let id = WAIT_QUEUE_IDS.fetch_add(1, Ordering::Relaxed);
        WAIT_QUEUES
            .lock()
            .insert(id, Arc::new(AxWaitQueueHandle::new()));
        id
    }

    fn destroy_wait_queue(queue: usize) {
        WAIT_QUEUES.lock().remove(&queue);
    }

    fn wait_queue_wait(queue: usize) {
        let queue = get_wait_queue(queue);
        ax_wait_queue_wait(queue.as_ref(), None);
    }

    fn wait_queue_wait_until(queue: usize, condition: Box<dyn Fn() -> bool + Send + 'static>) {
        let queue = get_wait_queue(queue);
        ax_wait_queue_wait_until(queue.as_ref(), condition, None);
    }

    fn wait_queue_wake_one(queue: usize) {
        let queue = get_wait_queue(queue);
        ax_wait_queue_wake(queue.as_ref(), 1);
    }

    fn wait_queue_wake_all(queue: usize) {
        let queue = get_wait_queue(queue);
        ax_wait_queue_wake(queue.as_ref(), u32::MAX);
    }
}
