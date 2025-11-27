extern crate alloc;

use alloc::{format, vec::Vec};
use core::time::Duration;

use rdif_block::Block;
use rdif_clk::ClockId;
use rdrive::{
    Device, DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use sdmmc::emmc::{self, EMmcHost};
use spin::Once;

use crate::iomap;

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
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);

    let mmio_base = iomap(base_reg.address, mmio_size)?;

    let clock = info.node.clocks().collect::<Vec<_>>();

    info!("perparing to init emmc with clock");

    for clk in &clock {
        info!(
            "clock: {}, select {}, name: {:?}, rate: {:?}",
            clk.node.name(),
            clk.select,
            clk.name,
            clk.clock_frequency
        );

        if clk.name == Some("core") {
            let id = info
                .phandle_to_device_id(clk.node.phandle().expect("clk no phandle"))
                .expect("no device id");

            let clk_dev = rdrive::get::<rdif_clk::Clk>(id).expect("clk not found");

            let clk_dev = ClkDev {
                inner: clk_dev,
                id: clk.select.into(),
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
    plat_dev.register(Block::new(dev));
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
    fn open(&mut self) -> Result<(), rdrive::KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), rdrive::KError> {
        Ok(())
    }
}

impl rdif_block::Interface for BlockDivce {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IQueue>> {
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

    fn handle_irq(&mut self) -> rdif_block::Event {
        rdif_block::Event::none()
    }
}

impl rdif_block::IQueue for BlockQueue {
    fn num_blocks(&self) -> usize {
        self.raw.get_block_num() as _
    }

    fn block_size(&self) -> usize {
        self.raw.get_block_size()
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rdif_block::BuffConfig {
        rdif_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        let id = request.block_id;
        match request.kind {
            rdif_block::RequestKind::Read(mut buffer) => {
                let blocks = buffer.len() / self.block_size();
                self.raw
                    .read_blocks(id as _, blocks as _, &mut buffer)
                    .map_err(maping_dev_err_to_blk_err)?;
                Ok(rdif_block::RequestId::new(0))
            }
            rdif_block::RequestKind::Write(items) => {
                let blocks = items.len() / self.block_size();
                self.raw
                    .write_blocks(id as _, blocks as _, items)
                    .map_err(maping_dev_err_to_blk_err)?;
                Ok(rdif_block::RequestId::new(0))
            }
        }
    }

    fn poll_request(
        &mut self,
        _request: rdif_block::RequestId,
    ) -> Result<(), rdif_block::BlkError> {
        Ok(())
    }
}

fn maping_dev_err_to_blk_err(err: sdmmc::err::SdError) -> rdif_block::BlkError {
    match err {
        sdmmc::err::SdError::Timeout | sdmmc::err::SdError::DataTimeout => {
            // transient timeout, ask caller to retry
            rdif_block::BlkError::Retry
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
            rdif_block::BlkError::Other("SD/MMC I/O error".into())
        }
        sdmmc::err::SdError::IoError => rdif_block::BlkError::Other("I/O error".into()),
        sdmmc::err::SdError::NoCard | sdmmc::err::SdError::UnsupportedCard => {
            // No card or unsupported card — treat as not supported
            rdif_block::BlkError::NotSupported
        }
        sdmmc::err::SdError::BusPower
        | sdmmc::err::SdError::Acmd12Error
        | sdmmc::err::SdError::AdmaError
        | sdmmc::err::SdError::CurrentLimit
        | sdmmc::err::SdError::TuningFailed
        | sdmmc::err::SdError::VoltageSwitchFailed
        | sdmmc::err::SdError::BusWidth => {
            rdif_block::BlkError::Other("SD/MMC controller error".into())
        }
        sdmmc::err::SdError::InvalidArgument => {
            rdif_block::BlkError::Other("Invalid argument".into())
        }
        sdmmc::err::SdError::BufferOverflow | sdmmc::err::SdError::MemoryError => {
            rdif_block::BlkError::NoMemory
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
