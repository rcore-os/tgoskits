use alloc::{format, sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull, time::Duration};

use ax_kspin::SpinNoIrq;
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
    block::{PlatformDeviceBlock, decode_fdt_irq},
    mmio::iomap,
};

const BLOCK_SIZE: usize = 512;

type PhytiumSdMmc = SdioSdmmc<PhytiumMci>;

module_driver!(
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
    let raw = Arc::new(SpinNoIrq::new(card));
    let dev = MciBlockDevice {
        raw: Some(raw),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: false,
        queue_created: false,
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
    raw: Option<Arc<SpinNoIrq<PhytiumSdMmc>>>,
    capacity_blocks: u64,
    irq_enabled: bool,
    queue_created: bool,
}

struct MciBlockQueue {
    raw: Arc<SpinNoIrq<PhytiumSdMmc>>,
    capacity_blocks: u64,
    id: usize,
    dma: DeviceDma,
    slot: BlockRequestSlot,
    pending: Option<BlockRequest>,
    completed: Vec<rd_block::RequestId>,
}

impl DriverGeneric for MciBlockDevice {
    fn name(&self) -> &str {
        "phytium-mci"
    }
}

impl rd_block::Interface for MciBlockDevice {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(MciBlockQueue {
                raw: dev.clone(),
                capacity_blocks: self.capacity_blocks,
                id: 0,
                dma: axklib::dma::device_with_mask(u32::MAX as u64),
                slot: BlockRequestSlot::default(),
                pending: None,
                completed: Vec::new(),
            }) as _
        })
    }

    fn enable_irq(&mut self) {
        if let Some(raw) = &self.raw {
            let mut raw = raw.lock();
            if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                warn!("phytium-mci: enable completion IRQ failed: {:?}", err);
                return;
            }
            self.irq_enabled = true;
        }
    }

    fn disable_irq(&mut self) {
        if let Some(raw) = &self.raw {
            let mut raw = raw.lock();
            if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                warn!("phytium-mci: disable completion IRQ failed: {:?}", err);
            }
        }
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> rd_block::Event {
        let Some(raw) = &self.raw else {
            return rd_block::Event::none();
        };
        let irq_event = raw.lock().host_mut().handle_irq();
        block_event_from_mci_irq(irq_event)
    }
}

fn block_event_from_mci_irq(irq_event: phytium_mci_host::Event) -> rd_block::Event {
    match irq_event {
        phytium_mci_host::Event::None => rd_block::Event::none(),
        phytium_mci_host::Event::CommandComplete
        | phytium_mci_host::Event::TransferComplete
        | phytium_mci_host::Event::ReceiveReady
        | phytium_mci_host::Event::TransmitReady
        | phytium_mci_host::Event::Error { .. }
        | phytium_mci_host::Event::Other { .. } => {
            let mut event = rd_block::Event::none();
            event.queue.insert(0);
            event
        }
    }
}

impl rd_block::IQueue for MciBlockQueue {
    fn num_blocks(&self) -> usize {
        self.capacity_blocks as usize
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn id(&self) -> usize {
        self.id
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: self.dma.dma_mask(),
            align: BLOCK_SIZE,
            size: BLOCK_SIZE,
        }
    }

    fn submit_request(
        &mut self,
        request: rd_block::Request<'_>,
    ) -> Result<rd_block::RequestId, rd_block::BlkError> {
        self.reap_pending_request()?;
        let mut raw = self.raw.lock();
        let start_block = block_addr_for_card(request.block_id, raw.is_high_capacity())?;
        match request.kind {
            rd_block::RequestKind::Read(buffer) => {
                if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                    return Err(rd_block::BlkError::Other(
                        "read buffer is not block aligned".into(),
                    ));
                }
                let ptr = NonNull::new(buffer.virt).ok_or_else(|| {
                    rd_block::BlkError::Other("read buffer pointer is null".into())
                })?;
                let size = NonZeroUsize::new(buffer.len())
                    .ok_or_else(|| rd_block::BlkError::Other("read buffer is empty".into()))?;
                let id = submit_read_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &self.dma,
                    &mut self.slot,
                    &mut self.pending,
                )?;
                Ok(rd_block::RequestId::new(usize::from(id)))
            }
            rd_block::RequestKind::Write(items) => {
                if !items.len().is_multiple_of(BLOCK_SIZE) {
                    return Err(rd_block::BlkError::Other(
                        "write buffer is not block aligned".into(),
                    ));
                }
                let ptr = NonNull::new(items.as_ptr() as *mut u8).ok_or_else(|| {
                    rd_block::BlkError::Other("write buffer pointer is null".into())
                })?;
                let size = NonZeroUsize::new(items.len())
                    .ok_or_else(|| rd_block::BlkError::Other("write buffer is empty".into()))?;
                let id = submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &self.dma,
                    &mut self.slot,
                    &mut self.pending,
                )?;
                Ok(rd_block::RequestId::new(usize::from(id)))
            }
        }
    }

    fn poll_request(&mut self, request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(());
        }
        self.poll_active_request(request)
    }
}

impl MciBlockQueue {
    fn poll_active_request(
        &mut self,
        request: rd_block::RequestId,
    ) -> Result<(), rd_block::BlkError> {
        match self.raw.lock().host_mut().poll_block_request(
            &mut self.pending,
            RequestId::new(usize::from(request)),
            &mut self.slot,
        ) {
            Ok(BlockPoll::Complete) => Ok(()),
            Ok(BlockPoll::Pending) => Err(rd_block::BlkError::Retry),
            Ok(_) => Err(rd_block::BlkError::Other(
                "Phytium MCI returned an unknown poll state".into(),
            )),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn pending_id(&self) -> Option<RequestId> {
        self.pending.as_ref().map(BlockRequest::id)
    }

    fn reap_pending_request(&mut self) -> Result<(), rd_block::BlkError> {
        let Some(active) = self.pending_id() else {
            return Ok(());
        };
        let id = rd_block::RequestId::new(usize::from(active));
        match self.poll_active_request(id) {
            Ok(()) => {
                self.completed.push(id);
                Ok(())
            }
            Err(rd_block::BlkError::Retry) => Err(rd_block::BlkError::Retry),
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
) -> Result<RequestId, rd_block::BlkError> {
    if pending.is_some() {
        return Err(rd_block::BlkError::Retry);
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
) -> Result<RequestId, rd_block::BlkError> {
    if pending.is_some() {
        return Err(rd_block::BlkError::Retry);
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

fn block_addr_for_card(block_id: usize, high_capacity: bool) -> Result<u32, rd_block::BlkError> {
    let block_id =
        u32::try_from(block_id).map_err(|_| rd_block::BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(rd_block::BlkError::InvalidBlockIndex(block_id as usize))
    }
}

fn map_dev_err_to_blk_err(err: Error) -> rd_block::BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => {
            rd_block::BlkError::NotSupported
        }
        Error::Misaligned | Error::InvalidArgument => {
            rd_block::BlkError::Other("Phytium MCI request is not block aligned".into())
        }
        _ => rd_block::BlkError::Other("Phytium MCI I/O error".into()),
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
