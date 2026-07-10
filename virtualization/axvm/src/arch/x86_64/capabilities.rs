//! x86_64 implementations of AxVM platform capability hooks.

use alloc::{format, sync::Arc};

use ax_errno::{AxResult, ax_err_type};
use axdevice_base::{BaseDeviceOps, DeviceRegistry as _, PortDeviceAdapter};

use super::X86_64Arch;
use crate::architecture::{AddressSpacePlatform, DevicePlatform, HostTimePlatform};

impl DevicePlatform for X86_64Arch {
    fn register_devices(
        _vm: &crate::AxVM,
        config: &crate::config::AxVMConfig,
        devices: &mut axdevice::AxVmDevices,
    ) -> AxResult {
        for port in config.pass_through_ports() {
            let passthrough = Arc::new(super::port::HostPortPassthrough::new(
                port.base,
                port.length,
            )?);
            let range = passthrough.address_range();
            debug!(
                "PT port region: [{:#x}~{:#x}]",
                range.start.number(),
                range.end.number(),
            );
            devices
                .register(PortDeviceAdapter::from_arc(passthrough))
                .map_err(|err| ax_err_type!(InvalidInput, format!("register PT port: {err:?}")))?;
        }
        for config in config.emu_devices() {
            super::register_arch_device(config, devices)?;
        }
        Ok(())
    }
}

impl AddressSpacePlatform for X86_64Arch {
    #[cfg(feature = "vmx")]
    fn append_owned_regions(regions: &mut alloc::vec::Vec<crate::layout::GuestOwnedRegion>) {
        regions.push(crate::layout::GuestOwnedRegion::new(
            x86_vcpu::X86_APIC_ACCESS_GPA,
            ax_memory_addr::PAGE_SIZE_4K,
            crate::layout::VmRegionKind::Reserved,
        ));
    }

    #[cfg(feature = "vmx")]
    fn map_address_space(
        address_space: &mut axaddrspace::AddrSpace<Self::NestedPageTable>,
    ) -> AxResult {
        address_space.map_linear(
            axvm_types::GuestPhysAddr::from(x86_vcpu::X86_APIC_ACCESS_GPA),
            super::x86_apic_access_page_addr(),
            ax_memory_addr::PAGE_SIZE_4K,
            axvm_types::MappingFlags::DEVICE
                | axvm_types::MappingFlags::READ
                | axvm_types::MappingFlags::WRITE,
        )
    }
}

impl HostTimePlatform for X86_64Arch {}
