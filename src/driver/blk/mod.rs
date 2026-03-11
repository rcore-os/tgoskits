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

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
use core::ptr::NonNull;

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
use rdif_block::Interface;

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
use rdrive::PlatformDevice;

#[cfg(feature = "sdmmc")]
mod rockchip;

#[cfg(feature = "phytium-blk")]
mod phytium;

/// DMA implementation for block devices.
#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
pub struct DmaImpl;

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
impl rdif_block::dma_api::DmaOp for DmaImpl {
    fn page_size(&self) -> usize {
        memory_addr::PAGE_SIZE_4K
    }

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: core::num::NonZeroUsize,
        align: usize,
        _direction: rdif_block::dma_api::DmaDirection,
    ) -> Result<rdif_block::dma_api::DmaMapHandle, rdif_block::dma_api::DmaError> {
        let layout = core::alloc::Layout::from_size_align(size.get(), align)?;
        let dma_addr =
            axvisor_api::memory::virt_to_phys((addr.as_ptr() as usize).into()).as_usize() as u64;

        if dma_addr > dma_mask || dma_addr.wrapping_add(size.get() as u64) > dma_mask {
            return Err(rdif_block::dma_api::DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: dma_mask,
            });
        }

        if !dma_addr.is_multiple_of(align as u64) {
            return Err(rdif_block::dma_api::DmaError::AlignMismatch {
                required: align,
                address: dma_addr.into(),
            });
        }

        Ok(unsafe { rdif_block::dma_api::DmaMapHandle::new(addr, dma_addr.into(), layout, None) })
    }

    unsafe fn unmap_single(&self, _handle: rdif_block::dma_api::DmaMapHandle) {}

    unsafe fn alloc_coherent(
        &self,
        dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<rdif_block::dma_api::DmaHandle> {
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        let cpu_addr = NonNull::new(ptr)?;

        let dma_addr = axvisor_api::memory::virt_to_phys((ptr as usize).into()).as_usize() as u64;
        if dma_addr > dma_mask || dma_addr.wrapping_add(layout.size() as u64) > dma_mask {
            unsafe { alloc::alloc::dealloc(cpu_addr.as_ptr(), layout) };
            return None;
        }

        Some(unsafe { rdif_block::dma_api::DmaHandle::new(cpu_addr, dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: rdif_block::dma_api::DmaHandle) {
        unsafe { alloc::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) }
    }
}

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
pub trait PlatformDeviceBlock {
    fn register_block<T: Interface>(self, dev: T);
}

#[cfg(any(feature = "sdmmc", feature = "phytium-blk"))]
impl PlatformDeviceBlock for PlatformDevice {
    fn register_block<T: Interface>(self, dev: T) {
        // Use rd_block::Block to wrap the Interface for axdriver compatibility
        self.register(rd_block::Block::new(dev, &DmaImpl));
    }
}
