//! Arm SMMUv3 firmware discovery and ArceOS physical-memory adapter.

extern crate alloc;

use alloc::{format, sync::Arc};
use core::{alloc::Layout, ptr::NonNull};

use arm_smmu_v3::{PhysicalMemory, PhysicalRegion, Smmu};
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use ax_sync::SpinLock;
use log::info;
use rdif_iommu::{Iommu, IommuError};
use rdrive::{
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

struct SmmuMemory;

static MEMORY: SmmuMemory = SmmuMemory;
static CONTROLLER: SpinLock<Option<Arc<Smmu>>> = SpinLock::new(None);

// SAFETY: axklib's page allocator returns directly addressable, physically
// contiguous pages; QEMU virt advertises a coherent SMMU page-table walker.
unsafe impl PhysicalMemory for SmmuMemory {
    fn allocate(&'static self, layout: Layout) -> Result<PhysicalRegion, IommuError> {
        let pages = layout.size().div_ceil(PAGE_SIZE_4K);
        let ptr = axklib::klib::dma_alloc_pages(u64::MAX, pages, layout.align().max(PAGE_SIZE_4K))
            .map_err(|_| IommuError::OutOfMemory)?;
        let physical = axklib::klib::mem_virt_to_phys(VirtAddr::from_usize(ptr.as_ptr() as usize))
            .as_usize() as u64;
        // SAFETY: the allocation remains owned by this region and deallocate
        // uses the same number of pages and allocator entry point.
        Ok(unsafe { PhysicalRegion::new(ptr, physical, layout, self) })
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, _physical: u64, layout: Layout) {
        axklib::klib::dma_dealloc_pages(ptr, layout.size().div_ceil(PAGE_SIZE_4K));
    }
}

crate::model_register!(
    name: "Arm SMMUv3",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::MSI,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,smmu-v3"],
        on_probe: probe_smmu
    }],
);

fn probe_smmu(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    probe_smmu_node(info, platform)
}

fn probe_smmu_node(
    info: rdrive::register::FdtInfo<'_>,
    platform: rdrive::PlatformDevice,
) -> Result<(), OnProbeError> {
    let node = info.node.as_node();
    if node.get_property("dma-coherent").is_none() {
        return Err(OnProbeError::Unsupported(
            "Arm SMMUv3 non-coherent table walker is not supported",
        ));
    }
    if node
        .get_property("#iommu-cells")
        .and_then(|prop| prop.get_u32())
        != Some(1)
    {
        return Err(OnProbeError::other("Arm SMMUv3 requires #iommu-cells = 1"));
    }
    if info.interrupts().len() < 4 {
        return Err(OnProbeError::other(
            "Arm SMMUv3 is missing firmware interrupts",
        ));
    }
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("Arm SMMUv3 has no register range"))?;
    if reg.size.unwrap_or(0) < 0x2_0000 {
        return Err(OnProbeError::other(
            "Arm SMMUv3 register range is too small",
        ));
    }
    if CONTROLLER.lock().is_some() {
        return Err(OnProbeError::Unsupported(
            "only one Arm SMMUv3 controller is supported",
        ));
    }

    let mmio = crate::mmio::iomap(reg.address as usize, 0x2_0000)?;
    // SAFETY: iomap returns a Device mapping that lives for the kernel lifetime;
    // the FDT range was checked above and rdrive retains the SMMU controller.
    let smmu = Arc::new(unsafe { Smmu::new(mmio, &MEMORY) }.map_err(|err| {
        OnProbeError::other(format!("Arm SMMUv3 initialization failed: {err:?}"))
    })?);
    platform.register(Iommu::new("arm-smmu-v3", smmu.clone()));
    *CONTROLLER.lock() = Some(smmu);
    info!("Arm SMMUv3 registered at {:#x}", reg.address);
    Ok(())
}

pub(super) fn fault_count() -> Result<u64, OnProbeError> {
    let smmu = CONTROLLER
        .lock()
        .as_ref()
        .cloned()
        .ok_or_else(|| OnProbeError::other("Arm SMMUv3 controller is unavailable"))?;
    smmu.fault_count()
        .map_err(|err| OnProbeError::other(format!("failed to drain SMMU faults: {err}")))
}

pub(super) fn drain_faults() -> Result<alloc::vec::Vec<arm_smmu_v3::SmmuFault>, OnProbeError> {
    let smmu = CONTROLLER
        .lock()
        .as_ref()
        .cloned()
        .ok_or_else(|| OnProbeError::other("Arm SMMUv3 controller is unavailable"))?;
    smmu.drain_faults()
        .map_err(|err| OnProbeError::other(format!("failed to drain SMMU faults: {err}")))
}
