use crate::{
    PciMem32, PciMem64,
    addr_alloc::{self, AddressAllocator, AllocPolicy},
};

#[derive(Default)]
pub struct SimpleBarAllocator {
    // Non-prefetchable windows
    mem32: Option<AddressAllocator>,
    mem64: Option<AddressAllocator>,
    // Prefetchable windows
    mem32_pref: Option<AddressAllocator>,
    mem64_pref: Option<AddressAllocator>,
}

impl SimpleBarAllocator {
    /// Convenience: add a 32-bit window with prefetchable attribute.
    pub fn set_mem32(
        &mut self,
        space: PciMem32,
        prefetchable: bool,
    ) -> Result<(), addr_alloc::Error> {
        let a = AddressAllocator::new(space.address as _, space.size as _)?;
        if prefetchable {
            self.mem32_pref = Some(a);
        } else {
            self.mem32 = Some(a);
        }
        Ok(())
    }

    /// Convenience: add a 64-bit window with prefetchable attribute.
    pub fn set_mem64(
        &mut self,
        space: PciMem64,
        prefetchable: bool,
    ) -> Result<(), addr_alloc::Error> {
        let a = AddressAllocator::new(space.address as _, space.size as _)?;
        if prefetchable {
            self.mem64_pref = Some(a);
        } else {
            self.mem64 = Some(a);
        }
        Ok(())
    }

    pub fn alloc_memory32(&mut self, size: u32, prefetchable: bool) -> Option<u32> {
        self.alloc_from_32(size, prefetchable)
    }

    pub fn alloc_memory64(&mut self, size: u64, prefetchable: bool) -> Option<u64> {
        if let Some(addr) = self.alloc_from_64(size, prefetchable) {
            return Some(addr);
        }

        // A 64-bit BAR can legally be programmed with an address below 4 GiB.
        // Keep the fallback on the same 32-bit allocator so 32-bit and 64-bit
        // BARs do not receive overlapping low MMIO ranges.
        if let Ok(size) = u32::try_from(size) {
            return self.alloc_from_32(size, prefetchable).map(u64::from);
        }

        None
    }

    fn alloc_from_32(&mut self, size: u32, prefetchable: bool) -> Option<u32> {
        if prefetchable && let Some(addr) = alloc_from(&mut self.mem32_pref, size as u64) {
            return Some(addr as u32);
        }

        alloc_from(&mut self.mem32, size as u64).map(|addr| addr as u32)
    }

    fn alloc_from_64(&mut self, size: u64, prefetchable: bool) -> Option<u64> {
        if prefetchable && let Some(addr) = alloc_from(&mut self.mem64_pref, size) {
            return Some(addr);
        }

        alloc_from(&mut self.mem64, size)
    }
}

fn alloc_from(allocator: &mut Option<AddressAllocator>, size: u64) -> Option<u64> {
    allocator
        .as_mut()?
        .allocate(size, size, AllocPolicy::FirstMatch)
        .ok()
        .map(|range| range.start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory64_falls_back_to_shared_memory32_window() {
        let mut allocator = SimpleBarAllocator::default();
        allocator
            .set_mem32(
                PciMem32 {
                    address: 0x1000_0000,
                    size: 0x2000,
                },
                false,
            )
            .unwrap();

        assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x1000_0000));
        assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_1000));
    }

    #[test]
    fn memory64_prefers_native_memory64_window() {
        let mut allocator = SimpleBarAllocator::default();
        allocator
            .set_mem32(
                PciMem32 {
                    address: 0x1000_0000,
                    size: 0x2000,
                },
                false,
            )
            .unwrap();
        allocator
            .set_mem64(
                PciMem64 {
                    address: 0x8_0000_0000,
                    size: 0x2000,
                },
                false,
            )
            .unwrap();

        assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x8_0000_0000));
        assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_0000));
    }
}
