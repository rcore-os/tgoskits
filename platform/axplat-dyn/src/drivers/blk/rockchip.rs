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
use core::time::Duration;

use rdif_clk::ClockId;
use rdrive::{
    Device, DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use sdmmc::emmc::{self, EMmcHost};
use spin::Once;

use crate::drivers::{blk::PlatformDeviceBlock, iomap};

module_driver!(
    name: "Rockchip sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,dwcmshc-sdhci"],
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

    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;

    let clock: Vec<_> = info.node.clocks().into_iter().collect();

    info!("perparing to init emmc with clock");

    for clk in &clock {
        info!(
            "clock: phandle {}, name: {:?}, cells: {:?}",
            clk.phandle, clk.name, clk.cells
        );

        if clk.name == Some("core".to_string()) {
            let id = info
                .phandle_to_device_id(clk.phandle)
                .expect("no device id");

            let clk_dev = rdrive::get::<rdif_clk::Clk>(id).expect("clk not found");

            let clk_dev = ClkDev {
                inner: clk_dev,
                id: (clk.select().unwrap_or(0) as usize).into(),
                // TODO: verify the id
                // id: 300.into(),
            };
            CLK_DEV.call_once(|| clk_dev);

            emmc::clock::init_global_clk(CLK_DEV.wait());
        }
    }

    let mut emmc = EMmcHost::new(mmio_base.as_ptr() as usize);
    emmc.init().map_err(|e| {
        OnProbeError::other(format!(
            "failed to initialize eMMC device at [PA:{:?}, SZ:0x{:x}): {e:?}",
            base_reg.address, mmio_size
        ))
    })?;
    let info = emmc.get_card_info().map_err(|e| {
        OnProbeError::other(format!(
            "failed to get eMMC card info at [PA:{:?}, SZ:0x{:x}): {e:?}",
            base_reg.address, mmio_size
        ))
    })?;
    info!("eMMC card info: {:#?}", info);

    let dev = BlockDivce { dev: Some(emmc) };
    plat_dev.register_block(dev);
    debug!("virtio block device registered successfully");
    Ok(())
}

struct BlockDivce {
    dev: Option<EMmcHost>,
}

struct BlockQueue {
    raw: EMmcHost,
}

impl DriverGeneric for BlockDivce {
    fn name(&self) -> &str {
        "rockchip-emmc"
    }
}

impl rd_block::Interface for BlockDivce {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        self.dev
            .take()
            .map(|dev| alloc::boxed::Box::new(BlockQueue { raw: dev }) as _)
    }

    fn enable_irq(&mut self) {
        todo!()
    }

    fn disable_irq(&mut self) {
        todo!()
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
}

impl rd_block::IQueue for BlockQueue {
    fn num_blocks(&self) -> usize {
        self.raw.get_block_num() as _
    }

    fn block_size(&self) -> usize {
        self.raw.get_block_size()
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(
        &mut self,
        request: rd_block::Request<'_>,
    ) -> Result<rd_block::RequestId, rd_block::BlkError> {
        let id = request.block_id;
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => {
                let blocks = buffer.len() / self.block_size();
                self.raw
                    .read_blocks(id as _, blocks as _, &mut buffer)
                    .map_err(maping_dev_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
            rd_block::RequestKind::Write(items) => {
                let blocks = items.len() / self.block_size();
                self.raw
                    .write_blocks(id as _, blocks as _, items)
                    .map_err(maping_dev_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
        }
    }

    fn poll_request(&mut self, _request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        Ok(())
    }
}

fn maping_dev_err_to_blk_err(err: sdmmc::err::SdError) -> rd_block::BlkError {
    match err {
        sdmmc::err::SdError::Timeout | sdmmc::err::SdError::DataTimeout => {
            // transient timeout, ask caller to retry
            rd_block::BlkError::Retry
        }
        sdmmc::err::SdError::Crc
        | sdmmc::err::SdError::DataCrc
        | sdmmc::err::SdError::EndBit
        | sdmmc::err::SdError::Index
        | sdmmc::err::SdError::DataEndBit
        | sdmmc::err::SdError::BadMessage
        | sdmmc::err::SdError::InvalidResponse
        | sdmmc::err::SdError::InvalidResponseType
        | sdmmc::err::SdError::CommandError
        | sdmmc::err::SdError::TransferError
        | sdmmc::err::SdError::DataError
        | sdmmc::err::SdError::CardError(..) => {
            // CRC/response/transfer related errors => I/O error
            rd_block::BlkError::Other("SD/MMC I/O error".into())
        }
        sdmmc::err::SdError::IoError => rd_block::BlkError::Other("I/O error".into()),
        sdmmc::err::SdError::NoCard | sdmmc::err::SdError::UnsupportedCard => {
            // No card or unsupported card — treat as not supported
            rd_block::BlkError::NotSupported
        }
        sdmmc::err::SdError::BusPower
        | sdmmc::err::SdError::Acmd12Error
        | sdmmc::err::SdError::AdmaError
        | sdmmc::err::SdError::CurrentLimit
        | sdmmc::err::SdError::TuningFailed
        | sdmmc::err::SdError::VoltageSwitchFailed
        | sdmmc::err::SdError::BusWidth => {
            rd_block::BlkError::Other("SD/MMC controller error".into())
        }
        sdmmc::err::SdError::InvalidArgument => {
            rd_block::BlkError::Other("Invalid argument".into())
        }
        sdmmc::err::SdError::BufferOverflow | sdmmc::err::SdError::MemoryError => {
            rd_block::BlkError::NoMemory
        }
    }
}

static CLK_DEV: Once<ClkDev> = Once::new();

struct ClkDev {
    inner: Device<rdif_clk::Clk>,
    id: ClockId,
}

impl emmc::clock::Clk for ClkDev {
    fn emmc_get_clk(&self) -> Result<u64, emmc::clock::ClkError> {
        let g = self.inner.lock().unwrap();
        g.get_rate(self.id)
            .map_err(|_| emmc::clock::ClkError::InvalidPeripheralId)
    }

    fn emmc_set_clk(&self, rate: u64) -> Result<u64, emmc::clock::ClkError> {
        let mut g = self.inner.lock().unwrap();
        g.set_rate(self.id, rate)
            .map_err(|_| emmc::clock::ClkError::InvalidPeripheralId)?;
        g.get_rate(self.id)
            .map_err(|_| emmc::clock::ClkError::InvalidPeripheralId)
    }
}

struct Osal {}

impl sdmmc::Kernel for Osal {
    fn sleep(us: u64) {
        axklib::time::busy_wait(Duration::from_micros(us));
    }
}

sdmmc::set_impl!(Osal);
