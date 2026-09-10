//! Starry axtests for the asynchronous block runtime boundary.

use alloc::sync::Arc;
use ax_fs_ng::{BlockDeviceHandle, BlockError};
use core::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};
use rdif_block::CompletedRequest;

async fn read_one(device: Arc<BlockDeviceHandle>, lba: u64) -> Result<CompletedRequest, BlockError> {
    // Make the first scheduling hand-off explicit so the composition check is
    // deterministic even when a device completes a request very quickly.
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
    device.axtest_read(lba).await
}

/// Drives two independent block requests from one task while the first
/// operation is pending. This exercises submission, IRQ completion and
/// per-request waker delivery through the real Starry runtime.
#[axtest::axtest]
fn block_runtime_async_double_read() {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    let device = devices
        .first()
        .cloned()
        .expect("block axtest requires an installed block device");
    let info = device.device_info();
    assert!(info.num_blocks >= 2, "block axtest requires at least two blocks");
    let block_size = info.logical_block_size;
    let mut first = pin!(read_one(Arc::clone(&device), 0));
    let mut second = pin!(read_one(device, 1));
    let mut first_result = None;
    let mut second_result = None;
    let mut first_pending_before_second = false;
    let (first_result, second_result) = crate::task::future::block_on(poll_fn(|cx| {
        let first_pending = if first_result.is_none() {
            match first.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    first_result = Some(result);
                    false
                }
                Poll::Pending => true,
            }
        } else {
            false
        };
        let second_polled = if second_result.is_none() {
            match second.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    second_result = Some(result);
                    true
                }
                Poll::Pending => true,
            }
        } else {
            false
        };
        if first_pending && second_polled {
            first_pending_before_second = true;
        }
        match (first_result.take(), second_result.take()) {
            (Some(first), Some(second)) => Poll::Ready((first, second)),
            (first, second) => {
                first_result = first;
                second_result = second;
                Poll::Pending
            }
        }
    }));

    assert!(
        first_pending_before_second,
        "second request was not polled while the first completion was pending"
    );
    let first = first_result.expect("first block request result");
    let second = second_result.expect("second block request result");
    assert_eq!(first.result, Ok(()));
    assert_eq!(second.result, Ok(()));
    assert_eq!(first.data.as_ref().map(|dma| dma.len().get()), Some(block_size));
    assert_eq!(second.data.as_ref().map(|dma| dma.len().get()), Some(block_size));
    assert_ne!(usize::from(first.id), usize::from(second.id));
}
