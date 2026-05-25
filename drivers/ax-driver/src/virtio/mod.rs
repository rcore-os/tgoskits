use core::{marker::PhantomData, ptr::NonNull};

use ax_alloc::{UsageKind, global_allocator};
#[cfg(feature = "virtio-net")]
use virtio_drivers::Error as VirtIoError;
use virtio_drivers::{
    BufferDirection, Hal as VirtIoHal, PhysAddr as VirtIoPhysAddr,
    transport::{DeviceType, Transport, mmio::MmioTransport},
};

#[cfg(feature = "virtio-blk")]
pub mod block;
#[cfg(feature = "virtio-gpu")]
pub mod display;
#[cfg(feature = "virtio-input")]
pub mod input;
#[cfg(feature = "virtio-net")]
pub mod net;
#[cfg(feature = "virtio-socket")]
pub mod vsock;

pub const MMIO_DEVICE_NAME: &str = "virtio-mmio";

pub struct VirtIoHalImpl(PhantomData<()>);

pub const fn has_static_mmio_drivers() -> bool {
    cfg!(any(
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket",
    ))
}

unsafe impl VirtIoHal for VirtIoHalImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtIoPhysAddr, NonNull<u8>) {
        let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma) else {
            return (0, NonNull::dangling());
        };
        let paddr = axklib::mem::virt_to_phys(vaddr.into()).as_usize() as VirtIoPhysAddr;
        let ptr = NonNull::new(vaddr as _).expect("DMA allocator returned null");
        (paddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        global_allocator().dealloc_pages(vaddr.as_ptr() as usize, pages, UsageKind::Dma);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: VirtIoPhysAddr, size: usize) -> NonNull<u8> {
        axklib::mmio::ioremap_raw((paddr as usize).into(), size)
            .map(|mmio| mmio.as_nonnull_ptr())
            .expect("failed to map VirtIO MMIO")
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtIoPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        axklib::mem::virt_to_phys(vaddr.into()).as_usize() as VirtIoPhysAddr
    }

    unsafe fn unshare(_paddr: VirtIoPhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
    }
}

pub fn probe_mmio_device(
    reg_base: *mut u8,
    reg_size: usize,
) -> Option<(DeviceType, MmioTransport<'static>)> {
    if reg_base.is_null() || reg_size == 0 {
        return None;
    }

    let header = NonNull::new(reg_base as *mut virtio_drivers::transport::mmio::VirtIOHeader)?;
    let transport = unsafe { MmioTransport::new(header, reg_size) }.ok()?;
    Some((transport.device_type(), transport))
}

pub fn register_static_mmio(
    plat_dev: rdrive::PlatformDevice,
    base: usize,
    size: usize,
) -> Result<(), rdrive::probe::OnProbeError> {
    if !has_static_mmio_drivers() {
        return Err(rdrive::probe::OnProbeError::NotMatch);
    }

    let mmio = axklib::mmio::ioremap_raw(base.into(), size).map_err(|err| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "failed to map virtio-mmio {base:#x}: {err:?}",
        ))
    })?;
    let Some((ty, transport)) = probe_mmio_device(mmio.as_ptr(), size) else {
        return Err(rdrive::probe::OnProbeError::NotMatch);
    };
    register_static_transport(plat_dev, ty, transport)
}

#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket",
))]
pub fn register_static_transport<T: Transport + 'static>(
    plat_dev: rdrive::PlatformDevice,
    ty: DeviceType,
    transport: T,
) -> Result<(), rdrive::probe::OnProbeError> {
    match ty {
        #[cfg(feature = "virtio-blk")]
        DeviceType::Block => block::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-net")]
        DeviceType::Network => net::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-gpu")]
        DeviceType::GPU => display::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-input")]
        DeviceType::Input => input::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-socket")]
        DeviceType::Socket => vsock::register_transport(plat_dev, transport),
        _ => Err(rdrive::probe::OnProbeError::NotMatch),
    }
}

#[cfg(not(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket",
)))]
pub fn register_static_transport<T: Transport + 'static>(
    _plat_dev: rdrive::PlatformDevice,
    _ty: DeviceType,
    _transport: T,
) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::NotMatch)
}

#[cfg(probe = "fdt")]
pub fn probe_fdt_mmio_device(
    info: &rdrive::register::FdtInfo<'_>,
) -> Result<(DeviceType, MmioTransport<'static>), rdrive::probe::OnProbeError> {
    let base_reg = info.node.regs().into_iter().next().ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
    })?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio_base = crate::mmio::iomap(base_reg.address as usize, mmio_size)?.as_ptr();
    probe_mmio_device(mmio_base, mmio_size).ok_or(rdrive::probe::OnProbeError::NotMatch)
}

#[cfg(feature = "virtio-net")]
pub fn map_virtio_error(err: VirtIoError) -> &'static str {
    match err {
        VirtIoError::QueueFull => "virtio queue full",
        VirtIoError::NotReady => "virtio device not ready",
        VirtIoError::WrongToken => "virtio queue returned a wrong token",
        VirtIoError::AlreadyUsed => "virtio resource is already used",
        VirtIoError::InvalidParam => "virtio invalid parameter",
        VirtIoError::DmaError => "virtio DMA error",
        VirtIoError::IoError => "virtio I/O error",
        VirtIoError::Unsupported => "virtio operation unsupported",
        VirtIoError::ConfigSpaceTooSmall => "virtio config space too small",
        VirtIoError::ConfigSpaceMissing => "virtio config space missing",
        VirtIoError::SocketDeviceError(_) => "virtio socket device error",
    }
}

#[cfg(feature = "virtio-net")]
pub trait VirtIoTransport: Transport + 'static {}

#[cfg(feature = "virtio-net")]
impl<T: Transport + 'static> VirtIoTransport for T {}
