use ax_std as std;

mod fault;

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use std::sync::Arc;

use ax_driver::block::{PlatformBlockDevice, RdifBlockDevice};
use dma_api::{CpuDmaBuffer, DmaDirection};
use rdif_block::{
    BatchSubmitDisposition, BlkError, BlockController, CompletedRequest, CompletionSink,
    ControlEvent, ControllerEvent, ControllerState, HardwareQueue, OwnedRequest, OwnedRequestBatch,
    RequestFlags, RequestId, RequestOp, SubmissionSink,
};

const VIRTIO_CONFIGURATION_CHANGE_INTERRUPT: u64 = 2;

#[derive(Default)]
struct Requests {
    accepted: Option<RequestId>,
    completed: Option<CompletedRequest>,
}

impl SubmissionSink for Requests {
    fn accepted(&mut self, id: RequestId) {
        assert!(self.accepted.replace(id).is_none(), "duplicate acceptance");
    }
}

impl CompletionSink for Requests {
    fn complete(&mut self, request: CompletedRequest) {
        assert!(
            self.completed.replace(request).is_none(),
            "duplicate completion"
        );
    }
}

fn request(queue: &dyn HardwareQueue, op: RequestOp) -> OwnedRequest {
    let direction = match op {
        RequestOp::Read => DmaDirection::FromDevice,
        RequestOp::Write => DmaDirection::ToDevice,
        RequestOp::Flush => return request_with_direction(queue, op, None),
    };
    request_with_direction(queue, op, Some(direction))
}

fn request_with_direction(
    queue: &dyn HardwareQueue,
    op: RequestOp,
    direction: Option<DmaDirection>,
) -> OwnedRequest {
    let data = direction.map(|direction| {
        let device = axklib::dma::device(queue.info().limits.dma);
        let mut buffer =
            CpuDmaBuffer::new_zero(&device, NonZeroUsize::new(512).unwrap(), 512, direction)
                .unwrap();
        if op == RequestOp::Write {
            buffer.copy_from_slice_cpu(&payload());
        }
        buffer.prepare_for_device()
    });
    OwnedRequest {
        op,
        // Flush has no addressed blocks; the driver must ignore this LBA.
        lba: 7,
        block_count: u32::from(data.is_some()),
        data,
        flags: RequestFlags::NONE,
    }
}

fn exercise_rejected_directions(queue: &mut dyn HardwareQueue) {
    for (op, direction) in [
        (RequestOp::Read, DmaDirection::ToDevice),
        (RequestOp::Write, DmaDirection::FromDevice),
    ] {
        let mut batch = OwnedRequestBatch::with_capacity(1);
        batch.push_back(request_with_direction(queue, op, Some(direction)));
        let mut sink = Requests::default();

        let result = queue.submit_batch_owned(&mut batch, &mut sink);

        assert_eq!(result.accepted(), 0);
        assert_eq!(
            result.disposition(),
            BatchSubmitDisposition::Fatal(BlkError::InvalidRequest)
        );
        assert_eq!(batch.len(), 1, "rejected DMA backing changed ownership");
        assert!(sink.accepted.is_none());
    }
}

fn submit(queue: &mut dyn HardwareQueue, request: OwnedRequest) -> Requests {
    let mut batch = OwnedRequestBatch::with_capacity(1);
    batch.push_back(request);
    let mut sink = Requests::default();
    assert_eq!(
        queue.submit_batch_owned(&mut batch, &mut sink).accepted(),
        1
    );
    assert!(batch.is_empty());
    queue.commit_submissions().unwrap();
    sink
}

fn payload() -> [u8; 512] {
    core::array::from_fn(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
}

// The callback only acknowledges hardware and publishes a queue-local event.
fn exercise_io(
    queue: &mut dyn HardwareQueue,
    endpoint: rdif_block::IrqEndpoint,
    irq: ax_driver::BindingIrq,
) {
    let pending = Arc::new(AtomicBool::new(false));
    let irq_pending = Arc::clone(&pending);
    let mut handler = endpoint.into_handler();
    let irq = ax_runtime::irq::resolve_binding_irq(irq).expect("resolve block IRQ");
    let handle = axklib::irq::request_shared(irq, move |_| {
        let ack = handler.ack();
        if ack.queues().contains(0) {
            irq_pending.store(true, Ordering::Release);
        }
        if ack.is_spurious() {
            axklib::irq::IrqReturn::Unhandled
        } else {
            axklib::irq::IrqReturn::Handled
        }
    })
    .expect("register real block IRQ");
    for op in [RequestOp::Write, RequestOp::Flush, RequestOp::Read] {
        let request = request(queue, op);
        let mut sink = submit(queue, request);
        let started = std::time::Instant::now();
        while sink.completed.is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "missing IRQ completion for {op:?}"
            );
            if pending.swap(false, Ordering::AcqRel) {
                queue.drain_completions(&mut sink).unwrap();
            } else {
                std::thread::yield_now();
            }
        }
        let completion = sink.completed.take().unwrap();
        assert_eq!(Some(completion.id), sink.accepted);
        assert_eq!(completion.result, Ok(()));
        if op == RequestOp::Read {
            assert_eq!(
                completion.data.unwrap().into_cpu_buffer().as_slice_cpu(),
                payload()
            );
        }
    }
    axklib::irq::disable(handle).unwrap();
    axklib::irq::free(handle).unwrap();
}

// Returns whether reset completed and the pending request was reclaimed.
fn exercise_stop(
    controller: &mut dyn BlockController,
    queue: &mut dyn HardwareQueue,
    event: ControllerEvent,
) -> bool {
    let request = request(queue, RequestOp::Read);
    let original_cpu_ptr = request.data.as_ref().unwrap().cpu_ptr();
    let mut sink = submit(queue, request);
    let stalled = controller.device_info().num_blocks == fault::STALLED_BLOCKS;
    let started = std::time::Instant::now();
    let mut state = controller.advance(event).unwrap().controller_state();
    let mut waited = false;
    while let ControllerState::RegisterPending { retry_after } = state {
        waited = true;
        assert_eq!(queue.shutdown(&mut sink), Err(BlkError::TimedOut));
        assert!(
            sink.completed.is_none(),
            "DMA returned before reset confirmation"
        );
        assert!(queue.commit_submissions().is_err());
        if stalled {
            return false;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "reset timed out"
        );
        std::thread::sleep(retry_after);
        state = controller
            .advance(ControllerEvent::RegisterRetry)
            .unwrap()
            .controller_state();
    }
    assert!(!stalled, "unconfirmed reset reported as complete");
    if controller.device_info().num_blocks == fault::DELAYED_BLOCKS {
        assert!(waited, "delayed reset was not exercised");
    }
    assert_eq!(state, ControllerState::Shutdown);
    queue.shutdown(&mut sink).unwrap();
    let completion = sink
        .completed
        .take()
        .expect("return pending DMA after reset");
    assert_eq!(Some(completion.id), sink.accepted);
    assert_eq!(completion.result, Err(BlkError::Io));
    let buffer = completion.data.unwrap().into_cpu_buffer();
    assert_eq!(buffer.cpu_ptr(), original_cpu_ptr);

    // Reuse the returned backing to prove a stopped queue preserves caller ownership.
    let mut batch = OwnedRequestBatch::with_capacity(1);
    batch.push_back(OwnedRequest {
        op: RequestOp::Read,
        lba: 7,
        block_count: 1,
        data: Some(buffer.prepare_for_device()),
        flags: RequestFlags::NONE,
    });
    let result = queue.submit_batch_owned(&mut batch, &mut sink);
    assert_eq!(result.accepted(), 0);
    assert!(matches!(
        result.disposition(),
        BatchSubmitDisposition::Fatal(_)
    ));
    assert_eq!(batch.len(), 1);
    queue.shutdown(&mut sink).unwrap();
    assert!(sink.completed.is_none(), "request completed twice");
    true
}

pub fn run() -> crate::TestResult {
    fault::assert_rejection_checked();
    let devices = rdrive::get_list::<PlatformBlockDevice>();
    assert_eq!(devices.len(), 3, "all block fixtures must be discovered");
    for device in devices {
        let (_, bindings, mut controller) = RdifBlockDevice::try_from(device).unwrap().into_parts();
        let mut update = controller
            .advance(ControllerEvent::Start { target_queues: 1 })
            .unwrap();
        let mut queue = update.take_queues().pop().expect("block queue");
        let mut endpoints = update.take_irq_endpoints();
        let event = if controller.device_info().num_blocks == 2048 {
            exercise_rejected_directions(&mut *queue);
            exercise_io(
                &mut *queue,
                endpoints.pop().unwrap(),
                bindings[0].irq.clone(),
            );
            // A live VirtIO configuration change cannot update the runtime's
            // frozen geometry, so the controller must stop without exposing
            // the pending DMA request again before reset completes.
            ControllerEvent::Irq(ControlEvent::new(0, VIRTIO_CONFIGURATION_CHANGE_INTERRUPT))
        } else {
            ControllerEvent::Watchdog { queue_id: 0 }
        };
        // No callback drains the next request; reset must return or quarantine it.
        if !exercise_stop(&mut *controller, &mut *queue, event) {
            // No other worker allocates DMA in this isolated kernel. An
            // unconfirmed reset must retain both rings and request backing.
            let dma_bytes = ax_alloc::global_allocator()
                .usages()
                .get(ax_alloc::UsageKind::Dma);
            drop(queue);
            assert_eq!(
                ax_alloc::global_allocator()
                    .usages()
                    .get(ax_alloc::UsageKind::Dma),
                dma_bytes
            );
        }
    }
    Ok(())
}
