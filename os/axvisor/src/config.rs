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
        VMImageConfig, VmMemoryBacking, VmMemoryConfig,
    },
    machine::{
        AddressRange, ControllerInputId, DeviceInstanceId, DeviceModelId, GuestMemoryRegion,
        HostDeviceId, HostDeviceSelector, HostPlatformSnapshot, VirtualDeviceDescriptor,
        VirtualDeviceSource, VmMachinePlanner, VmMachineRequest,
    },
};
#[cfg(feature = "fs")]
use axvm::{AxVmError, AxVmResult};
use axvm_types::MappingFlags;
use axvmconfig::{AxVMCrateConfig, DeviceSelectorConfig, MemoryBackingConfig, MemoryRegionConfig};

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

    let boot_policy = guest_boot_policy(&vm_create_config, &image_provider);
    let mut vm_config = build_axvm_config(&vm_create_config, boot_policy)?;
    let prepared_boot = prepare_guest_boot(&mut vm_config, vm_create_config, &image_provider)
        .with_context(|| format!("prepare boot resources for VM[{configured_vm_id}]"))?;

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

    let (claims_required, host_console) = vm.with_config(|config| {
        let plan = config.machine_plan();
        (!plan.claims().is_empty(), plan.host_console().cloned())
    });
    if claims_required {
        let live_generation = axvm::current_host_platform_snapshot()
            .context("refresh host platform snapshot before claiming devices")?
            .generation();
        let claim_provider = crate::host_devices::AxvisorHostDeviceClaimProvider::new(
            live_generation,
            vm_id,
            host_console,
        );
        vm.claim_host_devices(&claim_provider)
            .with_context(|| format!("claim passthrough devices for VM[{vm_id}]"))?;
    }

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

pub(crate) fn build_axvm_config(
    cfg: &AxVMCrateConfig,
    boot_policy: GuestBootPolicy,
) -> Result<AxVMConfig> {
    let machine_plan = build_machine_plan(cfg)?;
    let memory_regions = cfg
        .memory
        .regions
        .iter()
        .map(runtime_memory_region)
        .collect::<Result<Vec<_>>>()?;

    Ok(AxVMConfig::new(AxVMConfigParams {
        id: cfg.base.id,
        name: cfg.base.name.clone(),
        machine_plan,
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
        memory_regions,
        boot_policy,
    }))
}

fn build_machine_plan(cfg: &AxVMCrateConfig) -> Result<axvm::machine::VmMachinePlan> {
    let mut request = VmMachineRequest::new(cfg.machine.mode(), cfg.machine.firmware())
        .with_interrupt_delivery(cfg.machine.interrupt_delivery())
        .with_vcpu_count(cfg.base.cpu_num);
    for region in &cfg.memory.regions {
        request = request.with_memory(GuestMemoryRegion::new(AddressRange::new(
            region.guest_base,
            region.size,
        )?));
    }
    for selector in &cfg.devices.deny {
        request = request.deny(machine_selector(selector)?);
    }
    for device in configured_virtual_devices(cfg)? {
        request = request.with_virtual_device(device);
    }

    let snapshot = if cfg.machine.mode() == axvm_types::VmMachineMode::Virtual {
        HostPlatformSnapshot::new(0)
    } else {
        axvm::current_host_platform_snapshot()?
    };
    #[cfg(all(feature = "fs", target_arch = "x86_64"))]
    let snapshot = add_x86_host_filesystem_endpoint(cfg, snapshot)?;
    let plan = VmMachinePlanner::new(axvm::standard_machine_profile()?)
        .plan(&request, &snapshot)
        .map_err(anyhow::Error::from)?;
    finalize_machine_firmware(cfg, plan, &snapshot)
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn add_x86_host_filesystem_endpoint(
    cfg: &AxVMCrateConfig,
    mut snapshot: HostPlatformSnapshot,
) -> Result<HostPlatformSnapshot> {
    use ax_driver::pci::PciEndpointBar;
    use axvm::machine::{
        HostDeviceDescriptor, HostDeviceOwnership, HostInterruptResource, IoPortRange,
    };

    if !vm_config_needs_host_filesystem_release(cfg) {
        return Ok(snapshot);
    }

    let info = x86_host_fs_passthrough_pci_info();
    let resources = ax_driver::pci::taken_endpoint_resources(info.address).map_err(|error| {
        anyhow::anyhow!(
            "host filesystem PCI endpoint {} has no retained resource snapshot: {error:?}",
            info.address
        )
    })?;
    let binding = ax_driver::pci::resolve_intx_binding(info)
        .map_err(|error| anyhow::anyhow!("resolve host filesystem PCI INTx: {error:?}"))?
        .context("host filesystem PCI endpoint has no resolvable INTx route")?;
    let trigger = x86_intx_forwarding_trigger(&binding);
    let (_, _, _, guest_gsi) = axvm::boot::x86_qemu_passthrough_block_intx();
    let guest_gsi = u32::try_from(guest_gsi).context("guest PCI INTx GSI exceeds u32")?;
    let interrupt = match &binding {
        ax_driver::BindingIrq::Source(ax_driver::BindingIrqSource::AcpiGsiRoute(route)) => {
            HostInterruptResource::routed_acpi(guest_gsi, *route)
        }
        _ => HostInterruptResource::controller_input(guest_gsi, trigger),
    };
    let mut descriptor = HostDeviceDescriptor::new(
        HostDeviceId::new(format!("pci:{}", resources.address()))?,
        HostDeviceOwnership::Transferable,
    )
    .with_compatible(format!(
        "pci{:04x},{:04x}",
        resources.vendor_id(),
        resources.device_id()
    ))
    .with_interrupt(interrupt);

    for bar in resources.bars() {
        match *bar {
            PciEndpointBar::Memory { address, size, .. } => {
                let range = AddressRange::new(address, size)?;
                snapshot = snapshot.with_io_aperture(range);
                descriptor = descriptor.with_mmio(range);
            }
            PciEndpointBar::Io { port, size, .. } => {
                let port = u16::try_from(port).context("PCI I/O BAR base exceeds u16")?;
                let size = u16::try_from(size).context("PCI I/O BAR size exceeds u16")?;
                descriptor = descriptor.with_pio(IoPortRange::new(port, size)?);
            }
        }
    }
    if descriptor.mmio().is_empty() && descriptor.pio().is_empty() {
        bail!(
            "host filesystem PCI endpoint {} exposes no assignable BAR resources",
            resources.address()
        );
    }

    Ok(snapshot.with_device(descriptor))
}

#[cfg(target_arch = "aarch64")]
fn finalize_machine_firmware(
    cfg: &AxVMCrateConfig,
    plan: axvm::machine::VmMachinePlan,
    snapshot: &HostPlatformSnapshot,
) -> Result<axvm::machine::VmMachinePlan> {
    use axvm_types::GuestFirmwareKind;

    match cfg.machine.firmware() {
        GuestFirmwareKind::Auto | GuestFirmwareKind::Fdt => {
            let bytes = if cfg.machine.mode() == axvm_types::VmMachineMode::Virtual {
                let mut firmware = axvm::machine::Aarch64FdtConfig::new(cfg.base.cpu_num)?;
                if let Some(cmdline) = cfg.kernel.cmdline.as_deref() {
                    firmware = firmware.with_bootargs(cmdline);
                }
                axvm::machine::generate_aarch64_fdt(&plan, &firmware)?
            } else {
                let physical_cpus =
                    cfg.base.phys_cpu_ids.as_deref().context(
                        "AArch64 passthrough machine requires explicit physical CPU IDs",
                    )?;
                let mut firmware = axvm::machine::HostFdtConfig::new(physical_cpus.iter().copied());
                if let Some(cmdline) = cfg.kernel.cmdline.as_deref() {
                    firmware = firmware.with_bootargs(cmdline);
                }
                axvm::machine::generate_host_fdt(&plan, snapshot, &firmware)?
            };
            Ok(plan.with_device_tree_firmware(bytes))
        }
        GuestFirmwareKind::Acpi => {
            bail!("AArch64 virtual machines do not support ACPI firmware yet")
        }
    }
}

#[cfg(target_arch = "riscv64")]
fn finalize_machine_firmware(
    cfg: &AxVMCrateConfig,
    plan: axvm::machine::VmMachinePlan,
    snapshot: &HostPlatformSnapshot,
) -> Result<axvm::machine::VmMachinePlan> {
    use axvm_types::GuestFirmwareKind;

    match cfg.machine.firmware() {
        GuestFirmwareKind::Auto | GuestFirmwareKind::Fdt => {
            let bytes = if cfg.machine.mode() == axvm_types::VmMachineMode::Virtual {
                let mut firmware = axvm::machine::RiscvFdtConfig::new(cfg.base.cpu_num)?;
                if let Some(cmdline) = cfg.kernel.cmdline.as_deref() {
                    firmware = firmware.with_bootargs(cmdline);
                }
                axvm::machine::generate_riscv_fdt(&plan, &firmware)?
            } else {
                let physical_cpus = cfg
                    .base
                    .phys_cpu_ids
                    .as_deref()
                    .context("RISC-V passthrough machine requires explicit physical CPU IDs")?;
                let mut firmware = axvm::machine::HostFdtConfig::new(physical_cpus.iter().copied());
                if let Some(cmdline) = cfg.kernel.cmdline.as_deref() {
                    firmware = firmware.with_bootargs(cmdline);
                }
                axvm::machine::generate_host_fdt(&plan, snapshot, &firmware)?
            };
            Ok(plan.with_device_tree_firmware(bytes))
        }
        GuestFirmwareKind::Acpi => {
            bail!("RISC-V virtual machines do not support ACPI firmware")
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn finalize_machine_firmware(
    cfg: &AxVMCrateConfig,
    plan: axvm::machine::VmMachinePlan,
    _snapshot: &HostPlatformSnapshot,
) -> Result<axvm::machine::VmMachinePlan> {
    use axvm_types::GuestFirmwareKind;

    match cfg.machine.firmware() {
        GuestFirmwareKind::Auto | GuestFirmwareKind::Acpi => {
            const ACPI_LOAD_ADDRESS: u64 = 0x000e_0000;

            let image = axvm::machine::generate_x86_acpi(
                &plan,
                &axvm::machine::X86AcpiConfig::new(cfg.base.cpu_num, ACPI_LOAD_ADDRESS)?,
            )?;
            let image_end = image
                .load_address()
                .checked_add(image.len() as u64)
                .context("generated x86 ACPI image address overflows")?;
            if !plan
                .guest_memory()
                .iter()
                .any(|memory| memory.base() <= image.load_address() && image_end <= memory.end())
            {
                bail!(
                    "generated x86 ACPI image [{:#x}, {image_end:#x}) is outside guest RAM",
                    image.load_address()
                );
            }
            Ok(plan.with_acpi_firmware(image))
        }
        GuestFirmwareKind::Fdt => bail!("x86 virtual machines do not support FDT firmware"),
    }
}

#[cfg(target_arch = "loongarch64")]
fn finalize_machine_firmware(
    cfg: &AxVMCrateConfig,
    plan: axvm::machine::VmMachinePlan,
    _snapshot: &HostPlatformSnapshot,
) -> Result<axvm::machine::VmMachinePlan> {
    use axvm_types::GuestFirmwareKind;

    match cfg.machine.firmware() {
        GuestFirmwareKind::Auto | GuestFirmwareKind::Acpi => {
            let files = axvm::machine::generate_loongarch_fw_cfg_acpi(&plan, cfg.base.cpu_num)?;
            Ok(plan.with_fw_cfg_acpi_firmware(files))
        }
        GuestFirmwareKind::Fdt => {
            bail!("LoongArch virtual machines require ACPI firmware")
        }
    }
}

fn machine_selector(selector: &DeviceSelectorConfig) -> Result<HostDeviceSelector> {
    Ok(match selector {
        DeviceSelectorConfig::FdtPath { value } | DeviceSelectorConfig::AcpiPath { value } => {
            HostDeviceSelector::PathSubtree(HostDeviceId::new(value.clone())?)
        }
        DeviceSelectorConfig::Compatible { value } => {
            HostDeviceSelector::compatible(value.clone())?
        }
        DeviceSelectorConfig::Mmio { base, size } => {
            HostDeviceSelector::Mmio(AddressRange::new(*base, *size)?)
        }
        DeviceSelectorConfig::Interrupt { intid } => {
            HostDeviceSelector::Interrupt(ControllerInputId::new(*intid as usize))
        }
    })
}

#[cfg(target_arch = "aarch64")]
fn configured_virtual_devices(cfg: &AxVMCrateConfig) -> Result<Vec<VirtualDeviceDescriptor>> {
    let mut configured = cfg.devices.virtual_devices.clone();
    let console_disabled = cfg
        .devices
        .disable_defaults
        .iter()
        .any(|model| model == "console");
    let has_console = configured.iter().any(|device| device.model == "arm-pl011");
    if cfg.machine.interrupt_delivery() == axvm_types::InterruptDelivery::Mediated
        && !console_disabled
        && !has_console
    {
        configured.push(axvmconfig::VirtualDeviceConfig {
            id: "console0".into(),
            model: "arm-pl011".into(),
            source: axvmconfig::VirtualDeviceSourceConfig::Auto,
            backend: axvmconfig::VirtualDeviceBackendConfig::HostConsole {
                rx: axvmconfig::ConsoleRxMode::Exclusive,
                tx: axvmconfig::ConsoleTxMode::Shared,
            },
        });
    }

    configured
        .into_iter()
        .map(|device| {
            if device.model != "arm-pl011" {
                bail!(
                    "unsupported AArch64 virtual device model '{}'",
                    device.model
                );
            }
            let descriptor = VirtualDeviceDescriptor::new(
                DeviceInstanceId::new(device.id)?,
                DeviceModelId::new(device.model)?,
                axvm::pl011_device_requirements()?,
            )
            .with_compatible("arm,pl011")
            .with_source(machine_device_source(device.source)?)
            .with_backend(machine_device_backend(device.backend));
            Ok(descriptor)
        })
        .collect()
}

#[cfg(target_arch = "x86_64")]
fn configured_virtual_devices(cfg: &AxVMCrateConfig) -> Result<Vec<VirtualDeviceDescriptor>> {
    let mut configured = cfg.devices.virtual_devices.clone();
    let console_disabled = cfg
        .devices
        .disable_defaults
        .iter()
        .any(|model| model == "console");
    let has_console = configured.iter().any(|device| device.model == "x86-com1");
    if cfg.machine.interrupt_delivery() == axvm_types::InterruptDelivery::Mediated
        && !console_disabled
        && !has_console
    {
        configured.push(axvmconfig::VirtualDeviceConfig {
            id: "console0".into(),
            model: "x86-com1".into(),
            source: axvmconfig::VirtualDeviceSourceConfig::Auto,
            backend: axvmconfig::VirtualDeviceBackendConfig::HostConsole {
                rx: axvmconfig::ConsoleRxMode::Exclusive,
                tx: axvmconfig::ConsoleTxMode::Shared,
            },
        });
    }

    configured
        .into_iter()
        .map(|device| {
            if device.model != "x86-com1" {
                bail!("unsupported x86 virtual device model '{}'", device.model);
            }
            Ok(VirtualDeviceDescriptor::new(
                DeviceInstanceId::new(device.id)?,
                DeviceModelId::new(device.model)?,
                axvm::x86_com1_device_requirements()?,
            )
            .with_compatible("PNP0501")
            .with_source(machine_device_source(device.source)?)
            .with_backend(machine_device_backend(device.backend)))
        })
        .collect()
}

#[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
fn configured_virtual_devices(cfg: &AxVMCrateConfig) -> Result<Vec<VirtualDeviceDescriptor>> {
    let mut configured = cfg.devices.virtual_devices.clone();
    let console_disabled = cfg
        .devices
        .disable_defaults
        .iter()
        .any(|model| model == "console");
    let has_console = configured.iter().any(|device| device.model == "ns16550a");
    if cfg.machine.interrupt_delivery() == axvm_types::InterruptDelivery::Mediated
        && !console_disabled
        && !has_console
    {
        configured.push(axvmconfig::VirtualDeviceConfig {
            id: "console0".into(),
            model: "ns16550a".into(),
            source: axvmconfig::VirtualDeviceSourceConfig::Auto,
            backend: axvmconfig::VirtualDeviceBackendConfig::HostConsole {
                rx: axvmconfig::ConsoleRxMode::Exclusive,
                tx: axvmconfig::ConsoleTxMode::Shared,
            },
        });
    }

    configured
        .into_iter()
        .map(|device| {
            if device.model != "ns16550a" {
                bail!("unsupported virtual device model '{}'", device.model);
            }
            Ok(VirtualDeviceDescriptor::new(
                DeviceInstanceId::new(device.id)?,
                DeviceModelId::new(device.model)?,
                axvm::ns16550_device_requirements()?,
            )
            .with_compatible("ns16550a")
            .with_compatible("ns16550")
            .with_source(machine_device_source(device.source)?)
            .with_backend(machine_device_backend(device.backend)))
        })
        .collect()
}

fn machine_device_source(
    source: axvmconfig::VirtualDeviceSourceConfig,
) -> Result<VirtualDeviceSource> {
    Ok(match source {
        axvmconfig::VirtualDeviceSourceConfig::Auto => VirtualDeviceSource::Auto,
        axvmconfig::VirtualDeviceSourceConfig::Allocate => VirtualDeviceSource::Allocate,
        axvmconfig::VirtualDeviceSourceConfig::FdtPath { value }
        | axvmconfig::VirtualDeviceSourceConfig::AcpiPath { value } => {
            VirtualDeviceSource::Host(HostDeviceSelector::Id(HostDeviceId::new(value)?))
        }
        axvmconfig::VirtualDeviceSourceConfig::Compatible { value } => {
            VirtualDeviceSource::Host(HostDeviceSelector::compatible(value)?)
        }
    })
}

fn machine_device_backend(
    backend: axvmconfig::VirtualDeviceBackendConfig,
) -> axvm::machine::DeviceBackend {
    use axvm::machine::{ConsoleRxPolicy, ConsoleTxPolicy, DeviceBackend, HostConsoleBackend};

    match backend {
        axvmconfig::VirtualDeviceBackendConfig::None => DeviceBackend::None,
        axvmconfig::VirtualDeviceBackendConfig::HostConsole { rx, tx } => {
            let rx = match rx {
                axvmconfig::ConsoleRxMode::Exclusive => ConsoleRxPolicy::Exclusive,
                axvmconfig::ConsoleRxMode::Disabled => ConsoleRxPolicy::Disabled,
            };
            let tx = match tx {
                axvmconfig::ConsoleTxMode::Shared => ConsoleTxPolicy::Shared,
                axvmconfig::ConsoleTxMode::Exclusive => ConsoleTxPolicy::Exclusive,
                axvmconfig::ConsoleTxMode::Disabled => ConsoleTxPolicy::Disabled,
            };
            DeviceBackend::HostConsole(HostConsoleBackend::new(rx, tx))
        }
    }
}

fn runtime_memory_region(region: &MemoryRegionConfig) -> Result<VmMemoryConfig> {
    let gpa = usize::try_from(region.guest_base).context("guest memory base exceeds usize")?;
    let size = usize::try_from(region.size).context("guest memory size exceeds usize")?;
    let mut flags = MappingFlags::USER;
    if region.permissions.readable() {
        flags |= MappingFlags::READ;
    }
    if region.permissions.writable() {
        flags |= MappingFlags::WRITE;
    }
    if region.permissions.executable() {
        flags |= MappingFlags::EXECUTE;
    }
    let backing = match region.backing {
        MemoryBackingConfig::Allocate => VmMemoryBacking::Allocated,
        MemoryBackingConfig::IdentityAllocate => VmMemoryBacking::IdentityAllocated,
        MemoryBackingConfig::Host { host_base } => VmMemoryBacking::Host {
            host_base: usize::try_from(host_base)
                .context("host memory base exceeds usize")?
                .into(),
        },
        MemoryBackingConfig::Shared { host_base } => VmMemoryBacking::Shared {
            host_base: usize::try_from(host_base)
                .context("shared memory base exceeds usize")?
                .into(),
        },
        MemoryBackingConfig::Reserved => VmMemoryBacking::Reserved {
            host_base: gpa.into(),
        },
    };
    VmMemoryConfig::new(GuestPhysAddr::from(gpa), size, flags, backing).map_err(anyhow::Error::from)
}

#[cfg(feature = "fs")]
fn vm_config_needs_host_filesystem_release(config: &AxVMCrateConfig) -> bool {
    config.kernel.image_location.as_deref() == Some("fs")
        && config.machine.mode() == axvm_types::VmMachineMode::Passthrough
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
    use axvmconfig::MemoryPermissions;

    #[test]
    fn runtime_memory_region_preserves_non_identity_host_backing() {
        let region = MemoryRegionConfig {
            guest_base: 0x8000_0000,
            size: 0x20_0000,
            permissions: MemoryPermissions::try_from("rwx").unwrap(),
            backing: MemoryBackingConfig::Host {
                host_base: 0xa000_0000,
            },
        };

        let runtime = runtime_memory_region(&region).unwrap();

        assert_eq!(runtime.guest_base(), GuestPhysAddr::from(0x8000_0000));
        assert_eq!(
            runtime.backing().host_base(),
            Some(axvm_types::HostPhysAddr::from(0xa000_0000))
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn default_x86_console_replaces_the_host_com1_template() {
        use axvm::machine::{
            HostDeviceDescriptor, HostDeviceOwnership, HostInterruptResource, IoPortRange,
        };
        use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};

        let config = AxVMCrateConfig::from_toml(
            r#"
[machine]
mode = "passthrough"
firmware = "acpi"

[base]
id = 1
name = "x86-default-console"
cpu_num = 1

[kernel]
entry_point = 0x20_0000
kernel_path = "/guest/kernel"
kernel_load_addr = 0x20_0000
image_location = "fs"

[[memory.regions]]
guest_base = 0
size = 0x10_0000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []
"#,
        )
        .unwrap();
        let host_com1 = HostDeviceId::new("\\_SB.COM1").unwrap();
        let snapshot = HostPlatformSnapshot::new(1).with_device(
            HostDeviceDescriptor::new(host_com1.clone(), HostDeviceOwnership::Assignable)
                .with_compatible("PNP0501")
                .with_pio(IoPortRange::new(0x3f8, 8).unwrap())
                .with_interrupt(HostInterruptResource::controller_input(
                    4,
                    InterruptTriggerMode::EdgeTriggered,
                )),
        );
        let mut request =
            VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi);
        for device in configured_virtual_devices(&config).unwrap() {
            request = request.with_virtual_device(device);
        }

        let plan = VmMachinePlanner::new(axvm::standard_machine_profile().unwrap())
            .plan(&request, &snapshot)
            .unwrap();

        assert_eq!(plan.virtual_devices()[0].host_template(), Some(&host_com1));
        assert!(plan.assigned_host_pio().next().is_none());
    }
}
