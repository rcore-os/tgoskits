use alloc::collections::btree_map::BTreeMap;

use dma_api::{ContiguousArray, DeviceDma, DmaDirection};

use crate::{
    RknpuError,
    ioctrl::{RknpuMemCreate, RknpuMemSync},
};

const RKNPU_MEM_CACHEABLE: u32 = 1 << 1;
const RKNPU_MEM_WRITE_COMBINE: u32 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GemCachePolicy {
    NonCacheable,
    Cacheable,
    WriteCombine,
}

impl GemCachePolicy {
    fn from_flags(flags: u32) -> Self {
        if flags & RKNPU_MEM_CACHEABLE != 0 {
            Self::Cacheable
        } else if flags & RKNPU_MEM_WRITE_COMBINE != 0 {
            Self::WriteCombine
        } else {
            Self::NonCacheable
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemBufferInfo {
    pub obj_addr: usize,
    pub dma_addr: u64,
    pub size: usize,
    pub flags: u32,
    pub cache_policy: GemCachePolicy,
}

struct GemBuffer {
    data: ContiguousArray<u8>,
    flags: u32,
}

pub struct GemPool {
    dma: DeviceDma,
    pool: BTreeMap<u32, GemBuffer>,
    handle_counter: u32,
}

impl GemPool {
    pub fn new(dma: DeviceDma) -> Self {
        GemPool {
            dma,
            pool: BTreeMap::new(),
            handle_counter: 1,
        }
    }

    pub fn create(&mut self, args: &mut RknpuMemCreate) -> Result<(), RknpuError> {
        let data = self
            .dma
            .contiguous_array_zero_with_align::<u8>(
                args.size as _,
                0x1000,
                DmaDirection::Bidirectional,
            )
            .map_err(|_| RknpuError::DmaError)?;

        let handle = self.handle_counter;
        self.handle_counter = self.handle_counter.wrapping_add(1);

        args.handle = handle;
        args.sram_size = data.len() as _;
        args.dma_addr = data.dma_addr().as_u64();
        args.obj_addr = data.as_ptr().as_ptr() as _;
        self.pool.insert(
            args.handle,
            GemBuffer {
                data,
                flags: args.flags,
            },
        );
        Ok(())
    }

    pub fn get_buffer_info(&self, handle: u32) -> Option<GemBufferInfo> {
        self.pool.get(&handle).map(|buffer| GemBufferInfo {
            obj_addr: buffer.data.as_ptr().as_ptr() as usize,
            dma_addr: buffer.data.dma_addr().as_u64(),
            size: buffer.data.len(),
            flags: buffer.flags,
            cache_policy: GemCachePolicy::from_flags(buffer.flags),
        })
    }

    /// Get the physical address and size of the memory object.
    pub fn get_phys_addr_and_size(&self, handle: u32) -> Option<(u64, usize)> {
        self.get_buffer_info(handle)
            .map(|info| (info.dma_addr, info.size))
    }

    /// Get the CPU-visible virtual address and size of the memory object.
    pub fn get_obj_addr_and_size(&self, handle: u32) -> Option<(usize, usize)> {
        self.get_buffer_info(handle)
            .map(|info| (info.obj_addr, info.size))
    }

    pub fn sync(&mut self, args: &mut RknpuMemSync) -> Result<(), RknpuError> {
        const RKNPU_MEM_SYNC_TO_DEVICE: u32 = 1 << 0;
        const RKNPU_MEM_SYNC_FROM_DEVICE: u32 = 1 << 1;

        let Some((data, base_offset)) = self.pool.values_mut().find_map(|buffer| {
            let data = &mut buffer.data;
            let base = data.as_ptr().as_ptr() as u64;
            let end = base.checked_add(data.bytes_len() as u64)?;
            (args.obj_addr >= base && args.obj_addr < end).then_some((data, args.obj_addr - base))
        }) else {
            return Err(RknpuError::InvalidHandle);
        };

        let offset = usize::try_from(args.offset.saturating_add(base_offset))
            .map_err(|_| RknpuError::InvalidParameter)?;
        let requested_size =
            usize::try_from(args.size).map_err(|_| RknpuError::InvalidParameter)?;
        let size = if requested_size == 0 {
            data.bytes_len().saturating_sub(offset)
        } else {
            requested_size
        };

        if offset > data.bytes_len() || size > data.bytes_len().saturating_sub(offset) {
            return Err(RknpuError::InvalidParameter);
        }

        if args.flags & RKNPU_MEM_SYNC_TO_DEVICE != 0 {
            data.prepare_for_device(offset, size);
        }
        if args.flags & RKNPU_MEM_SYNC_FROM_DEVICE != 0 {
            data.complete_for_cpu(offset, size);
        }
        Ok(())
    }

    pub fn destroy(&mut self, handle: u32) {
        self.pool.remove(&handle);
    }

    pub fn comfirm_write_all(&mut self) -> Result<(), RknpuError> {
        for buffer in self.pool.values_mut() {
            buffer.data.prepare_for_device_all();
        }
        Ok(())
    }

    pub fn prepare_read_all(&mut self) -> Result<(), RknpuError> {
        for buffer in self.pool.values_mut() {
            buffer.data.complete_for_cpu_all();
        }
        Ok(())
    }
}
