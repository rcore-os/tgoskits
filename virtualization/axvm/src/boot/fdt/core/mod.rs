//! Architecture-neutral guest device-tree preparation.

use std::format;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use std::vec::Vec;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use axvmconfig::{GuestConfig, VMBootProtocol};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::boot::{BootImageProvider, fdt::GuestDtbImage};
use crate::{
    AxVmResult, ax_err_type,
    config::AxVMConfig,
    machine::{GuestGicCpuRegion, GuestGicProfile, GuestSerialFdtInterrupt},
};

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) mod cpu;
pub(crate) mod create;
mod device;
mod disabled;
mod import;
pub(crate) mod interrupt;
mod parser;
mod policy;
mod print;
mod references;
mod reserved;
pub(crate) mod serial;
pub(crate) mod timer;
pub(crate) mod tree;

#[cfg(test)]
mod tree_tests;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use parser::*;
pub use policy::{DecodedInterrupt, GuestFdtPolicy};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn prepare_dtb_guest(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    let host_fdt_bytes = try_get_host_fdt();
    resolve_machine_resources_from_host(vm_config, host_fdt_bytes)?;

    let uefi = vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi;
    // Load the developer-supplied DTB at most once so the console contract and
    // the guest image are always derived from the same bytes.
    let provided_dtb = if uefi {
        None
    } else {
        get_developer_provided_dtb(vm_config, vm_create_config, provider)?
    };
    // Explicit guest firmware owns the virtualized GIC and UART resources.
    // Resolve them exactly once so the console contract validated below is the
    // one the immutable device plan is actually built from.
    if !uefi {
        select_guest_machine_resources(vm_config, provided_dtb.as_deref())?;
    }
    resolve_console_profile(vm_config, host_fdt_bytes, provided_dtb.as_deref())?;

    if uefi {
        skip_guest_dtb(vm_config, vm_create_config);
        return Ok(None);
    }

    let guest_dtb = build_guest_dtb(vm_config, vm_create_config, provided_dtb, host_fdt_bytes)?;
    enrich_guest_config(vm_config, vm_create_config, guest_dtb.as_ref())?;
    Ok(guest_dtb)
}

/// Applies the console-source priority to the machine serial profile.
///
/// An explicit `console0` virtual device wins outright. Otherwise a supplied
/// guest DTB console contract is honored before the host-selected serial, and a
/// machine profile is preserved when neither firmware source selects one.
fn resolve_console_profile(
    vm_config: &mut AxVMConfig,
    host_fdt_bytes: Option<&[u8]>,
    provided_dtb: Option<&[u8]>,
) -> AxVmResult {
    let machine = crate::machine::current_machine_profile(vm_config.phys_cpu_ls.cpu_num());
    let Some(interrupt_encoding) = machine.serial_fdt_interrupt else {
        return Ok(());
    };
    let explicit_console = vm_config
        .virtual_device_requests()
        .iter()
        .any(|request| request.id == "console0");
    let Some(resolved) = serial::resolve_console_source(
        vm_config.serial_profile(),
        interrupt_encoding,
        explicit_console,
        provided_dtb,
        host_fdt_bytes,
    )?
    else {
        return Ok(());
    };

    let source = resolved.source;
    if source == serial::ConsoleSource::Supplied {
        validate_supplied_console_controller(vm_config, &resolved, interrupt_encoding)?;
    }
    let controller_phandle = vm_config
        .gic_profile()
        .and_then(|gic| gic.node_phandle)
        .or_else(|| vm_config.plic_profile().and_then(|plic| plic.node_phandle));
    let mut snapshot = resolved.snapshot;
    if source == serial::ConsoleSource::Supplied {
        serial::normalize_console_identity(
            &mut snapshot.identity,
            interrupt_encoding,
            controller_phandle,
        );
    }
    if snapshot.profile != vm_config.serial_profile() {
        info!(
            "VM[{}] virtual UART follows the firmware-selected UART: {:?}",
            vm_config.id(),
            snapshot.profile
        );
    }
    vm_config.replace_machine_serial(snapshot.profile, Some(snapshot.identity))?;
    Ok(())
}

/// Rejects a supplied DTB whose selected console names an interrupt controller
/// the prepared per-VM controller cannot serve.
fn validate_supplied_console_controller(
    vm_config: &AxVMConfig,
    resolved: &serial::ResolvedConsole,
    interrupt_encoding: GuestSerialFdtInterrupt,
) -> AxVmResult {
    match interrupt_encoding {
        GuestSerialFdtInterrupt::GicSpi => {
            let supplied = resolved.supplied_gic.as_ref().ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    "supplied guest DTB console has no usable GIC interrupt controller"
                )
            })?;
            let runtime = vm_config.gic_profile().ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    "per-VM AArch64 GIC is unavailable for the supplied console"
                )
            })?;
            if !gic_controller_layout_compatible(runtime, supplied) {
                return Err(ax_err_type!(
                    InvalidData,
                    "supplied guest DTB console GIC layout cannot be served by the per-VM GIC"
                ));
            }
        }
        GuestSerialFdtInterrupt::PlicSource => {
            let supplied = resolved.supplied_plic.as_ref().ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    "supplied guest DTB console has no usable PLIC interrupt controller"
                )
            })?;
            let runtime = vm_config.plic_profile().ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    "per-VM RISC-V PLIC is unavailable for the supplied console"
                )
            })?;
            if supplied.base != runtime.base || supplied.length != runtime.length {
                return Err(ax_err_type!(
                    InvalidData,
                    "supplied guest DTB console PLIC window cannot be served by the per-VM PLIC"
                ));
            }
        }
    }
    Ok(())
}

/// Returns whether the prepared per-VM GIC can serve a supplied console's GIC.
///
/// The node path and phandle are normalized later, so only the compatible model
/// and the runtime distributor/per-CPU geometry have to agree.
fn gic_controller_layout_compatible(runtime: &GuestGicProfile, supplied: &GuestGicProfile) -> bool {
    let cpu_region_matches = match (&runtime.cpu_region, &supplied.cpu_region) {
        (GuestGicCpuRegion::CpuInterface(runtime), GuestGicCpuRegion::CpuInterface(supplied)) => {
            runtime == supplied
        }
        (
            GuestGicCpuRegion::Redistributors(runtime),
            GuestGicCpuRegion::Redistributors(supplied),
        ) => runtime == supplied,
        _ => false,
    };
    runtime.compatible == supplied.compatible
        && runtime.distributor == supplied.distributor
        && cpu_region_matches
}

fn resolve_machine_resources_from_host(
    vm_config: &mut AxVMConfig,
    host_fdt_bytes: Option<&[u8]>,
) -> AxVmResult {
    let Some(host_fdt_bytes) = host_fdt_bytes else {
        return Ok(());
    };
    let host_fdt = fdt_edit::Fdt::from_bytes(host_fdt_bytes).map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse host FDT while resolving machine resources: {err:#?}")
        )
    })?;
    let machine = crate::machine::current_machine_profile(vm_config.phys_cpu_ls.cpu_num());
    if let Some(gic) = interrupt::host_gic_profile(&host_fdt)? {
        info!(
            "VM[{}] virtual GIC follows host firmware resources: {:?}",
            vm_config.id(),
            gic
        );
        vm_config.replace_machine_gic(gic)?;
    }
    if machine.timer.is_some() {
        let timer = timer::host_timer_profile(&host_fdt)?.ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "host FDT does not provide a valid arm,armv8-timer node"
            )
        })?;
        info!(
            "VM[{}] architectural timer follows host firmware PPIs",
            vm_config.id()
        );
        vm_config.replace_machine_timer(timer)?;
    }
    if let Some(plic) = interrupt::host_plic_profile(&host_fdt)? {
        info!(
            "VM[{}] virtual PLIC follows host firmware resources: {:?}",
            vm_config.id(),
            plic
        );
        vm_config.replace_machine_plic(plic)?;
    }
    Ok(())
}

pub(crate) fn selected_guest_fdt_policy() -> GuestFdtPolicy {
    super::guest_fdt_policy()
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn skip_guest_dtb(vm_config: &mut AxVMConfig, vm_create_config: &mut GuestConfig) {
    info!(
        "VM[{}] uses UEFI boot protocol, skipping guest DTB handling",
        vm_config.id()
    );
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn build_guest_dtb(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    provided_dtb: Option<Vec<u8>>,
    host_fdt_bytes: Option<&'static [u8]>,
) -> AxVmResult<Option<GuestDtbImage>> {
    match (host_fdt_bytes, provided_dtb) {
        (Some(host_bytes), Some(provided)) => {
            let host_fdt = parse_host_fdt(host_bytes)?;
            set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;
            info!("VM[{}] found DTB, parsing...", vm_config.id());
            reserve_excluded_device_ranges(vm_config, vm_create_config, &provided)?;
            update_provided_fdt(&provided, Some(host_bytes), vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (Some(host_bytes), None) => {
            let host_fdt = parse_host_fdt(host_bytes)?;
            set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;
            info!(
                "VM[{}] DTB not found, generating from the VM configuration",
                vm_config.id()
            );
            setup_guest_fdt_from_vmm(host_bytes, vm_config, vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (None, Some(provided)) => {
            info!("VM[{}] found DTB, parsing...", vm_config.id());
            reserve_excluded_device_ranges(vm_config, vm_create_config, &provided)?;
            update_provided_fdt(&provided, None, vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (None, None) => {
            warn!(
                "VM[{}] no guest DTB provided; continuing without generated DTB",
                vm_config.id()
            );
            Ok(None)
        }
    }
}

// Explicit guest firmware owns virtualized GIC and UART resources. Resolve
// them before reserving MMIO ranges and constructing the immutable device plan.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn select_guest_machine_resources(
    vm_config: &mut AxVMConfig,
    provided_dtb: Option<&[u8]>,
) -> AxVmResult {
    if let Some(current) = vm_config.gic_profile() {
        let gic = interrupt::select_guest_gic(
            current,
            provided_dtb,
            vm_config.uses_passthrough_address_space(),
        )?;
        vm_config.replace_machine_gic(gic)?;
    }
    let machine = crate::machine::current_machine_profile(vm_config.phys_cpu_ls.cpu_num());
    if let Some(interrupt_encoding) = machine.serial_fdt_interrupt
        && let Some(serial) = serial::select_guest_serial(
            vm_config.serial_profile(),
            provided_dtb,
            vm_config.uses_passthrough_address_space(),
            interrupt_encoding,
        )?
    {
        info!(
            "VM[{}] virtual UART follows explicit guest firmware: {:?}",
            vm_config.id(),
            serial.profile
        );
        vm_config.replace_machine_serial(
            serial.profile,
            Some(crate::machine::GuestSerialFirmwareIdentity::Fdt(
                serial.identity,
            )),
        )?;
    }
    Ok(())
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn parse_host_fdt(host_fdt_bytes: &'static [u8]) -> AxVmResult<fdt_edit::Fdt> {
    fdt_edit::Fdt::from_bytes(host_fdt_bytes)
        .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {err:#?}")))
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn enrich_guest_config(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    guest_dtb: Option<&GuestDtbImage>,
) -> AxVmResult {
    let Some(dtb) = guest_dtb.map(GuestDtbImage::as_bytes) else {
        clear_unresolved_dtb_config(vm_config, vm_create_config);
        return Ok(());
    };

    parse_reserved_memory_regions(vm_create_config, dtb)?;
    // Interrupt discovery needs the original FDT selectors. Address resolution
    // replaces them with MMIO mappings whose names need not be node paths.
    parse_vm_interrupt(vm_config, vm_create_config, dtb)?;
    parse_passthrough_devices_address(vm_config, vm_create_config, dtb)
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn clear_unresolved_dtb_config(vm_config: &mut AxVMConfig, vm_create_config: &mut GuestConfig) {
    error!(
        "VM[{}] DTB not found in memory, skipping...",
        vm_config.id()
    );
    let unresolved_devices = vm_config
        .pass_through_devices()
        .iter()
        .filter(|device| device.length == 0)
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved_devices.is_empty() {
        warn!(
            "VM[{}] clearing {} unresolved passthrough discovery device(s)",
            vm_config.id(),
            unresolved_devices.len()
        );
        for device in unresolved_devices {
            vm_config.remove_pass_through_device(device);
        }
    }
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn get_developer_provided_dtb(
    _vm_config: &AxVMConfig,
    config: &GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<Vec<u8>>> {
    config
        .kernel
        .dtb_path
        .as_deref()
        .map(|path| provider.read_file(path))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::{
        gic_controller_layout_compatible, resolve_console_profile,
        resolve_machine_resources_from_host,
    };
    use crate::{
        config::{AddressSpacePolicy, AxVMConfig, AxVMConfigParams},
        machine::{
            GuestGicCpuRegion, GuestGicProfile, GuestGicRedistributorProfile, GuestMmioRegion,
        },
    };

    fn gic_profile(distributor: (usize, usize), redistributor: (usize, usize)) -> GuestGicProfile {
        GuestGicProfile {
            compatible: "arm,gic-v3".into(),
            node_path: "/interrupt-controller@fe600000".into(),
            node_phandle: Some(1),
            distributor: GuestMmioRegion {
                base: distributor.0,
                length: distributor.1,
            },
            cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                regions: vec![GuestMmioRegion {
                    base: redistributor.0,
                    length: redistributor.1,
                }],
                stride: 0x2_0000,
            }),
            its: Vec::new(),
        }
    }

    #[test]
    fn virtualized_guest_without_host_debug_uart_keeps_machine_profile() {
        let mut config = AxVMConfig::new(AxVMConfigParams {
            address_space_policy: AddressSpacePolicy::Virtualized,
            ..Default::default()
        });
        let original = config.serial_profile();
        resolve_machine_resources_from_host(&mut config, None).unwrap();
        resolve_console_profile(&mut config, None, None).unwrap();
        assert_eq!(config.serial_profile(), original);
    }

    #[test]
    fn supplied_console_gic_layout_must_be_served_by_the_per_vm_controller() {
        let runtime = gic_profile((0xfe60_0000, 0x1_0000), (0xfe68_0000, 0x10_0000));

        // Path and phandle differences are normalized later and must not fail.
        let mut renamed = gic_profile((0xfe60_0000, 0x1_0000), (0xfe68_0000, 0x10_0000));
        renamed.node_path = "/soc/interrupt-controller@fe600000".into();
        renamed.node_phandle = Some(0x99);
        assert!(gic_controller_layout_compatible(&runtime, &renamed));

        // A QEMU-style GIC window cannot be served by the RK3588 per-VM GIC.
        let mismatched = gic_profile((0x0800_0000, 0x1_0000), (0x080a_0000, 0xf6_0000));
        assert!(!gic_controller_layout_compatible(&runtime, &mismatched));
    }
}
