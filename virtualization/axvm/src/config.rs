// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Runtime configuration structures for an AxVM instance.

use alloc::{string::String, vec::Vec};

pub use axvm_types::{
    GuestFirmwareKind, GuestPhysAddr, HostPhysAddr, InterruptDelivery, MappingFlags,
    VMBootProtocol, VmMachineMode,
};

use crate::{
    AxVmResult,
    arch::{ArchOps, CurrentArch},
    ax_err,
};

/// Policy used by AxVM when deriving runtime guest boot image addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuestBootPolicy {
    /// Keep the load addresses exactly as provided by the VM config.
    #[default]
    KeepConfigured,
    /// Adjust the kernel load address for boot protocols that require a
    /// reserved area inside the primary guest memory region.
    AdjustKernelForBootProtocol { protocol: VMBootProtocol },
}

/// A part of `AxVMConfig`, which represents a `VCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVCpuConfig {
    /// The entry address in GPA for the Bootstrap Processor (BSP).
    pub bsp_entry: GuestPhysAddr,
    /// The entry address in GPA for the Application Processor (AP).
    pub ap_entry: GuestPhysAddr,
}

/// Ramdisk image information.
#[derive(Debug, Default, Clone)]
pub struct RamdiskInfo {
    /// The load address in GPA for the ramdisk image.
    pub load_gpa: GuestPhysAddr,
    /// The size in bytes of the ramdisk image, `None` if not known yet.
    pub size: Option<usize>,
}

/// A part of `AxVMConfig`, which stores configuration attributes related to the load address of VM images.
#[derive(Debug, Default, Clone)]
pub struct VMImageConfig {
    /// The load address in GPA for the kernel image.
    pub kernel_load_gpa: GuestPhysAddr,
    /// Whether VM images are loaded from the host filesystem.
    pub loaded_from_filesystem: bool,
    /// The load address in GPA for the BIOS image, `None` if not used.
    pub bios_load_gpa: Option<GuestPhysAddr>,
    /// The load address in GPA for the device tree blob (DTB), `None` if not used.
    pub dtb_load_gpa: Option<GuestPhysAddr>,
    /// Ramdisk image info, `None` if not used.
    pub ramdisk: Option<RamdiskInfo>,
}

/// Physical ownership and backing of one guest memory region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmMemoryBacking {
    /// Zeroed memory allocated and owned by this VM.
    Allocated,
    /// VM-owned memory whose guest and host physical addresses are identical.
    IdentityAllocated,
    /// A host physical range exclusively assigned to this VM.
    Host { host_base: HostPhysAddr },
    /// A host physical range intentionally shared with another owner.
    Shared { host_base: HostPhysAddr },
    /// An identity-addressed platform range reserved for guest use.
    Reserved { host_base: HostPhysAddr },
}

/// Immutable runtime description of one guest memory mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmMemoryConfig {
    guest_base: GuestPhysAddr,
    size: usize,
    flags: MappingFlags,
    backing: VmMemoryBacking,
}

impl VmMemoryConfig {
    /// Creates a checked non-empty memory mapping description.
    pub fn new(
        guest_base: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        backing: VmMemoryBacking,
    ) -> AxVmResult<Self> {
        if size == 0
            || guest_base.as_usize().checked_add(size).is_none()
            || matches!(backing, VmMemoryBacking::IdentityAllocated) && guest_base.as_usize() != 0
            || backing
                .host_base()
                .is_some_and(|base| base.as_usize().checked_add(size).is_none())
        {
            return ax_err!(
                InvalidInput,
                alloc::format!(
                    "invalid memory region at guest {:#x} with size {size:#x}",
                    guest_base.as_usize()
                )
            );
        }
        Ok(Self {
            guest_base,
            size,
            flags,
            backing,
        })
    }

    /// Returns the guest physical base.
    pub const fn guest_base(self) -> GuestPhysAddr {
        self.guest_base
    }

    /// Returns the region length in bytes.
    pub const fn size(self) -> usize {
        self.size
    }

    /// Returns guest stage-2 access permissions.
    pub const fn flags(self) -> MappingFlags {
        self.flags
    }

    /// Returns the physical backing policy.
    pub const fn backing(self) -> VmMemoryBacking {
        self.backing
    }
}

impl VmMemoryBacking {
    /// Returns the first host physical address for externally backed memory.
    pub const fn host_base(self) -> Option<HostPhysAddr> {
        match self {
            Self::Allocated | Self::IdentityAllocated => None,
            Self::Host { host_base }
            | Self::Shared { host_base }
            | Self::Reserved { host_base } => Some(host_base),
        }
    }

    /// Returns whether AxVM must release the backing allocation on drop.
    pub const fn is_allocated(self) -> bool {
        matches!(self, Self::Allocated | Self::IdentityAllocated)
    }
}

/// Runtime configuration for one VM.
#[derive(Debug, Default)]
pub struct AxVMConfig {
    id: usize,
    name: String,
    machine_plan: crate::machine::VmMachinePlan,
    pub(crate) phys_cpu_ls: PhysCpuList,
    pub(crate) cpu_config: AxVCpuConfig,
    pub(crate) image_config: VMImageConfig,
    memory_regions: Vec<VmMemoryConfig>,
    boot_policy: GuestBootPolicy,
    arch: crate::arch::VmArchConfig,
}

/// Parameters used to build an [`AxVMConfig`].
#[derive(Debug, Default)]
pub struct AxVMConfigParams {
    pub id: usize,
    pub name: String,
    pub machine_plan: crate::machine::VmMachinePlan,
    pub phys_cpu_ls: PhysCpuList,
    pub cpu_config: AxVCpuConfig,
    pub image_config: VMImageConfig,
    pub memory_regions: Vec<VmMemoryConfig>,
    pub boot_policy: GuestBootPolicy,
}

impl AxVMConfig {
    pub fn new(params: AxVMConfigParams) -> Self {
        Self {
            id: params.id,
            name: params.name,
            machine_plan: params.machine_plan,
            phys_cpu_ls: params.phys_cpu_ls,
            cpu_config: params.cpu_config,
            image_config: params.image_config,
            memory_regions: params.memory_regions,
            boot_policy: params.boot_policy,
            arch: crate::arch::VmArchConfig::new(),
        }
    }

    /// Returns VM id.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Returns whether the platform is fully virtual or host-derived.
    pub const fn machine_mode(&self) -> VmMachineMode {
        self.machine_plan.mode()
    }

    /// Returns the selected guest firmware format.
    pub const fn firmware(&self) -> GuestFirmwareKind {
        self.machine_plan.firmware()
    }

    /// Returns VM name.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Returns configurations related to VM image load addresses.
    pub fn image_config(&self) -> &VMImageConfig {
        &self.image_config
    }

    /// Updates the DTB load address used as an architecture boot argument.
    pub(crate) fn update_dtb_load_gpa(&mut self, dtb_load_gpa: Option<GuestPhysAddr>) {
        self.image_config.dtb_load_gpa = dtb_load_gpa;
    }

    /// Returns whether VM images are loaded from the host filesystem.
    pub fn images_loaded_from_filesystem(&self) -> bool {
        self.image_config.loaded_from_filesystem
    }

    /// Returns the entry address in GPA for the Bootstrap Processor (BSP).
    pub fn bsp_entry(&self) -> GuestPhysAddr {
        // Retrieves BSP entry from the CPU configuration.
        self.cpu_config.bsp_entry
    }

    /// Returns the entry address in GPA for the Application Processor (AP).
    pub fn ap_entry(&self) -> GuestPhysAddr {
        // Retrieves AP entry from the CPU configuration.
        self.cpu_config.ap_entry
    }

    /// Returns configurations related to VM memory regions.
    pub fn memory_regions(&self) -> &[VmMemoryConfig] {
        &self.memory_regions
    }

    /// Returns the policy used to adjust runtime boot image addresses.
    pub fn boot_policy(&self) -> GuestBootPolicy {
        self.boot_policy
    }

    pub(crate) const fn arch(&self) -> &crate::arch::VmArchConfig {
        &self.arch
    }

    pub(crate) const fn arch_mut(&mut self) -> &mut crate::arch::VmArchConfig {
        &mut self.arch
    }

    /// Returns the interrupt mode of the VM.
    pub fn interrupt_delivery(&self) -> InterruptDelivery {
        self.machine_plan.interrupt_delivery()
    }

    /// Returns the immutable machine plan consumed by VM construction.
    pub const fn machine_plan(&self) -> &crate::machine::VmMachinePlan {
        &self.machine_plan
    }

    /// Relocate the guest kernel image while preserving the configured
    /// entry-point offsets relative to the load address.
    pub(crate) fn relocate_kernel_image(&mut self, kernel_load_gpa: GuestPhysAddr) {
        let old_load = self.image_config.kernel_load_gpa.as_usize();
        let new_load = kernel_load_gpa.as_usize();

        let bsp_offset = self
            .cpu_config
            .bsp_entry
            .as_usize()
            .checked_sub(old_load)
            .expect("BSP entry must not be below kernel load address");
        let ap_offset = self
            .cpu_config
            .ap_entry
            .as_usize()
            .checked_sub(old_load)
            .expect("AP entry must not be below kernel load address");

        self.image_config.kernel_load_gpa = kernel_load_gpa;
        self.cpu_config.bsp_entry = GuestPhysAddr::from(new_load + bsp_offset);
        self.cpu_config.ap_entry = GuestPhysAddr::from(new_load + ap_offset);
    }
}

/// Represents the list of physical CPUs available for the VM.
#[derive(Debug, Default, Clone)]
pub struct PhysCpuList {
    cpu_num: usize,
    phys_cpu_ids: Option<Vec<usize>>,
    phys_cpu_sets: Option<Vec<usize>>,
}

impl PhysCpuList {
    /// Creates a physical CPU list.
    pub fn new(
        cpu_num: usize,
        phys_cpu_ids: Option<Vec<usize>>,
        phys_cpu_sets: Option<Vec<usize>>,
    ) -> Self {
        Self {
            cpu_num,
            phys_cpu_ids,
            phys_cpu_sets,
        }
    }

    /// Returns vCpu id list and its corresponding pCpu affinity list, as well as its physical id.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    /// if the pCPU id is not provided, the vCpu's physical id will be set as vCpu id.
    ///
    /// Returns a vector of tuples, each tuple contains:
    /// - The vCpu id.
    /// - The pCpu affinity mask, `None` if not set.
    /// - The physical id of the vCpu, equal to vCpu id if not provided.
    pub fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        if let Some(phys_cpu_ids) = &self.phys_cpu_ids
            && self.cpu_num != phys_cpu_ids.len()
        {
            error!(
                "ERROR!!!: cpu_num: {}, phys_cpu_ids: {:?}",
                self.cpu_num, self.phys_cpu_ids
            );
        }
        CurrentArch::vcpu_affinities(
            self.cpu_num,
            self.phys_cpu_ids.as_deref(),
            self.phys_cpu_sets.as_deref(),
        )
    }

    /// Returns the number of CPUs.
    pub fn cpu_num(&self) -> usize {
        self.cpu_num
    }

    /// Returns the physical CPU IDs.
    pub fn phys_cpu_ids(&self) -> &Option<Vec<usize>> {
        &self.phys_cpu_ids
    }

    /// Returns the physical CPU sets.
    pub fn phys_cpu_sets(&self) -> &Option<Vec<usize>> {
        &self.phys_cpu_sets
    }

    /// Sets the guest CPU sets.
    pub fn set_guest_cpu_sets(&mut self, phys_cpu_sets: Vec<usize>) {
        self.phys_cpu_sets = Some(phys_cpu_sets);
    }

    /// Sets the CPU IDs exposed to the guest.
    pub fn set_guest_phys_cpu_ids(&mut self, phys_cpu_ids: Vec<usize>) {
        self.phys_cpu_ids = Some(phys_cpu_ids);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn memory_region(gpa: usize, size: usize, backing: VmMemoryBacking) -> VmMemoryConfig {
        VmMemoryConfig::new(
            GuestPhysAddr::from(gpa),
            size,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            backing,
        )
        .unwrap()
    }

    #[test]
    fn memory_regions_preserve_explicit_backing() {
        let main_memory = memory_region(
            0x8000_0000,
            0x200000,
            VmMemoryBacking::Host {
                host_base: HostPhysAddr::from(0xa000_0000),
            },
        );
        let reserved_memory = memory_region(
            0x110000,
            0x10000,
            VmMemoryBacking::Reserved {
                host_base: HostPhysAddr::from(0x110000),
            },
        );
        let config = AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: String::from("linux"),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            memory_regions: vec![main_memory, reserved_memory],
            ..Default::default()
        });

        let regions = config.memory_regions();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].guest_base(), GuestPhysAddr::from(0x8000_0000));
        assert_eq!(
            regions[0].backing().host_base(),
            Some(HostPhysAddr::from(0xa000_0000))
        );
        assert_eq!(regions[1].size(), 0x10000);
        assert!(matches!(
            regions[1].backing(),
            VmMemoryBacking::Reserved { .. }
        ));
    }
}
