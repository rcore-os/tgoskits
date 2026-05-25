use alloc::collections::btree_map::BTreeMap;

use dma_api::{ContiguousArray, DeviceDma, DmaDirection};

use crate::{
    RknpuError,
    ioctrl::{RknpuMemCreate, RknpuMemSync},
};

pub struct GemPool {
    dma: DeviceDma,
    pool: BTreeMap<u32, ContiguousArray<u8>>,
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
        self.pool.insert(args.handle, data);
        Ok(())
    }

    /// Get the physical address and size of the memory object.
    pub fn get_phys_addr_and_size(&self, handle: u32) -> Option<(u64, usize)> {
        self.pool
            .get(&handle)
            .map(|data| (data.dma_addr().as_u64(), data.len()))
    }

    /// Get the CPU-visible virtual address and size of the memory object.
    pub fn get_obj_addr_and_size(&self, handle: u32) -> Option<(usize, usize)> {
        self.pool
            .get(&handle)
            .map(|data| (data.as_ptr().as_ptr() as usize, data.len()))
    }

    pub fn sync(&mut self, args: &mut RknpuMemSync) -> Result<(), RknpuError> {
        const RKNPU_MEM_SYNC_TO_DEVICE: u32 = 1 << 0;
        const RKNPU_MEM_SYNC_FROM_DEVICE: u32 = 1 << 1;

        let Some((data, base_offset)) = self.pool.values_mut().find_map(|data| {
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
            data.sync_for_device(offset, size);
        }
        if args.flags & RKNPU_MEM_SYNC_FROM_DEVICE != 0 {
            data.sync_for_cpu(offset, size);
        }
        Ok(())
    }

    pub fn destroy(&mut self, handle: u32) {
        self.pool.remove(&handle);
    }

    pub fn comfirm_write_all(&mut self) -> Result<(), RknpuError> {
        for data in self.pool.values_mut() {
            data.sync_for_device_all();
        }
        Ok(())
    }

    pub fn prepare_read_all(&mut self) -> Result<(), RknpuError> {
        for data in self.pool.values_mut() {
            data.sync_for_cpu_all();
        }
        Ok(())
    }
}
