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

use alloc::{format, sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull, time::Duration};

use ax_kspin::SpinNoIrq;
use dma_api::DeviceDma;
use dwmmc_host::{AsyncDmaRequest, AsyncRequestSlot, DwMmc, RequestId};
use rdif_clk::ClockId;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use sdmmc_protocol::{
    Error,
    error::Phase,
    sdio::{DelayNs, SdioHost, SdioSdmmc},
};

use crate::drivers::{
    DmaImpl,
    blk::{PlatformDeviceBlock, decode_fdt_irq},
    iomap,
    soc::scmi,
};

const BLOCK_SIZE: usize = 512;
const DWMMC_STABLE_REFERENCE_CLOCK: u32 = 50_000_000;
const ENABLE_SD_SPEED_SELECTION: bool = true;
const RK3588_CRU_BASE: usize = 0xfd7c_0000;
const RK3588_CRU_SIZE: usize = 0x5c000;
const RK3588_SDMMC_CON0: usize = 0x0c30;
const RK3588_SDMMC_CON1: usize = 0x0c34;
const RK3588_SDMMC_PHASE_SHIFT: u32 = 1;
const RK3588_SDMMC_DRV_PHASE_DEG: u32 = 90;
const RK3588_SDMMC_SAMPLE_PHASE_DEG: u32 = 0;
const RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES: [u32; 8] = [0, 45, 90, 135, 180, 225, 270, 315];

type RockchipDwMmc = SdioSdmmc<DwMmc, AxDelay>;

module_driver!(
    name: "Rockchip SD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dw-mshc", "rockchip,rk3288-dw-mshc"],
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
        "rockchip-dwmmc probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;

    let mut host = unsafe { DwMmc::new(mmio_base) };
    let reference_clock = dwmmc_reference_clock(&info);
    if let Some(reference_clock) = reference_clock {
        info!(
            "rockchip-dwmmc: using ciu reference clock {} Hz",
            reference_clock
        );
        host.set_reference_clock(reference_clock);
        if is_rk3588_dwmmc(&info) {
            init_rk3588_sdmmc_phase(&info, reference_clock)?;
        }
    } else {
        warn!(
            "rockchip-dwmmc: ciu clock not found; leaving DWMMC divider bypassed and relying on \
             CRU rate"
        );
    }
    info!("rockchip-dwmmc: reset controller");
    host.reset_and_init()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    host.set_dma(DeviceDma::new(u32::MAX as u64, &DmaImpl));

    info!("rockchip-dwmmc: initialize card");
    let mut sd = SdioSdmmc::new(host, AxDelay);
    sd.set_sd_speed_selection_enabled(ENABLE_SD_SPEED_SELECTION);
    let card_info = sd.init().map_err(|e| {
        warn!("rockchip-dwmmc: card init failed: {:?}", e);
        card_init_error(base_reg.address, mmio_size, e)
    })?;
    info!(
        "rockchip-dwmmc card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} \
         cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    if let Some(reference_clock) = reference_clock
        && is_rk3588_dwmmc(&info)
    {
        tune_rk3588_sdmmc_sample_phase(&mut sd, reference_clock);
    }

    let irq_num = decode_fdt_irq(&info.interrupts());
    let raw = Arc::new(SpinNoIrq::new(sd));
    let dev = SdBlockDevice {
        raw: Some(raw.clone()),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: false,
        queue_created: false,
    };
    plat_dev.register_block_with_irq(dev, irq_num);
    info!("rockchip-sd block device registered irq={:?}", irq_num);
    Ok(())
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize DWMMC device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "rockchip-dwmmc: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: \
             {err:?}",
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

fn dwmmc_reference_clock(info: &FdtInfo<'_>) -> Option<u32> {
    let Some(clk) = info.find_clk_by_name("ciu") else {
        return None;
    };
    let Some(device_id) = info.phandle_to_device_id(clk.phandle) else {
        warn!(
            "[{}] ciu clock phandle {} has no device id",
            info.node.name(),
            clk.phandle
        );
        return None;
    };
    let clk_dev = match rdrive::get::<rdif_clk::Clk>(device_id) {
        Ok(clk_dev) => clk_dev,
        Err(_) => {
            let clock_id = clk.select().unwrap_or(0);
            if scmi::set_clock_rate(clk.phandle, clock_id, DWMMC_STABLE_REFERENCE_CLOCK as u64)
                .is_some()
            {
                return Some(DWMMC_STABLE_REFERENCE_CLOCK);
            }
            if let Some(rate) = scmi::clock_rate(clk.phandle, clock_id) {
                return validate_reference_clock(info, rate);
            }
            warn!(
                "[{}] ciu clock device {:?} is not registered",
                info.node.name(),
                device_id
            );
            return None;
        }
    };
    let mut clk_guard = match clk_dev.lock() {
        Ok(clk_guard) => clk_guard,
        Err(_) => {
            warn!(
                "[{}] ciu clock device {:?} is locked",
                info.node.name(),
                device_id
            );
            return None;
        }
    };
    let clock_id = ClockId::from(clk.select().unwrap_or(0) as usize);
    if let Err(err) = clk_guard.set_rate(clock_id, DWMMC_STABLE_REFERENCE_CLOCK as u64) {
        warn!(
            "[{}] failed to set ciu clock {:?} to {} Hz: {:?}",
            info.node.name(),
            clock_id,
            DWMMC_STABLE_REFERENCE_CLOCK,
            err
        );
    }
    let rate = match clk_guard.get_rate(clock_id) {
        Ok(rate) => rate,
        Err(err) => {
            warn!(
                "[{}] failed to read ciu clock {:?}: {:?}",
                info.node.name(),
                clock_id,
                err
            );
            return None;
        }
    };
    validate_reference_clock(info, rate)
}

fn is_rk3588_dwmmc(info: &FdtInfo<'_>) -> bool {
    info.node
        .as_node()
        .compatibles()
        .any(|compatible| compatible == "rockchip,rk3588-dw-mshc")
}

fn init_rk3588_sdmmc_phase(info: &FdtInfo<'_>, parent_rate: u32) -> Result<(), OnProbeError> {
    let has_drive_clk = info.find_clk_by_name("ciu-drive").is_some();
    let has_sample_clk = info.find_clk_by_name("ciu-sample").is_some();
    if !has_drive_clk || !has_sample_clk {
        warn!(
            "[{}] RK3588 SDMMC phase clocks missing: ciu-drive={} ciu-sample={}",
            info.node.name(),
            has_drive_clk,
            has_sample_clk
        );
        return Ok(());
    }

    let cru = iomap(RK3588_CRU_BASE.into(), RK3588_CRU_SIZE)?;
    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON0,
        parent_rate,
        RK3588_SDMMC_DRV_PHASE_DEG,
    );
    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON1,
        parent_rate,
        RK3588_SDMMC_SAMPLE_PHASE_DEG,
    );
    info!(
        "rockchip-dwmmc: RK3588 SDMMC phase configured: drive={}deg sample={}deg parent={}Hz",
        RK3588_SDMMC_DRV_PHASE_DEG, RK3588_SDMMC_SAMPLE_PHASE_DEG, parent_rate
    );
    Ok(())
}

fn tune_rk3588_sdmmc_sample_phase(sd: &mut RockchipDwMmc, parent_rate: u32) {
    let Ok(cru) = iomap(RK3588_CRU_BASE.into(), RK3588_CRU_SIZE) else {
        warn!("rockchip-dwmmc: failed to map RK3588 CRU for sample phase scan");
        return;
    };

    for sample_phase in RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES {
        set_rk3588_mmc_phase(cru, RK3588_SDMMC_CON1, parent_rate, sample_phase);

        let mut block0 = [0; BLOCK_SIZE];
        let block0_result = sd.read_block(0, &mut block0);
        let block0_valid = block0_result.is_ok() && has_mbr_signature(&block0);

        let mut block1 = [0; BLOCK_SIZE];
        let block1_result = sd.read_block(1, &mut block1);
        let block1_valid = block1_result.is_ok() && has_gpt_header(&block1);

        info!(
            "rockchip-dwmmc: sample phase probe {}deg: block0_ok={} mbr_sig={:02x}{:02x} \
             block0_head={:02x?} block1_ok={} gpt_head={:02x?}",
            sample_phase,
            block0_result.is_ok(),
            block0[511],
            block0[510],
            &block0[..16],
            block1_result.is_ok(),
            &block1[..8]
        );

        if block0_valid || block1_valid {
            set_rk3588_mmc_phase(cru, RK3588_SDMMC_CON1, parent_rate, sample_phase);
            info!(
                "rockchip-dwmmc: selected RK3588 SDMMC sample phase {}deg",
                sample_phase
            );
            return;
        }
    }

    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON1,
        parent_rate,
        RK3588_SDMMC_SAMPLE_PHASE_DEG,
    );
    warn!(
        "rockchip-dwmmc: no valid RK3588 SDMMC sample phase found; restored {}deg",
        RK3588_SDMMC_SAMPLE_PHASE_DEG
    );
}

fn set_rk3588_mmc_phase(cru: NonNull<u8>, offset: usize, parent_rate: u32, degrees: u32) {
    let delay_num = rk3588_mmc_delay_num(parent_rate, degrees);
    let raw_value = if delay_num != 0 { 1 << 10 } else { 0 }
        | ((delay_num & 0xff) << 2)
        | ((degrees / 90) & 0x03);
    let reg_value =
        ((0x07ff_u32 << RK3588_SDMMC_PHASE_SHIFT) << 16) | (raw_value << RK3588_SDMMC_PHASE_SHIFT);
    unsafe {
        (cru.as_ptr().add(offset) as *mut u32).write_volatile(reg_value);
    }
}

fn rk3588_mmc_delay_num(parent_rate: u32, degrees: u32) -> u32 {
    let degree = degrees % 360;
    let remainder = degree % 90;
    if parent_rate == 0 {
        0
    } else {
        div_round_closest(
            10_000_000_u64 * remainder as u64,
            (parent_rate as u64 / 1_000) * 36 * 6,
        )
        .min(255) as u32
    }
}

fn div_round_closest(numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        0
    } else {
        (numerator + denominator / 2) / denominator
    }
}

fn has_mbr_signature(block: &[u8; BLOCK_SIZE]) -> bool {
    block[510] == 0x55 && block[511] == 0xaa
}

fn has_gpt_header(block: &[u8; BLOCK_SIZE]) -> bool {
    &block[..8] == b"EFI PART"
}

fn validate_reference_clock(info: &FdtInfo<'_>, rate: u64) -> Option<u32> {
    if rate == 0 || rate > u32::MAX as u64 {
        warn!("[{}] invalid ciu clock rate {} Hz", info.node.name(), rate);
        return None;
    }
    Some(rate as u32)
}

struct SdBlockDevice {
    raw: Option<Arc<SpinNoIrq<RockchipDwMmc>>>,
    capacity_blocks: u64,
    irq_enabled: bool,
    queue_created: bool,
}

struct SdBlockQueue {
    raw: Arc<SpinNoIrq<RockchipDwMmc>>,
    capacity_blocks: u64,
    async_slot: AsyncRequestSlot,
    pending: Option<AsyncDmaRequest>,
    completed: Vec<rd_block::RequestId>,
}

impl DriverGeneric for SdBlockDevice {
    fn name(&self) -> &str {
        "rockchip-sd"
    }
}

impl rd_block::Interface for SdBlockDevice {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(SdBlockQueue {
                raw: dev.clone(),
                capacity_blocks: self.capacity_blocks,
                async_slot: AsyncRequestSlot::default(),
                pending: None,
                completed: Vec::new(),
            }) as _
        })
    }

    fn enable_irq(&mut self) {
        if let Some(raw) = &self.raw {
            let mut raw = raw.lock();
            if let Err(err) = SdioHost::enable_data_irq(raw.host_mut()) {
                warn!("rockchip-dwmmc: enable data IRQ failed: {:?}", err);
                return;
            }
            self.irq_enabled = true;
        }
    }

    fn disable_irq(&mut self) {
        if let Some(raw) = &self.raw {
            let mut raw = raw.lock();
            if let Err(err) = SdioHost::disable_data_irq(raw.host_mut()) {
                warn!("rockchip-dwmmc: disable data IRQ failed: {:?}", err);
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
        block_event_from_dwmmc_irq(irq_event)
    }
}

fn block_event_from_dwmmc_irq(irq_event: dwmmc_host::Event) -> rd_block::Event {
    match irq_event {
        dwmmc_host::Event::TransferComplete | dwmmc_host::Event::Error { .. } => {
            let mut event = rd_block::Event::none();
            event.queue.insert(0);
            event
        }
        _ => rd_block::Event::none(),
    }
}

impl rd_block::IQueue for SdBlockQueue {
    fn num_blocks(&self) -> usize {
        self.capacity_blocks as usize
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: u64::MAX,
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
        // Block I/O uses the host crate's submit/poll DMA API so completions
        // can be driven by IRQ wakeups. The SdioHost read_data/write_data
        // methods remain the synchronous protocol path, with PIO fallback.
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
                let request = raw
                    .host_mut()
                    .submit_dma_read_blocks(
                        start_block,
                        ptr,
                        size,
                        &DeviceDma::new(u32::MAX as u64, &DmaImpl),
                        &mut self.async_slot,
                    )
                    .map_err(map_dev_err_to_blk_err)?;
                let id = request.id();
                self.pending = Some(request);
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
                let request = raw
                    .host_mut()
                    .submit_dma_write_blocks(
                        start_block,
                        ptr,
                        size,
                        &DeviceDma::new(u32::MAX as u64, &DmaImpl),
                        &mut self.async_slot,
                    )
                    .map_err(map_dev_err_to_blk_err)?;
                let id = request.id();
                self.pending = Some(request);
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

impl SdBlockQueue {
    fn poll_active_request(
        &mut self,
        request: rd_block::RequestId,
    ) -> Result<(), rd_block::BlkError> {
        self.raw
            .lock()
            .host_mut()
            .poll_async_dma_request(
                &mut self.pending,
                RequestId::new(usize::from(request)),
                &mut self.async_slot,
            )
            .map_err(map_dev_err_to_blk_err)
    }

    fn reap_pending_request(&mut self) -> Result<(), rd_block::BlkError> {
        let Some(active) = self.pending.as_ref() else {
            return Ok(());
        };
        let id = rd_block::RequestId::new(usize::from(active.id()));
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
        Error::Timeout(_) => rd_block::BlkError::Retry,
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => {
            rd_block::BlkError::NotSupported
        }
        Error::Misaligned | Error::InvalidArgument => {
            rd_block::BlkError::Other("SD/MMC request is not block aligned".into())
        }
        _ => rd_block::BlkError::Other("DWMMC I/O error".into()),
    }
}

#[derive(Clone, Copy)]
struct AxDelay;

impl DelayNs for AxDelay {
    fn delay_ns(&mut self, ns: u32) {
        axklib::time::busy_wait(Duration::from_nanos(ns as u64));
    }

    fn delay_us(&mut self, us: u32) {
        axklib::time::busy_wait(Duration::from_micros(us as u64));
    }

    fn delay_ms(&mut self, ms: u32) {
        axklib::time::busy_wait(Duration::from_millis(ms as u64));
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
