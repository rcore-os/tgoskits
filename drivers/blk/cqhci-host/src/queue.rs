//! CQHCI tag allocation, submission, completion, and recovery.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use dma_api::{DeviceDma, DmaDeviceInfo, DmaDirection, InFlightDma};
use log::{info, warn};
use mbarrier::{rmb, wmb};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, CompletedRequest, CompletionSink,
    OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags, RequestId, RequestOp, SubmissionSink,
    validate_owned_request,
};
use sdmmc_protocol::rdif::{BlockConfig, CommandQueueAccelerator, CommandQueueActivation};

use crate::{
    CqhciPlatform, descriptor::DescriptorTables, irq::IrqState, read_u32, regs::*, write_u32,
};

const TRANSITION_TIMEOUT_NS: u64 = 20_000_000;

struct RequestSlot {
    request_id: Option<RequestId>,
    dma: Option<InFlightDma>,
}

struct FlushSlot {
    request_id: RequestId,
    needs_status: bool,
}

/// One CQHCI hardware queue with a single task-context owner.
pub struct CqhciQueue {
    base: usize,
    dma_info: DmaDeviceInfo,
    platform: Box<dyn CqhciPlatform>,
    irq: Arc<IrqState>,
    descriptors: Option<DescriptorTables>,
    slots: Vec<RequestSlot>,
    free_tags: Vec<usize>,
    flush: Option<FlushSlot>,
    staged: u32,
    active: bool,
    engine_armed: bool,
    data_path_restore_pending: bool,
    poisoned: bool,
    depth: usize,
    next_flush_id: usize,
    activation: Option<CommandQueueActivation>,
    info: Option<QueueInfo>,
}

impl CqhciQueue {
    /// Creates a disabled command queue over one CQHCI register file.
    ///
    /// # Safety
    ///
    /// `base` must address an exclusively owned CQHCI register file for the
    /// same physical eMMC host represented by `platform` and `dma`.
    pub(crate) unsafe fn new(
        base: core::ptr::NonNull<u8>,
        dma: DeviceDma,
        platform: Box<dyn CqhciPlatform>,
    ) -> Result<(Self, Arc<IrqState>), BlkError> {
        let descriptors = DescriptorTables::allocate(&dma)?;
        let irq = Arc::new(IrqState::new(base.as_ptr() as usize));
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(DATA_SLOTS)
            .map_err(|_| BlkError::NoMemory)?;
        slots.resize_with(DATA_SLOTS, || RequestSlot {
            request_id: None,
            dma: None,
        });
        let mut free_tags = Vec::new();
        free_tags
            .try_reserve_exact(DATA_SLOTS)
            .map_err(|_| BlkError::NoMemory)?;
        let queue = Self {
            base: base.as_ptr() as usize,
            dma_info: dma.info(),
            platform,
            irq: Arc::clone(&irq),
            descriptors: Some(descriptors),
            slots,
            free_tags,
            flush: None,
            staged: 0,
            active: false,
            engine_armed: false,
            data_path_restore_pending: false,
            poisoned: false,
            depth: 0,
            next_flush_id: usize::MAX / 2,
            activation: None,
            info: None,
        };
        Ok((queue, irq))
    }

    fn validate_activation_config(&self, config: &BlockConfig) -> Result<(), BlkError> {
        let supplied = config.limits.dma;
        let host = self.dma_info.constraints();
        let limits = supplied.constraints();
        // Safe activation cannot rebind this physical queue to another DMA
        // domain, change its cache-maintenance contract, or widen capability.
        if supplied.domain() != self.dma_info.domain()
            || supplied.coherency() != self.dma_info.coherency()
            || config.device.logical_block_size != 512
            || config.limits.max_blocks_per_request == 0
            || limits.addr_mask & !host.addr_mask != 0
            || host.align == 0
            || limits.align == 0
            || !limits.align.is_multiple_of(host.align)
            || !limits.align.is_multiple_of(4)
            || host.boundary.is_some_and(|boundary| {
                limits
                    .boundary
                    .is_none_or(|limit| limit == 0 || !boundary.is_multiple_of(limit))
            })
            || host
                .max_segment_size
                .is_some_and(|maximum| limits.max_segment_size.is_none_or(|limit| limit > maximum))
        {
            return Err(BlkError::InvalidRequest);
        }
        Ok(())
    }

    fn stage_next(&mut self, requests: &mut OwnedRequestBatch) -> Result<RequestId, BlkError> {
        let info = self.info.ok_or(BlkError::InvalidRequest)?;
        let Some(mut request) = requests.pop_front() else {
            return Err(BlkError::InvalidRequest);
        };
        if let Err(error) = validate_owned_request(info, &request) {
            requests.push_front(request);
            return Err(error);
        }
        if request.data.as_ref().is_some_and(|data| {
            !matches!(
                (request.op, data.direction()),
                (
                    RequestOp::Read,
                    DmaDirection::FromDevice | DmaDirection::Bidirectional
                ) | (
                    RequestOp::Write,
                    DmaDirection::ToDevice | DmaDirection::Bidirectional
                )
            )
        }) {
            requests.push_front(request);
            return Err(BlkError::InvalidRequest);
        }
        if request.op == RequestOp::Flush {
            return self.stage_flush(requests, request);
        }
        if self.flush.is_some() {
            requests.push_front(request);
            return Err(BlkError::Retry);
        }
        let Some(tag) = self.free_tags.pop() else {
            requests.push_front(request);
            return Err(BlkError::Retry);
        };
        let Some(segment) = request.data.as_ref().map(|data| data.segment()) else {
            self.free_tags.push(tag);
            requests.push_front(request);
            return Err(BlkError::InvalidRequest);
        };
        let Some(descriptors) = self.descriptors.as_mut() else {
            self.free_tags.push(tag);
            requests.push_front(request);
            return Err(BlkError::InvalidRequest);
        };
        if let Err(error) =
            descriptors.prepare_data(tag, request.op, request.lba, request.block_count, segment)
        {
            self.free_tags.push(tag);
            requests.push_front(request);
            return Err(error);
        }
        let dma = request.data.take().map(|dma| {
            // SAFETY: the request is installed in its tag before the staged
            // doorbell bit can be published to hardware.
            unsafe { dma.into_in_flight() }
        });
        let id = RequestId::new(tag);
        self.slots[tag].request_id = Some(id);
        self.slots[tag].dma = dma;
        self.staged |= 1 << tag;
        Ok(id)
    }

    fn stage_flush(
        &mut self,
        requests: &mut OwnedRequestBatch,
        request: OwnedRequest,
    ) -> Result<RequestId, BlkError> {
        if self.staged != 0
            || self.flush.is_some()
            || self.slots.iter().any(|slot| slot.request_id.is_some())
        {
            requests.push_front(request);
            return Err(BlkError::Retry);
        }
        let (Some(activation), Some(descriptors)) = (self.activation, self.descriptors.as_mut())
        else {
            requests.push_front(request);
            return Err(BlkError::InvalidRequest);
        };
        let id = RequestId::new(self.next_flush_id);
        self.next_flush_id = self.next_flush_id.wrapping_add(1).max(DATA_SLOTS);
        let command = activation.flush_command();
        descriptors.prepare_direct_command(
            command.index,
            command.argument,
            command.response == sdmmc_protocol::response::ResponseType::R1b,
        );
        self.flush = Some(FlushSlot {
            request_id: id,
            needs_status: activation.cache_enabled,
        });
        self.staged |= 1 << DIRECT_COMMAND_SLOT;
        Ok(id)
    }

    fn complete_tag(
        &mut self,
        tag: usize,
        result: Result<(), BlkError>,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if tag == DIRECT_COMMAND_SLOT {
            let flush = self.flush.take().ok_or(BlkError::Io)?;
            if let Some(descriptors) = self.descriptors.as_mut() {
                descriptors.clear_task(tag);
            }
            sink.complete(CompletedRequest::new(flush.request_id, result, None));
            return Ok(());
        }
        let slot = self.slots.get_mut(tag).ok_or(BlkError::Io)?;
        let id = slot.request_id.take().ok_or(BlkError::Io)?;
        let dma = slot.dma.take().map(|dma| {
            // SAFETY: TCN is the CQHCI terminal handoff for this tag.
            unsafe { dma.complete_after_quiesce() }
        });
        if let Some(descriptors) = self.descriptors.as_mut() {
            descriptors.clear_task(tag);
        }
        self.free_tags.push(tag);
        sink.complete(CompletedRequest::new(id, result, dma));
        Ok(())
    }

    fn recover_all(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.poisoned = true;
        if self.quiesce() {
            self.complete_all_after_quiesce(sink);
        } else {
            self.quarantine_all(sink);
        }
        self.staged = 0;
        Err(BlkError::Io)
    }

    fn wait_control(&self, mask: u32, expected: u32) -> bool {
        let deadline = self.platform.now_ns().saturating_add(TRANSITION_TIMEOUT_NS);
        loop {
            if read_u32(self.base, CTL) & mask == expected {
                return true;
            }
            if self.platform.now_ns() >= deadline {
                return false;
            }
            core::hint::spin_loop();
        }
    }

    fn quiesce(&mut self) -> bool {
        if !self.engine_armed {
            return true;
        }
        write_u32(self.base, ISGE, 0);
        write_u32(self.base, ISTE, 0);
        self.irq.set_active(false);
        write_u32(self.base, CTL, CTL_HALT);
        if !self.wait_control(CTL_HALT, CTL_HALT) {
            return false;
        }
        // The HALT readback alone is not our DMA ownership handoff. Require
        // the device to acknowledge task clear and read back CFG_ENABLE clear
        // before releasing any descriptor or payload memory.
        write_u32(self.base, CTL, CTL_HALT | CTL_CLEAR_ALL_TASKS);
        if !self.wait_control(CTL_CLEAR_ALL_TASKS, 0) {
            return false;
        }
        write_u32(self.base, CFG, read_u32(self.base, CFG) & !CFG_ENABLE);
        if read_u32(self.base, CFG) & CFG_ENABLE != 0 {
            return false;
        }
        rmb();
        self.engine_armed = false;
        true
    }

    fn restore_data_path(&mut self) -> Result<(), BlkError> {
        if self.data_path_restore_pending {
            self.platform.disable_data_path()?;
            self.data_path_restore_pending = false;
        }
        Ok(())
    }

    fn complete_direct_command(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let flush = self.flush.as_mut().ok_or(BlkError::Io)?;
        let result =
            CommandQueueActivation::check_response(read_u32(self.base, CRDCT), !flush.needs_status);
        if result.is_ok() && flush.needs_status {
            let command = self.activation.ok_or(BlkError::Io)?.status_command();
            self.descriptors
                .as_mut()
                .ok_or(BlkError::Io)?
                .prepare_direct_command(command.index, command.argument, false);
            flush.needs_status = false;
            wmb();
            write_u32(self.base, TDBR, 1 << DIRECT_COMMAND_SLOT);
            return Ok(());
        }
        self.complete_tag(DIRECT_COMMAND_SLOT, result, sink)
    }

    fn complete_all_after_quiesce(&mut self, sink: &mut dyn CompletionSink) {
        for tag in 0..self.depth {
            if self.slots[tag].request_id.is_some() {
                let _ = self.complete_tag(tag, Err(BlkError::Io), sink);
            }
        }
        if self.flush.is_some() {
            let _ = self.complete_tag(DIRECT_COMMAND_SLOT, Err(BlkError::Io), sink);
        }
    }

    fn quarantine_all(&mut self, sink: &mut dyn CompletionSink) {
        // Descriptor tables are device-owned too, not just payload buffers.
        if let Some(tables) = self.descriptors.take() {
            core::mem::forget(tables);
        }
        for tag in 0..self.depth {
            let slot = &mut self.slots[tag];
            let Some(id) = slot.request_id.take() else {
                continue;
            };
            if let Some(dma) = slot.dma.take() {
                let _quarantined = dma.quarantine();
            }
            sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
        }
        if let Some(flush) = self.flush.take() {
            sink.complete(CompletedRequest::new(
                flush.request_id,
                Err(BlkError::Io),
                None,
            ));
        }
        self.staged = 0;
    }
}

impl CommandQueueAccelerator for CqhciQueue {
    fn provisioned_depth(&self) -> usize {
        DATA_SLOTS
    }

    fn activate(
        &mut self,
        activation: CommandQueueActivation,
        config: &mut BlockConfig,
    ) -> Result<(), BlkError> {
        if self.active || self.poisoned || self.engine_armed {
            return Err(BlkError::InvalidRequest);
        }
        self.validate_activation_config(config)?;
        let depth = activation.card_depth.min(DATA_SLOTS);
        if depth == 0 {
            return Err(BlkError::NotSupported);
        }
        let task_list = self
            .descriptors
            .as_ref()
            .ok_or(BlkError::InvalidRequest)?
            .task_list_dma();
        self.platform.prepare(self.base)?;
        let mut cfg = read_u32(self.base, CFG);
        cfg &= !(CFG_ENABLE | CFG_TASK_DESC_128 | CFG_CRYPTO_GENERAL_ENABLE);
        cfg |= CFG_DCMD;
        write_u32(self.base, CFG, cfg);
        let stale_status = read_u32(self.base, IS);
        if stale_status != 0 {
            write_u32(self.base, IS, stale_status);
        }
        let stale_completions = read_u32(self.base, TCN);
        if stale_completions != 0 {
            write_u32(self.base, TCN, stale_completions);
        }
        write_u32(self.base, TDLBA, task_list as u32);
        write_u32(self.base, TDLBAU, (task_list >> 32) as u32);
        write_u32(self.base, SSC2, u32::from(activation.rca));
        write_u32(self.base, ISTE, 0);
        write_u32(self.base, ISGE, 0);
        self.engine_armed = true;
        write_u32(self.base, CFG, cfg | CFG_ENABLE);
        write_u32(self.base, CTL, 0);
        // Even a failed enable may have changed platform registers.
        self.data_path_restore_pending = true;
        if let Err(error) = self.platform.enable_data_path() {
            self.poisoned = true;
            if self.quiesce() {
                self.restore_data_path()?;
            }
            return Err(error);
        }
        self.depth = depth;
        self.free_tags.clear();
        self.free_tags.extend((0..depth).rev());
        self.activation = Some(activation);
        self.active = true;
        self.irq.take();
        self.irq.set_active(true);
        wmb();
        write_u32(self.base, ISTE, IS_RUNTIME_MASK);
        write_u32(self.base, ISGE, IS_RUNTIME_MASK);

        config.limits.max_inflight = depth;
        config.limits.max_submit_batch = depth;
        config.limits.max_segments = 1;
        let descriptor_blocks =
            ((TRANSFER_DESCRIPTORS_PER_SLOT - 1) * MAX_TRANSFER_DESCRIPTOR_BYTES / 512) as u32;
        config.limits.max_blocks_per_request = config
            .limits
            .max_blocks_per_request
            .min(u32::from(u16::MAX))
            .min(descriptor_blocks);
        let constraints = config.limits.dma.constraints();
        config.limits.dma = config.limits.dma.with_constraints(dma_api::DmaConstraints {
            addr_mask: constraints.addr_mask & u64::from(u32::MAX),
            max_segment_size: Some(
                constraints
                    .max_segment_size
                    .unwrap_or(usize::MAX)
                    .min(config.limits.max_blocks_per_request as usize * 512),
            ),
            ..constraints
        });
        config.limits.supports_flush = true;
        // FUA and PREFLUSH require hardware/task semantics not encoded here.
        // The runtime consumes NOWAIT independently of this capability mask.
        config.limits.supported_flags = RequestFlags::NONE;
        let info = QueueInfo {
            id: 0,
            device: config.device,
            limits: config.limits,
        };
        self.info = Some(info);
        info!(
            "cqhci: enabled version={:#x} caps={:#x} depth={} task_list={:#x}",
            read_u32(self.base, VER),
            read_u32(self.base, CAP),
            depth,
            task_list
        );
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        if !self.active || self.poisoned {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(BlkError::Io));
        }
        let mut accepted = 0;
        while accepted < self.depth && !requests.is_empty() {
            match self.stage_next(requests) {
                Ok(id) => {
                    sink.accepted(id);
                    accepted += 1;
                }
                Err(BlkError::Retry) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::QueueFull);
                }
                Err(error) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::Fatal(error));
                }
            }
        }
        BatchSubmitResult::new(accepted, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        if !self.active || self.poisoned {
            return Err(BlkError::Io);
        }
        if self.staged == 0 {
            return Ok(());
        }
        wmb();
        write_u32(self.base, TDBR, self.staged);
        self.staged = 0;
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if !self.active || self.poisoned {
            return Err(BlkError::Io);
        }
        let (status, completed) = self.irq.take();
        rmb();
        if status & IS_ERROR_MASK != 0 {
            warn!(
                "cqhci: error status={status:#x} terri={:#x} tdpe={:#x}",
                read_u32(self.base, TERRI),
                read_u32(self.base, TDPE)
            );
            return self.recover_all(sink);
        }
        let mut remaining = completed;
        let mut first_error = None;
        while remaining != 0 {
            let tag = remaining.trailing_zeros() as usize;
            remaining &= !(1 << tag);
            let result = if tag == DIRECT_COMMAND_SLOT {
                self.complete_direct_command(sink)
            } else {
                self.complete_tag(tag, Ok(()), sink)
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.poisoned = true;
        let quiesced = self.quiesce();
        if quiesced {
            self.complete_all_after_quiesce(sink);
        } else {
            self.quarantine_all(sink);
        }
        // Shutdown does not disable CMDQ_MODE_EN on the card. Keep ownership
        // after a successful activation so the protocol cannot submit legacy
        // commands to this poisoned session, even after hardware has stopped.
        if !quiesced {
            return Err(BlkError::Io);
        }
        self.restore_data_path()
    }
}

impl Drop for CqhciQueue {
    fn drop(&mut self) {
        // Safe callers may drop a live queue without explicit shutdown.
        // A timed-out engine must retain all memory that it could still read.
        let quiesced = self.quiesce();
        if quiesced && let Err(error) = self.restore_data_path() {
            // DMA is stopped, but Drop cannot report platform teardown errors.
            // Callers that need this result must use explicit shutdown.
            warn!("cqhci: failed to restore the quiesced data path on drop: {error:?}");
        }
        for slot in &mut self.slots {
            if let Some(dma) = slot.dma.take() {
                if quiesced {
                    // SAFETY: halt, task clear and disable have completed.
                    drop(unsafe { dma.complete_after_quiesce() });
                } else {
                    let _quarantined = dma.quarantine();
                }
            }
        }
        if !quiesced && let Some(tables) = self.descriptors.take() {
            core::mem::forget(tables);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };
    use std::alloc::{alloc_zeroed, dealloc};

    use dma_api::{
        CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo,
        DmaDirection, DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    };
    use rdif_block::{BatchSubmitDisposition, CompletedRequest, RequestFlags, SubmissionSink};
    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, SdMmcIrqHandle};

    use super::*;

    #[test]
    fn batch_doorbell_and_out_of_order_completion_preserve_dma_ownership() {
        let mut registers = [0u32; 32];
        let base = NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap();
        let dma = test_dma();
        let mut platform = TestPlatform::new(true);
        platform.disable_failures = 1;
        let platform_state = Arc::clone(&platform.state);
        let (mut queue, mut irq) = unsafe {
            crate::queue_and_irq(
                base,
                dma.clone(),
                Box::new(platform),
                TestLegacyIrq::default(),
            )
        }
        .unwrap();
        let mut config = BlockConfig::dma("cqhci-test", 1024, &dma);
        config.limits.supported_flags = RequestFlags::ALL_KNOWN;
        let activation = CommandQueueActivation {
            rca: 1,
            card_depth: 2,
            cache_enabled: true,
        };
        let mut foreign_config = config;
        foreign_config.limits.dma = DmaDeviceInfo::new(
            DmaDomainId::Translated(core::num::NonZeroU64::new(1).unwrap()),
            dma.info().coherency(),
            config.limits.dma.constraints(),
        );
        assert_eq!(
            queue.activate(activation, &mut foreign_config),
            Err(BlkError::InvalidRequest)
        );
        assert!(!queue.is_active());
        assert_eq!(
            read_u32(base.as_ptr() as usize, CFG),
            0,
            "a mismatched DMA capability must be rejected before touching hardware"
        );
        let mut foreign_coherency = config;
        foreign_coherency.limits.dma = DmaDeviceInfo::new(
            dma.info().domain(),
            DmaCoherency::Coherent,
            config.limits.dma.constraints(),
        );
        assert_eq!(
            queue.activate(activation, &mut foreign_coherency),
            Err(BlkError::InvalidRequest)
        );
        queue.activate(activation, &mut config).unwrap();
        let mut rejected_ids = SubmittedIds::default();
        for (op, direction) in [
            (RequestOp::Read, DmaDirection::ToDevice),
            (RequestOp::Write, DmaDirection::FromDevice),
        ] {
            let payload =
                CpuDmaBuffer::new_zero(&dma, NonZeroUsize::new(512).unwrap(), 512, direction)
                    .unwrap()
                    .prepare_for_device();
            let mut invalid = [OwnedRequest {
                op,
                lba: 0,
                block_count: 1,
                data: Some(payload),
                flags: RequestFlags::NONE,
            }]
            .into_iter()
            .collect();
            let result = queue.submit_batch_owned(&mut invalid, &mut rejected_ids);
            assert_eq!(result.accepted(), 0);
            assert_eq!(
                result.disposition(),
                BatchSubmitDisposition::Fatal(BlkError::InvalidRequest)
            );
            assert!(
                invalid.front().unwrap().data.is_some(),
                "rejection keeps DMA with the caller"
            );
        }
        let mut unsupported = read_request(&dma, 0);
        unsupported.flags = RequestFlags::FUA | RequestFlags::PREFLUSH;
        let mut unsupported = [unsupported].into_iter().collect();
        let result = queue.submit_batch_owned(&mut unsupported, &mut rejected_ids);
        assert_eq!(result.accepted(), 0);
        assert_eq!(
            result.disposition(),
            BatchSubmitDisposition::Fatal(BlkError::NotSupported)
        );
        assert!(rejected_ids.0.is_empty());
        assert_eq!(read_u32(base.as_ptr() as usize, TDBR), 0);
        let mut requests = vec![read_request(&dma, 0), read_request(&dma, 1)]
            .into_iter()
            .collect::<OwnedRequestBatch>();
        let mut submitted = SubmittedIds::default();

        let result = queue.submit_batch_owned(&mut requests, &mut submitted);
        assert_eq!(result.accepted(), 2);
        assert_eq!(result.disposition(), BatchSubmitDisposition::Continue);
        assert!(requests.is_empty());
        assert_eq!(submitted.0, vec![RequestId::new(0), RequestId::new(1)]);
        assert_eq!(read_u32(base.as_ptr() as usize, TDBR), 0);

        queue.commit_submissions().unwrap();
        assert_eq!(read_u32(base.as_ptr() as usize, TDBR), 0b11);

        write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
        write_u32(base.as_ptr() as usize, TCN, 1 << 1);
        let ack = irq.ack();
        assert!(ack.control_event().is_empty());
        write_u32(base.as_ptr() as usize, IS, 0);
        write_u32(base.as_ptr() as usize, TCN, 0);
        let mut completed = CompletedRequests::default();
        queue.drain_completions(&mut completed).unwrap();
        assert_eq!(completed.0.len(), 1);
        assert_eq!(completed.0[0].id, RequestId::new(1));
        assert!(completed.0[0].result.is_ok());
        assert!(completed.0[0].data.is_some());

        write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
        write_u32(base.as_ptr() as usize, TCN, 1);
        let _ = irq.ack();
        write_u32(base.as_ptr() as usize, IS, 0);
        write_u32(base.as_ptr() as usize, TCN, 0);
        queue.drain_completions(&mut completed).unwrap();
        assert_eq!(completed.0.len(), 2);
        assert_eq!(completed.0[1].id, RequestId::new(0));
        assert!(completed.0[1].result.is_ok());
        assert!(completed.0[1].data.is_some());

        write_u32(base.as_ptr() as usize, TDBR, 0);
        let mut barrier_batch = vec![read_request(&dma, 2), flush_request()]
            .into_iter()
            .collect::<OwnedRequestBatch>();
        let result = queue.submit_batch_owned(&mut barrier_batch, &mut submitted);
        assert_eq!(result.accepted(), 1);
        assert_eq!(result.disposition(), BatchSubmitDisposition::QueueFull);
        assert_eq!(barrier_batch.len(), 1);
        assert_eq!(barrier_batch.front().unwrap().op, RequestOp::Flush);
        queue.commit_submissions().unwrap();
        assert_eq!(read_u32(base.as_ptr() as usize, TDBR), 1);

        write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
        write_u32(base.as_ptr() as usize, TCN, 1);
        let _ = irq.ack();
        write_u32(base.as_ptr() as usize, IS, 0);
        write_u32(base.as_ptr() as usize, TCN, 0);
        queue.drain_completions(&mut completed).unwrap();

        write_u32(base.as_ptr() as usize, TDBR, 0);
        let result = queue.submit_batch_owned(&mut barrier_batch, &mut submitted);
        assert_eq!(result.accepted(), 1);
        assert_eq!(result.disposition(), BatchSubmitDisposition::Continue);
        queue.commit_submissions().unwrap();
        assert_eq!(
            read_u32(base.as_ptr() as usize, TDBR),
            1 << DIRECT_COMMAND_SLOT
        );

        write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
        write_u32(base.as_ptr() as usize, TCN, 1 << DIRECT_COMMAND_SLOT);
        let _ = irq.ack();
        write_u32(base.as_ptr() as usize, IS, 0);
        write_u32(base.as_ptr() as usize, TCN, 0);
        queue.drain_completions(&mut completed).unwrap();
        assert_eq!(
            completed.0.len(),
            3,
            "cache flush must wait for final card status"
        );
        assert_eq!(
            read_u32(base.as_ptr() as usize, TDBR),
            1 << DIRECT_COMMAND_SLOT
        );
        write_u32(base.as_ptr() as usize, CRDCT, (4 << 9) | (1 << 8));
        write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
        write_u32(base.as_ptr() as usize, TCN, 1 << DIRECT_COMMAND_SLOT);
        let _ = irq.ack();
        write_u32(base.as_ptr() as usize, IS, 0);
        write_u32(base.as_ptr() as usize, TCN, 0);
        queue.drain_completions(&mut completed).unwrap();
        assert_eq!(completed.0.len(), 4);
        assert!(completed.0[2].data.is_some());
        assert!(completed.0[3].result.is_ok());
        assert!(completed.0[3].data.is_none());
        assert_eq!(queue.shutdown(&mut completed), Err(BlkError::Io));
        assert!(platform_state.data_path_enabled.load(Ordering::Relaxed));
        assert_eq!(platform_state.disable_attempts.load(Ordering::Relaxed), 1);
        for _ in 0..2 {
            queue.shutdown(&mut completed).unwrap();
            assert!(
                queue.is_active(),
                "shutdown must keep card-session ownership"
            );
            let mut rejected = [flush_request()].into_iter().collect();
            let result = queue.submit_batch_owned(&mut rejected, &mut rejected_ids);
            assert_eq!(result.accepted(), 0);
            assert_eq!(
                result.disposition(),
                BatchSubmitDisposition::Fatal(BlkError::Io)
            );
            assert_eq!(rejected.len(), 1);
            assert_eq!(queue.commit_submissions(), Err(BlkError::Io));
        }
        assert!(rejected_ids.0.is_empty());
        assert_eq!(completed.0.len(), 4);
        drop(queue);
        assert!(!platform_state.data_path_enabled.load(Ordering::Relaxed));
        assert_eq!(
            platform_state.disable_attempts.load(Ordering::Relaxed),
            2,
            "retry a failed restoration, but do not repeat a successful one"
        );
    }

    #[test]
    fn drop_restores_only_started_data_paths_and_reclaims_quiesced_dma() {
        // None covers an untouched queue; the two results cover successful
        // activation and failure after the platform has changed host state.
        for enable_result in [None, Some(Ok(())), Some(Err(BlkError::Io))] {
            let backend = Box::leak(Box::new(TestDma::default()));
            let dma = DeviceDma::new(test_dma().info(), backend);
            let mut registers = [0u32; 32];
            let base = NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap();
            let mut platform = TestPlatform::new(true);
            platform.fail_enable = enable_result == Some(Err(BlkError::Io));
            let platform_state = Arc::clone(&platform.state);
            // SAFETY: the aligned register array outlives both endpoints.
            // All accesses are serialized and no physical DMA is performed.
            let (mut queue, irq) = unsafe {
                crate::queue_and_irq(
                    base,
                    dma.clone(),
                    Box::new(platform),
                    TestLegacyIrq::default(),
                )
            }
            .unwrap();
            if let Some(expected) = enable_result {
                assert_eq!(
                    queue.activate(
                        CommandQueueActivation {
                            rca: 1,
                            card_depth: 2,
                            cache_enabled: false,
                        },
                        &mut BlockConfig::dma("drop-lifecycle", 1024, &dma),
                    ),
                    expected
                );
                if expected.is_ok() {
                    let mut batch = [read_request(&dma, 0)].into_iter().collect();
                    let mut submitted = SubmittedIds::default();
                    assert_eq!(
                        queue
                            .submit_batch_owned(&mut batch, &mut submitted)
                            .accepted(),
                        1
                    );
                    queue.commit_submissions().unwrap();
                    assert!(batch.is_empty());
                } else {
                    assert!(!queue.is_active());
                    assert!(!platform_state.data_path_enabled.load(Ordering::Relaxed));
                }
            }
            // In particular, the live queue has no explicit shutdown here.
            drop(queue);
            drop(irq);
            assert!(!platform_state.data_path_enabled.load(Ordering::Relaxed));
            assert_eq!(
                platform_state.disable_attempts.load(Ordering::Relaxed),
                usize::from(enable_result.is_some()),
                "restore every attempted platform enable exactly once"
            );
            assert_eq!(
                backend.deallocations.load(Ordering::Relaxed),
                backend.allocations.load(Ordering::Relaxed),
                "a quiesced queue must release both descriptors and any payload"
            );
        }
    }

    #[test]
    fn failed_quiesce_quarantines_descriptors_and_payload_and_closes_queue() {
        for legacy_error in [false, true] {
            let backend = Box::leak(Box::new(TestDma::default()));
            let dma = DeviceDma::new(
                DmaDeviceInfo::new(
                    DmaDomainId::Direct,
                    DmaCoherency::NonCoherent,
                    DmaConstraints::new(u32::MAX as u64),
                ),
                backend,
            );
            let mut registers = [0u32; 32];
            let base = NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap();
            let platform = TestPlatform::new(false);
            let platform_state = Arc::clone(&platform.state);
            // SAFETY: the aligned register array outlives both endpoints and all
            // accesses are serialized by this test. No physical DMA is performed.
            let (mut queue, mut irq) = unsafe {
                crate::queue_and_irq(
                    base,
                    dma.clone(),
                    Box::new(platform),
                    TestLegacyIrq(if legacy_error {
                        HostEventKind::Error
                    } else {
                        HostEventKind::None
                    }),
                )
            }
            .unwrap();
            queue
                .activate(
                    CommandQueueActivation {
                        rca: 1,
                        card_depth: 2,
                        cache_enabled: false,
                    },
                    &mut BlockConfig::dma("quarantine", 1024, &dma),
                )
                .unwrap();
            let mut batch = [read_request(&dma, 0)].into_iter().collect();
            let mut submitted = SubmittedIds::default();
            assert_eq!(
                queue
                    .submit_batch_owned(&mut batch, &mut submitted)
                    .accepted(),
                1
            );
            queue.commit_submissions().unwrap();
            let (status, completed_tags) = if legacy_error {
                (0, 0)
            } else {
                (IS_RESPONSE_ERROR | IS_TASK_COMPLETE, 1)
            };
            write_u32(base.as_ptr() as usize, IS, status);
            write_u32(base.as_ptr() as usize, TCN, completed_tags);
            let ack = irq.ack();
            assert!(!ack.is_spurious());
            assert!(ack.queues().contains(0));
            let mut completed = CompletedRequests::default();
            assert_eq!(queue.drain_completions(&mut completed), Err(BlkError::Io));
            assert_eq!(completed.0.len(), 1);
            assert_eq!(completed.0[0].result, Err(BlkError::Io));
            assert!(
                completed.0[0].data.is_none(),
                "unquiesced memory must not return to the CPU"
            );
            // A genuinely new late interrupt must not revive the poisoned tag.
            write_u32(base.as_ptr() as usize, IS, IS_TASK_COMPLETE);
            write_u32(base.as_ptr() as usize, TCN, 1);
            irq.ack();
            assert_eq!(queue.drain_completions(&mut completed), Err(BlkError::Io));
            assert_eq!(
                completed.0.len(),
                1,
                "a stale event cannot complete the tag twice"
            );
            assert_eq!(queue.commit_submissions(), Err(BlkError::Io));
            assert_eq!(queue.shutdown(&mut completed), Err(BlkError::Io));
            assert!(
                queue.is_active(),
                "failed shutdown must keep card-session ownership"
            );
            let mut rejected = [flush_request()].into_iter().collect();
            let mut rejected_ids = SubmittedIds::default();
            let result = queue.submit_batch_owned(&mut rejected, &mut rejected_ids);
            assert_eq!(result.accepted(), 0);
            assert_eq!(
                result.disposition(),
                BatchSubmitDisposition::Fatal(BlkError::Io)
            );
            assert_eq!(rejected.len(), 1);
            assert!(rejected_ids.0.is_empty());
            assert_eq!(completed.0.len(), 1);
            drop(queue);
            drop(irq);
            assert_eq!(
                read_u32(base.as_ptr() as usize, CTL) & (CTL_HALT | CTL_CLEAR_ALL_TASKS),
                CTL_HALT | CTL_CLEAR_ALL_TASKS,
                "HALT readback cannot replace the device's task-clear acknowledgement"
            );
            assert_ne!(read_u32(base.as_ptr() as usize, CFG) & CFG_ENABLE, 0);
            assert!(platform_state.data_path_enabled.load(Ordering::Relaxed));
            assert_eq!(platform_state.disable_attempts.load(Ordering::Relaxed), 0);
            assert_eq!(
                backend.deallocations.load(Ordering::Relaxed),
                0,
                "neither descriptor table nor payload may be freed after failed task clear"
            );
        }
    }

    fn read_request(dma: &DeviceDma, lba: u64) -> OwnedRequest {
        let data = CpuDmaBuffer::new_zero(
            dma,
            NonZeroUsize::new(512).unwrap(),
            512,
            DmaDirection::FromDevice,
        )
        .unwrap()
        .prepare_for_device();
        OwnedRequest {
            op: RequestOp::Read,
            lba,
            block_count: 1,
            data: Some(data),
            flags: RequestFlags::NONE,
        }
    }

    fn flush_request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }

    #[derive(Default)]
    struct SubmittedIds(Vec<RequestId>);

    impl SubmissionSink for SubmittedIds {
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

    #[derive(Default)]
    struct PlatformState {
        data_path_enabled: AtomicBool,
        disable_attempts: AtomicUsize,
    }

    struct TestPlatform {
        base: usize,
        clear_tasks: bool,
        time: AtomicU64,
        state: Arc<PlatformState>,
        fail_enable: bool,
        disable_failures: usize,
    }

    impl TestPlatform {
        fn new(clear_tasks: bool) -> Self {
            Self {
                base: 0,
                clear_tasks,
                time: AtomicU64::new(0),
                state: Arc::new(PlatformState::default()),
                fail_enable: false,
                disable_failures: 0,
            }
        }
    }

    impl CqhciPlatform for TestPlatform {
        fn now_ns(&self) -> u64 {
            // Only the device's self-clearing control bit is emulated. Queue
            // ownership and completion run through the production methods.
            if self.clear_tasks && self.base != 0 {
                let control = read_u32(self.base, CTL);
                write_u32(self.base, CTL, control & !CTL_CLEAR_ALL_TASKS);
            }
            self.time.fetch_add(1_000_000, Ordering::Relaxed)
        }
        fn prepare(&mut self, cqhci_base: usize) -> Result<(), BlkError> {
            self.base = cqhci_base;
            Ok(())
        }

        fn enable_data_path(&mut self) -> Result<(), BlkError> {
            self.state.data_path_enabled.store(true, Ordering::Relaxed);
            if self.fail_enable {
                Err(BlkError::Io)
            } else {
                Ok(())
            }
        }

        fn disable_data_path(&mut self) -> Result<(), BlkError> {
            assert_ne!(self.base, 0, "an untouched platform must not be restored");
            assert_eq!(read_u32(self.base, CTL) & CTL_CLEAR_ALL_TASKS, 0);
            assert_eq!(read_u32(self.base, CFG) & CFG_ENABLE, 0);
            self.state.disable_attempts.fetch_add(1, Ordering::Relaxed);
            if self.disable_failures != 0 {
                self.disable_failures -= 1;
                return Err(BlkError::Io);
            }
            self.state.data_path_enabled.store(false, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestLegacyIrq(HostEventKind);

    #[derive(Default)]
    struct TestLegacyEvent(HostEventKind);

    impl HostEvent for TestLegacyEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }
    }

    impl SdMmcIrqHandle for TestLegacyIrq {
        type Event = TestLegacyEvent;

        fn handle_irq(&mut self) -> Self::Event {
            TestLegacyEvent(core::mem::take(&mut self.0))
        }
    }

    #[derive(Default)]
    struct TestDma {
        allocations: AtomicUsize,
        deallocations: AtomicUsize,
    }

    static TEST_DMA: TestDma = TestDma {
        allocations: AtomicUsize::new(0),
        deallocations: AtomicUsize::new(0),
    };
    static NEXT_DMA_ADDRESS: AtomicU64 = AtomicU64::new(0x1000_0000);

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let handle = allocate_test_dma(constraints, layout)?;
            self.allocations.fetch_add(1, Ordering::Relaxed);
            Some(handle)
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            self.deallocations.fetch_add(1, Ordering::Relaxed);
            // SAFETY: the allocation was created by this backend with the
            // exact layout stored in the consumed handle.
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let handle = allocate_test_dma(constraints, layout)?;
            self.allocations.fetch_add(1, Ordering::Relaxed);
            Some(handle)
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            unsafe { self.dealloc_contiguous(handle) };
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            Err(DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    fn allocate_test_dma(constraints: DmaConstraints, layout: Layout) -> Option<DmaAllocHandle> {
        let alignment = constraints.align.max(layout.align());
        let layout = Layout::from_size_align(layout.size(), alignment).ok()?;
        // SAFETY: `layout` is non-zero and valid; the returned pointer is
        // released with the same layout by the test DMA backend.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        let dma_address = NEXT_DMA_ADDRESS.fetch_add(0x0100_0000, Ordering::Relaxed);
        // SAFETY: this fake device defines the synthetic aligned bus address
        // as the mapping of the live allocation for the duration of the test.
        Some(unsafe { DmaAllocHandle::new(ptr, ptr, dma_address.into(), layout) })
    }

    fn test_dma() -> DeviceDma {
        DeviceDma::new(
            DmaDeviceInfo::new(
                DmaDomainId::Direct,
                DmaCoherency::NonCoherent,
                DmaConstraints::new(u32::MAX as u64),
            ),
            &TEST_DMA,
        )
    }
}
