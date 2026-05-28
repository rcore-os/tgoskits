extern crate alloc;

use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(any(plat_dyn, plat_static))]
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

#[cfg(any(plat_static, plat_dyn))]
crate::model_register!(
    name: "VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(any(plat_static, plat_dyn))]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::Block)?;
    register_transport(plat_dev, transport)
}

#[cfg(plat_dyn)]
crate::model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

#[cfg(plat_dyn)]
fn probe_fdt(
    info: rdrive::register::FdtInfo<'_>,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
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
        irq_enabled: AtomicBool::new(false),
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
    irq_enabled: AtomicBool,
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
            max_transfer_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
            preferred_transfer_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
            supported_flags: rdif_block::RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }

    fn queue_topology(&self) -> rdif_block::QueueTopology {
        rdif_block::QueueTopology::single(1)
    }

    fn create_queue(
        &mut self,
        config: rdif_block::QueueConfig,
    ) -> Option<alloc::boxed::Box<dyn rdif_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.dev.as_ref().map(|dev| {
            self.queue_created = true;
            alloc::boxed::Box::new(BlockQueue {
                id: config.id_hint.unwrap_or(0),
                depth: config.depth.max(1),
                mode: config.mode,
                raw: dev.clone(),
            }) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.with_mut(|dev| dev.raw.enable_interrupts());
        }
        self.irq_enabled.store(true, Ordering::Release);
    }

    fn disable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.with_mut(|dev| dev.raw.disable_interrupts());
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }
}

struct BlockQueue<T: Transport + 'static> {
    id: usize,
    depth: usize,
    mode: rdif_block::QueueMode,
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
            depth: self.depth,
            mode: self.mode,
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
                max_transfer_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
                preferred_transfer_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
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
