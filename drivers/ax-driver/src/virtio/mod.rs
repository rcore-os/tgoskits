use alloc::vec::Vec;
use core::{alloc::Layout, marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use ax_kspin::SpinNoIrq;
#[cfg(test)]
use dma_api::DmaAddr;
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

struct DmaAllocationRecord {
    handle: DmaAllocHandle,
}

// SAFETY: the token is never dereferenced through the registry. The VirtIO
// contract permits allocation and release callbacks on different CPUs, and
// removal transfers the unique token to the DMA backend before use.
unsafe impl Send for DmaAllocationRecord {}

struct DmaMappingRecord {
    handle: DmaMapHandle,
    direction: DmaDirection,
}

// SAFETY: the mapped buffer remains owned by VirtIO until `unshare`. The
// registry only transfers its unique DMA token between callbacks and never
// dereferences the stored CPU or bounce pointer.
unsafe impl Send for DmaMappingRecord {}

struct VirtIoDmaState {
    allocations: Vec<DmaAllocationRecord>,
    mappings: Vec<DmaMappingRecord>,
}

impl VirtIoDmaState {
    const fn new() -> Self {
        Self {
            allocations: Vec::new(),
            mappings: Vec::new(),
        }
    }

    fn record_allocation(&mut self, handle: DmaAllocHandle) -> Result<(), DmaAllocHandle> {
        if self.allocations.try_reserve(1).is_err() {
            return Err(handle);
        }
        self.allocations.push(DmaAllocationRecord { handle });
        Ok(())
    }

    fn take_allocation(
        &mut self,
        cpu_addr: NonNull<u8>,
        dma_addr: u64,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let index = self.allocations.iter().position(|record| {
            record.handle.as_ptr() == cpu_addr
                && record.handle.dma_addr().as_u64() == dma_addr
                && record.handle.layout() == layout
        })?;
        Some(self.allocations.swap_remove(index).handle)
    }

    fn record_mapping(
        &mut self,
        handle: DmaMapHandle,
        direction: DmaDirection,
    ) -> Result<(), DmaMapHandle> {
        if self.mappings.try_reserve(1).is_err() {
            return Err(handle);
        }
        self.mappings.push(DmaMappingRecord { handle, direction });
        Ok(())
    }

    fn take_mapping(
        &mut self,
        cpu_addr: NonNull<u8>,
        dma_addr: u64,
        layout: Layout,
        direction: DmaDirection,
    ) -> Option<DmaMapHandle> {
        let index = self.mappings.iter().position(|record| {
            record.handle.as_ptr() == cpu_addr
                && record.handle.dma_addr().as_u64() == dma_addr
                && record.handle.layout() == layout
                && record.direction == direction
        })?;
        Some(self.mappings.swap_remove(index).handle)
    }
}

// The lock protects token ownership only. DMA backend calls occur after the
// token has been inserted or removed, so this lock never nests with a backend
// allocator, IOMMU, or cache-maintenance lock.
static VIRTIO_DMA_STATE: SpinNoIrq<VirtIoDmaState> = SpinNoIrq::new(VirtIoDmaState::new());

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
        if let Err(handle) = VIRTIO_DMA_STATE.lock().record_allocation(handle) {
            unsafe { device.dealloc_coherent(handle) };
            return (0, NonNull::dangling());
        }
        (dma_addr, cpu_addr)
    }

    /// # Safety
    ///
    /// The arguments must be the unchanged values returned by [`Self::dma_alloc`].
    unsafe fn dma_dealloc(paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let Some(layout) = page_layout(pages) else {
            return -1;
        };
        let Some(handle) = VIRTIO_DMA_STATE
            .lock()
            .take_allocation(vaddr, paddr, layout)
        else {
            return -1;
        };
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
        if let Err(handle) = VIRTIO_DMA_STATE.lock().record_mapping(handle, direction) {
            unsafe {
                axklib::dma::device_with_mask(VIRTIO_DMA_MASK).unmap_streaming(handle);
            }
            panic!("failed to retain VirtIO DMA mapping ownership");
        }
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
        let handle = VIRTIO_DMA_STATE
            .lock()
            .take_mapping(cpu_addr, paddr, layout, direction)
            .expect("unshare must consume a live VirtIO DMA mapping");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_state_preserves_allocation_backend_token_and_consumes_once() {
        let cpu_addr = NonNull::new(0x1000usize as *mut u8).unwrap();
        let layout = Layout::from_size_align(0x2000, 0x1000).unwrap();
        let handle = unsafe {
            DmaAllocHandle::new_with_backend_token(cpu_addr, DmaAddr::from(0x3000), layout, 7)
        };
        let mut state = VirtIoDmaState::new();
        state.record_allocation(handle).unwrap();

        let restored = state.take_allocation(cpu_addr, 0x3000, layout).unwrap();

        assert_eq!(restored.backend_token(), 7);
        assert!(state.take_allocation(cpu_addr, 0x3000, layout).is_none());
    }

    #[test]
    fn dma_state_preserves_mapping_bounce_buffer_and_backend_token() {
        let cpu_addr = NonNull::new(0x1000usize as *mut u8).unwrap();
        let bounce_addr = NonNull::new(0x2000usize as *mut u8).unwrap();
        let layout = Layout::from_size_align(0x1000, 1).unwrap();
        let handle = unsafe {
            DmaMapHandle::new_with_backend_token(
                cpu_addr,
                DmaAddr::from(0x4000),
                layout,
                Some(bounce_addr),
                11,
            )
        };
        let mut state = VirtIoDmaState::new();
        state
            .record_mapping(handle, DmaDirection::FromDevice)
            .unwrap();

        let restored = state
            .take_mapping(cpu_addr, 0x4000, layout, DmaDirection::FromDevice)
            .unwrap();

        assert_eq!(restored.bounce_ptr(), Some(bounce_addr));
        assert_eq!(restored.backend_token(), 11);
    }
}
