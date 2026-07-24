use core::{alloc::Layout, marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use dma_api::{DmaAllocHandle, DmaDirection, DmaMapHandle};
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

const VIRTIO_DMA_MASK: u64 = u64::MAX;
const VIRTIO_DMA_ALIGN: usize = 0x1000;

fn virtio_direction(direction: BufferDirection) -> DmaDirection {
    match direction {
        BufferDirection::DriverToDevice => DmaDirection::ToDevice,
        BufferDirection::DeviceToDriver => DmaDirection::FromDevice,
        BufferDirection::Both => DmaDirection::Bidirectional,
    }
}

fn page_layout(pages: usize) -> Option<Layout> {
    Layout::from_size_align(pages.checked_mul(VIRTIO_DMA_ALIGN)?, VIRTIO_DMA_ALIGN).ok()
}

pub const fn has_static_mmio_drivers() -> bool {
    cfg!(any(
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket",
    ))
}

// SAFETY: every allocation and mapping is paired with the corresponding
// consume-on-release DMA token, and MMIO pointers come from the platform iomap
// capability for the exact range requested by the VirtIO transport.
unsafe impl VirtIoHal for VirtIoHalImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtIoPhysAddr, NonNull<u8>) {
        let Some(layout) = page_layout(pages) else {
            return (0, NonNull::dangling());
        };
        let device = axklib::dma::device_with_mask(VIRTIO_DMA_MASK);
        let Ok(handle) = (unsafe { device.alloc_coherent(layout) }) else {
            return (0, NonNull::dangling());
        };
        // SAFETY: `handle` uniquely owns a writable allocation of `layout`.
        unsafe {
            handle.as_ptr().write_bytes(0, layout.size());
        }
        let dma_addr = handle.dma_addr().as_u64() as VirtIoPhysAddr;
        let cpu_addr = handle.as_ptr();
        // The VirtIO HAL transfers this allocation as the raw tuple consumed
        // by `dma_dealloc`; the callback contract preserves unique ownership.
        (dma_addr, cpu_addr)
    }

    /// # Safety
    ///
    /// The arguments must be the unchanged values returned by [`Self::dma_alloc`].
    unsafe fn dma_dealloc(paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let Some(layout) = page_layout(pages) else {
            return -1;
        };
        // SAFETY: the VirtIO HAL requires these to be the unchanged values
        // returned by `dma_alloc`, so the raw tuple reconstructs its token.
        let handle = unsafe { DmaAllocHandle::new(vaddr, paddr.into(), layout) };
        unsafe { axklib::dma::device_with_mask(VIRTIO_DMA_MASK).dealloc_coherent(handle) };
        0
    }

    /// # Safety
    ///
    /// The physical range must describe the calling VirtIO device's MMIO BAR.
    unsafe fn mmio_phys_to_virt(paddr: VirtIoPhysAddr, size: usize) -> NonNull<u8> {
        axklib::mmio::ioremap_raw((paddr as usize).into(), size)
            .map(|mmio| mmio.as_nonnull_ptr())
            .expect("failed to map VirtIO MMIO")
    }

    /// # Safety
    ///
    /// `buffer` must remain live and obey the requested DMA ownership direction
    /// until [`Self::unshare`] is called with the same range.
    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> VirtIoPhysAddr {
        let size = buffer.len();
        let Some(size) = NonZeroUsize::new(size) else {
            return 0;
        };
        let cpu_addr = NonNull::new(buffer.as_ptr() as *mut u8).expect("non-empty DMA buffer");
        let direction = virtio_direction(direction);
        // SAFETY: the VirtIO Hal contract keeps `buffer` live until unshare.
        let handle = unsafe {
            axklib::dma::device_with_mask(VIRTIO_DMA_MASK)
                .map_streaming(cpu_addr, size, 1, direction)
                .expect("failed to map VirtIO DMA buffer")
        };
        axklib::dma::device_with_mask(VIRTIO_DMA_MASK).sync_map_for_device(
            &handle,
            0,
            size.get(),
            direction,
        );
        let dma_addr = handle.dma_addr().as_u64() as VirtIoPhysAddr;
        // `unshare` receives the same buffer, direction, and device address,
        // which form the raw ownership token for this identity mapping.
        dma_addr
    }

    /// # Safety
    ///
    /// The arguments must identify a live mapping created by [`Self::share`]
    /// and must not have been unmapped previously.
    unsafe fn unshare(paddr: VirtIoPhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
        let size = buffer.len();
        let Some(nonzero_size) = NonZeroUsize::new(size) else {
            return;
        };
        let cpu_addr = NonNull::new(buffer.as_ptr() as *mut u8).expect("non-empty DMA buffer");
        let layout = Layout::from_size_align(size, 1).expect("valid VirtIO buffer layout");
        let direction = virtio_direction(direction);
        // VIRTIO_DMA_MASK accepts every physical address, so this adapter
        // cannot create a bounce allocation. The callback tuple therefore
        // contains all state required to reconstruct the mapping token.
        let handle = unsafe { DmaMapHandle::new(cpu_addr, paddr.into(), layout, None) };
        let device = axklib::dma::device_with_mask(VIRTIO_DMA_MASK);
        device.sync_map_for_cpu(&handle, 0, nonzero_size.get(), direction);
        unsafe { device.unmap_streaming(handle) };
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
