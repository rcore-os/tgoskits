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

use alloc::{format, string::ToString, sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull, time::Duration};

use ax_kspin::SpinNoIrq;
use dma_api::DeviceDma;
use rdif_clk::ClockId;
use rdrive::{
    Device, DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use sdhci_host::{AsyncDmaRequest, AsyncRequestSlot, RequestId, Sdhci};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    sdio::{DelayNs, SdioSdmmc},
};
use spin::Once;

use crate::drivers::{
    DmaImpl,
    blk::{PlatformDeviceBlock, decode_fdt_irq},
    iomap,
};

const BLOCK_SIZE: usize = 512;
const SDHCI_POWER_330: u8 = 0x0e;

type RockchipSdhci = SdioSdmmc<Sdhci, AxDelay>;

module_driver!(
    name: "Rockchip sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dwcmshc", "rockchip,dwcmshc-sdhci"],
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
        "rockchip-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;

    init_core_clock(&info)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    if CLK_DEV.is_completed() {
        info!("rockchip-sdhci: using external CRU clock");
        host.set_external_clock(set_sdhci_clock);
    } else {
        warn!("rockchip-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    info!("rockchip-sdhci: reset controller");
    host.reset_all()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    host.set_power(SDHCI_POWER_330);
    host.enable_interrupts();
    host.set_dma(DeviceDma::new(u32::MAX as u64, &DmaImpl));

    info!("rockchip-sdhci: initialize card");
    let mut card = SdioSdmmc::new(host, AxDelay);
    let card_info = card
        .init()
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

    let irq_num = decode_fdt_irq(&info.interrupts());
    let raw = Arc::new(SpinNoIrq::new(card));
    let dev = BlockDevice {
        raw: Some(raw.clone()),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: false,
        queue_created: false,
    };
    plat_dev.register_block_with_irq(dev, irq_num);
    info!("rockchip-sdhci block device registered irq={:?}", irq_num);
    Ok(())
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
            "rockchip-sdhci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: \
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
    info!("rockchip-sdhci: core clock set to {} Hz", rate);
    Ok(())
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

struct BlockDevice {
    raw: Option<Arc<SpinNoIrq<RockchipSdhci>>>,
    capacity_blocks: u64,
    irq_enabled: bool,
    queue_created: bool,
}

struct BlockQueue {
    raw: Arc<SpinNoIrq<RockchipSdhci>>,
    capacity_blocks: u64,
    async_slot: AsyncRequestSlot,
    pending: Option<AsyncDmaRequest>,
    completed: Vec<rd_block::RequestId>,
}

impl DriverGeneric for BlockDevice {
    fn name(&self) -> &str {
        "rockchip-sdhci"
    }
}

impl rd_block::Interface for BlockDevice {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(BlockQueue {
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
            raw.lock().host_mut().enable_data_irq();
            self.irq_enabled = true;
        }
    }

    fn disable_irq(&mut self) {
        if let Some(raw) = &self.raw {
            raw.lock().host_mut().disable_data_irq();
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
        match raw.lock().host_mut().handle_irq() {
            sdhci_host::Event::TransferComplete | sdhci_host::Event::Error { .. } => {
                let mut event = rd_block::Event::none();
                event.queue.insert(0);
                event
            }
            _ => rd_block::Event::none(),
        }
    }
}

impl rd_block::IQueue for BlockQueue {
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

impl BlockQueue {
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
        _ => rd_block::BlkError::Other("SDHCI I/O error".into()),
    }
}

static CLK_DEV: Once<ClkDev> = Once::new();

struct ClkDev {
    inner: Device<rdif_clk::Clk>,
    id: ClockId,
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
