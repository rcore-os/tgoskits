extern crate alloc;

use alloc::boxed::Box;
use core::{alloc::Layout, ptr::NonNull};

use ax_driver_block::BlockDriverOps;
#[cfg(feature = "net")]
use ax_driver_net::NetDriverOps;
use ax_errno::AxError;
use ax_memory_addr::PAGE_SIZE_4K;
use ax_plat::mem::PhysAddr;
use heapless::Vec;
use rdrive::probe::OnProbeError;
use spin::Mutex;

mod pci;
#[cfg(feature = "rknpu")]
mod rknpu;
#[cfg(feature = "serial")]
mod serial;
mod soc;
mod virtio;

pub mod blk;
#[cfg(feature = "net")]
pub mod net;

const MAX_BLOCK_DEVICES: usize = 16;
#[cfg(feature = "net")]
const MAX_NET_DEVICES: usize = 16;

pub type DynBlockDevice = Box<dyn BlockDriverOps>;
#[cfg(feature = "net")]
pub type DynNetDevice = Box<dyn NetDriverOps>;

static BLOCK_DEVICES: Mutex<Vec<DynBlockDevice, MAX_BLOCK_DEVICES>> = Mutex::new(Vec::new());
#[cfg(feature = "net")]
static NET_DEVICES: Mutex<Vec<DynNetDevice, MAX_NET_DEVICES>> = Mutex::new(Vec::new());

pub fn clear_block_devices() {
    BLOCK_DEVICES.lock().clear();
}

pub fn register_block_device(device: DynBlockDevice) -> Result<(), DynBlockDevice> {
    BLOCK_DEVICES.lock().push(device)
}

pub fn take_block_devices() -> Vec<DynBlockDevice, MAX_BLOCK_DEVICES> {
    let mut devices = BLOCK_DEVICES.lock();
    core::mem::take(&mut *devices)
}

#[cfg(feature = "net")]
pub fn clear_net_devices() {
    NET_DEVICES.lock().clear();
}

#[cfg(feature = "net")]
pub fn register_net_device(device: DynNetDevice) -> Result<(), DynNetDevice> {
    NET_DEVICES.lock().push(device)
}

#[cfg(feature = "net")]
pub fn take_net_devices() -> Vec<DynNetDevice, MAX_NET_DEVICES> {
    let mut devices = NET_DEVICES.lock();
    core::mem::take(&mut *devices)
}

/// maps a mmio physical address to a virtual address.
pub(crate) fn iomap(addr: PhysAddr, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mem::iomap(addr, size)
        .map_err(|e| match e {
            AxError::NoMemory => OnProbeError::KError(rdrive::KError::NoMem),
            _ => OnProbeError::Other(alloc::format!("{e:?}").into()),
        })
        .map(|v| unsafe { NonNull::new_unchecked(v.as_mut_ptr()) })
}

pub fn probe_all_devices() -> Result<(), AxError> {
    clear_block_devices();
    #[cfg(feature = "net")]
    clear_net_devices();
    rdrive::probe_all(true).map_err(|_| AxError::BadState)?;

    for dev in rdrive::get_list::<rd_block::Block>() {
        let block = Box::new(blk::Block::from(dev));
        if register_block_device(block).is_err() {
            return Err(AxError::NoMemory);
        }
    }

    #[cfg(feature = "net")]
    for dev in rdrive::get_list::<net::PlatformNetDevice>() {
        let net = net::Net::try_from(dev).map_err(|err| match err {
            ax_driver_base::DevError::NoMemory => AxError::NoMemory,
            _ => AxError::BadState,
        })?;
        if register_net_device(Box::new(net)).is_err() {
            return Err(AxError::NoMemory);
        }
    }

    #[cfg(feature = "net")]
    for dev in rdrive::get_list::<net::PlatformNetDriver>() {
        let net = net::take_net_driver(dev).map_err(|err| match err {
            ax_driver_base::DevError::NoMemory => AxError::NoMemory,
            _ => AxError::BadState,
        })?;
        if register_net_device(net).is_err() {
            return Err(AxError::NoMemory);
        }
    }

    Ok(())
}

pub(crate) struct DmaImpl;

struct DmaPages {
    cpu_addr: NonNull<u8>,
    dma_addr: u64,
}

impl DmaPages {
    fn layout_pages(layout: Layout) -> usize {
        layout.size().div_ceil(PAGE_SIZE_4K)
    }

    fn layout_align(layout: Layout) -> usize {
        layout.align().max(PAGE_SIZE_4K)
    }

    unsafe fn alloc_for_layout(dma_mask: u64, layout: Layout) -> Result<Self, dma_api::DmaError> {
        if layout.size() == 0 {
            return Ok(Self {
                cpu_addr: NonNull::dangling(),
                dma_addr: 0,
            });
        }

        let num_pages = Self::layout_pages(layout);
        let align = Self::layout_align(layout);
        let cpu_vaddr = if dma_mask <= u32::MAX as u64 {
            ax_alloc::global_allocator().alloc_dma32_pages(
                num_pages,
                align,
                ax_alloc::UsageKind::Dma,
            )
        } else {
            ax_alloc::global_allocator().alloc_pages(num_pages, align, ax_alloc::UsageKind::Dma)
        }
        .map_err(|_| dma_api::DmaError::NoMemory)?;

        let cpu_addr = NonNull::new(cpu_vaddr as *mut u8).ok_or(dma_api::DmaError::NoMemory)?;
        let dma_addr = dma_addr_from_ptr(cpu_addr);
        if !dma_range_fits_mask(dma_addr, layout.size(), dma_mask) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(dma_api::DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: dma_mask,
            });
        }
        if !dma_addr_is_aligned(dma_addr, layout.align()) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(dma_api::DmaError::AlignMismatch {
                required: layout.align(),
                address: dma_addr.into(),
            });
        }

        Ok(Self { cpu_addr, dma_addr })
    }

    unsafe fn dealloc_pages(cpu_addr: NonNull<u8>, num_pages: usize) {
        if num_pages == 0 {
            return;
        }
        ax_alloc::global_allocator().dealloc_pages(
            cpu_addr.as_ptr() as usize,
            num_pages,
            ax_alloc::UsageKind::Dma,
        );
    }
}

#[inline]
fn dma_addr_from_ptr(ptr: NonNull<u8>) -> u64 {
    somehal::mem::virt_to_phys(ptr.as_ptr()) as u64
}

#[inline]
fn dma_range_fits_mask(dma_addr: u64, size: usize, dma_mask: u64) -> bool {
    if size == 0 {
        dma_addr <= dma_mask
    } else {
        dma_addr
            .checked_add(size.saturating_sub(1) as u64)
            .map(|end| end <= dma_mask)
            .unwrap_or(false)
    }
}

#[inline]
fn dma_addr_is_aligned(dma_addr: u64, align: usize) -> bool {
    dma_addr.is_multiple_of(align as u64)
}

impl dma_api::DmaOp for DmaImpl {
    fn page_size(&self) -> usize {
        PAGE_SIZE_4K
    }

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: core::num::NonZeroUsize,
        align: usize,
        direction: dma_api::DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout = Layout::from_size_align(size.get(), align)?;
        let dma_addr = dma_addr_from_ptr(addr);

        if dma_range_fits_mask(dma_addr, size.get(), dma_mask)
            && dma_addr_is_aligned(dma_addr, align)
        {
            return Ok(unsafe { dma_api::DmaMapHandle::new(addr, dma_addr.into(), layout, None) });
        }

        let map_pages = unsafe { DmaPages::alloc_for_layout(dma_mask, layout)? };
        let map_virt = map_pages.cpu_addr;

        if matches!(
            direction,
            dma_api::DmaDirection::ToDevice | dma_api::DmaDirection::Bidirectional
        ) {
            unsafe {
                map_virt
                    .as_ptr()
                    .copy_from_nonoverlapping(addr.as_ptr(), size.get());
            }
        }

        Ok(unsafe {
            dma_api::DmaMapHandle::new(addr, map_pages.dma_addr.into(), layout, Some(map_virt))
        })
    }

    unsafe fn unmap_single(&self, handle: dma_api::DmaMapHandle) {
        if let Some(map_virt) = handle.alloc_virt() {
            let num_pages = DmaPages::layout_pages(handle.layout());
            unsafe { DmaPages::dealloc_pages(map_virt, num_pages) };
        }
    }

    unsafe fn alloc_coherent(&self, dma_mask: u64, layout: Layout) -> Option<dma_api::DmaHandle> {
        let pages = unsafe { DmaPages::alloc_for_layout(dma_mask, layout).ok()? };
        unsafe {
            pages.cpu_addr.as_ptr().write_bytes(0, layout.size());
        }

        Some(unsafe { dma_api::DmaHandle::new(pages.cpu_addr, pages.dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        unsafe { DmaPages::dealloc_pages(handle.as_ptr(), num_pages) };
    }
}
