//! VM memory region planning.

use alloc::vec::Vec;
use core::alloc::Layout;

#[cfg(test)]
use axvm_types::HostVirtAddr;
use axvm_types::{GuestPhysAddr, HostPhysAddr, MappingFlags};

use super::{AxVM, VMMemoryRegion};
use crate::{
    AxVmResult, ax_err_type,
    config::{VmMemoryBacking, VmMemoryConfig},
};

const VM_MEMORY_ALIGN: usize = 2 * 1024 * 1024;

/// Prepared memory regions for one VM.
#[derive(Debug, Clone)]
pub struct PreparedMemoryLayout {
    main_memory: VMMemoryRegion,
    regions: Vec<VMMemoryRegion>,
}

impl PreparedMemoryLayout {
    fn new(regions: Vec<VMMemoryRegion>) -> AxVmResult<Self> {
        let main_memory = regions
            .iter()
            .find(|region| !matches!(region.backing, VmMemoryBacking::Reserved { .. }))
            .cloned()
            .ok_or_else(|| ax_err_type!(InvalidData, "VM must have at least one RAM region"))?;
        Ok(Self {
            main_memory,
            regions,
        })
    }

    /// Returns the primary memory region used by image and boot planning.
    pub fn main_memory(&self) -> &VMMemoryRegion {
        &self.main_memory
    }

    /// Returns all prepared VM memory regions.
    pub fn regions(&self) -> &[VMMemoryRegion] {
        &self.regions
    }
}

/// Selects whether an owned allocation has a fixed or allocator-derived GPA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GuestMemoryPlacement {
    /// Map the allocation at a configured guest physical address.
    Fixed(GuestPhysAddr),
    /// Use the allocation's host physical address as its guest physical address.
    Identity,
}

pub(crate) trait MemoryRegionMapper {
    fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion>;

    fn allocate_memory_region(
        &self,
        layout: Layout,
        placement: GuestMemoryPlacement,
        flags: MappingFlags,
    ) -> AxVmResult;

    fn map_backed_memory_region(
        &self,
        layout: Layout,
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        flags: MappingFlags,
        backing: VmMemoryBacking,
    ) -> AxVmResult;
}

impl MemoryRegionMapper for AxVM {
    fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion> {
        self.memory_regions()
    }

    fn allocate_memory_region(
        &self,
        layout: Layout,
        placement: GuestMemoryPlacement,
        flags: MappingFlags,
    ) -> AxVmResult {
        let (gpa, backing) = match placement {
            GuestMemoryPlacement::Fixed(gpa) => (Some(gpa), VmMemoryBacking::Allocated),
            GuestMemoryPlacement::Identity => (None, VmMemoryBacking::IdentityAllocated),
        };
        self.alloc_owned_memory_region_with_flags(layout, gpa, flags, backing)
            .map(|_| ())
    }

    fn map_backed_memory_region(
        &self,
        layout: Layout,
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        flags: MappingFlags,
        backing: VmMemoryBacking,
    ) -> AxVmResult {
        self.map_backed_memory_region(layout, gpa, hpa, flags, backing)
    }
}

pub(crate) struct MemoryLayoutBuilder<'a, M: MemoryRegionMapper + ?Sized> {
    mapper: &'a M,
    configs: &'a [VmMemoryConfig],
}

impl<'a, M: MemoryRegionMapper + ?Sized> MemoryLayoutBuilder<'a, M> {
    pub(crate) const fn new(mapper: &'a M, configs: &'a [VmMemoryConfig]) -> Self {
        Self { mapper, configs }
    }

    pub(crate) fn prepare(&self) -> AxVmResult<PreparedMemoryLayout> {
        let existing = self.mapper.prepared_memory_regions();
        if !existing.is_empty() {
            return PreparedMemoryLayout::new(existing);
        }

        for config in self.configs {
            MemoryRegionPlan::from_config(*config)?.apply(self.mapper)?;
        }

        PreparedMemoryLayout::new(self.mapper.prepared_memory_regions())
    }
}

/// One checked guest-memory mapping operation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryRegionPlan {
    operation: MemoryRegionOperation,
    layout: Layout,
    flags: MappingFlags,
}

#[derive(Debug, Clone, Copy)]
enum MemoryRegionOperation {
    Allocate(GuestMemoryPlacement),
    Map {
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        backing: VmMemoryBacking,
    },
}

impl MemoryRegionPlan {
    pub(crate) fn from_config(config: VmMemoryConfig) -> AxVmResult<Self> {
        let layout = Layout::from_size_align(config.size(), VM_MEMORY_ALIGN).map_err(|error| {
            ax_err_type!(
                InvalidInput,
                alloc::format!("invalid VM memory region {config:?}: {error:?}")
            )
        })?;
        let operation = match config.backing() {
            VmMemoryBacking::Allocated => {
                MemoryRegionOperation::Allocate(GuestMemoryPlacement::Fixed(config.guest_base()))
            }
            VmMemoryBacking::IdentityAllocated => {
                MemoryRegionOperation::Allocate(GuestMemoryPlacement::Identity)
            }
            backing @ (VmMemoryBacking::Host { host_base: hpa }
            | VmMemoryBacking::Shared { host_base: hpa }
            | VmMemoryBacking::Reserved { host_base: hpa }) => MemoryRegionOperation::Map {
                gpa: config.guest_base(),
                hpa,
                backing,
            },
        };
        Ok(Self {
            operation,
            layout,
            flags: config.flags(),
        })
    }

    fn apply(self, mapper: &(impl MemoryRegionMapper + ?Sized)) -> AxVmResult {
        match self.operation {
            MemoryRegionOperation::Allocate(placement) => {
                mapper.allocate_memory_region(self.layout, placement, self.flags)
            }
            MemoryRegionOperation::Map { gpa, hpa, backing } => {
                mapper.map_backed_memory_region(self.layout, gpa, hpa, self.flags, backing)
            }
        }
    }

    #[cfg(test)]
    const fn host_base(self) -> Option<HostPhysAddr> {
        match self.operation {
            MemoryRegionOperation::Allocate(_) => None,
            MemoryRegionOperation::Map { hpa, .. } => Some(hpa),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::cell::{Cell, RefCell};

    use super::*;

    const FLAGS: MappingFlags = MappingFlags::READ
        .union(MappingFlags::WRITE)
        .union(MappingFlags::USER);

    #[derive(Default)]
    struct FakeMemoryMapper {
        regions: RefCell<Vec<VMMemoryRegion>>,
        backed_calls: Cell<usize>,
    }

    impl MemoryRegionMapper for FakeMemoryMapper {
        fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion> {
            self.regions.borrow().clone()
        }

        fn allocate_memory_region(
            &self,
            layout: Layout,
            placement: GuestMemoryPlacement,
            flags: MappingFlags,
        ) -> AxVmResult {
            let hpa = HostPhysAddr::from(0x5000_0000);
            let (gpa, backing) = match placement {
                GuestMemoryPlacement::Fixed(gpa) => (gpa, VmMemoryBacking::Allocated),
                GuestMemoryPlacement::Identity => (
                    GuestPhysAddr::from(hpa.as_usize()),
                    VmMemoryBacking::IdentityAllocated,
                ),
            };
            self.regions.borrow_mut().push(VMMemoryRegion {
                gpa,
                hva: HostVirtAddr::from(0x5000_0000),
                hpa,
                layout,
                flags,
                backing,
                needs_dealloc: true,
            });
            Ok(())
        }

        fn map_backed_memory_region(
            &self,
            layout: Layout,
            gpa: GuestPhysAddr,
            hpa: HostPhysAddr,
            flags: MappingFlags,
            backing: VmMemoryBacking,
        ) -> AxVmResult {
            self.backed_calls.set(self.backed_calls.get() + 1);
            self.regions.borrow_mut().push(VMMemoryRegion {
                gpa,
                hva: HostVirtAddr::from(hpa.as_usize()),
                hpa,
                layout,
                flags,
                backing,
                needs_dealloc: false,
            });
            Ok(())
        }
    }

    #[test]
    fn memory_plan_preserves_non_identity_host_backing() {
        let config = VmMemoryConfig::new(
            GuestPhysAddr::from(0x8000_0000),
            0x20_0000,
            FLAGS,
            VmMemoryBacking::Host {
                host_base: HostPhysAddr::from(0xa000_0000),
            },
        )
        .unwrap();

        let plan = MemoryRegionPlan::from_config(config).unwrap();

        assert_eq!(plan.host_base(), Some(HostPhysAddr::from(0xa000_0000)));
    }

    #[test]
    fn prepare_memory_layout_maps_each_configured_backing_once() {
        let mapper = FakeMemoryMapper::default();
        let configs = vec![
            VmMemoryConfig::new(
                GuestPhysAddr::from(0x4000_0000),
                0x20_0000,
                FLAGS,
                VmMemoryBacking::Host {
                    host_base: HostPhysAddr::from(0x6000_0000),
                },
            )
            .unwrap(),
        ];
        let builder = MemoryLayoutBuilder::new(&mapper, &configs);

        let layout = builder.prepare().unwrap();
        let again = builder.prepare().unwrap();

        assert_eq!(layout.main_memory().gpa, GuestPhysAddr::from(0x4000_0000));
        assert_eq!(
            layout.main_memory().host_paddr(),
            HostPhysAddr::from(0x6000_0000)
        );
        assert_eq!(layout.regions().len(), 1);
        assert_eq!(again.regions().len(), 1);
        assert_eq!(mapper.backed_calls.get(), 1);
    }

    #[test]
    fn identity_allocated_memory_uses_the_allocator_physical_address_as_its_gpa() {
        let mapper = FakeMemoryMapper::default();
        let configs = vec![
            VmMemoryConfig::new(
                GuestPhysAddr::from(0),
                0x20_0000,
                FLAGS,
                VmMemoryBacking::IdentityAllocated,
            )
            .unwrap(),
        ];

        let layout = MemoryLayoutBuilder::new(&mapper, &configs)
            .prepare()
            .unwrap();
        let main_memory = layout.main_memory();

        assert_eq!(main_memory.gpa.as_usize(), main_memory.hpa.as_usize());
        assert_eq!(main_memory.backing, VmMemoryBacking::IdentityAllocated);
        assert!(main_memory.needs_dealloc);
    }
}
