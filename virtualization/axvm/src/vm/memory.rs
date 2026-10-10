//! VM memory region planning.

use std::{alloc::Layout, vec::Vec};

use axaddrspace::MappingFlags;
use axvm_types::{GuestPhysAddr, VmMemConfig, VmMemMappingType};

use super::{AxVM, VMMemoryRegion};
use crate::{AxVmResult, ax_err_type};

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
            .first()
            .cloned()
            .ok_or_else(|| ax_err_type!(InvalidData, "VM must have at least one memory region"))?;
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

pub(crate) trait MemoryRegionMapper {
    fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion>;
    fn allocate_memory_region(
        &mut self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
    ) -> AxVmResult<()>;
    fn map_reserved_memory_region(
        &mut self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
        flags: MappingFlags,
    ) -> AxVmResult;
}

impl MemoryRegionMapper for AxVM {
    fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion> {
        self.memory_regions()
    }

    fn allocate_memory_region(
        &mut self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
    ) -> AxVmResult<()> {
        self.alloc_memory_region(layout, gpa).map(|_| ())
    }

    fn map_reserved_memory_region(
        &mut self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
        flags: MappingFlags,
    ) -> AxVmResult {
        self.map_reserved_memory_region(layout, gpa, flags)
    }
}

pub(crate) struct MemoryLayoutBuilder<'a, M: MemoryRegionMapper + ?Sized> {
    mapper: &'a mut M,
    configs: &'a [VmMemConfig],
}

impl<'a, M: MemoryRegionMapper + ?Sized> MemoryLayoutBuilder<'a, M> {
    pub(crate) const fn new(mapper: &'a mut M, configs: &'a [VmMemConfig]) -> Self {
        Self { mapper, configs }
    }

    pub(crate) fn prepare(&mut self) -> AxVmResult<PreparedMemoryLayout> {
        let existing = self.mapper.prepared_memory_regions();
        if !existing.is_empty() {
            return PreparedMemoryLayout::new(existing);
        }

        for config in self.configs {
            let plan = MemoryRegionPlan::from_config(config)?;
            if plan.maps_reserved_memory() {
                self.mapper.map_reserved_memory_region(
                    plan.layout(),
                    plan.configured_gpa(),
                    plan.flags,
                )?;
            } else {
                self.mapper
                    .allocate_memory_region(plan.layout(), plan.configured_gpa())?;
            }
        }

        PreparedMemoryLayout::new(self.mapper.prepared_memory_regions())
    }
}

/// One planned guest memory mapping operation.
#[derive(Debug, Clone)]
pub(crate) struct MemoryRegionPlan {
    configured_gpa: Option<GuestPhysAddr>,
    layout: Layout,
    map_type: VmMemMappingType,
    flags: MappingFlags,
}

impl MemoryRegionPlan {
    pub(crate) fn from_config(config: &VmMemConfig) -> AxVmResult<Self> {
        let layout = Layout::from_size_align(config.size, VM_MEMORY_ALIGN).map_err(|err| {
            ax_err_type!(
                InvalidInput,
                std::format!("invalid VM memory region {config:?}: {err:?}")
            )
        })?;
        let configured_gpa = match config.map_type {
            VmMemMappingType::MapIdentical => None,
            VmMemMappingType::MapAlloc | VmMemMappingType::MapReserved => {
                Some(GuestPhysAddr::from(config.gpa))
            }
        };
        Ok(Self {
            configured_gpa,
            layout,
            map_type: config.map_type.clone(),
            flags: MappingFlags::from_bits(config.flags)
                .ok_or_else(|| ax_err_type!(InvalidInput, "invalid VM memory permissions"))?,
        })
    }

    pub(crate) fn configured_gpa(&self) -> Option<GuestPhysAddr> {
        self.configured_gpa
    }

    pub(crate) const fn layout(&self) -> Layout {
        self.layout
    }

    pub(crate) const fn maps_reserved_memory(&self) -> bool {
        matches!(self.map_type, VmMemMappingType::MapReserved)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        vec,
    };

    use super::*;

    #[derive(Default)]
    struct FakeMemoryMapper {
        regions: RefCell<Vec<VMMemoryRegion>>,
        map_reserved_calls: Cell<usize>,
        reserved_flags: Cell<Option<MappingFlags>>,
    }

    impl MemoryRegionMapper for FakeMemoryMapper {
        fn prepared_memory_regions(&self) -> Vec<VMMemoryRegion> {
            self.regions.borrow().clone()
        }

        fn allocate_memory_region(
            &mut self,
            layout: Layout,
            gpa: Option<GuestPhysAddr>,
        ) -> AxVmResult<()> {
            let gpa = gpa.unwrap_or_else(|| GuestPhysAddr::from(0x5000_0000));
            self.regions.borrow_mut().push(VMMemoryRegion {
                gpa,
                hva: gpa.as_usize().into(),
                layout,
                needs_dealloc: true,
            });
            Ok(())
        }

        fn map_reserved_memory_region(
            &mut self,
            layout: Layout,
            gpa: Option<GuestPhysAddr>,
            flags: MappingFlags,
        ) -> AxVmResult {
            let gpa = gpa.ok_or_else(|| ax_err_type!(InvalidInput, "reserved GPA is required"))?;
            self.reserved_flags.set(Some(flags));
            self.map_reserved_calls
                .set(self.map_reserved_calls.get() + 1);
            self.regions.borrow_mut().push(VMMemoryRegion {
                gpa,
                hva: gpa.as_usize().into(),
                layout,
                needs_dealloc: false,
            });
            Ok(())
        }
    }

    #[test]
    fn memory_region_plan_preserves_mapping_intent_from_vm_config() {
        let alloc = VmMemConfig {
            gpa: 0x8000_0000,
            size: 0x20_0000,
            flags: 0,
            map_type: VmMemMappingType::MapAlloc,
        };
        let reserved = VmMemConfig {
            gpa: 0x9000_0000,
            size: 0x10_0000,
            flags: 0,
            map_type: VmMemMappingType::MapReserved,
        };
        let identical = VmMemConfig {
            gpa: 0,
            size: 0x10_0000,
            flags: 0,
            map_type: VmMemMappingType::MapIdentical,
        };

        assert_eq!(
            MemoryRegionPlan::from_config(&alloc)
                .unwrap()
                .configured_gpa(),
            Some(GuestPhysAddr::from(0x8000_0000))
        );
        assert!(
            !MemoryRegionPlan::from_config(&alloc)
                .unwrap()
                .maps_reserved_memory()
        );
        assert!(
            MemoryRegionPlan::from_config(&reserved)
                .unwrap()
                .maps_reserved_memory()
        );
        assert!(
            MemoryRegionPlan::from_config(&identical)
                .unwrap()
                .configured_gpa()
                .is_none()
        );
    }

    #[test]
    fn prepare_memory_layout_maps_configured_reserved_region_once() {
        let mut mapper = FakeMemoryMapper::default();
        let configs = vec![VmMemConfig {
            gpa: 0x4000_0000,
            size: 0x20_0000,
            flags: MappingFlags::READ.bits(),
            map_type: VmMemMappingType::MapReserved,
        }];
        let mut builder = MemoryLayoutBuilder::new(&mut mapper, &configs);

        let layout = builder.prepare().unwrap();
        let again = builder.prepare().unwrap();

        assert_eq!(layout.main_memory().gpa, GuestPhysAddr::from(0x4000_0000));
        assert_eq!(layout.regions().len(), 1);
        assert_eq!(again.regions().len(), 1);
        assert_eq!(mapper.map_reserved_calls.get(), 1);
        assert_eq!(mapper.reserved_flags.get(), Some(MappingFlags::READ));
    }
}
