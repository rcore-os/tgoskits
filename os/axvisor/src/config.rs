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

#[cfg(feature = "fs")]
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
#[cfg(all(feature = "fs", target_arch = "x86_64"))]
use axvm::InterruptTriggerMode;
use axvm::{
    AxVM, GuestPhysAddr,
    boot::{
        BootImageProvider, StaticVmImage, boot_firmware_load_gpa, get_image_header,
        guest_boot_policy, init_guest_boot_resources, prepare_guest_boot,
    },
    config::{
        AxVCpuConfig, AxVMConfig, AxVMConfigParams, GuestBootPolicy, PhysCpuList, RamdiskInfo,
        VMImageConfig,
    },
};
#[cfg(feature = "fs")]
use axvm::{AxVmError, AxVmResult};
use axvmconfig::{AxVMCrateConfig, VMType};

#[cfg(feature = "fs")]
static HOST_FILESYSTEM_RELEASE_REQUIRED: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub mod vmcfg {
    use alloc::{string::String, vec, vec::Vec};

    /// Default static VM configs. Used when no VM config is provided.
    pub fn default_static_vm_configs() -> Vec<&'static str> {
        vec![]
    }

    /// Read VM configs from filesystem
    #[cfg(feature = "fs")]
    pub fn filesystem_vm_configs() -> Vec<String> {
        let config_dir = "/guest/vm_default";
        crate::manager::AxvmManager::filesystem_vm_configs(config_dir)
            .into_iter()
            .filter_map(
                |content| match axvmconfig::AxVMCrateConfig::from_toml(&content) {
                    Ok(_) => Some(content),
                    Err(e) => {
                        warn!("Filesystem VM config is invalid: {:?}", e);
                        None
                    }
                },
            )
            .collect()
    }

    /// Fallback function for when "fs" feature is not enabled
    #[cfg(not(feature = "fs"))]
    pub fn filesystem_vm_configs() -> Vec<String> {
        Vec::new()
    }

    include!(concat!(env!("OUT_DIR"), "/vm_configs.rs"));
}

pub fn init_guest_vms() {
    init_guest_boot_resources();

    // First try to get configs from filesystem if fs feature is enabled
    let mut gvm_raw_configs = vmcfg::filesystem_vm_configs();

    // If no filesystem configs found, fallback to static configs
    if gvm_raw_configs.is_empty() {
        let static_configs = vmcfg::static_vm_configs();
        if static_configs.is_empty() {
            info!("Static VM configs are empty.");
            info!("Now axvisor will entry the shell...");
        } else {
            info!("Using static VM configs.");
        }
        // Convert static configs to String type
        gvm_raw_configs.extend(static_configs.into_iter().map(|s| s.into()));
    }

    for raw_cfg_str in gvm_raw_configs {
        debug!("Initializing guest VM with config: {:#?}", raw_cfg_str);
        if let Err(e) = init_guest_vm(&raw_cfg_str) {
            error!("Failed to initialize guest VM: {e:#}");
        }
    }
}

pub fn init_guest_vm(raw_cfg: &str) -> Result<usize> {
    let image_provider = AxvisorBootImageProvider;
    let vm_create_config =
        AxVMCrateConfig::from_toml(raw_cfg).context("parse VM TOML configuration")?;
    let configured_vm_id = vm_create_config.base.id;

    #[cfg(feature = "fs")]
    let release_host_filesystem = vm_config_needs_host_filesystem_release(&vm_create_config);

    if let Some(linux) = get_image_header(&vm_create_config, &image_provider) {
        debug!(
            "VM[{}] Linux header: {:#x?}",
            vm_create_config.base.id, linux
        );
    }

    let mut vm_config = build_axvm_config(&vm_create_config);
    let prepared_boot = prepare_guest_boot(&mut vm_config, vm_create_config, &image_provider)
        .with_context(|| format!("prepare boot resources for VM[{configured_vm_id}]"))?;
    let prepared_config = prepared_boot.config();

    sync_axvm_config_from_crate_config(&mut vm_config, prepared_config);

    vm_config.set_boot_policy(guest_boot_policy(prepared_config, &image_provider));

    // info!("after parse_vm_interrupt, crate VM[{}] with config: {:#?}", vm_config.id(), vm_config);
    info!("Creating VM[{}] {:?}", vm_config.id(), vm_config.name());

    // Create VM.
    let vm = AxVM::new(vm_config).with_context(|| format!("create VM[{configured_vm_id}]"))?;
    let vm_id = vm.id();

    let memory_layout = vm
        .prepare_memory_layout()
        .with_context(|| format!("prepare memory layout for VM[{vm_id}]"))?;
    let main_mem = memory_layout.main_memory().clone();

    // Load corresponding images for VM.
    info!("VM[{}] created success, loading images...", vm.id());

    prepared_boot
        .load_images(main_mem, vm.clone(), &image_provider)
        .with_context(|| format!("load boot images for VM[{vm_id}]"))?;

    vm.prepare()
        .with_context(|| format!("prepare devices and vCPUs for VM[{vm_id}]"))?;

    if !axvm::register_vm(vm) {
        bail!("register VM[{vm_id}]: a VM with this ID already exists");
    }
    #[cfg(target_arch = "loongarch64")]
    crate::manager::register_loongarch_passthrough_irq_routes(vm_id);

    #[cfg(feature = "fs")]
    if release_host_filesystem {
        #[cfg(target_arch = "x86_64")]
        register_x86_host_fs_passthrough_irq_route();
        HOST_FILESYSTEM_RELEASE_REQUIRED.store(true, Ordering::Release);
    }

    Ok(vm_id)
}

pub(crate) fn build_axvm_config(cfg: &AxVMCrateConfig) -> AxVMConfig {
    AxVMConfig::new(AxVMConfigParams {
        id: cfg.base.id,
        name: cfg.base.name.clone(),
        vm_type: VMType::from(cfg.base.vm_type),
        phys_cpu_ls: PhysCpuList::new(
            cfg.base.cpu_num,
            cfg.base.phys_cpu_ids.clone(),
            cfg.base.phys_cpu_sets.clone(),
        ),
        cpu_config: AxVCpuConfig {
            bsp_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
            ap_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
        },
        image_config: VMImageConfig {
            kernel_load_gpa: GuestPhysAddr::from(cfg.kernel.kernel_load_addr),
            loaded_from_filesystem: cfg.kernel.image_location.as_deref() == Some("fs"),
            bios_load_gpa: boot_firmware_load_gpa(cfg),
            dtb_load_gpa: cfg.kernel.dtb_load_addr.map(GuestPhysAddr::from),
            ramdisk: cfg.kernel.ramdisk_load_addr.map(|addr| RamdiskInfo {
                load_gpa: GuestPhysAddr::from(addr),
                size: None,
            }),
        },
        emu_devices: cfg.devices.emu_devices.clone(),
        pass_through_devices: cfg.devices.passthrough_devices.clone(),
        excluded_devices: cfg.devices.excluded_devices.clone(),
        pass_through_addresses: cfg.devices.passthrough_addresses.clone(),
        reserved_address_ranges: Vec::new(),
        pass_through_ports: cfg.devices.passthrough_ports.clone(),
        address_space_policy: cfg.devices.address_space_policy,
        memory_regions: cfg.kernel.memory_regions.clone(),
        boot_policy: GuestBootPolicy::KeepConfigured,
        host_reserved_intids: cfg.devices.host_reserved_intids.clone(),
        interrupt_mode: cfg.devices.interrupt_mode,
    })
}

fn sync_axvm_config_from_crate_config(vm_config: &mut AxVMConfig, cfg: &AxVMCrateConfig) {
    vm_config.set_memory_regions(cfg.kernel.memory_regions.clone());
}

#[cfg(feature = "fs")]
fn vm_config_needs_host_filesystem_release(config: &AxVMCrateConfig) -> bool {
    config.kernel.image_location.as_deref() == Some("fs")
        && (!config.devices.passthrough_devices.is_empty()
            || !config.devices.passthrough_addresses.is_empty()
            || !config.devices.passthrough_ports.is_empty())
}

#[cfg(feature = "fs")]
pub fn host_filesystem_release_required() -> bool {
    HOST_FILESYSTEM_RELEASE_REQUIRED.load(Ordering::Acquire)
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn register_x86_host_fs_passthrough_irq_route() {
    let (_, _, _, guest_gsi) = axvm::boot::x86_qemu_passthrough_block_intx();
    let info = x86_host_fs_passthrough_pci_info();

    let route = match ax_driver::pci::resolve_intx_binding(info) {
        Ok(Some(binding)) => {
            let trigger = x86_intx_forwarding_trigger(&binding);
            resolve_binding_irq(binding).map(|host_irq| (host_irq, trigger))
        }
        Ok(None) => {
            warn!("x86 host filesystem passthrough PCI INTx route was not found for {info:?}");
            return;
        }
        Err(err) => {
            warn!("failed to resolve x86 host filesystem passthrough PCI INTx route: {err:?}");
            return;
        }
    };

    match route {
        Ok((host_irq, trigger)) => {
            axvm::register_x86_ioapic_irq_forwarding_route_with_trigger(
                guest_gsi, host_irq, trigger,
            );
            axvm::register_x86_ioapic_irq_forwarding_activator(
                guest_gsi,
                unmask_x86_host_fs_passthrough_intx,
            );
            info!(
                "Registered x86 host filesystem PCI INTx forwarding route: guest GSI \
                 {guest_gsi} <- host IRQ {host_irq:?}, trigger {trigger:?}"
            );
        }
        Err(err) => {
            warn!(
                "failed to resolve x86 host filesystem passthrough IRQ source into host IRQ: \
                 {err:?}"
            );
        }
    }
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
pub(crate) fn prepare_x86_host_fs_passthrough_devices() {
    let info = x86_host_fs_passthrough_pci_info();
    match ax_driver::pci::prepare_intx_passthrough(info) {
        Ok(()) => {
            info!("Prepared x86 host filesystem PCI INTx passthrough device {info:?}");
        }
        Err(err) => {
            warn!("failed to prepare x86 host filesystem PCI INTx passthrough device: {err:?}");
        }
    }
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn unmask_x86_host_fs_passthrough_intx() {
    let info = x86_host_fs_passthrough_pci_info();
    match ax_driver::pci::unmask_intx_passthrough(info) {
        Ok(()) => {
            info!("Unmasked x86 host filesystem PCI INTx passthrough device {info:?}");
        }
        Err(err) => {
            warn!("failed to unmask x86 host filesystem PCI INTx passthrough device: {err:?}");
        }
    }
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn x86_host_fs_passthrough_pci_info() -> ax_driver::probe::pci::PciInfo {
    use ax_driver::probe::pci::{PciAddress, PciInfo, PciIntxRoute};

    let (device, function, pin, _) = axvm::boot::x86_qemu_passthrough_block_intx();
    PciInfo {
        address: PciAddress::new(0, 0, device, function),
        interrupt_pin: pin,
        interrupt_line: 0,
        intx_route: Some(PciIntxRoute {
            root_device: device,
            root_function: function,
            root_pin: pin,
        }),
    }
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn resolve_binding_irq(
    binding: ax_driver::BindingIrq,
) -> Result<ax_hal::irq::IrqId, ax_hal::irq::IrqError> {
    use ax_hal::irq;

    match binding {
        ax_driver::BindingIrq::Id(irq) => Ok(irq),
        ax_driver::BindingIrq::Source(source) => match source {
            ax_driver::BindingIrqSource::AcpiGsi(gsi) => {
                irq::resolve_irq_source(irq::IrqSource::AcpiGsi(gsi))
            }
            ax_driver::BindingIrqSource::AcpiGsiRoute(route) => {
                irq::resolve_irq_source(irq::IrqSource::AcpiGsiRoute(route))
            }
            ax_driver::BindingIrqSource::FdtInterrupt(_) => Err(irq::IrqError::Unsupported),
        },
    }
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn x86_intx_forwarding_trigger(binding: &ax_driver::BindingIrq) -> InterruptTriggerMode {
    match binding {
        ax_driver::BindingIrq::Source(ax_driver::BindingIrqSource::AcpiGsiRoute(route)) => {
            match route.trigger {
                ax_hal::irq::AcpiIrqTrigger::Edge => InterruptTriggerMode::EdgeTriggered,
                ax_hal::irq::AcpiIrqTrigger::Level => InterruptTriggerMode::LevelTriggered,
            }
        }
        _ => InterruptTriggerMode::LevelTriggered,
    }
}

struct AxvisorBootImageProvider;

impl BootImageProvider for AxvisorBootImageProvider {
    fn static_vm_images(&self) -> &'static [StaticVmImage] {
        vmcfg::get_memory_images()
    }

    #[cfg(target_arch = "loongarch64")]
    fn static_firmware_images(&self) -> &'static [StaticVmImage] {
        vmcfg::get_firmware_images()
    }

    #[cfg(feature = "fs")]
    fn read_file(&self, file_name: &str) -> AxVmResult<alloc::vec::Vec<u8>> {
        crate::manager::AxvmManager::read_file(file_name)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    #[cfg(feature = "fs")]
    fn read_file_exact(
        &self,
        file_name: &str,
        read_size: usize,
    ) -> AxVmResult<alloc::vec::Vec<u8>> {
        crate::manager::AxvmManager::read_file_exact(file_name, read_size)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    #[cfg(feature = "fs")]
    fn file_size(&self, file_name: &str) -> AxVmResult<usize> {
        crate::manager::AxvmManager::file_size(file_name)
            .map_err(|error| boot_file_error("inspect guest image file", file_name, error))
    }
}

#[cfg(feature = "fs")]
fn boot_file_error(operation: &'static str, file_name: &str, error: anyhow::Error) -> AxVmError {
    AxVmError::Boot {
        operation,
        detail: format!("`{file_name}`: {error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axvmconfig::{VmMemConfig, VmMemMappingType};

    fn memory_region(gpa: usize, size: usize, map_type: VmMemMappingType) -> VmMemConfig {
        VmMemConfig {
            gpa,
            size,
            flags: 0x7,
            map_type,
        }
    }

    #[test]
    fn sync_axvm_config_keeps_fdt_reserved_memory_regions() {
        let mut crate_config = AxVMCrateConfig::default();
        crate_config.kernel.memory_regions.push(memory_region(
            0x8000_0000,
            0x200000,
            VmMemMappingType::MapIdentical,
        ));
        let mut vm_config = build_axvm_config(&crate_config);

        crate_config.kernel.memory_regions.push(memory_region(
            0x110000,
            0x10000,
            VmMemMappingType::MapReserved,
        ));
        assert_eq!(vm_config.memory_regions().len(), 1);

        sync_axvm_config_from_crate_config(&mut vm_config, &crate_config);

        let regions = vm_config.memory_regions();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[1].gpa, 0x110000);
        assert_eq!(regions[1].size, 0x10000);
        assert_eq!(regions[1].map_type, VmMemMappingType::MapReserved);
    }
}
