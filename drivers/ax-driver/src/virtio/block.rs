extern crate alloc;

use alloc::format;

use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(all(feature = "pci", any(plat_dyn, plat_static)))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::Transport,
};

use crate::{
    block::{PlatformDeviceBlock, SharedDriver},
    virtio::VirtIoHalImpl,
};

const VIRTIO_BLK_DMA_BUFFER_SIZE: usize = 32 * SECTOR_SIZE;

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
model_register!(
    name: "VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::Block)?;
    register_transport(probe.into_platform_device(), transport)
}

#[cfg(plat_dyn)]
model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

#[cfg(plat_dyn)]
fn probe_fdt(probe: rdrive::register::ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let (ty, transport) = crate::virtio::probe_fdt_mmio_device(&info)?;
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    register_transport(plat_dev, transport)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let dev = VirtIoBlkDevice::new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-blk: {err:?}")))?;
    plat_dev.register_block(BlockDevice {
        dev: Some(SharedDriver::new(dev)),
        queue_created: false,
    });
    log::info!("registered virtio block device");
    Ok(())
}

struct VirtIoBlkDevice<T: Transport + 'static> {
    raw: VirtIOBlk<VirtIoHalImpl, T>,
}

unsafe impl<T: Transport + 'static> Send for VirtIoBlkDevice<T> {}

impl<T: Transport + 'static> VirtIoBlkDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOBlk::new(transport)?;
        raw.disable_interrupts();
        Ok(Self { raw })
    }
}

struct BlockDevice<T: Transport + 'static> {
    dev: Option<SharedDriver<VirtIoBlkDevice<T>>>,
    queue_created: bool,
}

impl<T: Transport + 'static> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> rdif_block::Interface for BlockDevice<T> {
    fn device_info(&self) -> rdif_block::DeviceInfo {
        let blocks = self
            .dev
            .as_ref()
            .map(|dev| dev.with_mut(|raw| raw.raw.capacity()))
            .unwrap_or(0);
        rdif_block::DeviceInfo {
            name: Some("virtio-blk"),
            ..rdif_block::DeviceInfo::new(blocks, SECTOR_SIZE)
        }
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        rdif_block::QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 0x1000,
            max_blocks_per_request: (VIRTIO_BLK_DMA_BUFFER_SIZE / SECTOR_SIZE) as u32,
            max_segments: 1,
            max_segment_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
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
        self.dev.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(BlockQueue {
                id: 0,
                raw: dev.clone(),
            }) as _
        })
    }

    fn enable_irq(&self) {
        self.disable_irq();
    }

    fn disable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.with_mut(|dev| dev.raw.disable_interrupts());
        }
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }
}

struct BlockQueue<T: Transport + 'static> {
    id: usize,
    raw: SharedDriver<VirtIoBlkDevice<T>>,
}

// SAFETY: virtio-blk operations are submitted to the underlying synchronous
// driver and completed before `submit_request` returns. No request segment is
// retained after the call.
unsafe impl<T: Transport + 'static> rdif_block::IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        let blocks = self.raw.with_mut(|raw| raw.raw.capacity());
        rdif_block::QueueInfo {
            id: self.id,
            device: rdif_block::DeviceInfo {
                name: Some("virtio-blk"),
                ..rdif_block::DeviceInfo::new(blocks, SECTOR_SIZE)
            },
            limits: rdif_block::QueueLimits {
                dma_mask: u64::MAX,
                dma_alignment: 0x1000,
                max_blocks_per_request: (VIRTIO_BLK_DMA_BUFFER_SIZE / SECTOR_SIZE) as u32,
                max_segments: 1,
                max_segment_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
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
        rdif_block::validate_request(self.info(), &request)?;
        match request.op {
            rdif_block::RequestOp::Read => {
                let mut segment = request
                    .segments
                    .first()
                    .copied()
                    .ok_or(rdif_block::BlkError::InvalidRequest)?;
                self.raw
                    .with_mut(|raw| raw.raw.read_blocks(request.lba as usize, &mut segment))
                    .map_err(map_virtio_err_to_blk_err)?;
            }
            rdif_block::RequestOp::Write => {
                let segment = request
                    .segments
                    .first()
                    .ok_or(rdif_block::BlkError::InvalidRequest)?;
                self.raw
                    .with_mut(|raw| raw.raw.write_blocks(request.lba as usize, segment))
                    .map_err(map_virtio_err_to_blk_err)?;
            }
            rdif_block::RequestOp::Flush
            | rdif_block::RequestOp::Discard
            | rdif_block::RequestOp::WriteZeroes => return Err(rdif_block::BlkError::NotSupported),
        }
        Ok(rdif_block::RequestId::new(0))
    }

    fn poll_request(
        &mut self,
        _request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        Ok(rdif_block::RequestStatus::Complete)
    }
}

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rdif_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => rdif_block::BlkError::Other("bad internal state"),
        VirtIoError::AlreadyUsed => rdif_block::BlkError::Other("already exists"),
        VirtIoError::InvalidParam => rdif_block::BlkError::InvalidRequest,
        VirtIoError::DmaError => rdif_block::BlkError::NoMemory,
        VirtIoError::IoError => rdif_block::BlkError::Io,
        VirtIoError::Unsupported => rdif_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rdif_block::BlkError::Other("socket error"),
    }
}
