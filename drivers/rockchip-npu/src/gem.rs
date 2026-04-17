use alloc::collections::btree_map::BTreeMap;

use dma_api::{DArray, DeviceDma, DmaDirection};

use crate::{
    RknpuError,
    ioctrl::{RknpuMemCreate, RknpuMemSync},
};

pub struct GemPool {
    dma: DeviceDma,
    pool: BTreeMap<u32, DArray<u8>>,
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
            .array_zero_with_align::<u8>(args.size as _, 0x1000, DmaDirection::Bidirectional)
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

    pub fn sync(&mut self, _args: &mut RknpuMemSync) {}

    pub fn destroy(&mut self, handle: u32) {
        self.pool.remove(&handle);
    }

    pub fn comfirm_write_all(&mut self) -> Result<(), RknpuError> {
        for data in self.pool.values_mut() {
            data.confirm_write_all();
        }
        Ok(())
    }

    pub fn prepare_read_all(&mut self) -> Result<(), RknpuError> {
        for data in self.pool.values_mut() {
            data.prepare_read_all();
        }
        Ok(())
    }
}
