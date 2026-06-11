// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{format, string::ToString, vec::Vec};
use core::{
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use dma_api::DeviceDma;
use log::{info, warn};
use rdif_clk::ClockId;
use rdrive::{
    Device, DriverGeneric,
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{BlockRequest, BlockRequestSlot, HostClock, RequestId, Sdhci};
use sdmmc_protocol::{
    BlockPoll, BlockTransferMode, Error, OperationPoll,
    error::{ErrorContext, Phase},
    sdio::{CardInfo, CardInitPreference, SdioHost, SdioInitScratch, SdioSdmmc},
};
use spin::Once;

use crate::{
    block::{ProbeFdtBlock, SharedDriver},
    mmio::iomap,
};

const BLOCK_SIZE: usize = 512;
const SDHCI_POWER_330: u8 = 0x0e;

const DWCMSHC_P_VENDOR_AREA1: usize = 0xe8;
const DWCMSHC_AREA1_MASK: u16 = 0x0fff;
const DWCMSHC_HOST_CTRL3: usize = 0x08;
const DWCMSHC_EMMC_CONTROL: usize = 0x2c;
const DWCMSHC_CARD_IS_EMMC: u16 = 1 << 0;
const DWCMSHC_EMMC_DLL_CTRL: usize = 0x800;
const DWCMSHC_EMMC_DLL_RXCLK: usize = 0x804;
const DWCMSHC_EMMC_DLL_TXCLK: usize = 0x808;
const DWCMSHC_EMMC_DLL_STRBIN: usize = 0x80c;
const DWCMSHC_EMMC_DLL_CMDOUT: usize = 0x810;
const DWCMSHC_EMMC_MISC_CON: usize = 0x81c;
const DWCMSHC_EMMC_DLL_BYPASS: u32 = 1 << 24;
const DWCMSHC_EMMC_DLL_START: u32 = 1 << 0;
const DWCMSHC_EMMC_DLL_DLYENA: u32 = 1 << 27;
const DLL_RXCLK_ORI_GATE: u32 = 1 << 31;
const DLL_STRBIN_DELAY_NUM_SEL: u32 = 1 << 26;
const DLL_STRBIN_DELAY_NUM_DEFAULT: u32 = 0x16;
const DLL_STRBIN_DELAY_NUM_OFFSET: u32 = 16;
const MISC_INTCLK_EN: u32 = 1 << 1;

const DWC_MSHC_PTR_PHY_R: usize = 0x300;
const PHY_CNFG_R: usize = DWC_MSHC_PTR_PHY_R;
const PHY_CMDPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x04;
const PHY_DATAPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x06;
const PHY_CLKPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x08;
const PHY_STBPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0a;
const PHY_RSTNPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0c;
const PHY_SDCLKDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x1d;
const PHY_SDCLKDL_DC_R: usize = DWC_MSHC_PTR_PHY_R + 0x1e;
const PHY_SMPLDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x20;
const PHY_DLL_CTRL_R: usize = DWC_MSHC_PTR_PHY_R + 0x24;
const PHY_DLL_CNFG2_R: usize = DWC_MSHC_PTR_PHY_R + 0x26;
const PHY_CNFG_RSTN_DEASSERT: u32 = 1 << 0;
const PHY_CNFG_PAD_SP: u32 = 0x0c;
const PHY_CNFG_PAD_SN: u32 = 0x0c;
const PHY_PAD_RXSEL_3V3: u16 = 0x2;
const PHY_PAD_WEAKPULL_PULLUP: u16 = 0x1;
const PHY_PAD_WEAKPULL_PULLDOWN: u16 = 0x2;
const PHY_PAD_TXSLEW_CTRL_P: u16 = 0x3;
const PHY_PAD_TXSLEW_CTRL_N: u16 = 0x3;
const PHY_SDCLKDL_CNFG_UPDATE: u8 = 1 << 4;
const PHY_SDCLKDL_DC_DEFAULT: u8 = 0x32;
const PHY_SMPLDL_CNFG_BYPASS_EN: u8 = 1 << 1;
const PHY_DLL_CTRL_ENABLE: u8 = 0x1;
const PHY_DLL_CNFG2_JUMPSTEP: u8 = 0x0a;

static SDHCI_CLOCK: RockchipSdhciClock = RockchipSdhciClock;

type RockchipSdhci = SdioSdmmc<Sdhci>;

struct RockchipSdhciClock;

impl HostClock for RockchipSdhciClock {
    fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
        set_sdhci_clock(target_hz)
    }
}

crate::model_register!(
    name: "Rockchip RK3568 sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3568-dwcmshc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
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
        "rockchip-rk3568-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    init_core_clock(info)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    if CLK_DEV.is_completed() {
        info!("rockchip-rk3568-sdhci: using external CRU clock");
        host.set_external_clock(&SDHCI_CLOCK);
    } else {
        warn!("rockchip-rk3568-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    info!("rockchip-rk3568-sdhci: reset controller");
    host.reset_all()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    init_dwcmshc_after_reset(mmio_base);
    host.set_power(SDHCI_POWER_330);
    host.enable_interrupts();
    host.set_dma(axklib::dma::device_with_mask(u32::MAX as u64));

    info!("rockchip-rk3568-sdhci: initialize card");
    let mut card = SdioSdmmc::new(host);
    let card_info = poll_card_init_mmc(&mut card)
        .map_err(|e| card_init_error(base_reg.address, mmio_size, e))?;
    info!(
        "SDHCI card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} cid={} \
         ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let raw = SharedDriver::new(card);
    let dev = BlockDevice {
        raw: Some(raw.clone()),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: AtomicBool::new(false),
        queue_created: false,
        irq_handler_taken: false,
    };
    let irq = probe.register_block(dev)?;
    info!(
        "rockchip-rk3568-sdhci block device registered irq={:?}",
        irq
    );
    Ok(())
}

fn poll_card_init_mmc(card: &mut RockchipSdhci) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request =
        card.submit_init_with_preference(CardInitPreference::MmcFirst, &mut scratch)?;
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

fn init_dwcmshc_after_reset(base: NonNull<u8>) {
    let area1 = vendor_area1(base);

    // Match Linux rk35xx reset/set_clock setup for identification speed:
    // keep the internal clock ungated, disable command-conflict checking,
    // and put Rockchip's DLL path in bypass while the bus runs below 52 MHz.
    write_u32(
        base,
        DWCMSHC_EMMC_MISC_CON,
        read_u32(base, DWCMSHC_EMMC_MISC_CON) | MISC_INTCLK_EN,
    );
    write_u32(base, area1 + DWCMSHC_HOST_CTRL3, 0);
    write_u16(
        base,
        area1 + DWCMSHC_EMMC_CONTROL,
        read_u16(base, area1 + DWCMSHC_EMMC_CONTROL) | DWCMSHC_CARD_IS_EMMC,
    );
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_CTRL,
        DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START,
    );
    write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, DLL_RXCLK_ORI_GATE);
    write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, 0);
    write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, 0);
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_STRBIN,
        DWCMSHC_EMMC_DLL_DLYENA
            | DLL_STRBIN_DELAY_NUM_SEL
            | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET),
    );
    init_dwcmshc_phy_3v3(base);
    info!(
        "rockchip-rk3568-sdhci: dwcmshc vendor init area1={:#x}",
        area1
    );
}

fn init_dwcmshc_phy_3v3(base: NonNull<u8>) {
    let phy_cfg = PHY_CNFG_RSTN_DEASSERT | (PHY_CNFG_PAD_SP << 16) | (PHY_CNFG_PAD_SN << 20);
    write_u32(base, PHY_CNFG_R, phy_cfg);
    write_u8(base, PHY_SDCLKDL_CNFG_R, PHY_SDCLKDL_CNFG_UPDATE);
    write_u8(base, PHY_SDCLKDL_DC_R, PHY_SDCLKDL_DC_DEFAULT);
    write_u8(base, PHY_DLL_CNFG2_R, PHY_DLL_CNFG2_JUMPSTEP);
    write_u8(base, PHY_SDCLKDL_CNFG_R, 0);

    let pad_pullup = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLUP << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CMDPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_DATAPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_RSTNPAD_CNFG_R, pad_pullup);

    let clk_pad = (PHY_PAD_TXSLEW_CTRL_P << 5) | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CLKPAD_CNFG_R, clk_pad);

    let strobe_pad = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLDOWN << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_STBPAD_CNFG_R, strobe_pad);
    write_u8(base, PHY_SMPLDL_CNFG_R, PHY_SMPLDL_CNFG_BYPASS_EN);
    write_u8(base, PHY_DLL_CTRL_R, PHY_DLL_CTRL_ENABLE);
}

fn vendor_area1(base: NonNull<u8>) -> usize {
    (read_u16(base, DWCMSHC_P_VENDOR_AREA1) & DWCMSHC_AREA1_MASK) as usize
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}

fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize SDHCI device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "rockchip-rk3568-sdhci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping \
             controller: {err:?}",
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

fn init_core_clock(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    for clk in info.node.clocks() {
        info!(
            "rockchip-sdhci clock: phandle <{}>, name: {:?}, cells: {}",
            clk.phandle, clk.name, clk.cells
        );
        if clk.name == Some("core".to_string()) {
            let device_id = info.phandle_to_device_id(clk.phandle).ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] core clock phandle {} has no device id",
                    info.node.name(),
                    clk.phandle
                ))
            })?;
            let clk_dev = rdrive::get::<rdif_clk::Clk>(device_id).map_err(|_| {
                OnProbeError::other(format!(
                    "[{}] core clock device {:?} is not registered",
                    info.node.name(),
                    device_id
                ))
            })?;
            CLK_DEV.call_once(|| ClkDev {
                inner: clk_dev,
                id: (clk.select().unwrap_or(0) as usize).into(),
            });
            return Ok(());
        }
    }
    Ok(())
}

fn set_sdhci_clock(target_hz: u32) -> Result<(), Error> {
    let clk = CLK_DEV.wait();
    let mut clk_dev = clk.inner.lock().map_err(|_| clock_error())?;
    clk_dev
        .set_rate(clk.id, target_hz as u64)
        .map_err(|_| clock_error())?;
    let rate = clk_dev.get_rate(clk.id).map_err(|_| clock_error())?;
    info!("rockchip-rk3568-sdhci: core clock set to {} Hz", rate);
    Ok(())
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

struct BlockDevice {
    raw: Option<SharedDriver<RockchipSdhci>>,
    capacity_blocks: u64,
    irq_enabled: AtomicBool,
    queue_created: bool,
    irq_handler_taken: bool,
}

struct BlockQueue {
    raw: SharedDriver<RockchipSdhci>,
    capacity_blocks: u64,
    id: usize,
    dma: DeviceDma,
    slot: BlockRequestSlot,
    pending: Option<BlockRequest>,
    completed: Vec<rdif_block::RequestId>,
}

impl DriverGeneric for BlockDevice {
    fn name(&self) -> &str {
        "rockchip-rk3568-sdhci"
    }
}

impl rdif_block::Interface for BlockDevice {
    fn device_info(&self) -> rdif_block::DeviceInfo {
        rdif_block::DeviceInfo {
            name: Some("rockchip-rk3568-sdhci"),
            ..rdif_block::DeviceInfo::new(self.capacity_blocks, BLOCK_SIZE)
        }
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        rk3568_queue_limits(u32::MAX as u64)
    }

    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(BlockQueue::new(dev.clone(), self.capacity_blocks, 0)) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(raw) = &self.raw {
            let mut enabled = false;
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                    warn!(
                        "rockchip-rk3568-sdhci: enable completion IRQ failed: {:?}",
                        err
                    );
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
                    warn!(
                        "rockchip-rk3568-sdhci: disable completion IRQ failed: {:?}",
                        err
                    );
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
        Some(alloc::boxed::Box::new(BlockIrqHandler { raw }))
    }
}

struct BlockIrqHandler {
    raw: SharedDriver<RockchipSdhci>,
}

impl rdif_block::IrqHandler for BlockIrqHandler {
    fn handle_irq(&self) -> rdif_block::Event {
        self.raw
            .try_with_mut(|raw| block_event_from_sdhci_irq(raw.host_mut().handle_irq()))
            .unwrap_or_else(rdif_block::Event::none)
    }
}

fn block_event_from_sdhci_irq(irq_event: sdhci_host::Event) -> rdif_block::Event {
    match irq_event {
        sdhci_host::Event::None => rdif_block::Event::none(),
        sdhci_host::Event::CommandComplete
        | sdhci_host::Event::TransferComplete
        | sdhci_host::Event::Error { .. }
        | sdhci_host::Event::Other { .. } => {
            let mut event = rdif_block::Event::none();
            event.queues.insert(0);
            event
        }
    }
}

// SAFETY: This queue has one pending request slot and does not retain segment
// access after the request is completed or failed.
unsafe impl rdif_block::IQueue for BlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: rdif_block::DeviceInfo {
                name: Some("rockchip-rk3568-sdhci"),
                ..rdif_block::DeviceInfo::new(self.capacity_blocks, BLOCK_SIZE)
            },
            limits: rk3568_queue_limits(self.dma.dma_mask()),
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

impl BlockQueue {
    fn new(raw: SharedDriver<RockchipSdhci>, capacity_blocks: u64, id: usize) -> Self {
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
            // Block I/O uses the host crate's submit/poll request API so
            // completions can be driven by IRQ wakeups. Protocol data commands
            // use the same submit/poll contract through SdioHost.
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
                "SDHCI returned an unknown poll state",
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

fn rk3568_block_transfer_mode() -> BlockTransferMode {
    BlockTransferMode::Fifo
}

fn rk3568_queue_limits(dma_mask: u64) -> rdif_block::QueueLimits {
    let (max_blocks_per_request, max_segment_size) = match rk3568_block_transfer_mode() {
        BlockTransferMode::Fifo => (1, BLOCK_SIZE),
        BlockTransferMode::Dma => (u16::MAX as u32 + 1, usize::MAX),
        _ => (1, BLOCK_SIZE),
    };
    rdif_block::QueueLimits {
        dma_mask,
        dma_alignment: BLOCK_SIZE,
        max_inflight: 1,
        max_blocks_per_request,
        max_segments: 1,
        max_segment_size,
        supported_flags: rdif_block::RequestFlags::NONE,
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}

fn submit_read_request(
    host: &mut Sdhci,
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
        transfer_dma(rk3568_block_transfer_mode(), dma),
        rk3568_block_transfer_mode(),
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => host
            .submit_read_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn submit_write_request(
    host: &mut Sdhci,
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
        transfer_dma(rk3568_block_transfer_mode(), dma),
        rk3568_block_transfer_mode(),
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => host
            .submit_write_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn transfer_dma(mode: BlockTransferMode, dma: &DeviceDma) -> Option<&DeviceDma> {
    match mode {
        BlockTransferMode::Dma => Some(dma),
        BlockTransferMode::Fifo => None,
        _ => None,
    }
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
            rdif_block::BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => rdif_block::BlkError::Io,
    }
}

static CLK_DEV: Once<ClkDev> = Once::new();

struct ClkDev {
    inner: Device<rdif_clk::Clk>,
    id: ClockId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk3568_block_io_uses_fifo_transfer_mode() {
        assert_eq!(rk3568_block_transfer_mode(), BlockTransferMode::Fifo);
    }

    #[test]
    fn rk3568_fifo_queue_limits_single_block_requests() {
        let limits = rk3568_queue_limits(u32::MAX as u64);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, BLOCK_SIZE);
        assert_eq!(limits.max_segments, 1);
    }
}
