use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;
#[cfg(not(feature = "paging"))]
use core::ptr::NonNull;

use ax_driver::{DriverGeneric, PlatformDevice, block::PlatformDeviceBlock, probe::OnProbeError};
use ax_kspin::SpinNoIrq;
#[cfg(not(feature = "paging"))]
use ax_plat::mem::{pa, phys_to_virt};
use log::{info, warn};
use mmio_api::MmioRaw;
#[cfg(not(feature = "paging"))]
use mmio_api::{MapError, MmioAddr, MmioOp};
use rd_block::{BlkError, BuffConfig, Event, IQueue, Interface, Request, RequestId};
use sdhci_host::{BlockRequest, BlockRequestSlot, Sdhci};
use sdmmc_protocol::{
    BlockPoll, BlockTransferMode, Error, OperationPoll,
    error::Phase,
    sdio::{CardInfo, CardInitPreference, SdioInitScratch, SdioSdmmc},
};

use crate::config::devices;

const BLOCK_SIZE: usize = 512;
const SDHCI_POWER_330: u8 = 0x0e;
const DEVICE_NAME: &str = "k230-sdhci1";

type K230Sdhci = SdioSdmmc<Sdhci>;

#[cfg(not(feature = "paging"))]
static DIRECT_MMIO: DirectMmio = DirectMmio;

#[cfg(not(feature = "paging"))]
struct DirectMmio;

#[cfg(not(feature = "paging"))]
impl MmioOp for DirectMmio {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        let ptr = NonNull::new(phys_to_virt(pa!(addr.as_usize())).as_mut_ptr())
            .ok_or(MapError::Invalid)?;
        Ok(unsafe { MmioRaw::new(addr, ptr, size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}

ax_driver::model_register!(
    name: "Static K230 SDHCI1",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_sdhci1,
    }],
);

fn probe_sdhci1(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    register_sdhci(
        plat_dev,
        devices::SD1_PADDR,
        devices::SD1_SIZE,
        devices::SD1_IRQ,
    )
}

fn register_sdhci(
    plat_dev: PlatformDevice,
    paddr: usize,
    size: usize,
    irq: usize,
) -> Result<(), OnProbeError> {
    let mmio = map_mmio_raw(paddr, size).map_err(|err| {
        OnProbeError::other(format!("failed to map K230 SDHCI at {paddr:#x}: {err:?}"))
    })?;
    info!("k230-sdhci: probe SD1 at [PA:{paddr:#x}, SZ:{size:#x})");

    let mut host = unsafe { Sdhci::new(mmio.as_nonnull_ptr()) };
    host.reset_all()
        .map_err(|err| init_error(paddr, size, err))?;
    host.set_power(SDHCI_POWER_330);
    host.enable_interrupts();

    let mut card = SdioSdmmc::new(host);
    card.set_sd_uhs_selection_enabled(false);
    card.set_sd_speed_selection_enabled(false);
    let card_info =
        poll_card_init_sd(&mut card).map_err(|err| card_init_error(paddr, size, err))?;
    info!(
        "k230-sdhci: card kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
    );

    let raw = Arc::new(SpinNoIrq::new(card));
    let dev = BlockDevice {
        raw: raw.clone(),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        queue_created: false,
        irq_enabled: false,
    };
    plat_dev.register_block_with_irq(dev, Some(irq));
    info!("k230-sdhci: block device registered irq={irq}");
    Ok(())
}

fn map_mmio_raw(base: usize, size: usize) -> Result<MmioRaw, mmio_api::MapError> {
    #[cfg(feature = "paging")]
    {
        axklib::mmio::ioremap_raw(base.into(), size)
    }
    #[cfg(not(feature = "paging"))]
    {
        DIRECT_MMIO.ioremap(base.into(), size)
    }
}

fn poll_card_init_sd(card: &mut K230Sdhci) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request =
        card.submit_init_with_preference(CardInitPreference::SdFirst, &mut scratch)?;
    loop {
        match card.poll_init_request(&mut request)? {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    wait_init_pace();
                } else {
                    core::hint::spin_loop();
                }
            }
            OperationPoll::Complete(info) => return Ok(info),
            _ => return Err(Error::UnsupportedCommand),
        }
    }
}

fn wait_init_pace() {
    #[cfg(feature = "paging")]
    {
        axklib::time::busy_wait(core::time::Duration::from_millis(10));
    }

    #[cfg(not(feature = "paging"))]
    {
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }
}

struct BlockDevice {
    raw: Arc<SpinNoIrq<K230Sdhci>>,
    capacity_blocks: u64,
    queue_created: bool,
    irq_enabled: bool,
}

struct BlockQueue {
    raw: Arc<SpinNoIrq<K230Sdhci>>,
    capacity_blocks: u64,
    id: usize,
    slot: BlockRequestSlot,
    pending: Option<BlockRequest>,
    completed: Vec<RequestId>,
}

impl DriverGeneric for BlockDevice {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for BlockDevice {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(BlockQueue {
            raw: self.raw.clone(),
            capacity_blocks: self.capacity_blocks,
            id: 0,
            slot: BlockRequestSlot::default(),
            pending: None,
            completed: Vec::new(),
        }))
    }

    fn enable_irq(&mut self) {
        self.raw.lock().host_mut().enable_completion_irq();
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.raw.lock().host_mut().disable_completion_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        block_event_from_sdhci_irq(self.raw.lock().host_mut().handle_irq())
    }
}

fn block_event_from_sdhci_irq(irq_event: sdhci_host::Event) -> Event {
    match irq_event {
        sdhci_host::Event::None => Event::none(),
        sdhci_host::Event::CommandComplete
        | sdhci_host::Event::TransferComplete
        | sdhci_host::Event::Error { .. }
        | sdhci_host::Event::Other { .. } => {
            let mut event = Event::none();
            event.queue.insert(0);
            event
        }
    }
}

impl IQueue for BlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.capacity_blocks as usize
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn buff_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: BLOCK_SIZE,
            size: BLOCK_SIZE,
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        self.reap_pending_request()?;
        let mut raw = self.raw.lock();
        let start_block = block_addr_for_card(request.block_id, raw.is_high_capacity())?;
        let id = match request.kind {
            rd_block::RequestKind::Read(buffer) => {
                if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                    return Err(BlkError::Other("read buffer is not block aligned".into()));
                }
                let ptr = core::ptr::NonNull::new(buffer.virt)
                    .ok_or_else(|| BlkError::Other("read buffer pointer is null".into()))?;
                let size = NonZeroUsize::new(buffer.len())
                    .ok_or_else(|| BlkError::Other("read buffer is empty".into()))?;
                submit_read_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &mut self.slot,
                    &mut self.pending,
                )?
            }
            rd_block::RequestKind::Write(items) => {
                if !items.len().is_multiple_of(BLOCK_SIZE) {
                    return Err(BlkError::Other("write buffer is not block aligned".into()));
                }
                let ptr = core::ptr::NonNull::new(items.as_ptr() as *mut u8)
                    .ok_or_else(|| BlkError::Other("write buffer pointer is null".into()))?;
                let size = NonZeroUsize::new(items.len())
                    .ok_or_else(|| BlkError::Other("write buffer is empty".into()))?;
                submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    &mut self.slot,
                    &mut self.pending,
                )?
            }
        };
        Ok(RequestId::new(usize::from(id)))
    }

    fn poll_request(&mut self, request: RequestId) -> Result<(), BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(());
        }
        self.poll_active_request(request)
    }
}

impl BlockQueue {
    fn pending_id(&self) -> Option<sdhci_host::RequestId> {
        self.pending.as_ref().map(BlockRequest::id)
    }

    fn reap_pending_request(&mut self) -> Result<(), BlkError> {
        let Some(active) = self.pending_id() else {
            return Ok(());
        };
        let id = RequestId::new(usize::from(active));
        match self.poll_active_request(id) {
            Ok(()) => {
                self.completed.push(id);
                Ok(())
            }
            Err(BlkError::Retry) => Err(BlkError::Retry),
            Err(err) => Err(err),
        }
    }

    fn poll_active_request(&mut self, request: RequestId) -> Result<(), BlkError> {
        match self.raw.lock().host_mut().poll_block_request(
            &mut self.pending,
            sdhci_host::RequestId::new(usize::from(request)),
            &mut self.slot,
        ) {
            Ok(BlockPoll::Complete) => Ok(()),
            Ok(BlockPoll::Pending) => Err(BlkError::Retry),
            Ok(_) => Err(BlkError::Other(
                "SDHCI returned an unknown poll state".into(),
            )),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }
}

fn submit_read_request(
    host: &mut Sdhci,
    start_block: u32,
    buffer: core::ptr::NonNull<u8>,
    size: NonZeroUsize,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<sdhci_host::RequestId, BlkError> {
    if pending.is_some() {
        return Err(BlkError::Retry);
    }
    let request = host
        .submit_read_blocks(
            start_block,
            buffer,
            size,
            None,
            BlockTransferMode::Fifo,
            slot,
        )
        .map_err(map_dev_err_to_blk_err)?;
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn submit_write_request(
    host: &mut Sdhci,
    start_block: u32,
    buffer: core::ptr::NonNull<u8>,
    size: NonZeroUsize,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<sdhci_host::RequestId, BlkError> {
    if pending.is_some() {
        return Err(BlkError::Retry);
    }
    let request = host
        .submit_write_blocks(
            start_block,
            buffer,
            size,
            None,
            BlockTransferMode::Fifo,
            slot,
        )
        .map_err(map_dev_err_to_blk_err)?;
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn block_addr_for_card(block_id: usize, high_capacity: bool) -> Result<u32, BlkError> {
    let block_id = u32::try_from(block_id).map_err(|_| BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(BlkError::InvalidBlockIndex(block_id as usize))
    }
}

fn map_dev_err_to_blk_err(err: Error) -> BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned".into())
        }
        _ => BlkError::Other("SDHCI I/O error".into()),
    }
}

fn init_error(address: usize, size: usize, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize K230 SDHCI at [PA:{address:#x}, SZ:{size:#x}): {err:?}",
    ))
}

fn card_init_error(address: usize, size: usize, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "k230-sdhci: no responsive card at [PA:{address:#x}, SZ:{size:#x}); skipping \
             controller: {err:?}",
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
