use alloc::{format, vec::Vec};
use core::{
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use dma_api::DeviceDma;
use log::{info, warn};
use phytium_mci_host::{BlockRequest, BlockRequestSlot, PhytiumMci, RequestId};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError, register::FdtInfo};
use sdmmc_protocol::{
    BlockPoll, BlockTransferMode, Error, OperationPoll,
    error::Phase,
    sdio::{CardInfo, CardInitPreference, SdioHost, SdioInitScratch, SdioSdmmc},
};

use crate::{
    block::{PlatformDeviceBlock, SharedDriver, decode_fdt_irq},
    mmio::iomap,
};

const BLOCK_SIZE: usize = 512;

type PhytiumSdMmc = SdioSdmmc<PhytiumMci>;

crate::model_register!(
    name: "Phytium MCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["phytium,mci"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    info!(
        "phytium-mci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { PhytiumMci::new(mmio_base) };
    info!("phytium-mci: reset controller");
    host.reset_and_init()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;

    info!("phytium-mci: initialize card");
    let mut card = SdioSdmmc::new(host);
    card.set_sd_uhs_selection_enabled(false);
    let preference = card_init_preference(&info);
    let card_info = poll_card_init(&mut card, preference).map_err(|e| {
        warn!("phytium-mci: card init failed: {:?}", e);
        card_init_error(base_reg.address, mmio_size, e)
    })?;
    info!(
        "phytium-mci card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} \
         cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let irq_num = decode_fdt_irq(&info.interrupts());
    let raw = SharedDriver::new(card);
    let dev = MciBlockDevice {
        raw: Some(raw),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: AtomicBool::new(false),
        queue_created: false,
        irq_handler_taken: false,
    };
    plat_dev.register_block_with_irq(dev, irq_num);
    info!("phytium-mci block device registered irq={:?}", irq_num);
    Ok(())
}

fn poll_card_init(
    card: &mut PhytiumSdMmc,
    preference: CardInitPreference,
) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = card.submit_init_with_preference(preference, &mut scratch)?;
    loop {
        match card.poll_init_request(&mut request)? {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    axklib::time::busy_wait(Duration::from_millis(10));
                } else {
                    core::hint::spin_loop();
                }
            }
            OperationPoll::Complete(info) => return Ok(info),
            _ => return Err(Error::UnsupportedCommand),
        }
    }
}

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize Phytium MCI device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "phytium-mci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: {err:?}",
            address, size
        );
        return OnProbeError::NotMatch;
    }

    init_error(address, size, err)
}

fn is_absent_card_init_error(err: Error) -> bool {
    match err {
        Error::NoCard => true,
        Error::Timeout(ctx) | Error::Crc(ctx) | Error::BadResponse(ctx) => {
            ctx.cmd.is_some()
                && matches!(
                    ctx.phase,
                    Phase::CommandSend | Phase::ResponseWait | Phase::Init
                )
        }
        _ => false,
    }
}

struct MciBlockDevice {
    raw: Option<SharedDriver<PhytiumSdMmc>>,
    capacity_blocks: u64,
    irq_enabled: AtomicBool,
    queue_created: bool,
    irq_handler_taken: bool,
}

struct MciBlockQueue {
    raw: SharedDriver<PhytiumSdMmc>,
    capacity_blocks: u64,
    id: usize,
    dma: DeviceDma,
    slot: BlockRequestSlot,
    pending: Option<BlockRequest>,
    completed: Vec<rdif_block::RequestId>,
}

impl DriverGeneric for MciBlockDevice {
    fn name(&self) -> &str {
        "phytium-mci"
    }
}

impl rdif_block::Interface for MciBlockDevice {
    fn device_info(&self) -> rdif_block::DeviceInfo {
        rdif_block::DeviceInfo {
            name: Some("phytium-mci"),
            ..rdif_block::DeviceInfo::new(self.capacity_blocks, BLOCK_SIZE)
        }
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        rdif_block::QueueLimits {
            dma_mask: u32::MAX as u64,
            dma_alignment: BLOCK_SIZE,
            max_blocks_per_request: u16::MAX as u32 + 1,
            max_segments: 1,
            max_segment_size: usize::MAX,
            supported_flags: rdif_block::RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }

    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(MciBlockQueue::new(dev.clone(), self.capacity_blocks, 0)) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(raw) = &self.raw {
            let mut enabled = false;
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                    warn!("phytium-mci: enable completion IRQ failed: {:?}", err);
                    return;
                }
                enabled = true;
            });
            self.irq_enabled.store(enabled, Ordering::Release);
        }
    }

    fn disable_irq(&self) {
        if let Some(raw) = &self.raw {
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                    warn!("phytium-mci: disable completion IRQ failed: {:?}", err);
                }
            });
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        alloc::vec![rdif_block::IrqSourceInfo::legacy(
            rdif_block::IdList::from_bits(1),
        )]
    }

    fn take_irq_handler(
        &mut self,
        source_id: usize,
    ) -> Option<alloc::boxed::Box<dyn rdif_block::IrqHandler>> {
        if source_id != 0 {
            return None;
        }
        if self.irq_handler_taken {
            return None;
        }
        let raw = self.raw.as_ref()?.clone();
        self.irq_handler_taken = true;
        Some(alloc::boxed::Box::new(MciBlockIrqHandler { raw }))
    }
}

struct MciBlockIrqHandler {
    raw: SharedDriver<PhytiumSdMmc>,
}

impl rdif_block::IrqHandler for MciBlockIrqHandler {
    fn handle_irq(&self) -> rdif_block::Event {
        self.raw
            .try_with_mut(|raw| block_event_from_mci_irq(raw.host_mut().handle_irq()))
            .unwrap_or_else(rdif_block::Event::none)
    }
}

fn block_event_from_mci_irq(irq_event: phytium_mci_host::Event) -> rdif_block::Event {
    match irq_event {
        phytium_mci_host::Event::None => rdif_block::Event::none(),
        phytium_mci_host::Event::CommandComplete
        | phytium_mci_host::Event::TransferComplete
        | phytium_mci_host::Event::ReceiveReady
        | phytium_mci_host::Event::TransmitReady
        | phytium_mci_host::Event::Error { .. }
        | phytium_mci_host::Event::Other { .. } => {
            let mut event = rdif_block::Event::none();
            event.queues.insert(0);
            event
        }
    }
}

// SAFETY: MciBlockQueue owns a single pending request slot and does not access
// request segments after that request completes or fails.
unsafe impl rdif_block::IQueue for MciBlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: rdif_block::DeviceInfo {
                name: Some("phytium-mci"),
                ..rdif_block::DeviceInfo::new(self.capacity_blocks, BLOCK_SIZE)
            },
            limits: rdif_block::QueueLimits {
                dma_mask: self.dma.dma_mask(),
                dma_alignment: BLOCK_SIZE,
                max_blocks_per_request: u16::MAX as u32 + 1,
                max_segments: 1,
                max_segment_size: usize::MAX,
                supported_flags: rdif_block::RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        }
    }

    fn submit_request(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.submit_request_inner(request)
    }

    fn poll_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        self.poll_request_inner(request)
    }
}

impl MciBlockQueue {
    fn new(raw: SharedDriver<PhytiumSdMmc>, capacity_blocks: u64, id: usize) -> Self {
        Self {
            raw,
            capacity_blocks,
            id,
            dma: axklib::dma::device_with_mask(u32::MAX as u64),
            slot: BlockRequestSlot::default(),
            pending: None,
            completed: Vec::new(),
        }
    }

    fn queue_info(&self) -> rdif_block::QueueInfo {
        rdif_block::IQueue::info(self)
    }

    fn submit_request_inner(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        let info = self.queue_info();
        rdif_block::validate_request(info, &request)?;
        self.reap_pending_request()?;
        let raw = self.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.lba, raw.is_high_capacity())?;
            let buffer = request
                .segments
                .first()
                .copied()
                .ok_or(rdif_block::BlkError::InvalidRequest)?;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(rdif_block::BlkError::Other("buffer is not block aligned"));
            }
            let ptr = NonNull::new(buffer.virt)
                .ok_or(rdif_block::BlkError::Other("buffer pointer is null"))?;
            let size = NonZeroUsize::new(buffer.len())
                .ok_or(rdif_block::BlkError::Other("buffer is empty"))?;
            let id = match request.op {
                rdif_block::RequestOp::Read => submit_read_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &self.dma,
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Write => submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &self.dma,
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => {
                    return Err(rdif_block::BlkError::NotSupported);
                }
            };
            Ok(rdif_block::RequestId::new(usize::from(id)))
        })
    }

    fn poll_request_inner(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(rdif_block::RequestStatus::Complete);
        }
        self.poll_active_request(request)
    }

    fn poll_active_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        let raw = self.raw.clone();
        match raw.with_mut(|raw| {
            raw.host_mut().poll_block_request(
                &mut self.pending,
                RequestId::new(usize::from(request)),
                &mut self.slot,
            )
        }) {
            Ok(BlockPoll::Complete) => Ok(rdif_block::RequestStatus::Complete),
            Ok(BlockPoll::Pending) => Ok(rdif_block::RequestStatus::Pending),
            Ok(_) => Err(rdif_block::BlkError::Other(
                "Phytium MCI returned an unknown poll state",
            )),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn pending_id(&self) -> Option<RequestId> {
        self.pending.as_ref().map(BlockRequest::id)
    }

    fn reap_pending_request(&mut self) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        let Some(active) = self.pending_id() else {
            return Ok(rdif_block::RequestStatus::Complete);
        };
        let id = rdif_block::RequestId::new(usize::from(active));
        match self.poll_active_request(id) {
            Ok(rdif_block::RequestStatus::Complete) => {
                self.completed.push(id);
                Ok(rdif_block::RequestStatus::Complete)
            }
            Ok(rdif_block::RequestStatus::Pending) => Err(rdif_block::BlkError::Retry),
            Err(err) => Err(err),
        }
    }
}

fn submit_read_request(
    host: &mut PhytiumMci,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: &DeviceDma,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<RequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_read_blocks(
        start_block,
        buffer,
        size,
        Some(dma),
        BlockTransferMode::Dma,
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => {
            warn!(
                "phytium-mci: DMA read unavailable ({:?}); falling back to FIFO",
                err
            );
            host.submit_read_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?
        }
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn submit_write_request(
    host: &mut PhytiumMci,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: &DeviceDma,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<RequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_write_blocks(
        start_block,
        buffer,
        size,
        Some(dma),
        BlockTransferMode::Dma,
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => {
            warn!(
                "phytium-mci: DMA write unavailable ({:?}); falling back to FIFO",
                err
            );
            host.submit_write_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?
        }
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

fn block_addr_for_card(block_id: u64, high_capacity: bool) -> Result<u32, rdif_block::BlkError> {
    let block_id =
        u32::try_from(block_id).map_err(|_| rdif_block::BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(rdif_block::BlkError::InvalidBlockIndex(block_id as u64))
    }
}

fn map_dev_err_to_blk_err(err: Error) -> rdif_block::BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => {
            rdif_block::BlkError::NotSupported
        }
        Error::Misaligned | Error::InvalidArgument => {
            rdif_block::BlkError::Other("Phytium MCI request is not block aligned")
        }
        _ => rdif_block::BlkError::Io,
    }
}

#[cfg(test)]
mod tests {
    use sdmmc_protocol::error::ErrorContext;

    use super::*;

    #[test]
    fn command_timeout_during_card_init_is_absent_card() {
        let err = Error::Timeout(ErrorContext::for_cmd(Phase::ResponseWait, 1));

        assert!(is_absent_card_init_error(err));
    }

    #[test]
    fn data_timeout_after_card_init_is_not_absent_card() {
        let err = Error::Timeout(ErrorContext::for_cmd(Phase::DataRead, 17));

        assert!(!is_absent_card_init_error(err));
    }
}
