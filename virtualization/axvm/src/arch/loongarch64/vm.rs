//! LoongArch64 VM resource creation and initialization.

use std::{collections::BTreeMap, sync::Arc};

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceManagerResult, DeviceModel, DeviceNodeId,
    DeviceNodeSpec, DeviceRequirements, ResourceRequest, ResourceSlot,
};
use axdevice_base::{AccessWidth, Device, DeviceAccess, DeviceContext, DeviceResult, Resource};
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};

use super::{
    policy::{LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig},
    *,
};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::*,
    vm::{
        prepare::{device_plan::*, devices::*, vcpus::*, *},
        *,
    },
};

/// Frozen device ownership and host-source routes, retained by this VM only.
pub(crate) struct LoongArchVmPlan {
    devices: VmDevicePlan,
    pub(super) physical_routes: Box<[super::irq::LoongArchPhysicalRoute]>,
}

impl ArchitectureVmPlan for LoongArchVmPlan {
    fn devices(&self) -> &VmDevicePlan {
        &self.devices
    }
}

impl LoongArch64Arch {
    pub(crate) fn create_vm_resources(
        config: &mut AxVMConfig,
        fw_cfg_payload: Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        super::boot::probe::apply_host_serial(config)?;
        let device_plan = plan_devices(config, fw_cfg_payload)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(page_table, device_plan, |root_paddr| {
            let gpa_bits = match levels {
                3 => 39,
                4 => 48,
                _ => {
                    return ax_err!(
                        InvalidInput,
                        "unsupported LoongArch nested page-table levels"
                    );
                }
            };
            Ok(NestedPagingConfig::new(root_paddr, levels, gpa_bits, 0))
        })
    }

    pub(crate) fn init_vm(vm: &mut AxVM) -> AxVmResult {
        let vm_id = vm.id();
        let ports = vm.device_access_ports();
        vm.prepare_resources_with(|resources, config| {
            let placements = resources.vcpu_placements(config);
            let state_count = placements
                .iter()
                .map(|placement| placement.id)
                .max()
                .map_or(0, |vcpu_id| vcpu_id + 1);
            let iocsr_state =
                loongarch_result(super::policy::LoongArchIocsrState::new(state_count))
                    .map_err(|error| AxVmError::vcpu("create LoongArch IOCSR state", error))?;
            let dtb_addr = config.image_config().dtb_load_gpa.unwrap_or_default();
            let firmware_boot = uses_firmware_boot(config);
            let boot_args = direct_boot_args(firmware_boot);
            let mut vcpus = PreparedVcpus::create(vm_id, &placements, |placement| {
                Ok(LoongArchVCpuCreateConfig {
                    cpu_id: placement.id,
                    dtb_addr: dtb_addr.as_usize(),
                    boot_args,
                    boot_stack_top: 0,
                    firmware_boot,
                    iocsr_state: iocsr_state.clone(),
                })
            })?;
            let devices = PreparedDevices::build_planned(resources, ports)?;
            let interrupt_controller = devices
                .devices()
                .interrupt_controller(axdevice_base::InterruptControllerId::new(0))?;
            resources.prepare_guest_address_space(vm_id, config, &[])?;
            vcpus.setup(resources, config, build_vcpu_setup_config)?;

            Ok(PreparedVm::new(vcpus, devices, interrupt_controller))
        })
    }
}

fn plan_devices(
    config: &AxVMConfig,
    fw_cfg_payload: Arc<axdevice::FwCfgPayloadSlot>,
) -> AxVmResult<LoongArchVmPlan> {
    const PCH_PIC_BASE: usize = 0x1000_0000;
    const PCH_PIC_SIZE: usize = 0x1000;
    const FW_CFG_BASE: usize = 0x1e02_0000;
    const FW_CFG_SIZE: usize = 0x18;
    let controller_id = DeviceNodeId::new("pch-pic")?;
    // The plan owns one lower run cell; the device model and its per-run
    // interrupt runtime share it, so no callback has to resolve a run by id.
    let run = super::irq::new_run_binding();
    let pch_pic = super::irq::LoongArchPchPicModel::new(
        PCH_PIC_BASE,
        PCH_PIC_SIZE,
        Arc::clone(&run),
        Arc::new(super::irq::LoongArchPchPicOutputSink::new(Arc::clone(&run))),
    );
    let mut nodes = std::vec![
        DeviceNodeSpec::host_replacement(controller_id.clone(), pch_pic),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("fw-cfg")?,
            Arc::new(axdevice::FwCfgPayloadFactory::deferred(
                axvm_types::GuestPhysAddr::from(FW_CFG_BASE),
                FW_CFG_SIZE,
                fw_cfg_payload,
            )),
        ),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("loongarch-firmware-mmio")?,
            Arc::new(LoongArchFirmwareMmioModel),
        ),
    ];
    crate::configured::append_configured_devices(
        config,
        &mut nodes,
        &controller_id,
        axdevice_base::InterruptControllerId::new(0),
        Some(super::pci_config::host_key()),
    )?;
    let pch_pic_range = PCH_PIC_BASE as u64..(PCH_PIC_BASE + PCH_PIC_SIZE) as u64;
    let devices = VmDevicePlan::with_pci_host_for_vm(
        config,
        nodes,
        std::slice::from_ref(&pch_pic_range),
        super::resource_pools::create()?,
        super::pci_config::provider()?,
    )?;
    let physical_routes = physical_routes(config, devices.graph())?;
    Ok(LoongArchVmPlan {
        devices,
        physical_routes,
    })
}

const RTC_MMIO_BASE: u64 = 0x100d_0100;
const RTC_MMIO_SIZE: u64 = 0x100;
// The FDT GED registers live at 0x100e001c, while resource claims use the
// enclosing aligned page required by the graph allocator.
const GED_MMIO_BASE: u64 = 0x100e_0000;
const GED_MMIO_SIZE: u64 = 0x1000;
const FLASH0_MMIO_BASE: u64 = 0x1c00_0000;
const FLASH1_MMIO_BASE: u64 = 0x1d00_0000;
const FLASH_MMIO_SIZE: u64 = 0x0100_0000;

const RTC_SLOT: &str = "rtc";
const GED_SLOT: &str = "ged";
const FLASH0_SLOT: &str = "flash0";
const FLASH1_SLOT: &str = "flash1";

/// Owns the fixed QEMU firmware windows advertised by the LoongArch guest
/// FDT/ACPI tables. The firmware table is part of the guest contract even
/// when AxVM does not emulate a full RTC, flash, or GED implementation, so a
/// typed open-bus device retires those accesses instead of allowing an
/// unclaimed MMIO fault to escape the execution layer.
struct LoongArchFirmwareMmioModel;

impl DeviceModel for LoongArchFirmwareMmioModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(RTC_SLOT)?,
                RTC_MMIO_SIZE,
                RTC_MMIO_SIZE,
                ResourceRequest::Fixed(RTC_MMIO_BASE),
            )?
            .with_mmio(
                ResourceSlot::new(GED_SLOT)?,
                GED_MMIO_SIZE,
                GED_MMIO_SIZE,
                ResourceRequest::Fixed(GED_MMIO_BASE),
            )?
            .with_mmio(
                ResourceSlot::new(FLASH0_SLOT)?,
                FLASH_MMIO_SIZE,
                FLASH_MMIO_SIZE,
                ResourceRequest::Fixed(FLASH0_MMIO_BASE),
            )?
            .with_mmio(
                ResourceSlot::new(FLASH1_SLOT)?,
                FLASH_MMIO_SIZE,
                FLASH_MMIO_SIZE,
                ResourceRequest::Fixed(FLASH1_MMIO_BASE),
            )
    }

    fn firmware(&self) -> axdevice::DeviceFirmwareSpec {
        axdevice::DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let mut bundle = DeviceBundle::new();
        for slot in [RTC_SLOT, GED_SLOT, FLASH0_SLOT, FLASH1_SLOT] {
            let (base, size) = context.mmio(slot)?;
            bundle.add_device(Arc::new(LoongArchFirmwareMmioAperture {
                name: slot,
                resource: [Resource::MmioRange { base, size }],
            }));
        }
        Ok(bundle)
    }
}

struct LoongArchFirmwareMmioAperture {
    name: &'static str,
    resource: [Resource; 1],
}

impl Device for LoongArchFirmwareMmioAperture {
    fn name(&self) -> &str {
        self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resource
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Ok(match access.width() {
            AccessWidth::Byte => u8::MAX as u64,
            AccessWidth::Word => u16::MAX as u64,
            AccessWidth::Dword => u32::MAX as u64,
            AccessWidth::Qword => u64::MAX,
        })
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmLoongArchVcpu as VmArchVcpuOps>::SetupConfig> {
    let firmware_boot = uses_firmware_boot(config);
    Ok(LoongArchVCpuSetupConfig {
        boot_args: direct_boot_args(firmware_boot),
        boot_stack_top: 0,
        firmware_boot,
    })
}

fn direct_boot_args(firmware_boot: bool) -> [usize; 3] {
    if firmware_boot {
        [0; 3]
    } else {
        super::boot::direct_linux_boot_args()
    }
}

fn uses_firmware_boot(config: &AxVMConfig) -> bool {
    matches!(
        config.boot_policy(),
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: crate::config::VMBootProtocol::Uefi,
        }
    )
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    crate::architecture::minimum_recorded_target_cpu_capability(
        "LoongArch nested page-table levels",
        vcpu_mappings,
        |cpu_id| {
            crate::percpu::select_cpu_virtualization_capability(cpu_id, |levels, _, _| {
                levels as u64
            })
        },
    )
    .map(|levels| levels as usize)
    .map_err(|error| {
        crate::architecture::unsupported_target_cpu_capability(
            "select LoongArch target CPU capability",
            error,
        )
    })
}

/// Select only actual host mappings after virtual-device replacements were
/// subtracted. Guest firmware defaults do not assign a host interrupt source.
fn physical_routes(
    config: &AxVMConfig,
    graph: &axdevice::ResolvedDeviceGraph,
) -> AxVmResult<Box<[super::irq::LoongArchPhysicalRoute]>> {
    let mappings: Vec<_> = graph
        .nodes()
        .filter_map(|node| node.host_mapping())
        .collect();
    if mappings.is_empty() && config.pass_through_irqs().is_empty() {
        return Ok(Box::new([]));
    }
    ax_std::os::arceos::driver::probe::acpi::with_acpi(|acpi| {
        let mut sources = BTreeMap::new();
        for device in acpi.resource_devices().map_err(|error| {
            AxVmError::invalid_config(std::format!("collect LoongArch assigned IRQs: {error}"))
        })? {
            let assigned = device.memory_ranges.iter().any(|range| {
                range.base.checked_add(range.size).is_some_and(|end| {
                    mappings.iter().any(|mapping| {
                        mapping.host_base() <= range.base
                            && end <= mapping.host_base() + mapping.length()
                    })
                })
            });
            if assigned {
                for route in device.irq_routes {
                    sources.insert(route.gsi, usize::from(route.controller_input));
                }
            }
        }
        for interrupt in config.pass_through_irqs() {
            let route = acpi
                .routing()
                .resolve_gsi(interrupt.source)
                .ok_or_else(|| {
                    AxVmError::invalid_config(std::format!(
                        "assigned LoongArch GSI {} has no host controller",
                        interrupt.source
                    ))
                })?;
            sources.insert(route.gsi, usize::from(route.controller_input));
        }
        sources
            .into_iter()
            .map(|(source, guest_input)| {
                let physical_irq = source as usize;
                if physical_irq >= super::irq::LOONGARCH_MAX_IRQ_COUNT || guest_input >= 64 {
                    return Err(AxVmError::unsupported(
                        "bind LoongArch assigned IRQ",
                        "source or controller input exceeds the fixed route capacity",
                    ));
                }
                Ok(super::irq::LoongArchPhysicalRoute {
                    physical_irq,
                    guest_input,
                })
            })
            .collect::<AxVmResult<Vec<_>>>()
            .map(Vec::into_boxed_slice)
    })
    .ok_or_else(|| {
        AxVmError::unsupported(
            "bind LoongArch assigned IRQ",
            "host ACPI routing is unavailable",
        )
    })?
}
