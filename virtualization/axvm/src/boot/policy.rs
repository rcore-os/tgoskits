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

//! Guest boot-description ownership for AxVM.

use alloc::vec::Vec;

use axvm_types::GuestPhysAddr;

use super::BootImageProvider;

/// Selects the guest address-adjustment policy for the current architecture.
pub fn guest_boot_policy(
    config: &axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> crate::config::GuestBootPolicy {
    if crate::boot::is_x86_linux_image_config(config, provider) {
        crate::config::GuestBootPolicy::KeepConfigured
    } else {
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: config.kernel.effective_boot_protocol(),
        }
    }
}

/// Resolves the configured or architecture-default boot firmware load address.
pub fn boot_firmware_load_gpa(config: &axvmconfig::AxVMCrateConfig) -> Option<GuestPhysAddr> {
    if !config.kernel.enable_bios {
        return None;
    }

    config
        .kernel
        .bios_load_addr
        .map(GuestPhysAddr::from)
        .or_else(|| crate::arch::default_boot_firmware_load_gpa(config))
}

/// Device-tree boot description owned by the VM lifecycle.
#[derive(Debug, Clone)]
pub struct GuestDeviceTree {
    load_gpa: GuestPhysAddr,
    bytes: Vec<u8>,
}

impl GuestDeviceTree {
    /// Creates a DTB descriptor with owned bytes.
    pub fn generated(load_gpa: GuestPhysAddr, bytes: Vec<u8>) -> Self {
        Self { load_gpa, bytes }
    }

    /// Returns the guest physical load address of the DTB.
    pub const fn load_gpa(&self) -> GuestPhysAddr {
        self.load_gpa
    }

    /// Returns the owned DTB bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn range(&self) -> (usize, usize) {
        (self.load_gpa.as_usize(), self.bytes.len())
    }
}

/// ACPI boot-description tables owned by the VM lifecycle.
#[derive(Debug, Clone)]
pub struct GuestAcpiTables {
    rsdp_gpa: GuestPhysAddr,
    bytes: Vec<u8>,
}

impl GuestAcpiTables {
    /// Creates an ACPI table descriptor.
    pub fn generated(rsdp_gpa: GuestPhysAddr, bytes: Vec<u8>) -> Self {
        Self { rsdp_gpa, bytes }
    }

    /// Returns the guest physical address of the RSDP.
    pub const fn rsdp_gpa(&self) -> GuestPhysAddr {
        self.rsdp_gpa
    }

    /// Returns the owned ACPI table bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn range(&self) -> (usize, usize) {
        (self.rsdp_gpa.as_usize(), self.bytes.len())
    }
}

/// Guest boot-description payload selected for this VM.
#[derive(Debug, Default, Clone)]
pub struct GuestBootDescription {
    device_tree: Option<GuestDeviceTree>,
    acpi_tables: Option<GuestAcpiTables>,
}

impl GuestBootDescription {
    /// Creates an empty boot-description object.
    pub const fn none() -> Self {
        Self {
            device_tree: None,
            acpi_tables: None,
        }
    }

    /// Replaces the device-tree description.
    pub fn set_device_tree(&mut self, device_tree: GuestDeviceTree) {
        self.device_tree = Some(device_tree);
        self.acpi_tables = None;
    }

    /// Replaces the ACPI table description.
    pub fn set_acpi_tables(&mut self, acpi_tables: GuestAcpiTables) {
        self.acpi_tables = Some(acpi_tables);
        self.device_tree = None;
    }

    /// Returns the device-tree descriptor, if this VM boots with DTB.
    pub const fn device_tree(&self) -> Option<&GuestDeviceTree> {
        self.device_tree.as_ref()
    }

    /// Returns the ACPI descriptor, if this VM boots with ACPI.
    pub const fn acpi_tables(&self) -> Option<&GuestAcpiTables> {
        self.acpi_tables.as_ref()
    }

    pub(crate) fn occupied_ranges(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.device_tree
            .iter()
            .map(GuestDeviceTree::range)
            .chain(self.acpi_tables.iter().map(GuestAcpiTables::range))
    }
}

/// Device-tree builder abstraction owned by AxVM.
///
/// Platform-specific host FDT parsing still lives in the monitor today; this
/// type establishes the AxVM-side lifecycle boundary for generated DTBs.
#[derive(Debug, Default)]
pub struct GuestFdtBuilder {
    bytes: Vec<u8>,
}

impl GuestFdtBuilder {
    /// Creates a builder from already generated DTB bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Finishes the builder and returns a VM-owned DTB descriptor.
    pub fn build(self, load_gpa: GuestPhysAddr) -> GuestDeviceTree {
        GuestDeviceTree::generated(load_gpa, self.bytes)
    }
}
