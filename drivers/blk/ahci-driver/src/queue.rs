use alloc::{string::String, sync::Arc};
use core::{
    array,
    mem::{forget, size_of},
    num::NonZeroUsize,
    sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering, fence},
};

use dma_api::{CoherentBox, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma};
use log::{info, warn};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, CompletedRequest, CompletionSink,
    DeviceInfo, HardwareQueue, OwnedRequest, OwnedRequestBatch, QueueInfo, QueueLimits,
    RequestFlags, RequestId, RequestOp, SubmissionSink, validate_owned_request,
};

use crate::{
    NcqPolicy,
    command::{
        CommandHeader, CommandList, CommandTable, IdentifyGeometry, IoCommandSpec,
        LOGICAL_BLOCK_SIZE, MAX_LBA28_BLOCKS, MAX_LBA48_BLOCKS, ReceivedFis, identify_command,
        io_command, parse_identify,
    },
    registers::{PORT_IRQ_COMPLETIONS, PORT_IRQ_FATAL, PORT_IRQ_LINK, PortRegisters, QUEUE_ID},
};

const INIT_PENDING: u8 = 0;
const INIT_READY: u8 = 1;
const INIT_FAILED: u8 = 2;
const MAX_COMMAND_SLOTS: usize = 32;
const MAX_TRANSFER_BYTES: usize = MAX_LBA48_BLOCKS as usize * LOGICAL_BLOCK_SIZE;

pub(super) struct GeometryState {
    state: AtomicU8,
    blocks: AtomicU64,
    queue_depth: AtomicU8,
}

pub(super) struct AhciQueueConfig {
    pub(super) hba_slots: usize,
    pub(super) hba_ncq: bool,
    pub(super) ncq_policy: NcqPolicy,
}

impl GeometryState {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(INIT_PENDING),
            blocks: AtomicU64::new(0),
            queue_depth: AtomicU8::new(1),
        }
    }

    fn publish_ready(&self, geometry: IdentifyGeometry, queue_depth: usize) {
        self.blocks.store(geometry.blocks, Ordering::Relaxed);
        self.queue_depth
            .store(queue_depth.try_into().unwrap_or(32), Ordering::Relaxed);
        self.state.store(INIT_READY, Ordering::Release);
    }

    pub(super) fn publish_failed(&self) {
        self.state.store(INIT_FAILED, Ordering::Release);
    }

    pub(super) fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == INIT_READY
    }

    pub(super) fn is_failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == INIT_FAILED
    }

    pub(super) fn blocks(&self) -> u64 {
        if self.is_ready() {
            self.blocks.load(Ordering::Relaxed)
        } else {
            0
        }
    }

    pub(super) fn queue_depth(&self) -> usize {
        usize::from(self.queue_depth.load(Ordering::Acquire))
    }
}

enum ActiveCommand {
    Identify {
        data: InFlightDma,
    },
    Request {
        id: RequestId,
        op: RequestOp,
        lba: u64,
        blocks: u32,
        data: Option<InFlightDma>,
    },
}

#[derive(Default)]
struct RequestIdSequence {
    next: usize,
}

impl RequestIdSequence {
    fn take(&mut self) -> RequestId {
        let id = RequestId::new(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }
}

pub(super) struct AhciQueue {
    name: String,
    port: PortRegisters,
    dma: DeviceDma,
    geometry: Arc<GeometryState>,
    irq_status: Arc<AtomicU32>,
    command_list: Option<CoherentBox<CommandList>>,
    received_fis: Option<CoherentBox<ReceivedFis>>,
    command_tables: Option<CoherentBox<[CommandTable; MAX_COMMAND_SLOTS]>>,
    active: [Option<ActiveCommand>; MAX_COMMAND_SLOTS],
    issued_mask: u32,
    staged_mask: u32,
    staged_ncq_mask: u32,
    hba_slots: usize,
    hba_ncq: bool,
    ncq_policy: NcqPolicy,
    queue_depth: usize,
    ncq: bool,
    lba48: bool,
    supports_flush: bool,
    flush_ext: bool,
    fua: bool,
    request_ids: RequestIdSequence,
    initialized: bool,
    stopped: bool,
}

impl AhciQueue {
    pub(super) fn new(
        name: String,
        port: PortRegisters,
        dma: DeviceDma,
        geometry: Arc<GeometryState>,
        irq_status: Arc<AtomicU32>,
        config: AhciQueueConfig,
    ) -> Result<Self, BlkError> {
        let command_list = dma
            .coherent_box_zero_with_align::<CommandList>(1024)
            .map_err(|_| BlkError::NoMemory)?;
        let received_fis = dma
            .coherent_box_zero_with_align::<ReceivedFis>(256)
            .map_err(|_| BlkError::NoMemory)?;
        let command_tables = dma
            .coherent_box_zero_with_align::<[CommandTable; MAX_COMMAND_SLOTS]>(128)
            .map_err(|_| BlkError::NoMemory)?;
        Ok(Self {
            name,
            port,
            dma,
            geometry,
            irq_status,
            command_list: Some(command_list),
            received_fis: Some(received_fis),
            command_tables: Some(command_tables),
            active: array::from_fn(|_| None),
            issued_mask: 0,
            staged_mask: 0,
            staged_ncq_mask: 0,
            hba_slots: config.hba_slots.clamp(1, MAX_COMMAND_SLOTS),
            hba_ncq: config.hba_ncq,
            ncq_policy: config.ncq_policy,
            queue_depth: 1,
            ncq: false,
            lba48: false,
            supports_flush: false,
            flush_ext: false,
            fua: false,
            request_ids: RequestIdSequence::default(),
            initialized: false,
            stopped: false,
        })
    }

    pub(super) fn command_list_dma(&self) -> u64 {
        self.command_list
            .as_ref()
            .expect("AHCI command list is live")
            .dma_addr()
            .as_u64()
    }

    pub(super) fn received_fis_dma(&self) -> u64 {
        self.received_fis
            .as_ref()
            .expect("AHCI received FIS is live")
            .dma_addr()
            .as_u64()
    }

    pub(super) fn begin_identify(&mut self) -> Result<(), BlkError> {
        if self.active.iter().any(Option::is_some) || self.port.active_slots() != 0 || self.stopped
        {
            return Err(BlkError::Io);
        }
        let prepared = CpuDmaBuffer::new_zero(
            &self.dma,
            NonZeroUsize::new(LOGICAL_BLOCK_SIZE).expect("nonzero identify length"),
            2,
            DmaDirection::FromDevice,
        )
        .map_err(|_| BlkError::NoMemory)?
        .prepare_for_device();
        let (header, table) =
            identify_command(self.command_table_dma(0), prepared.dma_addr().as_u64())?;
        self.write_command(0, header, table);
        // SAFETY: Slot zero is reserved until PxCI clears or the engine is
        // stopped. Completion and shutdown retain this backing until then.
        let data = unsafe { prepared.into_in_flight() };
        self.active[0] = Some(ActiveCommand::Identify { data });
        self.staged_mask = 1;
        self.commit_staged()
    }

    fn queue_info(&self) -> QueueInfo {
        let scheduling_depth = if self.initialized {
            self.queue_depth
        } else if self.hba_ncq && matches!(self.ncq_policy, NcqPolicy::Auto) {
            self.hba_slots
        } else {
            1
        };
        let max_blocks = if self.lba48 {
            MAX_LBA48_BLOCKS
        } else {
            MAX_LBA28_BLOCKS
        };
        let supported_flags = if self.fua {
            RequestFlags::FUA
        } else {
            RequestFlags::NONE
        };
        QueueInfo {
            id: QUEUE_ID,
            device: DeviceInfo {
                name: Some("ahci"),
                model: Some("sata-ahci"),
                ..DeviceInfo::new(self.geometry.blocks(), LOGICAL_BLOCK_SIZE)
            },
            limits: QueueLimits {
                dma: {
                    let current = self.dma.info().constraints();
                    let queue_max_segment_size = if self.lba48 {
                        MAX_TRANSFER_BYTES
                    } else {
                        MAX_LBA28_BLOCKS as usize * LOGICAL_BLOCK_SIZE
                    };
                    self.dma.info().with_constraints(dma_api::DmaConstraints {
                        align: current.align.max(2),
                        max_segment_size: Some(
                            current
                                .max_segment_size
                                .map_or(queue_max_segment_size, |max| {
                                    max.min(queue_max_segment_size)
                                }),
                        ),
                        ..current
                    })
                },
                dma_length_alignment: LOGICAL_BLOCK_SIZE,
                max_inflight: scheduling_depth,
                max_submit_batch: scheduling_depth,
                max_blocks_per_request: max_blocks,
                max_segments: 1,
                supported_flags,
                supports_flush: self.supports_flush,
            },
        }
    }

    fn prepare_request(
        &self,
        request: &OwnedRequest,
        slot: usize,
    ) -> Result<(CommandHeader, CommandTable, bool), BlkError> {
        validate_owned_request(self.queue_info(), request)?;
        if request.flags.contains(RequestFlags::FUA) && request.op != RequestOp::Write {
            return Err(BlkError::InvalidRequest);
        }
        if let Some(data) = request.data.as_ref() {
            let direction_matches = match request.op {
                RequestOp::Read => matches!(
                    data.direction(),
                    DmaDirection::FromDevice | DmaDirection::Bidirectional
                ),
                RequestOp::Write => matches!(
                    data.direction(),
                    DmaDirection::ToDevice | DmaDirection::Bidirectional
                ),
                RequestOp::Flush => false,
            };
            if !direction_matches {
                return Err(BlkError::InvalidRequest);
            }
        }
        let queued = self.ncq && request.op != RequestOp::Flush;
        let command = io_command(IoCommandSpec {
            command_table_dma: self.command_table_dma(slot),
            op: request.op,
            lba: request.lba,
            blocks: request.block_count,
            data_dma: request.data.as_ref().map(|data| data.dma_addr().as_u64()),
            data_len: request.data_len(),
            flags: request.flags,
            lba48: self.lba48,
            flush_ext: self.flush_ext,
            ncq: queued,
            tag: slot as u8,
        })?;
        Ok((command.0, command.1, queued))
    }

    fn accept_request(
        &mut self,
        slot: usize,
        mut request: OwnedRequest,
        id: RequestId,
        header: CommandHeader,
        table: CommandTable,
        queued: bool,
    ) {
        let op = request.op;
        let lba = request.lba;
        let blocks = request.block_count;
        self.write_command(slot, header, table);
        let data = request.data.take().map(|prepared| {
            // SAFETY: This tag is exclusively reserved and all terminal paths
            // observe a cleared tag or stopped engine before returning DMA.
            unsafe { prepared.into_in_flight() }
        });
        self.active[slot] = Some(ActiveCommand::Request {
            id,
            op,
            lba,
            blocks,
            data,
        });
        let bit = 1_u32 << slot;
        self.staged_mask |= bit;
        if queued {
            self.staged_ncq_mask |= bit;
        }
    }

    fn write_command(&mut self, slot: usize, header: CommandHeader, table: CommandTable) {
        let command_list = self
            .command_list
            .as_mut()
            .expect("AHCI command list is live");
        // SAFETY: `slot` is reserved by `active[slot] == None`; both coherent
        // arrays have 32 entries and hardware does not own the staged tag yet.
        unsafe {
            command_list
                .as_ptr()
                .cast::<CommandHeader>()
                .as_ptr()
                .add(slot)
                .write(header);
            self.command_tables
                .as_mut()
                .expect("AHCI command tables are live")
                .as_ptr()
                .cast::<CommandTable>()
                .as_ptr()
                .add(slot)
                .write(table);
        }
    }

    fn command_table_dma(&self, slot: usize) -> u64 {
        self.command_tables
            .as_ref()
            .expect("AHCI command tables are live")
            .dma_addr()
            .as_u64()
            + (slot * size_of::<CommandTable>()) as u64
    }

    fn commit_staged(&mut self) -> Result<(), BlkError> {
        if self.staged_mask == 0 || self.staged_mask & self.issued_mask != 0 {
            return Err(BlkError::InvalidRequest);
        }
        let staged = self.staged_mask;
        let queued = self.staged_ncq_mask;
        fence(Ordering::Release);
        self.port.issue_commands(staged, queued);
        self.issued_mask |= staged;
        self.staged_mask = 0;
        self.staged_ncq_mask = 0;
        Ok(())
    }

    fn complete_identify(&mut self, data: InFlightDma) -> Result<(), BlkError> {
        // SAFETY: Slot zero has cleared, so the HBA no longer owns the buffer.
        let completed = unsafe { data.complete_after_quiesce() };
        let buffer = completed.into_cpu_buffer();
        let geometry = parse_identify(buffer.as_slice_cpu())?;
        (self.ncq, self.queue_depth) =
            negotiate_queue(geometry, self.hba_slots, self.hba_ncq, self.ncq_policy);
        self.lba48 = geometry.lba48;
        self.supports_flush = geometry.flush;
        self.flush_ext = geometry.flush_ext;
        self.fua = geometry.fua;
        self.initialized = true;
        info!(
            "{}: IDENTIFY blocks={} lba48={} ncq={} depth={} flush={} flush_ext={} fua={}",
            self.name,
            geometry.blocks,
            geometry.lba48,
            self.ncq,
            self.queue_depth,
            geometry.flush,
            geometry.flush_ext,
            geometry.fua
        );
        self.geometry.publish_ready(geometry, self.queue_depth);
        Ok(())
    }

    fn complete_request(
        data: Option<InFlightDma>,
        id: RequestId,
        result: Result<(), BlkError>,
        sink: &mut dyn CompletionSink,
    ) {
        let data = data.map(|data| {
            // SAFETY: The corresponding PxCI/PxSACT tag has cleared.
            unsafe { data.complete_after_quiesce() }
        });
        sink.complete(CompletedRequest::new(id, result, data));
    }

    fn quarantine_active(active: &mut [Option<ActiveCommand>; MAX_COMMAND_SLOTS]) {
        for command in active.iter_mut().filter_map(Option::take) {
            match command {
                ActiveCommand::Identify { data } => {
                    let _quarantined = data.quarantine();
                }
                ActiveCommand::Request { data, .. } => {
                    if let Some(data) = data {
                        let _quarantined = data.quarantine();
                    }
                }
            }
        }
    }

    fn forget_hardware_memory(&mut self) {
        if let Some(command_list) = self.command_list.take() {
            forget(command_list);
        }
        if let Some(received_fis) = self.received_fis.take() {
            forget(received_fis);
        }
        if let Some(command_tables) = self.command_tables.take() {
            forget(command_tables);
        }
    }

    fn free_slot(&self, request: &OwnedRequest) -> Option<usize> {
        if request.op == RequestOp::Flush {
            return (self.issued_mask == 0 && self.staged_mask == 0 && self.active[0].is_none())
                .then_some(0);
        }
        (0..self.queue_depth).find(|&slot| {
            let bit = 1_u32 << slot;
            self.active[slot].is_none()
                && self.issued_mask & bit == 0
                && self.staged_mask & bit == 0
        })
    }

    fn complete_slot(
        &mut self,
        slot: usize,
        result: Result<(), BlkError>,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        let Some(active) = self.active[slot].take() else {
            return Ok(());
        };
        match active {
            ActiveCommand::Identify { data } => {
                if result.is_err() {
                    // SAFETY: The identify tag cleared before this call.
                    drop(unsafe { data.complete_after_quiesce() });
                    self.geometry.publish_failed();
                    return Err(BlkError::Io);
                }
                if let Err(error) = self.complete_identify(data) {
                    self.geometry.publish_failed();
                    return Err(error);
                }
            }
            ActiveCommand::Request {
                id,
                op,
                lba,
                blocks,
                data,
                ..
            } => {
                if result.is_err() {
                    warn!(
                        "{}: AHCI {:?} failed tag={} id={id:?} lba={lba} blocks={blocks} \
                         tfd={:#010x} serr={:#010x} ci={:#010x} sact={:#010x}",
                        self.name,
                        op,
                        slot,
                        self.port.task_file_status(),
                        self.port.sata_error(),
                        self.port.command_issue(),
                        self.port.sata_active()
                    );
                }
                Self::complete_request(data, id, result, sink);
            }
        }
        Ok(())
    }
}

fn negotiate_queue(
    geometry: IdentifyGeometry,
    hba_slots: usize,
    hba_ncq: bool,
    policy: NcqPolicy,
) -> (bool, usize) {
    let ncq = hba_ncq && matches!(policy, NcqPolicy::Auto) && geometry.ncq_depth.is_some();
    let depth = if ncq {
        hba_slots
            .min(geometry.ncq_depth.map_or(1, usize::from))
            .clamp(1, MAX_COMMAND_SLOTS)
    } else {
        1
    };
    (ncq, depth)
}

const fn completed_slots(issued: u32, command_issue: u32, sata_active: u32) -> u32 {
    issued & !(command_issue | sata_active)
}

impl HardwareQueue for AhciQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        if requests.is_empty() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        }
        if self.stopped || !self.initialized {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(BlkError::Io));
        }

        let mut accepted = 0;
        while accepted < self.queue_depth {
            let Some(request) = requests.front() else {
                break;
            };
            let Some(slot) = self.free_slot(request) else {
                break;
            };
            let (header, table, queued) = match self.prepare_request(request, slot) {
                Ok(command) => command,
                Err(error) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::Fatal(error));
                }
            };
            let request = requests
                .pop_front()
                .expect("front request was validated above");
            let id = self.request_ids.take();
            self.accept_request(slot, request, id, header, table, queued);
            sink.accepted(id);
            accepted += 1;
            if !queued {
                break;
            }
        }
        let disposition = if requests.is_empty() {
            BatchSubmitDisposition::Continue
        } else {
            BatchSubmitDisposition::QueueFull
        };
        BatchSubmitResult::new(accepted, disposition)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.commit_staged()
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.stopped {
            return Err(BlkError::Io);
        }
        let irq_status = self.irq_status.swap(0, Ordering::AcqRel);
        let status_error =
            irq_status & (PORT_IRQ_FATAL | PORT_IRQ_LINK) != 0 || self.port.task_file_error();
        let command_issue = self.port.command_issue();
        let sata_active = self.port.sata_active();
        let hardware_active = command_issue | sata_active;
        let completed = completed_slots(self.issued_mask, command_issue, sata_active);
        self.issued_mask &= hardware_active;
        let completion_seen = irq_status & PORT_IRQ_COMPLETIONS != 0;

        for slot in 0..MAX_COMMAND_SLOTS {
            let bit = 1_u32 << slot;
            if completed & bit == 0 {
                continue;
            }
            let result = if status_error || !completion_seen {
                Err(BlkError::Io)
            } else {
                Ok(())
            };
            self.complete_slot(slot, result, sink)?;
        }

        if status_error && self.issued_mask != 0 {
            warn!(
                "{}: AHCI active commands failed irq={irq_status:#010x} issued={:#010x} \
                 tfd={:#010x} serr={:#010x} ci={command_issue:#010x} sact={sata_active:#010x}",
                self.name,
                self.issued_mask,
                self.port.task_file_status(),
                self.port.sata_error(),
            );
            return Err(BlkError::Io);
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.geometry.publish_failed();
        if !self.port.engine_stopped() {
            Self::quarantine_active(&mut self.active);
            self.forget_hardware_memory();
            return Err(BlkError::Io);
        }
        for active in self.active.iter_mut().filter_map(Option::take) {
            match active {
                ActiveCommand::Identify { data } => {
                    // SAFETY: Both command and FIS engines are stopped.
                    drop(unsafe { data.complete_after_quiesce() });
                }
                ActiveCommand::Request { id, data, .. } => {
                    Self::complete_request(data, id, Err(BlkError::Io), sink);
                }
            }
        }
        self.issued_mask = 0;
        self.staged_mask = 0;
        self.staged_ncq_mask = 0;
        Ok(())
    }
}

impl Drop for AhciQueue {
    fn drop(&mut self) {
        if !self.stopped && !self.port.engine_stopped() {
            Self::quarantine_active(&mut self.active);
            self.forget_hardware_memory();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{MAX_PRD_BYTES, MAX_PRDS};

    #[test]
    fn sequential_requests_receive_distinct_queue_ids() {
        let mut sequence = RequestIdSequence::default();

        assert_eq!(usize::from(sequence.take()), 0);
        assert_eq!(usize::from(sequence.take()), 1);
    }

    #[test]
    fn identify_depth_is_clamped_by_hba_slots_and_policy() {
        let first_port = IdentifyGeometry {
            blocks: 1024,
            lba48: true,
            flush: true,
            flush_ext: true,
            fua: true,
            ncq_depth: Some(8),
        };
        let second_port = IdentifyGeometry {
            ncq_depth: Some(32),
            ..first_port
        };

        assert_eq!(
            negotiate_queue(first_port, 32, true, NcqPolicy::Auto),
            (true, 8)
        );
        assert_eq!(
            negotiate_queue(second_port, 16, true, NcqPolicy::Auto),
            (true, 16)
        );
        assert_eq!(
            negotiate_queue(second_port, 32, true, NcqPolicy::Disabled),
            (false, 1)
        );
        assert_eq!(
            negotiate_queue(second_port, 32, false, NcqPolicy::Auto),
            (false, 1)
        );
    }

    #[test]
    fn ncq_completion_mask_accepts_out_of_order_tags() {
        let issued = 0b1111;

        assert_eq!(completed_slots(issued, 0, 0b1010), 0b0101);
        assert_eq!(completed_slots(issued & !0b0101, 0, 0b0010), 0b1000);
    }

    #[test]
    fn command_table_capacity_covers_max_lba48_transfer() {
        assert!(MAX_PRDS * MAX_PRD_BYTES >= MAX_TRANSFER_BYTES);
    }
}
