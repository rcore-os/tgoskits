use alloc::sync::Arc;
use core::{
    mem,
    sync::atomic::{Ordering, fence},
};

use dma_api::{DmaDirection, InFlightDma};
use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, DmaQuiesced, IQueue, IdList, OwnedRequest,
    QueueEventBatch, QueueExecution, QueueInfo, QueueKind, QueueLimits, RequestFlags, RequestId,
    RequestOp, ServiceProgress, ServiceRerunReason, SubmitError, SubmitOutcome,
    validate_owned_request,
};

use crate::{
    ata::AtaDevice,
    command::{PortCommandMemory, max_prdt_bytes},
    irq::{HostShared, IRQ_SNAPSHOT_CAPACITY},
    quarantine::{AhciDmaQuarantine, AhciDmaQuarantineReason},
    registers::{PX_CI, PX_IE, write_port},
};

const COMMAND_SLOT: usize = 0;

pub(crate) struct ReadyPort {
    pub port: usize,
    pub ata: AtaDevice,
    pub command_memory: PortCommandMemory,
}

impl ReadyPort {
    pub(crate) fn queue_info(
        &self,
        name: &'static str,
        dma_mask: u64,
        dma_domain: dma_api::DmaDomainId,
        irq_source_id: usize,
        request_timeout_ns: u64,
    ) -> QueueInfo {
        queue_info(
            self.port,
            self.ata,
            name,
            dma_mask,
            dma_domain,
            irq_source_id,
            request_timeout_ns,
        )
    }

    pub(crate) fn into_quarantine(
        self,
        shared: &Arc<HostShared>,
        controller_cookie: usize,
        reason: AhciDmaQuarantineReason,
    ) -> AhciDmaQuarantine {
        AhciDmaQuarantine::new(
            self.port,
            reason,
            controller_cookie,
            self.command_memory,
            None,
            shared,
        )
    }
}

pub(crate) struct AhciPortQueue {
    info: QueueInfo,
    ata: AtaDevice,
    command_memory: QueueDmaOwner,
    shared: Arc<HostShared>,
    controller_cookie: usize,
    epoch: u64,
    last_reclaim_epoch: Option<u64>,
    destroyable_epoch: Option<u64>,
    inflight: Option<InflightRequest>,
    pending_completion_generation: Option<u64>,
}

enum QueueDmaOwner {
    Live(PortCommandMemory),
    Quarantined(AhciDmaQuarantine),
    Released,
}

impl QueueDmaOwner {
    fn live_mut(&mut self) -> Option<&mut PortCommandMemory> {
        match self {
            Self::Live(command_memory) => Some(command_memory),
            Self::Quarantined(quarantine) => {
                let _retained_owner = quarantine;
                None
            }
            Self::Released => None,
        }
    }

    const fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct QueueBinding {
    pub name: &'static str,
    pub dma_mask: u64,
    pub dma_domain: dma_api::DmaDomainId,
    pub irq_source_id: usize,
    pub request_timeout_ns: u64,
    pub controller_cookie: usize,
}

impl QueueBinding {
    pub(crate) fn queue_info(self, port: usize, ata: AtaDevice) -> QueueInfo {
        queue_info(
            port,
            ata,
            self.name,
            self.dma_mask,
            self.dma_domain,
            self.irq_source_id,
            self.request_timeout_ns,
        )
    }
}

impl AhciPortQueue {
    pub(crate) fn new(ready: ReadyPort, shared: Arc<HostShared>, binding: QueueBinding) -> Self {
        let info = ready.queue_info(
            binding.name,
            binding.dma_mask,
            binding.dma_domain,
            binding.irq_source_id,
            binding.request_timeout_ns,
        );
        let epoch = shared.port(ready.port).epoch();
        Self {
            info,
            ata: ready.ata,
            command_memory: QueueDmaOwner::Live(ready.command_memory),
            shared,
            controller_cookie: binding.controller_cookie,
            epoch,
            // A proof must be newer than the epoch in which this queue and
            // its command memory were published. Accepting that same epoch
            // would let a stale lifecycle token reclaim a live request.
            last_reclaim_epoch: Some(epoch),
            destroyable_epoch: None,
            inflight: None,
            pending_completion_generation: None,
        }
    }

    fn prepare_request(
        &mut self,
        id: RequestId,
        mut request: OwnedRequest,
    ) -> Result<InflightRequest, SubmitError> {
        if let Err(error) = validate_owned_request(self.info, &request) {
            return Err(SubmitError::new(id, error, request));
        }
        if !self.shared.port(self.info.id).is_online() {
            return Err(SubmitError::new(id, BlkError::Offline, request));
        }

        let prepared_dma = match request.op {
            RequestOp::Read | RequestOp::Write => {
                let Some(buffer) = request.data.take() else {
                    return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
                };
                if !direction_matches(request.op, buffer.direction()) {
                    request.data = Some(buffer);
                    return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
                }
                if buffer.domain_id() != self.info.limits.dma_domain {
                    request.data = Some(buffer);
                    return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
                }
                Some(buffer.prepare_for_device())
            }
            RequestOp::Flush => None,
            RequestOp::Discard | RequestOp::WriteZeroes => {
                return Err(SubmitError::new(id, BlkError::NotSupported, request));
            }
        };

        let data = prepared_dma
            .as_ref()
            .map(|dma| (dma.dma_addr(), dma.len().get()));
        let command_memory = self
            .command_memory
            .live_mut()
            .expect("an admitted AHCI request retains live command memory");
        if let Err(error) = command_memory.build_io(
            request.op,
            request.lba,
            request.block_count,
            data,
            self.ata.lba48,
        ) {
            if let Some(prepared) = prepared_dma {
                request.data = Some(prepared.into_cpu_buffer());
            }
            return Err(SubmitError::new(id, error, request));
        }

        let dma = prepared_dma.map(|prepared| unsafe {
            // SAFETY: the request is published in `self.inflight` before the
            // PxCI doorbell. Only an acknowledged IRQ snapshot or a linear
            // controller quiescence proof can return this ownership.
            prepared.into_in_flight()
        });
        Ok(InflightRequest {
            id,
            request,
            dma,
            generation: 0,
        })
    }

    fn complete_from_snapshot(
        &mut self,
        generation: u64,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if !self
            .shared
            .port(self.info.id)
            .clear_active_request(generation)
        {
            return Err(BlkError::Io);
        }
        let Some(mut inflight) = self.inflight.take() else {
            return Err(BlkError::Io);
        };
        if let Some(dma) = inflight.dma.take() {
            let completed = unsafe {
                // SAFETY: the IRQ endpoint observed PxCI clear for this slot
                // after acknowledging the command-completion source. AHCI then
                // no longer owns the command's data PRDT backing.
                dma.complete_after_quiesce()
            };
            inflight.request.data = Some(completed.into_cpu_buffer());
        }
        sink.complete(CompletedRequest::new(inflight.id, Ok(()), inflight.request));
        Ok(())
    }

    fn return_before_doorbell(&mut self) -> Option<OwnedRequest> {
        let mut inflight = self.inflight.take()?;
        if let Some(dma) = inflight.dma.take() {
            let completed = unsafe {
                // SAFETY: publication of the active request failed before the
                // PxCI doorbell was written, so hardware never observed this
                // PRDT and cannot own the data buffer.
                dma.complete_after_quiesce()
            };
            inflight.request.data = Some(completed.into_cpu_buffer());
        }
        Some(inflight.request)
    }

    fn return_after_controller_quiesce(&mut self, sink: &mut dyn CompletionSink) {
        let Some(mut inflight) = self.inflight.take() else {
            return;
        };
        if let Some(dma) = inflight.dma.take() {
            let completed = unsafe {
                // SAFETY: the caller supplied the controller-bound linear
                // DmaQuiesced proof before entering this helper.
                dma.complete_after_quiesce()
            };
            inflight.request.data = Some(completed.into_cpu_buffer());
        }
        sink.complete(CompletedRequest::new(
            inflight.id,
            Err(BlkError::Cancelled),
            inflight.request,
        ));
    }
}

impl IQueue for AhciPortQueue {
    fn id(&self) -> usize {
        self.info.id
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if !self.command_memory.is_live() {
            return Err(SubmitError::new(id, BlkError::Offline, request));
        }
        if self.inflight.is_some() {
            return Err(SubmitError::new(id, BlkError::Retry, request));
        }
        if self.shared.port(self.info.id).active_request_generation() != 0 {
            return Err(SubmitError::new(id, BlkError::Retry, request));
        }

        let inflight = self.prepare_request(id, request)?;
        self.inflight = Some(inflight);
        let Some(_register_window) = self.shared.try_claim_register_window() else {
            let request = self
                .return_before_doorbell()
                .expect("the just-prepared AHCI request remains locally owned");
            return Err(SubmitError::new(id, BlkError::Retry, request));
        };

        let generation = self.shared.port(self.info.id).next_request_generation();
        self.inflight
            .as_mut()
            .expect("the prepared AHCI request remains locally owned")
            .generation = generation;
        if !self
            .shared
            .port(self.info.id)
            .publish_active_request(generation)
        {
            drop(_register_window);
            let request = self
                .return_before_doorbell()
                .expect("the just-prepared AHCI request remains locally owned");
            return Err(SubmitError::new(id, BlkError::Retry, request));
        }
        fence(Ordering::Release);
        write_port(
            self.shared.registers(),
            self.info.id,
            PX_CI,
            1 << COMMAND_SLOT,
        );
        Ok(SubmitOutcome::Queued)
    }

    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if events.queue_id() != self.id() {
            return Err(BlkError::InvalidRequest);
        }
        let port_id = self.id();
        if self.shared.port(port_id).take_overflow() {
            self.pending_completion_generation = None;
            freeze_port(&self.shared, port_id);
            if let Some(inflight) = self.inflight.as_ref() {
                self.shared
                    .port(port_id)
                    .clear_active_request(inflight.generation);
            }
            return Err(BlkError::Io);
        }

        let mut processed = 0;
        let mut completion_generation = self.pending_completion_generation.take();
        while processed < IRQ_SNAPSHOT_CAPACITY {
            let Some(snapshot) = self.shared.port(port_id).pop_snapshot() else {
                break;
            };
            processed += 1;
            if snapshot.epoch != self.epoch {
                continue;
            }
            if snapshot.has_error() {
                self.pending_completion_generation = None;
                freeze_port(&self.shared, port_id);
                if let Some(inflight) = self.inflight.as_ref() {
                    self.shared
                        .port(port_id)
                        .clear_active_request(inflight.generation);
                }
                return Err(BlkError::Io);
            }
            if let Some(generation) = self.inflight.as_ref().map(|inflight| inflight.generation)
                && snapshot.completes(COMMAND_SLOT, generation)
            {
                completion_generation = Some(generation);
            }
        }

        if self.shared.port(port_id).has_snapshots() {
            // Keep ownership local until every already-published IRQ fact has
            // been classified. A later error in the bounded continuation must
            // win over an earlier command-complete snapshot.
            self.pending_completion_generation = completion_generation;
            return Ok(events.requeue_service(ServiceRerunReason::RetainedFacts));
        }
        if let Some(generation) = completion_generation {
            self.complete_from_snapshot(generation, sink)?;
        }
        Ok(ServiceProgress::Idle)
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if proof.controller_cookie() != self.controller_cookie {
            return Err(BlkError::InvalidDmaProof);
        }
        if self
            .last_reclaim_epoch
            .is_some_and(|last_epoch| proof.epoch().get() <= last_epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        self.return_after_controller_quiesce(sink);
        self.pending_completion_generation = None;
        self.epoch = proof.epoch().get();
        self.last_reclaim_epoch = Some(self.epoch);
        self.destroyable_epoch = Some(self.epoch);
        let port = self.shared.port(self.id());
        port.clear_any_active_request();
        port.discard_stale_snapshots();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        if self.inflight.is_some() || self.shared.port(self.id()).is_online() {
            return Err(BlkError::Busy);
        }
        if self.destroyable_epoch != self.last_reclaim_epoch || self.destroyable_epoch.is_none() {
            return Err(BlkError::InvalidDmaProof);
        }
        let owner = mem::replace(&mut self.command_memory, QueueDmaOwner::Released);
        let QueueDmaOwner::Live(command_memory) = owner else {
            self.command_memory = owner;
            return Err(BlkError::Offline);
        };
        // The matching controller proof was consumed by
        // `reclaim_after_quiesce`, so ordinary Rust destruction is safe here.
        drop(command_memory);
        Ok(())
    }
}

impl Drop for AhciPortQueue {
    fn drop(&mut self) {
        let owner = mem::replace(&mut self.command_memory, QueueDmaOwner::Released);
        let QueueDmaOwner::Live(command_memory) = owner else {
            self.command_memory = owner;
            return;
        };
        self.shared.port(self.info.id).set_online(false);
        let data_dma = self
            .inflight
            .as_mut()
            .and_then(|inflight| inflight.dma.take());
        self.command_memory = QueueDmaOwner::Quarantined(AhciDmaQuarantine::new(
            self.info.id,
            AhciDmaQuarantineReason::QueueAbandoned,
            self.controller_cookie,
            command_memory,
            data_dma,
            &self.shared,
        ));
    }
}

struct InflightRequest {
    id: RequestId,
    request: OwnedRequest,
    dma: Option<InFlightDma>,
    generation: u64,
}

fn queue_info(
    port: usize,
    ata: AtaDevice,
    name: &'static str,
    dma_mask: u64,
    dma_domain: dma_api::DmaDomainId,
    irq_source_id: usize,
    request_timeout_ns: u64,
) -> QueueInfo {
    let max_blocks = (max_prdt_bytes() / ata.logical_block_size).min(if ata.lba48 {
        u16::MAX as usize + 1
    } else {
        256
    });
    let mut limits = QueueLimits::simple(ata.logical_block_size, dma_mask);
    limits.dma_domain = dma_domain;
    limits.dma_alignment = 2;
    limits.max_blocks_per_request = u32::try_from(max_blocks).unwrap_or(u32::MAX);
    limits.max_segment_size = max_prdt_bytes();
    limits.request_timeout_ns = request_timeout_ns;
    limits.supports_flush = ata.flush;
    limits.supported_flags = RequestFlags::SYNC | RequestFlags::META | RequestFlags::NOWAIT;

    let mut sources = IdList::none();
    sources.insert(irq_source_id);
    QueueInfo {
        id: port,
        device: ata.device_info(name),
        limits,
        kind: QueueKind::Interrupt { sources },
        execution: QueueExecution::Serialized,
    }
}

fn direction_matches(op: RequestOp, direction: DmaDirection) -> bool {
    match op {
        RequestOp::Read => matches!(
            direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Write => matches!(
            direction,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ),
        _ => true,
    }
}

pub(crate) fn freeze_port(shared: &HostShared, port: usize) {
    shared.port(port).set_online(false);
    write_port(shared.registers(), port, PX_IE, 0);
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::num::NonZeroUsize;

    use dma_api::{CpuDmaBuffer, DmaDirection};
    use rdif_block::{BIrqEndpoint, ControllerEpoch, Event, IrqCapture, RequestFlags};

    use super::*;
    use crate::{
        registers::{
            HOST_IS, IRQ_D2H_REG_FIS, IRQ_TASK_FILE_ERROR, MMIO_REQUIRED_SIZE, PX_CI, PX_IS,
            PX_SERR, PX_TFD, TFD_ERR, port_offset, tests_support::FakeRegisters,
        },
        test_support::TEST_DMA,
    };

    #[test]
    fn port_queue_contract_is_serialized_and_interrupt_only() {
        let ata = AtaDevice {
            num_blocks: 4096,
            logical_block_size: 512,
            lba48: true,
            flush: true,
        };
        let info = queue_info(
            3,
            ata,
            "test-ahci",
            u64::MAX,
            dma_api::DmaDomainId::legacy_global(),
            7,
            1_000,
        );

        assert_eq!(info.execution, QueueExecution::Serialized);
        let QueueKind::Interrupt { sources } = info.kind else {
            panic!("AHCI queue cannot be inline");
        };
        assert!(sources.contains(7));
    }

    #[test]
    fn stale_same_epoch_snapshot_cannot_complete_the_next_request() {
        let (mut queue, shared, registers, mut irq) = test_queue();
        let mut completions = TestCompletions::default();

        assert!(matches!(
            queue.submit_owned(RequestId::new(1), flush_request()),
            Ok(SubmitOutcome::Queued)
        ));
        let first_event = complete_slot(&registers, irq.as_mut(), IRQ_D2H_REG_FIS, 0);
        let first_batch = first_event.for_queue(0).unwrap();
        queue
            .service_events(&first_batch, &mut completions)
            .unwrap();
        assert_eq!(completions.ids(), [RequestId::new(1)]);

        // A non-command FIS can be acknowledged after the old request is
        // disarmed but before the next direct submission. Its stable snapshot
        // carries generation zero and must not be reinterpreted later.
        let stale_event = complete_slot(&registers, irq.as_mut(), IRQ_D2H_REG_FIS, 0);
        assert!(matches!(
            queue.submit_owned(RequestId::new(2), flush_request()),
            Ok(SubmitOutcome::Queued)
        ));
        let stale_batch = stale_event.for_queue(0).unwrap();
        queue
            .service_events(&stale_batch, &mut completions)
            .unwrap();
        assert_eq!(completions.ids(), [RequestId::new(1)]);

        let second_event = complete_slot(&registers, irq.as_mut(), IRQ_D2H_REG_FIS, 0);
        let second_batch = second_event.for_queue(0).unwrap();
        queue
            .service_events(&second_batch, &mut completions)
            .unwrap();
        assert_eq!(completions.ids(), [RequestId::new(1), RequestId::new(2)]);

        // Re-servicing the same routing hint cannot return ownership twice.
        queue
            .service_events(&second_batch, &mut completions)
            .unwrap();
        assert_eq!(completions.ids().len(), 2);
        shared.port(0).set_online(false);
    }

    #[test]
    fn submit_does_not_publish_request_while_irq_owns_register_window() {
        let (mut queue, shared, registers, _irq) = test_queue();
        let _irq_window = shared
            .try_claim_register_window()
            .expect("test must own the destructive IRQ register window");
        registers.clear_access_log();

        let error = queue
            .submit_owned(RequestId::new(3), flush_request())
            .expect_err("submission must defer while IRQ capture owns the HBA window");

        assert_eq!(error.error(), BlkError::Retry);
        assert_eq!(shared.port(0).active_request_generation(), 0);
        assert!(
            registers
                .writes()
                .iter()
                .all(|write| write.offset != port_offset(0, PX_CI)),
            "a deferred submission must not ring the command doorbell"
        );
    }

    #[test]
    fn combined_error_and_completion_never_publishes_success() {
        let (mut queue, _shared, registers, mut irq) = test_queue();
        let mut completions = TestCompletions::default();
        assert!(matches!(
            queue.submit_owned(RequestId::new(9), flush_request()),
            Ok(SubmitOutcome::Queued)
        ));

        registers.set(port_offset(0, PX_SERR), 0x10);
        let event = complete_slot(
            &registers,
            irq.as_mut(),
            IRQ_D2H_REG_FIS | IRQ_TASK_FILE_ERROR,
            TFD_ERR,
        );
        let batch = event.for_queue(0).unwrap();
        let result = queue.service_events(&batch, &mut completions);

        assert_eq!(result, Err(BlkError::Io));
        assert!(completions.0.is_empty());
    }

    #[test]
    fn queued_error_after_completion_is_classified_before_success() {
        let (mut queue, _shared, registers, mut irq) = test_queue();
        let mut completions = TestCompletions::default();
        assert!(matches!(
            queue.submit_owned(RequestId::new(10), flush_request()),
            Ok(SubmitOutcome::Queued)
        ));

        let completion_event = complete_slot(&registers, irq.as_mut(), IRQ_D2H_REG_FIS, 0);
        registers.set(port_offset(0, PX_SERR), 0x20);
        let _error_event = complete_slot(&registers, irq.as_mut(), IRQ_TASK_FILE_ERROR, TFD_ERR);

        let batch = completion_event.for_queue(0).unwrap();
        let result = queue.service_events(&batch, &mut completions);

        assert_eq!(result, Err(BlkError::Io));
        assert!(
            completions.0.is_empty(),
            "the worker must classify every queued error before publishing success"
        );
    }

    #[test]
    fn quiescence_reclaim_returns_data_ownership_exactly_once() {
        let (mut queue, shared, _registers, _irq) = test_queue();
        let dma = dma_api::DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let data = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(512).unwrap(),
            2,
            DmaDirection::FromDevice,
        )
        .unwrap();
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 1,
            block_count: 1,
            data: Some(data),
            flags: RequestFlags::NONE,
        };
        assert!(matches!(
            queue.submit_owned(RequestId::new(17), request),
            Ok(SubmitOutcome::Queued)
        ));

        shared.port(0).set_online(false);
        let cookie = Arc::as_ptr(&shared).expose_provenance();
        let stale_proof = unsafe {
            // SAFETY: this fake controller has no bus-mastering hardware. The
            // proof intentionally predates the queue's next recovery epoch.
            DmaQuiesced::new(ControllerEpoch::new(1), cookie)
        };
        let mut completions = TestCompletions::default();
        assert_eq!(
            queue.reclaim_after_quiesce(&stale_proof, &mut completions),
            Err(BlkError::InvalidDmaProof),
            "a proof from the queue's publication epoch must not return DMA ownership"
        );
        assert!(completions.0.is_empty());

        let proof = unsafe {
            // SAFETY: this fake controller has no bus-mastering hardware. The
            // test closes queue admission before fabricating the proof.
            DmaQuiesced::new(ControllerEpoch::new(2), cookie)
        };
        queue
            .reclaim_after_quiesce(&proof, &mut completions)
            .unwrap();
        assert_eq!(
            queue.reclaim_after_quiesce(&proof, &mut completions),
            Err(BlkError::InvalidDmaProof),
            "one queue cannot consume the same controller epoch twice"
        );

        assert_eq!(completions.0.len(), 1);
        let completion = completions.0.pop().unwrap();
        assert_eq!(completion.id, RequestId::new(17));
        assert_eq!(completion.result, Err(BlkError::Cancelled));
        assert!(completion.request.data.is_some());
        queue.shutdown().unwrap();
        assert!(matches!(queue.command_memory, QueueDmaOwner::Released));
    }

    #[test]
    fn abandoned_queue_quarantines_without_hardware_teardown() {
        let (queue, shared, registers, _irq) = test_queue();
        registers.clear_access_log();

        drop(queue);

        assert!(
            registers.writes().is_empty(),
            "queue Drop must not infer or execute an AHCI stop protocol"
        );
        assert!(!shared.port(0).is_online());
    }

    fn test_queue() -> (
        AhciPortQueue,
        Arc<HostShared>,
        Arc<FakeRegisters>,
        BIrqEndpoint,
    ) {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        let dma = dma_api::DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let ready = ReadyPort {
            port: 0,
            ata: AtaDevice {
                num_blocks: 4096,
                logical_block_size: 512,
                lba48: true,
                flush: true,
            },
            command_memory: PortCommandMemory::allocate(&dma).unwrap(),
        };
        let queue = AhciPortQueue::new(
            ready,
            Arc::clone(&shared),
            QueueBinding {
                name: "test-ahci",
                dma_mask: u64::MAX,
                dma_domain: dma.domain_id(),
                irq_source_id: 0,
                request_timeout_ns: 1_000,
                controller_cookie: Arc::as_ptr(&shared).expose_provenance(),
            },
        );
        let (irq, _control) = shared.take_io_source().unwrap().into_parts();
        (queue, shared, registers, irq)
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

    fn complete_slot(
        registers: &FakeRegisters,
        irq: &mut dyn rdif_block::IrqEndpoint<Event = Event, Fault = BlkError>,
        status: u32,
        task_file: u32,
    ) -> Event {
        registers.set(port_offset(0, PX_CI), 0);
        registers.set(port_offset(0, PX_TFD), task_file);
        registers.set(port_offset(0, PX_IS), status);
        registers.set(HOST_IS, 1);
        let IrqCapture::Captured { event, .. } = irq.capture() else {
            panic!("test IRQ endpoint must capture the programmed status")
        };
        event
    }

    #[derive(Default)]
    struct TestCompletions(Vec<CompletedRequest>);

    impl TestCompletions {
        fn ids(&self) -> Vec<RequestId> {
            self.0.iter().map(|completion| completion.id).collect()
        }
    }

    impl CompletionSink for TestCompletions {
        fn complete(&mut self, completion: CompletedRequest) {
            self.0.push(completion);
        }
    }
}
