use alloc::sync::Arc;
use core::{
    mem::forget,
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

use super::{
    command::{
        CommandHeader, CommandList, CommandTable, IdentifyGeometry, IoCommandSpec,
        LOGICAL_BLOCK_SIZE, MAX_LBA28_BLOCKS, MAX_LBA48_BLOCKS, MAX_PRD_BYTES, ReceivedFis,
        identify_command, io_command, parse_identify,
    },
    registers::{PORT_IRQ_COMPLETIONS, PORT_IRQ_FATAL, PORT_IRQ_LINK, PortRegisters, QUEUE_ID},
};

const INIT_PENDING: u8 = 0;
const INIT_READY: u8 = 1;
const INIT_FAILED: u8 = 2;

pub(super) struct GeometryState {
    state: AtomicU8,
    blocks: AtomicU64,
}

impl GeometryState {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(INIT_PENDING),
            blocks: AtomicU64::new(0),
        }
    }

    fn publish_ready(&self, geometry: IdentifyGeometry) {
        self.blocks.store(geometry.blocks, Ordering::Relaxed);
        self.state.store(INIT_READY, Ordering::Release);
    }

    fn publish_failed(&self) {
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
    name: &'static str,
    port: PortRegisters,
    dma: DeviceDma,
    geometry: Arc<GeometryState>,
    irq_status: Arc<AtomicU32>,
    command_list: Option<CoherentBox<CommandList>>,
    received_fis: Option<CoherentBox<ReceivedFis>>,
    command_table: Option<CoherentBox<CommandTable>>,
    active: Option<ActiveCommand>,
    lba48: bool,
    supports_flush: bool,
    flush_ext: bool,
    fua: bool,
    request_ids: RequestIdSequence,
    staged: bool,
    initialized: bool,
    stopped: bool,
}

impl AhciQueue {
    pub(super) fn new(
        name: &'static str,
        port: PortRegisters,
        dma: DeviceDma,
        geometry: Arc<GeometryState>,
    ) -> Result<Self, BlkError> {
        let command_list = dma
            .coherent_box_zero_with_align::<CommandList>(1024)
            .map_err(|_| BlkError::NoMemory)?;
        let received_fis = dma
            .coherent_box_zero_with_align::<ReceivedFis>(256)
            .map_err(|_| BlkError::NoMemory)?;
        let command_table = dma
            .coherent_box_zero_with_align::<CommandTable>(128)
            .map_err(|_| BlkError::NoMemory)?;
        Ok(Self {
            name,
            port,
            dma,
            geometry,
            irq_status: Arc::new(AtomicU32::new(0)),
            command_list: Some(command_list),
            received_fis: Some(received_fis),
            command_table: Some(command_table),
            active: None,
            lba48: false,
            supports_flush: false,
            flush_ext: false,
            fua: false,
            request_ids: RequestIdSequence::default(),
            staged: false,
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

    pub(super) fn irq_status_latch(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.irq_status)
    }

    pub(super) fn begin_identify(&mut self) -> Result<(), BlkError> {
        if self.active.is_some() || self.port.slot_zero_active() || self.stopped {
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
            identify_command(self.command_table_dma(), prepared.dma_addr().as_u64());
        self.write_command(header, table);
        // SAFETY: `commit_staged` below starts slot zero with this exact
        // backing, and every completion/shutdown path waits for slot or engine
        // quiescence before returning or quarantining it.
        let data = unsafe { prepared.into_in_flight() };
        self.active = Some(ActiveCommand::Identify { data });
        self.staged = true;
        self.commit_staged()
    }

    fn queue_info(&self) -> QueueInfo {
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
                name: Some(self.name),
                model: Some("sata-ahci"),
                ..DeviceInfo::new(self.geometry.blocks(), LOGICAL_BLOCK_SIZE)
            },
            limits: QueueLimits {
                dma_mask: self.dma.dma_mask(),
                dma_domain: self.dma.domain_id(),
                dma_alignment: 2,
                dma_length_alignment: LOGICAL_BLOCK_SIZE,
                segment_boundary: None,
                max_inflight: 1,
                max_submit_batch: 1,
                max_blocks_per_request: max_blocks,
                max_segments: 1,
                max_segment_size: MAX_PRD_BYTES,
                supported_flags,
                supports_flush: self.supports_flush,
            },
        }
    }

    fn prepare_request(
        &self,
        request: &OwnedRequest,
    ) -> Result<(CommandHeader, CommandTable), BlkError> {
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
        io_command(IoCommandSpec {
            command_table_dma: self.command_table_dma(),
            op: request.op,
            lba: request.lba,
            blocks: request.block_count,
            data_dma: request.data.as_ref().map(|data| data.dma_addr().as_u64()),
            data_len: request.data_len(),
            flags: request.flags,
            lba48: self.lba48,
            flush_ext: self.flush_ext,
        })
    }

    fn accept_request(
        &mut self,
        mut request: OwnedRequest,
        id: RequestId,
        header: CommandHeader,
        table: CommandTable,
    ) {
        let op = request.op;
        let lba = request.lba;
        let blocks = request.block_count;
        self.write_command(header, table);
        let data = request.data.take().map(|prepared| {
            // SAFETY: The queue has reserved its only hardware slot, and the
            // runtime must call `commit_submissions` exactly once after this
            // accepted ID. Completion and teardown observe hardware quiescence.
            unsafe { prepared.into_in_flight() }
        });
        self.active = Some(ActiveCommand::Request {
            id,
            op,
            lba,
            blocks,
            data,
        });
        self.staged = true;
    }

    fn write_command(&mut self, header: CommandHeader, table: CommandTable) {
        let command_list = self
            .command_list
            .as_mut()
            .expect("AHCI command list is live");
        // SAFETY: Slot zero is reserved by `active == None`, PxCI[0] is clear,
        // and the command list is coherent DMA memory owned by this queue.
        unsafe {
            command_list
                .as_ptr()
                .cast::<CommandHeader>()
                .as_ptr()
                .write(header);
        }
        self.command_table
            .as_mut()
            .expect("AHCI command table is live")
            .write_cpu(table);
    }

    fn command_table_dma(&self) -> u64 {
        self.command_table
            .as_ref()
            .expect("AHCI command table is live")
            .dma_addr()
            .as_u64()
    }

    fn commit_staged(&mut self) -> Result<(), BlkError> {
        if !self.staged || self.active.is_none() || self.port.slot_zero_active() {
            return Err(BlkError::InvalidRequest);
        }
        // Publish coherent descriptor writes before the MMIO doorbell.
        fence(Ordering::Release);
        self.port.issue_slot_zero();
        self.staged = false;
        Ok(())
    }

    fn complete_identify(&mut self, data: InFlightDma) -> Result<(), BlkError> {
        // SAFETY: PxCI[0] is clear, so AHCI has stopped accessing this PRD.
        let completed = unsafe { data.complete_after_quiesce() };
        let buffer = completed.into_cpu_buffer();
        let geometry = parse_identify(buffer.as_slice_cpu())?;
        self.lba48 = geometry.lba48;
        self.supports_flush = geometry.flush;
        self.flush_ext = geometry.flush_ext;
        self.fua = geometry.fua;
        self.initialized = true;
        info!(
            "{}: IDENTIFY blocks={} lba48={} flush={} flush_ext={} fua={}",
            self.name,
            geometry.blocks,
            geometry.lba48,
            geometry.flush,
            geometry.flush_ext,
            geometry.fua
        );
        self.geometry.publish_ready(geometry);
        Ok(())
    }

    fn complete_request(
        data: Option<InFlightDma>,
        id: RequestId,
        result: Result<(), BlkError>,
        sink: &mut dyn CompletionSink,
    ) {
        let data = data.map(|data| {
            // SAFETY: PxCI[0] is clear before this function is called.
            unsafe { data.complete_after_quiesce() }
        });
        sink.complete(CompletedRequest::new(id, result, data));
    }

    fn quarantine_dma(active: ActiveCommand) {
        match active {
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

    fn forget_hardware_memory(&mut self) {
        if let Some(command_list) = self.command_list.take() {
            forget(command_list);
        }
        if let Some(received_fis) = self.received_fis.take() {
            forget(received_fis);
        }
        if let Some(command_table) = self.command_table.take() {
            forget(command_table);
        }
    }
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
        if self.active.is_some() || self.staged || self.port.slot_zero_active() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull);
        }

        let command = match requests
            .front()
            .map(|request| self.prepare_request(request))
        {
            Some(Ok(command)) => command,
            Some(Err(error)) => {
                return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
            }
            None => return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue),
        };
        let request = requests
            .pop_front()
            .expect("front request was validated above");
        let id = self.request_ids.take();
        self.accept_request(request, id, command.0, command.1);
        sink.accepted(id);
        let disposition = if requests.is_empty() {
            BatchSubmitDisposition::Continue
        } else {
            BatchSubmitDisposition::QueueFull
        };
        BatchSubmitResult::new(1, disposition)
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
        if self.port.slot_zero_active() {
            return if status_error {
                Err(BlkError::Io)
            } else {
                Ok(())
            };
        }
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let completion_seen = irq_status & PORT_IRQ_COMPLETIONS != 0;
        let result = if status_error || !completion_seen {
            Err(BlkError::Io)
        } else {
            Ok(())
        };
        match active {
            ActiveCommand::Identify { data } => {
                if result.is_err() {
                    // SAFETY: PxCI[0] is clear.
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
            } => {
                if result.is_err() {
                    warn!(
                        "{}: AHCI {:?} failed id={id:?} lba={lba} blocks={blocks} \
                         irq={irq_status:#010x} tfd={:#010x} serr={:#010x} ci={:#010x}",
                        self.name,
                        op,
                        self.port.task_file_status(),
                        self.port.sata_error(),
                        self.port.command_issue()
                    );
                }
                Self::complete_request(data, id, result, sink);
            }
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
            if let Some(active) = self.active.take() {
                Self::quarantine_dma(active);
            }
            self.forget_hardware_memory();
            return Err(BlkError::Io);
        }
        if let Some(active) = self.active.take() {
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
        Ok(())
    }
}

impl Drop for AhciQueue {
    fn drop(&mut self) {
        if !self.stopped && !self.port.engine_stopped() {
            if let Some(active) = self.active.take() {
                Self::quarantine_dma(active);
            }
            self.forget_hardware_memory();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_requests_receive_distinct_queue_ids() {
        let mut sequence = RequestIdSequence::default();

        assert_eq!(usize::from(sequence.take()), 0);
        assert_eq!(usize::from(sequence.take()), 1);
    }
}
