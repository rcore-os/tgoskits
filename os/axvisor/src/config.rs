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

#[cfg(all(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
)))]
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use axvm::{AxVmError, AxVmResult};
use axvm::{boot::*, config::*, *};
use axvmconfig::{GuestConfig, GuestType, HostDeviceAssignment};

#[cfg(all(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
)))]
static HOST_FILESYSTEM_RELEASE_REQUIRED: AtomicBool = AtomicBool::new(false);

pub fn init_guest_vms() -> Result<()> {
    init_guest_boot_resources();
    let context = ax_fs_ng::current_fs_context();
    let configs = axvisor::builtin::selected_configs(&context.lock())?;
    for raw in configs {
        init_guest_vm(&raw)?;
    }
    Ok(())
}

pub(crate) fn prepare_guest_vm(raw_cfg: &str) -> Result<VmCreatePlan> {
    let image_provider = AxvisorBootImageProvider;
    let vm_create_config =
        GuestConfig::from_toml(raw_cfg).context("parse VM TOML configuration")?;
    let configured_vm_id = vm_create_config.base.id;

    if let Some(path) = crate::guest_images::missing_guest_image(&vm_create_config) {
        bail!("guest image `{path}` does not exist");
    }

    if let Some(linux) = get_image_header(&vm_create_config, &image_provider) {
        debug!(
            "VM[{}] Linux header: {:#x?}",
            vm_create_config.base.id, linux
        );
    }

    let mut vm_config = build_axvm_config(&vm_create_config)?;
    let prepared_boot = prepare_guest_boot(&mut vm_config, vm_create_config, &image_provider)
        .with_context(|| format!("prepare boot resources for VM[{configured_vm_id}]"))?;
    let prepared_config = prepared_boot.config();
    sync_axvm_config_from_crate_config(&mut vm_config, prepared_config);
    vm_config.set_boot_policy(guest_boot_policy(prepared_config, &image_provider));

    Ok(VmCreatePlan {
        config: vm_config,
        boot: prepared_boot,
        images: alloc::sync::Arc::new(image_provider),
        vcpu_schedule_policy: {
            #[cfg(feature = "bench-fifo-vcpu-policy")]
            {
                let priority = axvm::RtPriority::new(80)
                    .expect("benchmark vCPU FIFO priority must be a valid real-time priority");
                axvm::SchedulePolicy::fifo(priority)
            }
            #[cfg(not(feature = "bench-fifo-vcpu-policy"))]
            {
                axvm::SchedulePolicy::default()
            }
        },
    })
}

pub fn init_guest_vm(raw_cfg: &str) -> Result<usize> {
    let vm_id = GuestConfig::from_toml(raw_cfg)
        .context("parse VM TOML configuration")?
        .base
        .id;

    #[cfg(all(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
    let release_host_filesystem = vm_config_needs_host_filesystem_release(
        &GuestConfig::from_toml(raw_cfg).context("parse VM TOML configuration")?,
    );

    crate::manager::manager()
        .create_vm_from_toml_and_wait(raw_cfg)
        .with_context(|| format!("create VM[{vm_id}]"))?;

    #[cfg(all(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
    if release_host_filesystem {
        HOST_FILESYSTEM_RELEASE_REQUIRED.store(true, Ordering::Release);
    }

    Ok(vm_id)
}

pub(crate) fn build_axvm_config(cfg: &GuestConfig) -> Result<AxVMConfig> {
    let machine = axvm::machine::current_machine_profile(cfg.base.cpu_num);
    let serial_profile = machine.serial;
    let mut passthrough_devices = cfg.devices.unresolved_host_devices();
    if cfg.base.guest_type == GuestType::Passthrough
        && passthrough_devices.is_empty()
        && let Some(path) = machine.default_passthrough_device_path
    {
        passthrough_devices.insert(
            0,
            HostDeviceAssignment {
                name: path.into(),
                ..Default::default()
            },
        );
    }
    let mut virtual_device_catalog = axvm::ConfiguredDeviceCatalog::new();
    axvm::machine::register_devices(&mut virtual_device_catalog)
        .context("register AxVM virtual-device models")?;
    Ok(AxVMConfig::new(AxVMConfigParams {
        id: cfg.base.id,
        name: cfg.base.name.clone(),
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
            loaded_from_filesystem: true,
            bios_load_gpa: boot_firmware_load_gpa(cfg),
            dtb_load_gpa: cfg.kernel.dtb_load_addr.map(GuestPhysAddr::from),
            ramdisk: cfg.kernel.ramdisk_load_addr.map(|addr| RamdiskInfo {
                load_gpa: GuestPhysAddr::from(addr),
                size: None,
            }),
        },
        pass_through_devices: passthrough_devices,
        excluded_devices: cfg.devices.disabled_device_paths(),
        pass_through_addresses: Vec::new(),
        reserved_address_ranges: Vec::new(),
        pass_through_ports: Vec::new(),
        address_space_policy: cfg.base.guest_type.address_space_policy(),
        memory_regions: cfg.kernel.memory_regions.clone(),
        boot_policy: GuestBootPolicy::KeepConfigured,
        serial_profile: Some(serial_profile),
        serial_backend_factory: Some(crate::guest_console::serial_backend_factory(cfg.base.id)),
        virtual_device_requests: cfg.devices.virtual_device_requests().to_vec(),
        virtual_device_catalog: alloc::sync::Arc::new(virtual_device_catalog),
    }))
}

fn sync_axvm_config_from_crate_config(vm_config: &mut AxVMConfig, cfg: &GuestConfig) {
    vm_config.set_memory_regions(cfg.kernel.memory_regions.clone());
}

#[cfg(all(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
)))]
fn vm_config_needs_host_filesystem_release(config: &GuestConfig) -> bool {
    (ax_fs_ng::root::root_kind() == Some(ax_fs_ng::root::RootKind::Block)
        && config.base.guest_type == GuestType::Passthrough)
        || !config.devices.passthrough.is_empty()
}

#[cfg(all(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
)))]
pub fn host_filesystem_release_required() -> bool {
    HOST_FILESYSTEM_RELEASE_REQUIRED.load(Ordering::Acquire)
}

struct AxvisorBootImageProvider;

impl BootImageProvider for AxvisorBootImageProvider {
    fn read_file(&self, file_name: &str) -> AxVmResult<alloc::vec::Vec<u8>> {
        crate::manager::AxvmManager::read_file(file_name)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    fn read_file_exact(
        &self,
        file_name: &str,
        read_size: usize,
    ) -> AxVmResult<alloc::vec::Vec<u8>> {
        crate::manager::AxvmManager::read_file_exact(file_name, read_size)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    fn file_size(&self, file_name: &str) -> AxVmResult<usize> {
        crate::manager::AxvmManager::file_size(file_name)
            .map_err(|error| boot_file_error("inspect guest image file", file_name, error))
    }
}

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
        let mut crate_config = GuestConfig::default();
        crate_config.kernel.memory_regions.push(memory_region(
            0x8000_0000,
            0x200000,
            VmMemMappingType::MapIdentical,
        ));
        let mut vm_config = build_axvm_config(&crate_config).unwrap();

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
