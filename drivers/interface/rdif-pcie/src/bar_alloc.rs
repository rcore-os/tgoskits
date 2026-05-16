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
        if prefetchable
            && let Some(set) = self.mem32_pref.as_mut()
            && let Ok(addr) = set.allocate(size as _, size as _, AllocPolicy::FirstMatch)
        {
            return Some(addr.start() as _);
        }

        let res = self
            .mem32
            .as_mut()?
            .allocate(size as _, size as _, AllocPolicy::FirstMatch)
            .ok()?;
        Some(res.start() as _)
    }

    pub fn alloc_memory64(&mut self, size: u64, prefetchable: bool) -> Option<u64> {
        if prefetchable
            && let Some(set) = self.mem64_pref.as_mut()
            && let Ok(addr) = set.allocate(size as _, size as _, AllocPolicy::FirstMatch)
        {
            return Some(addr.start() as _);
        }

        let res = self
            .mem64
            .as_mut()?
            .allocate(size as _, size as _, AllocPolicy::FirstMatch)
            .ok()?;
        Some(res.start() as _)
    }
}
