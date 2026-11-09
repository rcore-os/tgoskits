//! Axtests for the vsock event worker's connection-request dispatch.

use alloc::sync::Arc;
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_net::{
    VsockEventWorker,
    vsock::{
        VsockAddr, VsockConnId, VsockEvent, axtest_connection_pending, axtest_listen,
        axtest_unlisten,
    },
};
use ax_runtime::thread::builder;

/// Listening port for the synthetic request; below the manager's ephemeral
/// range, so it cannot collide with `allocate_port`.
const TEST_LISTEN_PORT: u32 = 0xa5a5;

fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = ax_hal::time::monotonic_time();
    while !ready() {
        if ax_hal::time::monotonic_time().saturating_sub(start) >= timeout {
            return false;
        }
        ax_std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// A connection request for a listening port must be published to the accept
/// queue, and the dispatch must complete while the calling task still runs:
/// `publish` re-acquires the same static manager mutex that serves the
/// request, so a dispatch that keeps the request guard alive cannot finish.
///
/// Determinism: the dispatch runs on its own kernel thread and the case
/// observes completion with a deadline, so the reentrant acquisition fails
/// this case instead of wedging the axtest runner; publication is then read
/// from the manager's accept queue.
#[axtest::axtest]
fn connection_request_publishes_without_reentering_the_manager_lock() {
    axtest_listen(TEST_LISTEN_PORT).unwrap();

    let conn_id = VsockConnId {
        peer_addr: VsockAddr {
            cid: 2,
            port: 60000,
        },
        local_port: TEST_LISTEN_PORT,
    };

    let dispatched = Arc::new(AtomicBool::new(false));
    let worker = builder("vsock-conn-request".into())
        .spawn({
            let dispatched = dispatched.clone();
            move || {
                let mut worker = VsockEventWorker::with_inert_device_for_test();
                worker.handle_event_for_test(VsockEvent::ConnectionRequest(conn_id));
                dispatched.store(true, Ordering::Release);
            }
        })
        .unwrap();

    assert!(
        wait_until(Duration::from_secs(5), || dispatched
            .load(Ordering::Acquire)),
        "vsock connection-request dispatch did not complete"
    );
    worker.join().unwrap();

    assert!(
        axtest_connection_pending(TEST_LISTEN_PORT),
        "connection request was not published to the accept queue"
    );

    axtest_unlisten(TEST_LISTEN_PORT);
}
