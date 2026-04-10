extern crate alloc;

use alloc::format;
use core::{marker::PhantomData, ptr::NonNull};

use ax_alloc::{UsageKind, global_allocator};
use ax_driver_base::DeviceType;
use ax_driver_block::BlockDriverOps;
use ax_driver_virtio::{BufferDirection, PhysAddr as VirtIoPhysAddr, Transport, VirtIoHal};
use ax_plat::mem::PhysAddr;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};

use super::PlatformDeviceBlock;
use crate::drivers::iomap;

pub(super) type VirtIoBlkDevice<T> = ax_driver_virtio::VirtIoBlkDev<VirtIoHalImpl, T>;

module_driver!(
    name: "Virtio Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["virtio,mmio"],
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

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio_base = PhysAddr::from_usize(base_reg.address as usize);

    let mmio_base = iomap(mmio_base, mmio_size)?.as_ptr();

    let (ty, transport) =
        ax_driver_virtio::probe_mmio_device(mmio_base, mmio_size).ok_or(OnProbeError::NotMatch)?;

    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }

    let dev = VirtIoBlkDevice::try_new(transport).map_err(|e| {
        OnProbeError::other(format!(
            "failed to initialize Virtio Block device at [PA:{mmio_base:?},): {e:?}"
        ))
    })?;

    register_virtio_block(plat_dev, dev);
    debug!("virtio block device registered successfully");
    Ok(())
}

pub(super) fn register_virtio_block<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    dev: VirtIoBlkDevice<T>,
) {
    plat_dev.register_block(BlockDevice { dev: Some(dev) });
}

struct BlockDevice<T: Transport + 'static> {
    dev: Option<VirtIoBlkDevice<T>>,
}

struct BlockQueue<T: Transport + 'static> {
    raw: VirtIoBlkDevice<T>,
}

impl<T: Transport + 'static> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> rd_block::Interface for BlockDevice<T> {
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

impl<T: Transport + 'static> rd_block::IQueue for BlockQueue<T> {
    fn num_blocks(&self) -> usize {
        self.raw.num_blocks() as _
    }

    fn block_size(&self) -> usize {
        self.raw.block_size()
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
                self.raw
                    .read_block(id as _, &mut buffer)
                    .map_err(map_dev_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
            rd_block::RequestKind::Write(items) => {
                self.raw
                    .write_block(id as _, items)
                    .map_err(map_dev_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
        }
    }

    fn poll_request(&mut self, _request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        Ok(())
    }
}

fn map_dev_err_to_blk_err(err: ax_driver_base::DevError) -> rd_block::BlkError {
    match err {
        ax_driver_base::DevError::Again => rd_block::BlkError::Retry,
        ax_driver_base::DevError::AlreadyExists => {
            rd_block::BlkError::Other("Already exists".into())
        }
        ax_driver_base::DevError::BadState => {
            rd_block::BlkError::Other("Bad internal state".into())
        }
        ax_driver_base::DevError::InvalidParam => {
            rd_block::BlkError::Other("Invalid parameter".into())
        }
        ax_driver_base::DevError::Io => rd_block::BlkError::Other("I/O error".into()),
        ax_driver_base::DevError::NoMemory => rd_block::BlkError::NoMemory,
        ax_driver_base::DevError::ResourceBusy => rd_block::BlkError::Other("Resource busy".into()),
        ax_driver_base::DevError::Unsupported => rd_block::BlkError::NotSupported,
    }
}

pub(super) struct VirtIoHalImpl(PhantomData<()>);

unsafe impl VirtIoHal for VirtIoHalImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtIoPhysAddr, NonNull<u8>) {
        let vaddr = if let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma)
        {
            vaddr
        } else {
            return (0, NonNull::dangling());
        };
        let paddr = somehal::mem::virt_to_phys(vaddr as *mut u8) as VirtIoPhysAddr;
        let ptr = NonNull::new(vaddr as _).unwrap();
        (paddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        global_allocator().dealloc_pages(vaddr.as_ptr() as usize, pages, UsageKind::Dma);
        0
    }

    #[inline]
    unsafe fn mmio_phys_to_virt(paddr: VirtIoPhysAddr, size: usize) -> NonNull<u8> {
        iomap((paddr as usize).into(), size).expect("failed to map virtio MMIO region")
    }

    #[inline]
    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtIoPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        somehal::mem::virt_to_phys(vaddr as *mut u8) as VirtIoPhysAddr
    }

    #[inline]
    unsafe fn unshare(_paddr: VirtIoPhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
    }
}
