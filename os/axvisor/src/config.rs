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

#[cfg(all(
    feature = "fs",
    any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )
))]
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::collections::BTreeMap;
use anyhow::{Context, Result, bail};
#[cfg(feature = "fs")]
use axvm::{AxVmError, AxVmResult};
use axvm::{boot::*, config::*, *};
use axvmconfig::{GuestConfig, GuestType, HostDeviceAssignment};

#[cfg(all(
    feature = "fs",
    any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )
))]
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
                |content| match axvmconfig::GuestConfig::from_toml(&content) {
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

    let parsed_configs = gvm_raw_configs
        .iter()
        .map(|raw| GuestConfig::from_toml(raw).context("parse VM TOML configuration"))
        .collect::<Result<alloc::vec::Vec<_>>>();
    let parsed_configs = match parsed_configs {
        Ok(configs) => configs,
        Err(error) => {
            error!("Refusing to create the default VM set: {error:#}");
            return;
        }
    };
    let dedicated_mask = ax_std::os::arceos::modules::ax_runtime::dedicated_cpu_mask();
    let host_cpu_count = ax_std::os::arceos::modules::ax_runtime::hal::cpu_num();
    if let Err(error) =
        validate_dedicated_cpu_ownership(&parsed_configs, dedicated_mask, host_cpu_count)
    {
        error!("Refusing to create the default VM set: {error:#}");
        return;
    }

    for raw_cfg_str in gvm_raw_configs {
        debug!("Initializing guest VM with config: {:#?}", raw_cfg_str);
        if let Err(e) = init_guest_vm(&raw_cfg_str) {
            error!("Failed to initialize guest VM: {e:#}");
        }
    }
}

fn validate_dedicated_cpu_ownership(
    configs: &[GuestConfig],
    dedicated_mask: usize,
    host_cpu_count: usize,
) -> Result<()> {
    validate_dedicated_cpu_ownership_with_resolver(
        configs,
        dedicated_mask,
        host_cpu_count,
        ax_std::os::arceos::modules::ax_hal::topology::resolve_cpu_index,
    )
}

fn validate_dedicated_cpu_ownership_with_resolver(
    configs: &[GuestConfig],
    dedicated_mask: usize,
    host_cpu_count: usize,
    mut resolve_cpu_index: impl FnMut(usize) -> Option<usize>,
) -> Result<()> {
    let mut owners = BTreeMap::<usize, (usize, usize)>::new();

    for config in configs {
        if let Some(phys_cpu_ids) = config.base.phys_cpu_ids.as_deref() {
            if phys_cpu_ids.len() != config.base.cpu_num {
                bail!(
                    "VM[{}] has {} vCPUs but {} phys_cpu_ids entries",
                    config.base.id,
                    config.base.cpu_num,
                    phys_cpu_ids.len()
                );
            }
            for (vcpu_id, &cpu_id) in phys_cpu_ids.iter().enumerate() {
                record_dedicated_hardware_cpu_owner(
                    &mut owners,
                    dedicated_mask,
                    host_cpu_count,
                    config.base.id,
                    vcpu_id,
                    cpu_id,
                    &mut resolve_cpu_index,
                )?;
            }
            continue;
        }

        if let Some(phys_cpu_sets) = config.base.phys_cpu_sets.as_deref() {
            if phys_cpu_sets.len() != config.base.cpu_num {
                bail!(
                    "VM[{}] has {} vCPUs but {} phys_cpu_sets entries",
                    config.base.id,
                    config.base.cpu_num,
                    phys_cpu_sets.len()
                );
            }
            for (vcpu_id, &cpu_set) in phys_cpu_sets.iter().enumerate() {
                let dedicated_candidates = cpu_set & dedicated_mask;
                if dedicated_candidates == 0 {
                    continue;
                }
                if cpu_set.count_ones() != 1 {
                    bail!(
                        "VM[{}] vCPU{} affinity {cpu_set:#b} mixes a dedicated CPU with other CPUs",
                        config.base.id,
                        vcpu_id
                    );
                }
                record_dedicated_logical_cpu_owner(
                    &mut owners,
                    dedicated_mask,
                    host_cpu_count,
                    config.base.id,
                    vcpu_id,
                    cpu_set.trailing_zeros() as usize,
                )?;
            }
            continue;
        }

        for vcpu_id in 0..config.base.cpu_num {
            // With no explicit placement, AxVM uses the dense vCPU index as
            // the host logical CPU index. This is already in mask space and
            // must not be translated as a hardware ID.
            record_dedicated_logical_cpu_owner(
                &mut owners,
                dedicated_mask,
                host_cpu_count,
                config.base.id,
                vcpu_id,
                vcpu_id,
            )?;
        }
    }

    Ok(())
}

fn record_dedicated_hardware_cpu_owner(
    owners: &mut BTreeMap<usize, (usize, usize)>,
    dedicated_mask: usize,
    host_cpu_count: usize,
    vm_id: usize,
    vcpu_id: usize,
    cpu_id: usize,
    resolve_cpu_index: &mut impl FnMut(usize) -> Option<usize>,
) -> Result<()> {
    let logical_cpu_id = resolve_cpu_index(cpu_id).ok_or_else(|| {
        anyhow::anyhow!("VM[{vm_id}] vCPU{vcpu_id} targets unknown hardware CPU ID {cpu_id:#x}")
    })?;
    if logical_cpu_id >= host_cpu_count {
        bail!(
            "VM[{vm_id}] vCPU{vcpu_id} targets offline hardware CPU ID {cpu_id:#x} \
             (logical CPU {logical_cpu_id})"
        );
    }

    record_dedicated_logical_cpu_owner(
        owners,
        dedicated_mask,
        host_cpu_count,
        vm_id,
        vcpu_id,
        logical_cpu_id,
    )
}

fn record_dedicated_logical_cpu_owner(
    owners: &mut BTreeMap<usize, (usize, usize)>,
    dedicated_mask: usize,
    host_cpu_count: usize,
    vm_id: usize,
    vcpu_id: usize,
    logical_cpu_id: usize,
) -> Result<()> {
    if logical_cpu_id >= host_cpu_count {
        bail!(
            "VM[{vm_id}] vCPU{vcpu_id} targets logical CPU {logical_cpu_id}, outside the \
             {host_cpu_count} usable host CPUs"
        );
    }
    if logical_cpu_id >= usize::BITS as usize || dedicated_mask & (1usize << logical_cpu_id) == 0 {
        return Ok(());
    }
    if let Some((owner_vm, owner_vcpu)) = owners.insert(logical_cpu_id, (vm_id, vcpu_id)) {
        bail!(
            "dedicated logical CPU {logical_cpu_id} is assigned to both VM[{owner_vm}] \
             vCPU{owner_vcpu} and VM[{vm_id}] vCPU{vcpu_id}"
        );
    }
    Ok(())
}

pub fn init_guest_vm(raw_cfg: &str) -> Result<usize> {
    let image_provider = AxvisorBootImageProvider;
    let vm_create_config =
        GuestConfig::from_toml(raw_cfg).context("parse VM TOML configuration")?;
    let configured_vm_id = vm_create_config.base.id;

    #[cfg(all(
        feature = "fs",
        any(
            target_arch = "aarch64",
            target_arch = "x86_64",
            target_arch = "loongarch64"
        )
    ))]
    let release_host_filesystem = vm_config_needs_host_filesystem_release(&vm_create_config);

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

    if !axvm::register_vm(vm.clone()) {
        bail!("register VM[{vm_id}]: a VM with this ID already exists");
    }

    #[cfg(all(
        feature = "fs",
        any(
            target_arch = "aarch64",
            target_arch = "x86_64",
            target_arch = "loongarch64"
        )
    ))]
    if release_host_filesystem {
        axvm::host::register_block_passthrough_irq(&vm)
            .context("register host block passthrough IRQ route")?;
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
        && cfg.devices.inherits_host_devices()
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
    let mut vm_config = AxVMConfig::new(AxVMConfigParams {
        id: cfg.base.id,
        name: cfg.base.name.clone(),
        phys_cpu_ls: PhysCpuList::new(
            cfg.base.cpu_num,
            cfg.base.phys_cpu_ids.clone(),
            cfg.base.phys_cpu_sets.clone(),
        ),
        aarch64_virtual_timer_only: cfg.base.aarch64_virtual_timer_only,
        aarch64_wfi_policy: match cfg.base.aarch64_wfi_policy {
            axvmconfig::Aarch64WfiPolicy::Auto => axvm::Aarch64WfiPolicy::Auto,
            axvmconfig::Aarch64WfiPolicy::Trap => axvm::Aarch64WfiPolicy::Trap,
            axvmconfig::Aarch64WfiPolicy::Passthrough => axvm::Aarch64WfiPolicy::Passthrough,
        },
        host_sched_priority: cfg.base.host_sched_priority,
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
    });

    #[cfg(target_arch = "aarch64")]
    // QEMU's virt PCI host maps slot 2 INTA to GIC SPI 5.  The passthrough
    // StarryOS root disk is the NVMe endpoint in that slot; retain the small
    // INTx fallback route so the guest can bring its block controller online
    // when MSI-X is not available through the passthrough path.
    if cfg
        .devices
        .passthrough
        .iter()
        .any(|device| device.path == "/pcie@10000000")
    {
        vm_config.add_pass_through_irq(5, axvm_types::InterruptTriggerMode::EdgeTriggered);
    }
    Ok(vm_config)
}

fn sync_axvm_config_from_crate_config(vm_config: &mut AxVMConfig, cfg: &GuestConfig) {
    vm_config.set_memory_regions(cfg.kernel.memory_regions.clone());
}

#[cfg(all(
    feature = "fs",
    any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )
))]
fn vm_config_needs_host_filesystem_release(config: &GuestConfig) -> bool {
    // A passthrough Guest may still boot a raw kernel from memory while
    // owning a physical PCI block device (StarryOS is one such Guest).  The
    // host filesystem must release the PCI controller in that case too;
    // restricting this check to `image_location = "fs"` leaves the host NVMe
    // driver holding the device and the Guest sees no usable root disk.
    config.base.guest_type == GuestType::Passthrough || !config.devices.passthrough.is_empty()
}

#[cfg(all(
    feature = "fs",
    any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )
))]
pub fn host_filesystem_release_required() -> bool {
    HOST_FILESYSTEM_RELEASE_REQUIRED.load(Ordering::Acquire)
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
    #[test]
    fn build_axvm_config_copies_virtual_timer_only_contract() {
        let mut crate_config = GuestConfig::default();
        crate_config.base.aarch64_virtual_timer_only = true;

        let vm_config = build_axvm_config(&crate_config).unwrap();

        assert!(vm_config.aarch64_virtual_timer_only());
    }

    #[test]
    fn build_axvm_config_copies_explicit_wfi_policy() {
        let mut crate_config = GuestConfig::default();
        crate_config.base.aarch64_wfi_policy = axvmconfig::Aarch64WfiPolicy::Trap;

        let vm_config = build_axvm_config(&crate_config).unwrap();

        assert_eq!(vm_config.aarch64_wfi_policy(), axvm::Aarch64WfiPolicy::Trap);
    }

    #[test]
    fn explicit_passthrough_selection_does_not_also_assign_host_root() {
        let mut crate_config = GuestConfig::default();
        crate_config.base.guest_type = GuestType::Passthrough;
        crate_config.devices.passthrough = vec![axvmconfig::PhysicalDeviceRef {
            path: "/virtio_mmio@a003c00".into(),
        }];

        let vm_config = build_axvm_config(&crate_config).unwrap();
        let names = vm_config
            .pass_through_devices()
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["/virtio_mmio@a003c00"]);
    }

    #[test]
    fn empty_passthrough_selection_keeps_legacy_root_assignment() {
        let mut crate_config = GuestConfig::default();
        crate_config.base.guest_type = GuestType::Passthrough;

        let vm_config = build_axvm_config(&crate_config).unwrap();

        assert!(
            vm_config
                .pass_through_devices()
                .iter()
                .any(|device| device.name == "/")
        );
    }

    #[test]
    fn passthrough_guest_can_disable_legacy_root_assignment() {
        let mut crate_config = GuestConfig::default();
        crate_config.base.guest_type = GuestType::Passthrough;
        crate_config.devices.inherit_host_devices = Some(false);

        let vm_config = build_axvm_config(&crate_config).unwrap();

        assert!(vm_config.pass_through_devices().is_empty());
    }

    fn vm_config(id: usize, cpu_ids: &[usize]) -> GuestConfig {
        let mut config = GuestConfig::default();
        config.base.id = id;
        config.base.cpu_num = cpu_ids.len();
        config.base.phys_cpu_ids = Some(cpu_ids.to_vec());
        config
    }

    #[test]
    fn dedicated_cpu_has_exactly_one_vm_vcpu_owner() {
        let validate = |configs: &[GuestConfig]| {
            validate_dedicated_cpu_ownership_with_resolver(configs, 0b10, 4, Some)
        };
        assert!(validate(&[vm_config(1, &[1])]).is_ok());

        let error = validate(&[vm_config(1, &[1]), vm_config(2, &[1])]).unwrap_err();
        assert!(error.to_string().contains("assigned to both"));
    }

    #[test]
    fn dedicated_cpu_rejects_migratable_affinity() {
        let mut config = GuestConfig::default();
        config.base.id = 3;
        config.base.cpu_num = 1;
        config.base.phys_cpu_sets = Some(vec![0b11]);

        let error = validate_dedicated_cpu_ownership_with_resolver(&[config], 0b10, 4, |_| None)
            .unwrap_err();
        assert!(error.to_string().contains("mixes a dedicated CPU"));
    }

    #[test]
    fn dedicated_cpu_set_is_already_a_logical_mask() {
        let mut config = GuestConfig::default();
        config.base.id = 4;
        config.base.cpu_num = 1;
        config.base.phys_cpu_sets = Some(vec![0b1_0000]);

        let mut resolver_called = false;
        let result = validate_dedicated_cpu_ownership_with_resolver(&[config], 0b1_0000, 8, |_| {
            resolver_called = true;
            None
        });

        assert!(result.is_ok());
        assert!(!resolver_called);
    }

    #[test]
    fn clustered_hardware_mpidr_is_resolved_once() {
        let result = validate_dedicated_cpu_ownership_with_resolver(
            &[vm_config(5, &[0x100])],
            0b1_0000,
            8,
            |hardware_id| (hardware_id == 0x100).then_some(4),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn vm_cpu_placement_rejects_offline_cpu() {
        let error =
            validate_dedicated_cpu_ownership_with_resolver(&[vm_config(1, &[4])], 0, 4, Some)
                .unwrap_err();
        assert!(error.to_string().contains("offline hardware CPU ID 0x4"));
    }
}
