//! Nested-page-fault handling shared by architectures that report raw faults.

use axaddrspace::NestedPageTableOps;
use axvm_types::{GuestPhysAddr, MappingFlags};

use crate::vm::AxVMResources;

pub(crate) fn handle(vm: &crate::AxVM, addr: GuestPhysAddr, access_flags: MappingFlags) -> bool {
    vm.with_resources_mut(|resources| {
        let handled = resources
            .address_space
            .handle_page_fault(addr, access_flags);
        log_fault(vm.id(), resources, addr, access_flags, handled);
        Ok(handled)
    })
    .unwrap_or(false)
}

fn log_fault(
    vm_id: usize,
    resources: &AxVMResources,
    addr: GuestPhysAddr,
    access_flags: MappingFlags,
    handled: bool,
) {
    log_page_table_query(vm_id, resources, addr, access_flags, handled);
    log_translation(vm_id, resources, addr, handled);
    log_memory_region(vm_id, resources, addr, handled);
}

fn log_page_table_query(
    vm_id: usize,
    resources: &AxVMResources,
    addr: GuestPhysAddr,
    access_flags: MappingFlags,
    handled: bool,
) {
    let root = resources.address_space.page_table_root();
    match resources.address_space.page_table().query(addr) {
        Ok((hpa, flags, size)) if handled => debug!(
            "VM[{}] stage2 query hit: gpa={:#x} -> hpa={:#x}, access={:?}, pte_flags={:?}, \
             page_size={:?}, root={:#x}",
            vm_id,
            addr.as_usize(),
            hpa.as_usize(),
            access_flags,
            flags,
            size,
            root.as_usize()
        ),
        Ok((hpa, flags, size)) => warn!(
            "VM[{}] stage2 query hit: gpa={:#x} -> hpa={:#x}, access={:?}, pte_flags={:?}, \
             page_size={:?}, root={:#x}",
            vm_id,
            addr.as_usize(),
            hpa.as_usize(),
            access_flags,
            flags,
            size,
            root.as_usize()
        ),
        Err(error) if handled => debug!(
            "VM[{}] stage2 query miss: gpa={:#x}, access={:?}, error={:?}, root={:#x}",
            vm_id,
            addr.as_usize(),
            access_flags,
            error,
            root.as_usize()
        ),
        Err(error) => warn!(
            "VM[{}] stage2 query miss: gpa={:#x}, access={:?}, error={:?}, root={:#x}",
            vm_id,
            addr.as_usize(),
            access_flags,
            error,
            root.as_usize()
        ),
    }
}

fn log_translation(vm_id: usize, resources: &AxVMResources, addr: GuestPhysAddr, handled: bool) {
    let translation = resources.address_space.translate(addr);
    if handled {
        debug!(
            "VM[{}] stage2 translate: gpa={:#x} -> {:?}",
            vm_id,
            addr.as_usize(),
            translation
        );
    } else {
        warn!(
            "VM[{}] stage2 translate: gpa={:#x} -> {:?}",
            vm_id,
            addr.as_usize(),
            translation
        );
    }
}

fn log_memory_region(vm_id: usize, resources: &AxVMResources, addr: GuestPhysAddr, handled: bool) {
    for (index, region) in resources.memory_regions.iter().enumerate() {
        let start = region.gpa.as_usize();
        let end = start + region.size();
        if !(start..end).contains(&addr.as_usize()) {
            continue;
        }
        if handled {
            debug_region(vm_id, index, start, end, region);
        } else {
            warn_region(vm_id, index, start, end, region);
        }
    }
}

fn debug_region(
    vm_id: usize,
    index: usize,
    start: usize,
    end: usize,
    region: &crate::VMMemoryRegion,
) {
    debug!(
        "VM[{}] stage2 region hit[{}]: gpa=[{:#x},{:#x}) hva={:#x} hpa={:#x} size={:#x} \
         identical={}",
        vm_id,
        index,
        start,
        end,
        region.hva.as_usize(),
        region.host_paddr().as_usize(),
        region.size(),
        region.is_identical()
    );
}

fn warn_region(
    vm_id: usize,
    index: usize,
    start: usize,
    end: usize,
    region: &crate::VMMemoryRegion,
) {
    warn!(
        "VM[{}] stage2 region hit[{}]: gpa=[{:#x},{:#x}) hva={:#x} hpa={:#x} size={:#x} \
         identical={}",
        vm_id,
        index,
        start,
        end,
        region.hva.as_usize(),
        region.host_paddr().as_usize(),
        region.size(),
        region.is_identical()
    );
}
