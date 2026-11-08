use core::{num::NonZeroUsize, time::Duration};

use rdif_block::{
    BatchSubmitDisposition, BlockController, CompletedRequest, CompletionSink, ControllerEvent,
    ControllerState, DriverGeneric, OwnedRequest, OwnedRequestBatch, RequestFlags, RequestId,
    RequestOp, SubmissionSink,
};

use super::*;
use crate::rdif::{BlockConfig, BlockDevice};

#[test]
fn controller_uses_card_diagnostic_identity() {
    let host = MockHost::new(Vec::new());
    let config = BlockConfig::dma("sdmmc-test", 1, test_device_dma());
    let parts = host.into_parts();
    let mut card = SdMmcCard::new(parts.bus);
    card.set_diagnostic_identity("rockchip-dwmmc:/mmc@fe2c0000");

    let controller = BlockDevice::new(card, parts.irq, config);

    assert_eq!(controller.name(), "rockchip-dwmmc:/mmc@fe2c0000");
}

#[test]
fn controller_teardown_is_idempotent_after_watchdog_shutdown() {
    let host = MockHost::new(Vec::new());
    let config = BlockConfig::dma("sdmmc-test", 1, test_device_dma());
    let parts = host.into_parts();
    let mut controller = BlockDevice::new(SdMmcCard::new(parts.bus), parts.irq, config);

    let start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    assert_eq!(start.controller_state(), ControllerState::Ready);
    assert_eq!(
        controller
            .advance(ControllerEvent::Watchdog { queue_id: 0 })
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );

    assert_eq!(
        controller
            .advance(ControllerEvent::QuiesceIrqs)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
    assert_eq!(
        controller
            .advance(ControllerEvent::Shutdown)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
}

#[test]
fn ready_online_smp_repeats_info_without_reissuing_resources() {
    let host = MockHost::new(Vec::new());
    let config = BlockConfig::dma("sdmmc-test", 1, test_device_dma());
    let parts = host.into_parts();
    let mut controller = BlockDevice::new(SdMmcCard::new(parts.bus), parts.irq, config);
    let mut start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    assert_eq!(start.controller_state(), ControllerState::Ready);
    assert_eq!(start.take_queues().len(), 1);
    assert_eq!(start.take_irq_endpoints().len(), 1);
    let expected_info = start.take_device_info().unwrap();

    for _ in 0..2 {
        let mut update = controller
            .advance(ControllerEvent::OnlineSmp { target_queues: 1 })
            .unwrap();

        assert_eq!(update.controller_state(), ControllerState::Ready);
        assert!(update.take_queues().is_empty());
        assert!(update.take_irq_endpoints().is_empty());
        assert_eq!(update.take_device_info(), Some(expected_info));
    }
}

#[derive(Default)]
struct AcceptedIds(Vec<RequestId>);

impl SubmissionSink for AcceptedIds {
    fn accepted(&mut self, id: RequestId) {
        self.0.push(id);
    }
}

#[derive(Default)]
struct CompletedRequests(Vec<CompletedRequest>);

impl CompletionSink for CompletedRequests {
    fn complete(&mut self, request: CompletedRequest) {
        self.0.push(request);
    }
}

#[test]
fn queue_surfaces_register_retry_requested_after_a_data_irq() {
    let mut host = MockHost::new(Vec::from([ok_r1()]));
    host.complete_after_irq_register_retry = true;
    let config = BlockConfig::dma("sdmmc-test", 8, test_device_dma());
    let parts = host.into_parts();
    let mut controller = BlockDevice::new(SdMmcCard::new(parts.bus), parts.irq, config);
    let mut start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    let mut queue = start.take_queues().remove(0);
    let data = dma_api::CpuDmaBuffer::new_zero(
        test_device_dma(),
        NonZeroUsize::new(512).unwrap(),
        512,
        dma_api::DmaDirection::ToDevice,
    )
    .unwrap()
    .prepare_for_device();
    let mut batch = OwnedRequestBatch::from_iter([OwnedRequest {
        op: RequestOp::Write,
        lba: 0,
        block_count: 1,
        data: Some(data),
        flags: RequestFlags::NONE,
    }]);
    let mut accepted = AcceptedIds::default();

    let result = queue.submit_batch_owned(&mut batch, &mut accepted);
    assert_eq!(result.accepted(), 1);
    assert_eq!(result.disposition(), BatchSubmitDisposition::Continue);
    queue.commit_submissions().unwrap();

    let mut completed = CompletedRequests::default();
    queue.drain_completions(&mut completed).unwrap();

    assert!(completed.0.is_empty());
    assert_eq!(
        queue.register_retry_after(),
        Some(Duration::from_millis(1)),
        "a protocol register wait created during IRQ drain must be scheduled by the runtime"
    );
    queue.advance_register_retry(&mut completed).unwrap();
    assert_eq!(completed.0.len(), 1);
    assert_eq!(completed.0[0].result, Ok(()));
}

/// A queue engine that owns no DMA and exposes only flush barriers. The real
/// protocol scheduler and legacy transaction adapter own the maintenance flow.
struct MaintenanceEngine {
    trace: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    active: bool,
    pending: Option<RequestId>,
    failure: Option<usize>,
}

impl crate::rdif::CommandQueueAccelerator for MaintenanceEngine {
    fn provisioned_depth(&self) -> usize {
        2
    }
    fn activate(
        &mut self,
        _: crate::rdif::CommandQueueActivation,
        config: &mut BlockConfig,
    ) -> Result<(), crate::rdif::BlkError> {
        self.active = true;
        config.limits.max_inflight = 2;
        config.limits.max_submit_batch = 2;
        Ok(())
    }
    fn is_active(&self) -> bool {
        self.active
    }
    fn is_idle(&self) -> bool {
        self.pending.is_none()
    }
    fn now_ns(&self) -> u64 {
        100
    }
    fn suspend(&mut self) -> Result<(), crate::rdif::BlkError> {
        assert!(self.is_idle());
        self.trace.lock().unwrap().push(250);
        self.active = false;
        if self.failure == Some(6) {
            return Err(crate::rdif::BlkError::Io);
        }
        Ok(())
    }
    fn resume(&mut self) -> Result<(), crate::rdif::BlkError> {
        self.trace.lock().unwrap().push(251);
        if self.failure == Some(5) {
            return Err(crate::rdif::BlkError::Io);
        }
        self.active = true;
        Ok(())
    }
    fn submit_batch_owned(
        &mut self,
        batch: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> rdif_block::BatchSubmitResult {
        assert!(self.active);
        if self.pending.is_some() {
            return rdif_block::BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull);
        }
        assert_eq!(batch.front().unwrap().op, RequestOp::Flush);
        let _ = batch.pop_front();
        let id = RequestId::new(0);
        self.pending = Some(id);
        sink.accepted(id);
        rdif_block::BatchSubmitResult::new(1, BatchSubmitDisposition::Continue)
    }
    fn commit_submissions(&mut self) -> Result<(), crate::rdif::BlkError> {
        Ok(())
    }
    fn drain_completions(
        &mut self,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), crate::rdif::BlkError> {
        if let Some(id) = self.pending.take() {
            sink.complete(CompletedRequest::new(id, Ok(()), None));
        }
        Ok(())
    }
    fn shutdown(&mut self, _: &mut dyn CompletionSink) -> Result<(), crate::rdif::BlkError> {
        self.trace.lock().unwrap().push(252);
        self.active = false;
        Ok(())
    }
}

#[test]
fn maintenance_drains_engine_and_runs_legacy_trim_before_resume_or_terminal_error() {
    use std::sync::{Arc, Mutex};

    use crate::cmd::ext_csd as e;
    for failure in [
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(5),
        Some(6),
    ] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let mut cid = [0; 16];
        cid[0] = 0x15;
        cid[1..3].copy_from_slice(&0x0100_u16.to_be_bytes());
        cid[3..9].copy_from_slice(b"ABCDEF");
        let mut replies = std::vec![
            ok_r1(),
            ocr_ready_mmc_sector(),
            Response::R2(cid),
            ok_r1(),
            csd_v2_response(),
            ok_r1(),
            ok_r1(),
            ok_r1(),
            r1_tran_ready(),
            ok_r1(),
            r1_tran_ready(),
            ok_r1(),
            r1_tran_ready(),
        ];
        for stage in 0..5 {
            replies.push(if failure == Some(stage) {
                Response::R1(R1Response { raw: 1 << 15 })
            } else if stage == 4 {
                Response::R1(R1Response { raw: 7 << 9 })
            } else {
                r1_tran_ready()
            });
        }
        replies.push(r1_tran_ready());
        let mut host = MockHost::new(replies);
        host.command_trace = Some(Arc::clone(&trace));
        let mut ext = ext_csd_blob();
        for (offset, value) in [
            (e::REV, 8),
            (e::ERASE_GROUP_DEF, 1),
            (e::HC_ERASE_GRP_SIZE, 1),
            (e::SEC_FEATURE_SUPPORT, 0x10),
            (e::TRIM_MULT, 1),
            (e::CMDQ_SUPPORT, 1),
            (e::CMDQ_DEPTH, 3),
        ] {
            ext[offset] = value;
        }
        host.next_read_payload = Some(ext);
        let config = BlockConfig::dma("maintenance-test", 0, test_device_dma());
        let mut controller = BlockDevice::new_initializing_accelerated(
            SdMmcCard::new(host),
            std::boxed::Box::new(MaintenanceIrq),
            config,
            CardInitPreference::MmcFirst,
            std::boxed::Box::new(MaintenanceEngine {
                trace: Arc::clone(&trace),
                active: false,
                pending: None,
                failure,
            }),
        );
        let mut start = controller
            .advance(ControllerEvent::Start { target_queues: 1 })
            .unwrap();
        let mut queue = start.take_queues().remove(0);
        let mut completed = CompletedRequests::default();
        for _ in 0..100 {
            if queue.info().limits.max_write_zeroes_blocks != 0 {
                break;
            }
            if queue.register_retry_after().is_some() {
                queue.advance_register_retry(&mut completed).unwrap();
            } else {
                queue.drain_completions(&mut completed).unwrap();
            }
        }
        assert_ne!(queue.info().limits.max_write_zeroes_blocks, 0);
        trace.lock().unwrap().clear();
        let mut accepted = AcceptedIds::default();
        let mut flush = OwnedRequestBatch::from_iter([OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }]);
        assert_eq!(
            queue
                .submit_batch_owned(&mut flush, &mut accepted)
                .accepted(),
            1
        );
        let mut trim = OwnedRequestBatch::from_iter([OwnedRequest {
            op: RequestOp::WriteZeroes,
            lba: 1023,
            block_count: 2,
            data: None,
            flags: RequestFlags::NONE,
        }]);
        assert_eq!(
            queue
                .submit_batch_owned(&mut trim, &mut accepted)
                .disposition(),
            BatchSubmitDisposition::QueueFull
        );
        assert!(
            trace.lock().unwrap().is_empty(),
            "engine must drain before legacy maintenance"
        );
        queue.drain_completions(&mut completed).unwrap();
        completed.0.clear();
        if failure == Some(6) {
            assert!(matches!(
                queue
                    .submit_batch_owned(&mut trim, &mut accepted)
                    .disposition(),
                BatchSubmitDisposition::Fatal(_)
            ));
            assert_eq!(trim.len(), 1);
            assert!(completed.0.is_empty());
            queue.shutdown(&mut completed).unwrap();
            assert_eq!(*trace.lock().unwrap(), [250, 252]);
            continue;
        }
        assert_eq!(
            queue
                .submit_batch_owned(&mut trim, &mut accepted)
                .accepted(),
            1
        );
        for _ in 0..10 {
            if !completed.0.is_empty() {
                break;
            }
            let _ = queue.drain_completions(&mut completed);
        }
        assert_eq!(completed.0.len(), 1);
        let observed = trace.lock().unwrap().clone();
        if failure == Some(5) {
            assert!(completed.0[0].result.is_err());
            assert_eq!(observed, [250, 13, 35, 36, 38, 13, 13, 251, 252]);
        } else if let Some(stage) = failure {
            assert!(completed.0[0].result.is_err());
            let commands = [13, 35, 36, 38, 13];
            let mut expected = std::vec![250];
            expected.extend_from_slice(&commands[..=stage]);
            expected.push(252);
            assert_eq!(observed, expected);
            let mut retry = OwnedRequestBatch::from_iter([OwnedRequest {
                op: RequestOp::WriteZeroes,
                lba: 1023,
                block_count: 2,
                data: None,
                flags: RequestFlags::NONE,
            }]);
            assert!(matches!(
                queue
                    .submit_batch_owned(&mut retry, &mut accepted)
                    .disposition(),
                BatchSubmitDisposition::Fatal(_)
            ));
            assert_eq!(retry.len(), 1, "failed native zero must never replay");
        } else {
            assert_eq!(completed.0[0].result, Ok(()));
            assert_eq!(observed, [250, 13, 35, 36, 38, 13, 13, 251]);
        }
        queue.shutdown(&mut completed).unwrap();
    }
}

struct MaintenanceIrq;
impl rdif_block::HardIrqHandler for MaintenanceIrq {
    fn ack(&mut self) -> rdif_block::IrqAck {
        rdif_block::IrqAck::spurious(0)
    }
}
