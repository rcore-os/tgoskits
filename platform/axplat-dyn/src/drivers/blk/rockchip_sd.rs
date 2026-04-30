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

use alloc::format;

use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};

use crate::drivers::{blk::PlatformDeviceBlock, iomap};

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
    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;

    // simple-sdmmc handles controller-level clock setup and card initialization internally
    let sd = unsafe { simple_sdmmc::SdMmc::new(mmio_base.as_ptr() as usize) };

    let dev = SdBlockDevice { dev: Some(sd) };
    plat_dev.register_block(dev);
    info!("rockchip-sd block device registered");
    Ok(())
}

struct SdBlockDevice {
    dev: Option<simple_sdmmc::SdMmc>,
}

struct SdBlockQueue {
    raw: simple_sdmmc::SdMmc,
}

impl DriverGeneric for SdBlockDevice {
    fn name(&self) -> &str {
        "rockchip-sd"
    }
}

impl rd_block::Interface for SdBlockDevice {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        self.dev
            .take()
            .map(|dev| alloc::boxed::Box::new(SdBlockQueue { raw: dev }) as _)
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
}

impl rd_block::IQueue for SdBlockQueue {
    fn num_blocks(&self) -> usize {
        self.raw.num_blocks() as _
    }

    fn block_size(&self) -> usize {
        simple_sdmmc::SdMmc::BLOCK_SIZE
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: u64::MAX,
            align: self.block_size(),
            size: self.block_size(),
        }
    }

    fn submit_request(
        &mut self,
        request: rd_block::Request<'_>,
    ) -> Result<rd_block::RequestId, rd_block::BlkError> {
        let start_block = request.block_id as u32;
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => {
                self.raw.read_blocks(start_block, &mut buffer);
                Ok(rd_block::RequestId::new(0))
            }
            rd_block::RequestKind::Write(items) => {
                self.raw.write_blocks(start_block, items);
                Ok(rd_block::RequestId::new(0))
            }
        }
    }

    fn poll_request(&mut self, _request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        Ok(())
    }
}
