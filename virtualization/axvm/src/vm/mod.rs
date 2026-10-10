//! Resource construction owned exclusively by the VM control task.

use std::{alloc::Layout, boxed::Box, string::String, sync::Arc, vec::Vec};

use axaddrspace::AddrSpace;
use axdevice::{
    DeviceRuntime, FwCfgKernelPayload, FwCfgPayloadSlot, FwCfgPlatformConfig, RuntimeAccessPorts,
};
use axvm_types::*;

use crate::{
    AxVmError, AxVmResult,
    arch::current::{ArchNestedPageTable, ArchVCpu},
    ax_err_type,
    boot::GuestBootDescription,
    config::{AxVMConfig, PhysCpuList},
    guest_memory::{GuestRange, MappingLease, MemoryBacking},
    host::{HostMemory, default_host, paging::virt_to_phys},
    layout::VmAddressLayout,
    vcpu::AxVCpu,
};

pub(crate) mod boot;
pub(crate) mod memory;
pub(crate) mod prepare;
#[cfg(any(test, target_arch = "aarch64"))]
mod timer_wait;
pub use memory::PreparedMemoryLayout;
#[cfg(target_arch = "aarch64")]
pub(crate) use timer_wait::{VcpuTimerWaitGeneration, VcpuTimerWaitToken};

const VM_ASPACE_BASE: usize = 0;
const VM_ASPACE_SIZE: usize = 0x7fff_ffff_f000;
pub(crate) type VCpu = AxVCpu<ArchVCpu>;

/// Architecture-independent observation copied from the owning vCPU task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VcpuSnapshot {
    pub id: usize,
    pub state: VmVcpuState,
    pub phys_cpu_set: Option<usize>,
}

pub(crate) fn width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}
pub(crate) fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}

pub(crate) struct AxVMResources {
    pub(crate) address_space: AddrSpace<ArchNestedPageTable>,
    pub(crate) nested_paging: NestedPagingConfig,
    pub(crate) memory_regions: Vec<VMMemoryRegion>,
    pub(crate) phys_cpu_ls: PhysCpuList,
    pub(crate) vcpu_list: Option<Box<[Option<VCpu>]>>,
    pub(crate) devices: Option<Arc<DeviceRuntime>>,
    pub(crate) interrupt_controller: Option<Arc<dyn axdevice_base::VirtualInterruptController>>,
    pub(crate) address_layout: Option<VmAddressLayout>,
    pub(crate) boot_description: GuestBootDescription,
    pub(crate) device_plan: Arc<crate::arch::current::ArchVmPlan>,
    pub(crate) memory_leases: Vec<MappingLease>,
}

impl AxVMResources {
    pub(crate) fn from_page_table(
        page_table: ArchNestedPageTable,
        device_plan: crate::arch::current::ArchVmPlan,
        build_nested_paging: impl FnOnce(HostPhysAddr) -> AxVmResult<NestedPagingConfig>,
    ) -> AxVmResult<Self> {
        let address_space = AddrSpace::new_empty(
            page_table,
            GuestPhysAddr::from(VM_ASPACE_BASE),
            VM_ASPACE_SIZE,
        )
        .map_err(|error| AxVmError::from_addrspace("create guest address space", error))?;
        let nested_paging = build_nested_paging(address_space.page_table_root())?;
        Ok(Self {
            address_space,
            nested_paging,
            memory_regions: Vec::new(),
            phys_cpu_ls: PhysCpuList::default(),
            vcpu_list: None,
            devices: None,
            interrupt_controller: None,
            address_layout: None,
            boot_description: GuestBootDescription::none(),
            device_plan: Arc::new(device_plan),
            memory_leases: Vec::new(),
        })
    }

    pub(crate) fn planned_devices(&self) -> &crate::vm::prepare::device_plan::VmDevicePlan {
        use crate::vm::prepare::device_plan::ArchitectureVmPlan;

        self.device_plan.devices()
    }

    pub(crate) fn devices(&self) -> AxVmResult<Arc<DeviceRuntime>> {
        self.devices
            .clone()
            .ok_or_else(|| ax_err_type!(BadState, "VM devices are not prepared"))
    }

    pub(crate) fn reset_transient_resources(&mut self) -> AxVmResult<Option<Arc<DeviceRuntime>>> {
        if let Some(devices) = &self.devices {
            devices
                .stop_lifecycle_devices()
                .map_err(|error| AxVmError::device("stop VM devices", error))?;
        }
        self.vcpu_list = None;
        self.interrupt_controller = None;
        self.address_layout = None;
        self.address_space.clear();
        for lease in &self.memory_leases {
            self.address_space
                .map_linear(
                    lease.range.start,
                    lease.host,
                    lease.range.length,
                    lease.flags,
                )
                .map_err(|error| AxVmError::from_addrspace("restore guest RAM mappings", error))?;
        }
        Ok(self.devices.take())
    }
}

pub struct FwCfgDeviceConfig {
    pub base: GuestPhysAddr,
    pub size: usize,
    pub kernel: FwCfgKernelPayload,
    pub initrd: Option<Arc<[u8]>>,
    pub cmdline: Option<String>,
    pub cpu_num: u16,
    pub platform: FwCfgPlatformConfig,
}

/// Represents a memory region in a virtual machine.
#[derive(Debug, Clone)]
pub struct VMMemoryRegion {
    /// Guest physical address.
    pub gpa: GuestPhysAddr,
    /// Host virtual address.
    pub hva: HostVirtAddr,
    /// Memory layout of the region.
    pub layout: Layout,
    /// Whether this region was allocated by the allocator and needs to be deallocated
    pub needs_dealloc: bool,
}

impl VMMemoryRegion {
    /// Returns the size of the memory region.
    pub fn size(&self) -> usize {
        self.layout.size()
    }

    /// Returns the host physical address backing this guest memory region.
    pub fn host_paddr(&self) -> HostPhysAddr {
        virt_to_phys(self.hva)
    }

    /// Returns `true` if the guest physical address is identical to the host physical address.
    pub fn is_identical(&self) -> bool {
        self.gpa.as_usize() == self.host_paddr().as_usize()
    }
}

/// Construction state. It is moved into one control owner and is never shared.
pub(crate) struct AxVM {
    id: VMId,
    name: String,
    config: AxVMConfig,
    pub(crate) resources: AxVMResources,
    ports: RuntimeAccessPorts,
    fw_cfg_payload: Arc<FwCfgPayloadSlot>,
}

impl AxVM {
    pub(crate) fn new(mut config: AxVMConfig, ports: RuntimeAccessPorts) -> AxVmResult<Self> {
        let fw_cfg_payload = Arc::new(FwCfgPayloadSlot::new());
        let resources = crate::arch::current::CurrentArch::create_vm_resources(
            &mut config,
            fw_cfg_payload.clone(),
        )?;
        Ok(Self {
            id: config.id(),
            name: config.name(),
            config,
            resources,
            ports,
            fw_cfg_payload,
        })
    }

    pub(crate) const fn id(&self) -> VMId {
        self.id
    }
    pub(crate) fn name(&self) -> String {
        self.name.clone()
    }
    pub(crate) const fn config(&self) -> &AxVMConfig {
        &self.config
    }
    pub(crate) fn config_mut(&mut self) -> &mut AxVMConfig {
        &mut self.config
    }
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn uses_passthrough_address_space(&self) -> bool {
        self.config.uses_passthrough_address_space()
    }
    pub(crate) fn memory_regions(&self) -> Vec<VMMemoryRegion> {
        self.resources.memory_regions.clone()
    }
    pub(crate) fn nested_page_table_root(&self) -> HostPhysAddr {
        self.resources.address_space.page_table_root()
    }
    pub(crate) fn device_access_ports(&self) -> RuntimeAccessPorts {
        self.ports.clone()
    }
    pub(crate) fn replace_access_ports(&mut self, ports: RuntimeAccessPorts) {
        self.ports = ports;
    }
    pub(crate) fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        self.config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids()
    }
    pub(crate) fn get_vcpu_guest_mpidrs(&self) -> Vec<(usize, u64)> {
        self.resources
            .vcpu_list
            .iter()
            .flat_map(|cpus| cpus.iter())
            .flatten()
            .filter_map(|cpu| cpu.guest_mpidr().map(|mpidr| (cpu.id(), mpidr)))
            .collect()
    }
    pub(crate) fn vcpu_snapshots(&self) -> Vec<VcpuSnapshot> {
        self.resources
            .vcpu_list
            .iter()
            .flat_map(|cpus| cpus.iter())
            .flatten()
            .map(|cpu| VcpuSnapshot {
                id: cpu.id(),
                state: cpu.state(),
                phys_cpu_set: cpu.phys_cpu_set(),
            })
            .collect()
    }
    pub(crate) fn get_devices(&self) -> AxVmResult<Arc<DeviceRuntime>> {
        self.resources.devices()
    }
    pub(crate) fn device_count(&self) -> usize {
        self.resources
            .devices
            .as_ref()
            .map_or(0, |devices| devices.devices().count())
    }
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn with_architecture_plan<R>(
        &self,
        read: impl FnOnce(&crate::arch::current::ArchVmPlan) -> AxVmResult<R>,
    ) -> AxVmResult<R> {
        read(&self.resources.device_plan)
    }
    #[cfg(not(target_arch = "aarch64"))]
    pub(crate) fn with_planned_device_graph<R>(
        &self,
        read: impl FnOnce(&axdevice::ResolvedDeviceGraph) -> AxVmResult<R>,
    ) -> AxVmResult<R> {
        read(self.resources.planned_devices().graph())
    }
    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) fn set_guest_device_tree(
        &mut self,
        address: GuestPhysAddr,
        bytes: Vec<u8>,
    ) -> AxVmResult {
        self.config.set_dtb_load_gpa(address);
        self.resources
            .boot_description
            .set_device_tree(crate::boot::GuestFdtBuilder::from_bytes(bytes).build(address));
        Ok(())
    }
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn set_guest_acpi_tables(
        &mut self,
        address: GuestPhysAddr,
        bytes: Vec<u8>,
    ) -> AxVmResult {
        self.resources
            .boot_description
            .set_acpi_tables(crate::boot::GuestAcpiTables::generated(address, bytes));
        Ok(())
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
    pub(crate) fn add_fw_cfg_device(&mut self, config: FwCfgDeviceConfig) -> AxVmResult {
        self.fw_cfg_payload
            .set(axdevice::FwCfgPayloadConfig {
                base: config.base,
                size: config.size,
                kernel: config.kernel,
                initrd: config.initrd,
                cmdline: config.cmdline,
                cpu_num: config.cpu_num,
                platform: config.platform,
            })
            .map_err(Into::into)
    }

    /// Discards the previous boot input after its run has fully retired.
    pub(crate) fn clear_boot_payload(&mut self) {
        drop(self.fw_cfg_payload.clear());
    }

    pub(crate) fn alloc_memory_region(
        &mut self,
        layout: Layout,
        guest: Option<GuestPhysAddr>,
    ) -> AxVmResult {
        let backing = MemoryBacking::allocate(layout)?;
        let host = virt_to_phys(backing.address());
        let guest = guest.unwrap_or_else(|| host.as_usize().into());
        let flags =
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER;
        let lease = MappingLease::new(
            GuestRange::new(guest, layout.size())?,
            host,
            flags,
            backing.clone(),
            0,
        )?;
        self.resources
            .address_space
            .map_linear(guest, host, layout.size(), flags)
            .map_err(|error| AxVmError::from_addrspace("map allocated guest RAM", error))?;
        self.resources.memory_regions.push(VMMemoryRegion {
            gpa: guest,
            hva: backing.address(),
            layout,
            needs_dealloc: true,
        });
        self.resources.memory_leases.push(lease);
        Ok(())
    }

    pub(crate) fn map_reserved_memory_region(
        &mut self,
        layout: Layout,
        guest: Option<GuestPhysAddr>,
        flags: MappingFlags,
    ) -> AxVmResult {
        let guest =
            guest.ok_or_else(|| ax_err_type!(InvalidInput, "reserved memory GPA is required"))?;
        let host = HostPhysAddr::from(guest.as_usize());
        let virtual_address = default_host().phys_to_virt(host);
        // SAFETY: MapReserved is the platform/configuration contract for RAM
        // excluded from the host allocator for the lifetime of the monitor.
        // Updates of passthrough resources require a separate DMA quiet proof.
        let backing = unsafe {
            MemoryBacking::reserved(virtual_address, layout.size(), Arc::new(ReservedRam))
        };
        let flags = flags | MappingFlags::USER;
        let lease = MappingLease::new(
            GuestRange::new(guest, layout.size())?,
            host,
            flags,
            backing,
            0,
        )?;
        self.resources
            .address_space
            .map_linear(guest, host, layout.size(), flags)
            .map_err(|error| AxVmError::from_addrspace("map reserved guest RAM", error))?;
        self.resources.memory_regions.push(VMMemoryRegion {
            gpa: guest,
            hva: virtual_address,
            layout,
            needs_dealloc: false,
        });
        self.resources.memory_leases.push(lease);
        Ok(())
    }

    pub(crate) fn prepare_memory_layout(&mut self) -> AxVmResult<PreparedMemoryLayout> {
        let configs = self.config.memory_regions().to_vec();
        let layout = memory::MemoryLayoutBuilder::new(self, &configs).prepare()?;
        let main = layout.main_memory();
        boot::BootImagePlan::new(main.gpa, main.is_identical()).apply_to_config(&mut self.config);
        Ok(layout)
    }

    pub(crate) fn write_to_guest(&mut self, guest: GuestPhysAddr, input: &[u8]) -> AxVmResult {
        copy_owner_memory(
            &self.resources.address_space,
            guest,
            input.len(),
            |offset, address| {
                // SAFETY: the owner is loading unpublished RAM and retains its
                // allocation. No Rust reference to guest memory escapes this call.
                unsafe { address.write_volatile(input[offset]) };
            },
        )?;
        // Images may span non-contiguous RAM; each translated page is cleaned.
        let mut offset = 0;
        while offset < input.len() {
            let address = self
                .resources
                .address_space
                .translate(guest + offset)
                .ok_or_else(|| ax_err_type!(InvalidInput, "unmapped guest image"))?;
            let count = (0x1000 - address.as_usize() % 0x1000).min(input.len() - offset);
            crate::arch::current::make_guest_memory_visible(
                default_host().phys_to_virt(address),
                count,
            );
            offset += count;
        }
        Ok(())
    }
}

struct ReservedRam;

fn copy_owner_memory(
    space: &AddrSpace<ArchNestedPageTable>,
    start: GuestPhysAddr,
    length: usize,
    mut copy: impl FnMut(usize, *mut u8),
) -> AxVmResult {
    start
        .as_usize()
        .checked_add(length)
        .ok_or_else(|| ax_err_type!(InvalidInput, "guest copy range overflows"))?;
    let mut offset = 0;
    while offset < length {
        let address = space
            .translate(start + offset)
            .ok_or_else(|| ax_err_type!(InvalidInput, "unmapped guest copy"))?;
        let count = (0x1000 - address.as_usize() % 0x1000).min(length - offset);
        let pointer = default_host().phys_to_virt(address).as_mut_ptr();
        for index in 0..count {
            copy(offset + index, pointer.wrapping_add(index));
        }
        offset += count;
    }
    Ok(())
}
