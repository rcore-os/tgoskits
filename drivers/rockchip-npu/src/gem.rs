use alloc::collections::btree_map::BTreeMap;
use dma_api::{DVec, Direction};

use crate::{
    RknpuError,
    ioctrl::{RknpuMemCreate, RknpuMemSync},
};

pub struct GemPool {
    pool: BTreeMap<u32, DVec<u8>>,
    handle_counter: u32,
}

impl GemPool {
    pub const fn new() -> Self {
        GemPool {
            pool: BTreeMap::new(),
            handle_counter: 1,
        }
    }

    pub fn create(&mut self, args: &mut RknpuMemCreate) -> Result<(), RknpuError> {
        let data = DVec::zeros(
            u32::MAX as _,
            args.size as _,
            0x1000,
            Direction::Bidirectional,
        )
        .unwrap();

        let handle = self.handle_counter;
        self.handle_counter = self.handle_counter.wrapping_add(1);

        args.handle = handle;
        args.sram_size = data.len() as _;
        args.dma_addr = data.bus_addr();
        args.obj_addr = data.as_ptr() as _;
        self.pool.insert(args.handle, data);
        Ok(())
    }

    /// Get the physical address and size of the memory object.
    pub fn get_phys_addr_and_size(&self, handle: u32) -> Option<(u64, usize)> {
        self.pool
            .get(&handle)
            .map(|dvec| (dvec.bus_addr(), dvec.len()))
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

impl Default for GemPool {
    fn default() -> Self {
        Self::new()
    }
}
