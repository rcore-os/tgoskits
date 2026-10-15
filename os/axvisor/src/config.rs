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

extern crate alloc;

use alloc::sync::Arc;

use anyhow::{Context, Result, bail};
use axvm::{AxVmError, AxVmResult};
use axvm::{boot::*, config::*, *};
use axvmconfig::{GuestConfig, GuestType, HostDeviceAssignment};

pub fn load_builtin_catalog() -> Result<axvisor::builtin::BuiltinCatalog> {
    init_guest_boot_resources();
    let context = ax_fs_ng::current_fs_context();
    axvisor::builtin::load_catalog(&context.lock())
}

pub(crate) struct PreparedGuestVm {
    pub plan: VmCreatePlan,
    pub release_host_filesystem: bool,
}

pub(crate) fn prepare_guest_vm(
    raw_cfg: &str,
    backend: Arc<dyn axvm::GuestCpuFreqBackend>,
) -> Result<PreparedGuestVm> {
    let image_provider = AxvisorBootImageProvider;
    let mut vm_create_config =
        GuestConfig::from_toml(raw_cfg).context("parse VM TOML configuration")?;
    materialize_guest_cpufreq_tables(&mut vm_create_config, backend.as_ref())?;
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
    validate_guest_cpufreq_placement(&vm_config, backend.as_ref())?;
    let release_host_filesystem = vm_config_needs_host_filesystem_release(&vm_create_config);
    let prepared_boot = prepare_guest_boot(&mut vm_config, vm_create_config, &image_provider)
        .with_context(|| format!("prepare boot resources for VM[{configured_vm_id}]"))?;
    let prepared_config = prepared_boot.config();
    sync_axvm_config_from_crate_config(&mut vm_config, prepared_config);
    vm_config.set_boot_policy(guest_boot_policy(prepared_config, &image_provider));

    Ok(PreparedGuestVm {
        plan: VmCreatePlan {
            config: vm_config,
            boot: prepared_boot,
            images: alloc::sync::Arc::new(image_provider),
            vcpu_schedule_policy: axvm::SchedulePolicy::default(),
            cpu_freq_backend: Some(backend),
        },
        release_host_filesystem,
    })
}

fn materialize_guest_cpufreq_tables(
    config: &mut GuestConfig,
    backend: &dyn axvm::GuestCpuFreqBackend,
) -> Result<()> {
    for request in config
        .devices
        .virtual_devices
        .iter_mut()
        .filter(|request| request.model == axvm::VIRTUAL_CPUFREQ_MODEL)
    {
        let options = axvm::VirtualCpuFreqOptions::from_request(request)
            .map_err(|error| anyhow::anyhow!("parse virtual-cpufreq options: {error}"))?;
        let domain = options.domain;
        let table = backend
            .performance_table(domain)
            .map_err(|error| anyhow::anyhow!("materialize virtual-cpufreq OPP table: {error}"))?;
        axvm::VirtualCpuFreqOptions::replace_performance_table(request, table)
            .map_err(|error| anyhow::anyhow!("materialize virtual-cpufreq OPP table: {error}"))?;
    }
    Ok(())
}

fn validate_guest_cpufreq_placement(
    vm_config: &AxVMConfig,
    backend: &dyn axvm::GuestCpuFreqBackend,
) -> Result<()> {
    let requests = vm_config
        .virtual_device_requests()
        .iter()
        .filter(|request| request.model == axvm::VIRTUAL_CPUFREQ_MODEL)
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return Ok(());
    }
    if requests.len() != 1 {
        bail!(
            "virtual-cpufreq permits exactly one device per VM, found {}",
            requests.len()
        );
    }
    let request = requests[0];
    let options = axvm::VirtualCpuFreqOptions::from_request(request)
        .map_err(|error| anyhow::anyhow!("parse virtual-cpufreq options: {error}"))?;
    let domain = options.domain;
    let vcpu_count = usize::try_from(options.vcpu_count).expect("validated vcpu_count");
    let placements = vm_config.phys_cpu_ls().get_vcpu_affinities_pcpu_ids();
    if vcpu_count != placements.len() || vcpu_count != vm_config.phys_cpu_ls().cpu_num() {
        bail!("virtual-cpufreq vcpu_count={vcpu_count} must cover every configured vCPU");
    }
    let mut host_cpus = Vec::with_capacity(placements.len());
    for (vcpu, affinity, _) in placements {
        let mask = affinity.ok_or_else(|| {
            anyhow::anyhow!(
                "virtual-cpufreq requires an explicit single-CPU affinity for vCPU {vcpu}"
            )
        })?;
        if mask == 0 || !mask.is_power_of_two() {
            bail!("virtual-cpufreq vCPU {vcpu} affinity {mask:#x} crosses CPU frequency domains");
        }
        host_cpus.push(mask.trailing_zeros() as usize);
    }
    backend
        .validate_placement(domain, &host_cpus)
        .map_err(|error| anyhow::anyhow!("validate virtual-cpufreq host placement: {error}"))?;
    Ok(())
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

fn vm_config_needs_host_filesystem_release(config: &GuestConfig) -> bool {
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    ))]
    {
        return (ax_fs_ng::root::root_kind() == Some(ax_fs_ng::root::RootKind::Block)
            && config.base.guest_type == GuestType::Passthrough)
            || !config.devices.passthrough.is_empty();
    }
    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
    {
        let _ = config;
        false
    }
}

struct AxvisorBootImageProvider;

impl BootImageProvider for AxvisorBootImageProvider {
    fn read_file(&self, file_name: &str) -> AxVmResult<alloc::vec::Vec<u8>> {
        if let Some(bytes) = axvisor::builtin::read_boot_asset(file_name)
            .map_err(|error| boot_file_error("read retained builtin asset", file_name, error))?
        {
            return Ok(bytes);
        }
        crate::manager::AxvmManager::read_file(file_name)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    fn read_file_exact(
        &self,
        file_name: &str,
        read_size: usize,
    ) -> AxVmResult<alloc::vec::Vec<u8>> {
        if let Some(bytes) = axvisor::builtin::read_boot_asset_exact(file_name, read_size)
            .map_err(|error| boot_file_error("read retained builtin asset", file_name, error))?
        {
            return Ok(bytes);
        }
        crate::manager::AxvmManager::read_file_exact(file_name, read_size)
            .map_err(|error| boot_file_error("read guest image file", file_name, error))
    }

    fn file_size(&self, file_name: &str) -> AxVmResult<usize> {
        if let Some(size) = axvisor::builtin::boot_asset_size(file_name)
            .map_err(|error| boot_file_error("inspect retained builtin asset", file_name, error))?
        {
            return Ok(size);
        }
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
