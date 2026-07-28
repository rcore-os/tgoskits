//! AArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use arm_vcpu::{ArmVcpuCreateConfig, ArmVcpuSetupConfig};
use ax_memory_addr::PhysAddr;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, ServiceCardinality, ServiceKey,
};
use axdevice_base::Device;
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, NestedPagingConfig, VMInterruptMode, VmArchVcpuOps,
};

use super::{Aarch64Arch, npt};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::AxVMConfig,
    vm::{
        AxVM, AxVMResources,
        prepare::{
            ArchDeviceBootstrap, PreparedVm, VmInitRequest,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init, default_device_factories,
            devices::PreparedDevices,
            validate_guest_dtb,
            vcpus::{PreparedVcpus, vcpu_placements},
        },
    },
};

impl Aarch64Arch {
    pub(crate) fn create_vm_resources(config: AxVMConfig) -> AxVmResult<AxVMResources> {
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
            nested_paging_config(root_paddr, levels, &placements)
        })
    }

    pub(crate) fn init_vm(vm: &AxVM, request: VmInitRequest<'_>) -> AxVmResult {
        match request {
            VmInitRequest::Default => {
                let (factories, interrupt_fabric) = prepare_device_bootstrap(vm)?.into_parts();
                init_vm_with(vm, &factories, interrupt_fabric)
            }
            VmInitRequest::Provided {
                factories,
                interrupt_fabric,
            } => init_vm_with(vm, factories, interrupt_fabric),
        }
    }
}

fn prepare_device_bootstrap(vm: &AxVM) -> AxVmResult<ArchDeviceBootstrap> {
    let mut factories = default_device_factories()?;
    register_device_factories(&mut factories)?;
    Ok(ArchDeviceBootstrap::new(
        factories,
        crate::InterruptFabric::new(vm.interrupt_mode()),
    ))
}

fn init_vm_with(
    vm: &AxVM,
    factories: &axdevice::DeviceFactoryRegistry,
    interrupt_fabric: crate::InterruptFabric,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_fabric, |resources, interrupt_fabric| {
        let placements = vcpu_placements(resources);
        let dtb_addr = resources
            .config()
            .image_config()
            .dtb_load_gpa
            .unwrap_or_default();
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
            Ok(ArmVcpuCreateConfig {
                mpidr_el1: placement.phys_cpu_id as _,
                dtb_addr: dtb_addr.as_usize(),
            })
        })?;
        let extra_devices = arch_extra_device_configs(resources.config());
        let devices = PreparedDevices::build_common_with_extra(
            resources,
            factories,
            interrupt_fabric,
            &extra_devices,
            vm.device_access_ports(),
        )?;
        assign_arch_device_state(vm, resources.config(), devices.devices())?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmArmVcpu as VmArchVcpuOps>::SetupConfig> {
    let passthrough = config.interrupt_mode() == VMInterruptMode::Passthrough;
    Ok(ArmVcpuSetupConfig {
        passthrough_interrupt: passthrough,
        passthrough_timer: passthrough,
    })
}

fn assign_arch_device_state(
    vm: &AxVM,
    config: &AxVMConfig,
    devices: &axdevice::DeviceRuntime,
) -> AxVmResult {
    if config.interrupt_mode() == VMInterruptMode::Passthrough {
        assign_passthrough_spis(vm, config, devices)?;
    }
    Ok(())
}

fn arch_extra_device_configs(config: &AxVMConfig) -> alloc::vec::Vec<EmulatedDeviceConfig> {
    if config.interrupt_mode() == VMInterruptMode::Passthrough {
        return alloc::vec![];
    }
    alloc::vec![EmulatedDeviceConfig {
        name: "aarch64-vtimer".into(),
        base_gpa: 0,
        length: 0,
        irq_id: 0,
        emu_type: EmulatedDeviceType::Aarch64Vtimer,
        cfg_list: alloc::vec![],
    }]
}

fn assign_passthrough_spis(
    vm: &AxVM,
    config: &AxVMConfig,
    devices: &axdevice::DeviceRuntime,
) -> AxVmResult {
    if config.pass_through_spis().is_empty() {
        return Ok(());
    }
    let cpu_id = vm.id() - 1; // FIXME: get the real CPU id.
    let Ok(gicd) = devices.services().require::<Aarch64GicDistributorKey>() else {
        // A passthrough-only guest intentionally has no emulated GICD service:
        // its interrupt controller is described by the forwarded host FDT.
        // SPI assignment is meaningful only when a virtual distributor exists.
        return Ok(());
    };

    for spi in config.pass_through_spis() {
        gicd.assign_spi(*spi + 32, cpu_id, (0, 0, 0, cpu_id as _))
            .map_err(|error| AxVmError::interrupt("assign passthrough SPI", error))?;
    }
    Ok(())
}

/// Typed architecture capability used only for passthrough SPI assignment.
trait Aarch64GicDistributorOps: Send + Sync {
    fn assign_spi(
        &self,
        irq: u32,
        cpu_phys_id: usize,
        target_cpu_affinity: (u8, u8, u8, u8),
    ) -> DeviceManagerResult;
}

struct Aarch64GicDistributorKey;

impl ServiceKey for Aarch64GicDistributorKey {
    type Service = dyn Aarch64GicDistributorOps;

    const NAME: &'static str = "aarch64-gic-distributor";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

impl Aarch64GicDistributorOps for arm_vgic::v3::vgicd::VGicD {
    fn assign_spi(
        &self,
        irq: u32,
        cpu_phys_id: usize,
        target_cpu_affinity: (u8, u8, u8, u8),
    ) -> DeviceManagerResult {
        self.assign_irq(irq, cpu_phys_id, target_cpu_affinity)
            .map_err(|error| DeviceManagerError::UnexpectedResponse {
                operation: "assign passthrough SPI",
                detail: alloc::format!("{error}"),
            })
    }
}

struct Aarch64VgicFactory;
struct Aarch64GicRedistributorFactory;
struct Aarch64GicDistributorFactory;
struct Aarch64GitsFactory;

impl DeviceFactory for Aarch64VgicFactory {
    fn device_type(&self) -> axvm_types::EmulatedDeviceType {
        axvm_types::EmulatedDeviceType::InterruptController
    }

    fn build(
        &self,
        _config: &axvm_types::EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        #[allow(clippy::arc_with_non_send_sync)]
        let device: Arc<dyn Device> = Arc::new(arm_vgic::Vgic::new());
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            device,
        )))
    }
}

impl DeviceFactory for Aarch64GicRedistributorFactory {
    fn device_type(&self) -> axvm_types::EmulatedDeviceType {
        axvm_types::EmulatedDeviceType::GPPTRedistributor
    }

    fn build(
        &self,
        config: &axvm_types::EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let [cpu_num, stride, pcpu_id] = config.cfg_list.as_slice() else {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual GIC redistributor",
                detail: "device requires cpu_num, stride and pcpu_id".into(),
            });
        };
        let mut bundle = DeviceBundle::new();
        for index in 0..*cpu_num {
            let base = config
                .base_gpa
                .checked_add(index.checked_mul(*stride).ok_or_else(|| {
                    DeviceManagerError::InvalidConfig {
                        operation: "build virtual GIC redistributor",
                        detail: "redistributor address overflows".into(),
                    }
                })?)
                .ok_or_else(|| DeviceManagerError::InvalidConfig {
                    operation: "build virtual GIC redistributor",
                    detail: "redistributor address overflows".into(),
                })?;
            #[allow(clippy::arc_with_non_send_sync)]
            let device: Arc<dyn Device> = Arc::new(arm_vgic::v3::vgicr::VGicR::new(
                base.into(),
                Some(config.length),
                pcpu_id + index,
            ));
            bundle.push(DeviceRegistration::Device(device));
        }
        Ok(bundle)
    }
}

impl DeviceFactory for Aarch64GicDistributorFactory {
    fn device_type(&self) -> axvm_types::EmulatedDeviceType {
        axvm_types::EmulatedDeviceType::GPPTDistributor
    }

    fn build(
        &self,
        config: &axvm_types::EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        #[allow(clippy::arc_with_non_send_sync)]
        let distributor = Arc::new(arm_vgic::v3::vgicd::VGicD::new(
            config.base_gpa.into(),
            Some(config.length),
        ));
        #[allow(clippy::arc_with_non_send_sync)]
        let device: Arc<dyn Device> = distributor.clone();
        let service: Arc<dyn Aarch64GicDistributorOps> = distributor;
        DeviceBundle::from_registration(DeviceRegistration::Device(device))
            .with_service::<Aarch64GicDistributorKey>(service)
    }
}

impl DeviceFactory for Aarch64GitsFactory {
    fn device_type(&self) -> axvm_types::EmulatedDeviceType {
        axvm_types::EmulatedDeviceType::GPPTITS
    }

    fn build(
        &self,
        config: &axvm_types::EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let [host_gits_base] = config.cfg_list.as_slice() else {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual GITS",
                detail: "device requires host_gits_base".into(),
            });
        };
        #[allow(clippy::arc_with_non_send_sync)]
        let device: Arc<dyn Device> = Arc::new(arm_vgic::v3::gits::Gits::new(
            config.base_gpa.into(),
            Some(config.length),
            PhysAddr::from_usize(*host_gits_base),
            false,
        ));
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            device,
        )))
    }
}

fn register_device_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    registry.register(Arc::new(Aarch64VgicFactory))?;
    registry.register(Arc::new(Aarch64GicRedistributorFactory))?;
    registry.register(Arc::new(Aarch64GicDistributorFactory))?;
    registry.register(Arc::new(Aarch64GitsFactory))?;
    registry.register(Arc::new(super::vtimer::Aarch64VtimerFactory))
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    let mut selected = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let levels = crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
            .unwrap_or_else(arm_vcpu::max_guest_page_table_levels);
        if levels == 0 {
            return ax_err!(
                Unsupported,
                "AArch64 nested paging is not enabled on target CPU"
            );
        }
        selected = selected.min(levels);
    }
    if selected == usize::MAX {
        selected = arm_vcpu::max_guest_page_table_levels();
    }
    match selected {
        3 | 4 => Ok(selected),
        _ => ax_err!(Unsupported, "unsupported AArch64 stage-2 page-table levels"),
    }
}

fn nested_paging_config(
    root_paddr: ax_memory_addr::PhysAddr,
    levels: usize,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
) -> AxVmResult<NestedPagingConfig> {
    let mut pa_bits = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let bits =
            crate::percpu::cpu_guest_phys_addr_bits(cpu_id).unwrap_or_else(arm_vcpu::pa_bits);
        pa_bits = pa_bits.min(bits);
    }
    if pa_bits == usize::MAX {
        pa_bits = arm_vcpu::pa_bits();
    }

    let gpa_bits = match levels {
        3 => 39,
        4 => 48,
        _ => return ax_err!(InvalidInput, "unsupported AArch64 stage-2 levels"),
    };
    Ok(NestedPagingConfig::new(
        root_paddr, levels, gpa_bits, pa_bits,
    ))
}
