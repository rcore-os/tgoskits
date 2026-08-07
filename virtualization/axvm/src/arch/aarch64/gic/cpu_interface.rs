//! Checked GICH/ICH register save and restore.

use std::sync::OnceLock;

use arm_gic_driver::v3::{
    ICH_AP1R0_EL2, ICH_AP1R1_EL2, ICH_AP1R2_EL2, ICH_AP1R3_EL2, ICH_HCR_EL2, ICH_LR_EL2,
    ICH_VMCR_EL2, ICH_VTR_EL2, LocalRegisterCopy, Readable, Writeable, ich_lr_el2_get,
    ich_lr_el2_set, ich_lr_el2_write,
};
use arm_vcpu::ArmHostIrqConfig;
use arm_vgic::{
    CpuInterfaceState, GicV3BackendError, GicVcpuId, HostGicVersion, IntId, InterruptState,
    ListRegisterBacking, ListRegisterState, PhysicalIrqId, Priority, VgicBackendCapabilities,
};
use ax_std::os::arceos::sync::IrqSafeMutex;

const V2_SGI_TOKEN: usize = 1usize << (usize::BITS as usize - 1);
const V2_SGI_SOURCE_SHIFT: usize = 24;

enum HostCpuInterface {
    V2 {
        hypervisor: IrqSafeMutex<arm_gic_driver::v2::HypervisorInterface>,
        trap: arm_gic_driver::v2::TrapOp,
        capabilities: VgicBackendCapabilities,
        irq_config: ArmHostIrqConfig,
    },
    V3 {
        capabilities: VgicBackendCapabilities,
        irq_config: ArmHostIrqConfig,
    },
}

impl HostCpuInterface {
    const fn capabilities(&self) -> VgicBackendCapabilities {
        match self {
            Self::V2 { capabilities, .. } | Self::V3 { capabilities, .. } => *capabilities,
        }
    }

    const fn irq_config(&self) -> ArmHostIrqConfig {
        match self {
            Self::V2 { irq_config, .. } | Self::V3 { irq_config, .. } => *irq_config,
        }
    }
}

static HOST_CPU_INTERFACE: OnceLock<HostCpuInterface> = OnceLock::new();

fn host_cpu_interface() -> Result<&'static HostCpuInterface, GicV3BackendError> {
    // Discovery is the only operation that takes the `rdrive` device lock.
    // The returned register capability is immutable, and every vCPU/IRQ hot
    // path below uses it directly so a hard IRQ cannot re-enter `rdrive` while
    // interrupted code already owns the same non-IRQ-safe device lock.
    HOST_CPU_INTERFACE.get_or_try_init(discover_host_cpu_interface)
}

fn discover_host_cpu_interface() -> Result<HostCpuInterface, GicV3BackendError> {
    super::try_with_gic("inspect host VGIC capabilities", |intc| {
        if let Some(gic) = intc.typed_mut::<arm_gic_driver::v2::Gic>() {
            let irq_config =
                ArmHostIrqConfig::gicv2_mmio(usize::from(gic.gicc_addr())).map_err(|_| {
                    GicV3BackendError::new(
                        "inspect host VGIC capabilities",
                        "the GICv2 CPU-interface address is invalid",
                    )
                })?;
            let interface = gic.hypervisor_interface().ok_or_else(|| {
                GicV3BackendError::new(
                    "inspect host VGIC capabilities",
                    "the GICv2 driver has no GICH virtualization interface",
                )
            })?;
            let capabilities = VgicBackendCapabilities::new(
                HostGicVersion::V2,
                interface.get_list_register_count().min(16),
                interface.priority_bits(),
                false,
            );
            return Ok(HostCpuInterface::V2 {
                hypervisor: IrqSafeMutex::new(interface),
                trap: gic.cpu_interface().trap_operations(),
                capabilities,
                irq_config,
            });
        }
        if intc.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            return Ok(HostCpuInterface::V3 {
                capabilities: VgicBackendCapabilities::new(
                    HostGicVersion::V3,
                    hardware_v3_list_register_count(),
                    (ICH_VTR_EL2.read(ICH_VTR_EL2::PRIBITS) + 1) as u8,
                    false,
                ),
                irq_config: ArmHostIrqConfig::gicv3_sysreg(),
            });
        }
        Err(GicV3BackendError::new(
            "inspect host VGIC capabilities",
            "the registered interrupt controller is neither GICv2 nor GICv3",
        ))
    })?
}

pub(super) fn capabilities() -> Result<VgicBackendCapabilities, GicV3BackendError> {
    host_cpu_interface().map(HostCpuInterface::capabilities)
}

pub(super) fn host_irq_config() -> Result<ArmHostIrqConfig, GicV3BackendError> {
    host_cpu_interface().map(HostCpuInterface::irq_config)
}

pub(super) fn load(
    capabilities: VgicBackendCapabilities,
    vcpu: GicVcpuId,
    state: &CpuInterfaceState,
) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "load virtual CPU interface")?;
    let host = checked_host_cpu_interface(capabilities, "load virtual CPU interface")?;
    match host {
        HostCpuInterface::V2 { hypervisor, .. } => load_v2(hypervisor, state),
        HostCpuInterface::V3 { .. } => load_v3(state),
    }
}

pub(super) fn save(
    capabilities: VgicBackendCapabilities,
    vcpu: GicVcpuId,
    state: &mut CpuInterfaceState,
) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "save virtual CPU interface")?;
    let host = checked_host_cpu_interface(capabilities, "save virtual CPU interface")?;
    match host {
        HostCpuInterface::V2 { hypervisor, .. } => save_v2(hypervisor, state),
        HostCpuInterface::V3 { .. } => save_v3(state),
    }
}

fn checked_host_cpu_interface(
    capabilities: VgicBackendCapabilities,
    operation: &'static str,
) -> Result<&'static HostCpuInterface, GicV3BackendError> {
    let host = host_cpu_interface()?;
    let discovered = host.capabilities();
    if discovered != capabilities {
        return Err(GicV3BackendError::new(
            operation,
            std::format!(
                "cached host capabilities {discovered:?} do not match backend capabilities \
                 {capabilities:?}"
            ),
        ));
    }
    Ok(host)
}

fn load_v2(
    hypervisor: &IrqSafeMutex<arm_gic_driver::v2::HypervisorInterface>,
    state: &CpuInterfaceState,
) -> Result<(), GicV3BackendError> {
    let interface = hypervisor.lock();
    require_lr_count(
        state.list_registers().len(),
        interface.get_list_register_count().min(16),
        "load GICv2 CPU interface",
    )?;
    interface.set_hcr_raw(0);
    for index in 0..interface.get_list_register_count().min(16) {
        let raw = match state.list_registers().get(index).copied().flatten() {
            Some(entry) => encode_v2_list_register(entry)?,
            None => 0,
        };
        interface
            .set_list_register_raw(index, raw)
            .map_err(|detail| GicV3BackendError::new("load GICv2 list register", detail))?;
    }
    interface.set_apr_raw(state.apr()[0] as u32);
    interface.set_vmcr_raw(v2_vmcr(state));
    data_sync_barrier();
    interface.set_hcr_raw(state.hcr() as u32 | 1);
    instruction_sync_barrier();
    Ok(())
}

fn save_v2(
    hypervisor: &IrqSafeMutex<arm_gic_driver::v2::HypervisorInterface>,
    state: &mut CpuInterfaceState,
) -> Result<(), GicV3BackendError> {
    let interface = hypervisor.lock();
    require_lr_count(
        state.list_registers().len(),
        interface.get_list_register_count().min(16),
        "save GICv2 CPU interface",
    )?;
    data_sync_barrier();
    state.set_hcr(interface.hcr_raw() as u64);
    state.set_vmcr(interface.vmcr_raw() as u64);
    let _ = state.set_apr(0, interface.apr_raw() as u64);
    for (index, slot) in state.list_registers_mut().iter_mut().enumerate() {
        let raw = interface.list_register_raw(index).ok_or_else(|| {
            GicV3BackendError::new(
                "save GICv2 list register",
                std::format!("GICH_LR{index} is not implemented"),
            )
        })?;
        *slot = decode_v2_list_register(index, raw, *slot)?;
    }
    for index in 0..interface.get_list_register_count().min(16) {
        interface
            .set_list_register_raw(index, 0)
            .map_err(|detail| GicV3BackendError::new("clear GICv2 list register", detail))?;
    }
    interface.set_hcr_raw(0);
    instruction_sync_barrier();
    Ok(())
}

pub(super) fn acknowledge_host_irq() -> Result<Option<usize>, GicV3BackendError> {
    let host = host_cpu_interface()?;
    let raw_ack = match host {
        HostCpuInterface::V2 { trap, .. } => u32::from(trap.ack()),
        HostCpuInterface::V3 { .. } => arm_gic_driver::v3::ack1().to_u32(),
    };
    Ok(finish_pending_host_irq_with(host, raw_ack))
}

pub(super) fn finish_pending_host_irq(raw_ack: u32) -> Result<Option<usize>, GicV3BackendError> {
    Ok(finish_pending_host_irq_with(host_cpu_interface()?, raw_ack))
}

fn finish_pending_host_irq_with(host: &HostCpuInterface, raw_ack: u32) -> Option<usize> {
    match host {
        HostCpuInterface::V2 { trap, .. } => {
            let ack = arm_gic_driver::v2::Ack::from(raw_ack);
            if ack.is_special() {
                return None;
            }
            trap.eoi(ack);
            Some(match ack {
                arm_gic_driver::v2::Ack::Other(intid) => intid.to_u32() as usize,
                arm_gic_driver::v2::Ack::SGI { intid, cpu_id } => {
                    V2_SGI_TOKEN | (cpu_id << V2_SGI_SOURCE_SHIFT) | intid.to_u32() as usize
                }
            })
        }
        HostCpuInterface::V3 { .. } => {
            // SAFETY: the raw IAR value is masked to the architectural 24-bit
            // INTID field before this checked special-INTID handling.
            let ack = unsafe { arm_gic_driver::IntId::raw(raw_ack & 0x00ff_ffff) };
            if ack.is_special() {
                return None;
            }
            arm_gic_driver::v3::eoi1(ack);
            Some(ack.to_u32() as usize)
        }
    }
}

pub(super) fn deactivate_host_irq(token: usize) -> Result<(), GicV3BackendError> {
    let raw = super::host_irq_intid(token);
    match host_cpu_interface()? {
        HostCpuInterface::V2 { trap, .. } => {
            let intid = arm_gic_driver::checked_intid(raw, 1020).map_err(|_| {
                GicV3BackendError::new(
                    "deactivate acknowledged host IRQ",
                    std::format!("INTID {raw} is outside the GICv2 interrupt range"),
                )
            })?;
            let ack = if token & V2_SGI_TOKEN != 0 {
                arm_gic_driver::v2::Ack::SGI {
                    intid,
                    cpu_id: (token >> V2_SGI_SOURCE_SHIFT) & 0xff,
                }
            } else {
                arm_gic_driver::v2::Ack::Other(intid)
            };
            trap.dir(ack);
        }
        HostCpuInterface::V3 { .. } => {
            // ICC_IAR1_EL1 and ICC_DIR_EL1 carry a 24-bit INTID. Unlike the
            // Distributor-backed SPI limit, this path must retain host LPIs
            // so MSI/MSI-X actions dispatched while a vCPU is running can be
            // deactivated after their leaf handler completes.
            let intid = arm_gic_driver::checked_intid(raw, 1 << 24).map_err(|_| {
                GicV3BackendError::new(
                    "deactivate acknowledged host IRQ",
                    std::format!("INTID {raw} is outside the GICv3 interrupt range"),
                )
            })?;
            arm_gic_driver::v3::dir(intid);
        }
    }
    Ok(())
}

pub(super) fn deactivate_spi(intid: arm_gic_driver::IntId) -> Result<(), GicV3BackendError> {
    match host_cpu_interface()? {
        HostCpuInterface::V2 { trap, .. } => {
            trap.dir(arm_gic_driver::v2::Ack::Other(intid));
        }
        HostCpuInterface::V3 { .. } => arm_gic_driver::v3::dir(intid),
    }
    Ok(())
}

fn v2_vmcr(state: &CpuInterfaceState) -> u32 {
    ((state.v2_enabled() as u32) << 1)
        | ((state.v2_eoi_mode() as u32) << 9)
        | (u32::from(state.v2_binary_point()) << 21)
        | (u32::from(state.v2_priority_mask().raw() >> 3) << 27)
}

fn encode_v2_list_register(entry: ListRegisterState) -> Result<u32, GicV3BackendError> {
    let state = match entry.state() {
        InterruptState::Inactive => 0,
        InterruptState::Pending => 1,
        InterruptState::Active => 2,
        InterruptState::ActivePending => 3,
    };
    let mut raw = entry.intid().raw()
        | (u32::from(entry.priority().raw() >> 3) << 23)
        | (state << 28)
        | (1 << 30);
    match entry.backing() {
        ListRegisterBacking::Software => {
            if entry.maintenance_on_eoi() {
                raw |= 1 << 19;
            }
        }
        ListRegisterBacking::Physical(physical) => {
            let physical = u32::try_from(physical.raw()).map_err(|_| {
                GicV3BackendError::new(
                    "encode GICv2 list register",
                    std::format!("physical IRQ {} does not fit GICH_LR", physical.raw()),
                )
            })?;
            if physical >= 1024 {
                return Err(GicV3BackendError::new(
                    "encode GICv2 list register",
                    std::format!("physical IRQ {physical} exceeds the 10-bit GICH_LR field"),
                ));
            }
            raw |= (physical << 10) | (1 << 31);
        }
    }
    Ok(raw)
}

fn decode_v2_list_register(
    index: usize,
    raw: u32,
    previous: Option<ListRegisterState>,
) -> Result<Option<ListRegisterState>, GicV3BackendError> {
    let state = match (raw >> 28) & 0x3 {
        0 => return Ok(None),
        1 => InterruptState::Pending,
        2 => InterruptState::Active,
        3 => InterruptState::ActivePending,
        _ => unreachable!(),
    };
    let intid = decode_intid(index, raw & 0x3ff, "GICv2")?;
    let priority = Priority::new((((raw >> 23) & 0x1f) << 3) as u8);
    if raw & (1 << 31) == 0 {
        require_software_backing(index, previous, "GICv2")?;
        return Ok(Some(ListRegisterState::new_software(
            intid,
            priority,
            state,
            if raw & (1 << 19) != 0 {
                arm_vgic::TriggerMode::Level
            } else {
                arm_vgic::TriggerMode::Edge
            },
        )));
    }
    let physical = PhysicalIrqId::new(u64::from((raw >> 10) & 0x3ff));
    validate_physical_backing(index, previous, intid, physical, "GICv2")?;
    Ok(Some(ListRegisterState::new_physical(
        intid, priority, state, physical,
    )))
}

fn load_v3(state: &CpuInterfaceState) -> Result<(), GicV3BackendError> {
    require_lr_count(
        state.list_registers().len(),
        hardware_v3_list_register_count(),
        "load GICv3 CPU interface",
    )?;
    let apr_count = hardware_v3_apr_count()?;
    if state.apr()[apr_count..].iter().any(|value| *value != 0) {
        return Err(GicV3BackendError::new(
            "load GICv3 active priorities",
            std::format!("saved state uses APR{apr_count} or above"),
        ));
    }

    ICH_HCR_EL2.set(0);
    instruction_sync_barrier();
    ICH_VMCR_EL2.set(state.vmcr());
    write_v3_apr(state.apr(), apr_count);
    for index in 0..hardware_v3_list_register_count() {
        match state.list_registers().get(index).copied().flatten() {
            Some(entry) => write_v3_list_register(index, entry)?,
            None => ich_lr_el2_set(index, LocalRegisterCopy::new(0)),
        }
    }
    data_sync_barrier();
    ICH_HCR_EL2.set(hardware_v3_hcr_for_load(state.hcr()));
    instruction_sync_barrier();
    Ok(())
}

fn save_v3(state: &mut CpuInterfaceState) -> Result<(), GicV3BackendError> {
    require_lr_count(
        state.list_registers().len(),
        hardware_v3_list_register_count(),
        "save GICv3 CPU interface",
    )?;
    let apr_count = hardware_v3_apr_count()?;
    data_sync_barrier();
    instruction_sync_barrier();
    let result = (|| {
        state.set_hcr(saved_v3_hcr(ICH_HCR_EL2.get(), state.hcr()));
        state.set_vmcr(ICH_VMCR_EL2.get());
        for (index, value) in read_v3_apr(apr_count).into_iter().enumerate() {
            if !state.set_apr(index, value) {
                return Err(GicV3BackendError::new(
                    "save GICv3 active priorities",
                    std::format!("APR index {index} is outside saved state"),
                ));
            }
        }
        for (index, slot) in state.list_registers_mut().iter_mut().enumerate() {
            *slot = read_v3_list_register(index, *slot)?;
        }
        Ok(())
    })();
    for index in 0..hardware_v3_list_register_count() {
        ich_lr_el2_set(index, LocalRegisterCopy::new(0));
    }
    ICH_HCR_EL2.set(0);
    instruction_sync_barrier();
    result
}

fn hardware_v3_list_register_count() -> usize {
    (ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1).min(16)
}

fn hardware_v3_apr_count() -> Result<usize, GicV3BackendError> {
    match ICH_VTR_EL2.read(ICH_VTR_EL2::PREBITS) as usize + 1 {
        5 => Ok(1),
        6 => Ok(2),
        7 => Ok(4),
        count => Err(GicV3BackendError::new(
            "inspect GICv3 active-priority registers",
            std::format!("unsupported preemption-bit count {count}"),
        )),
    }
}

fn hardware_v3_hcr_for_load(saved: u64) -> u64 {
    let adapter_traps = ICH_HCR_EL2::TC::SET.value | ICH_HCR_EL2::TDIR::SET.value;
    let deactivation_trap = if ICH_VTR_EL2.read(ICH_VTR_EL2::TDS) != 0 {
        ICH_HCR_EL2::TDIR::SET.value
    } else {
        ICH_HCR_EL2::TC::SET.value
    };
    (saved & !adapter_traps) | deactivation_trap | ICH_HCR_EL2::EN::SET.value
}

fn saved_v3_hcr(hardware: u64, previous: u64) -> u64 {
    let adapter_traps = ICH_HCR_EL2::TC::SET.value | ICH_HCR_EL2::TDIR::SET.value;
    (hardware & !adapter_traps) | (previous & ICH_HCR_EL2::TDIR::SET.value)
}

fn write_v3_apr(apr: &[u64; 4], count: usize) {
    ICH_AP1R0_EL2.set(apr[0]);
    if count >= 2 {
        ICH_AP1R1_EL2.set(apr[1]);
    }
    if count == 4 {
        ICH_AP1R2_EL2.set(apr[2]);
        ICH_AP1R3_EL2.set(apr[3]);
    }
}

fn read_v3_apr(count: usize) -> [u64; 4] {
    let mut apr = [0; 4];
    apr[0] = ICH_AP1R0_EL2.get();
    if count >= 2 {
        apr[1] = ICH_AP1R1_EL2.get();
    }
    if count == 4 {
        apr[2] = ICH_AP1R2_EL2.get();
        apr[3] = ICH_AP1R3_EL2.get();
    }
    apr
}

fn write_v3_list_register(index: usize, entry: ListRegisterState) -> Result<(), GicV3BackendError> {
    let state = match entry.state() {
        InterruptState::Inactive => ICH_LR_EL2::STATE::Invalid,
        InterruptState::Pending => ICH_LR_EL2::STATE::Pending,
        InterruptState::Active => ICH_LR_EL2::STATE::Active,
        InterruptState::ActivePending => ICH_LR_EL2::STATE::PendingAndActive,
    };
    let mut fields = ICH_LR_EL2::VINTID.val(u64::from(entry.intid().raw()))
        + ICH_LR_EL2::PRIORITY.val(u64::from(entry.priority().raw()))
        + ICH_LR_EL2::GROUP::SET
        + state;
    if entry.maintenance_on_eoi() {
        fields = fields + ICH_LR_EL2::EOI::SET;
    }
    if let ListRegisterBacking::Physical(physical) = entry.backing() {
        let pintid = u16::try_from(physical.raw()).map_err(|_| {
            GicV3BackendError::new(
                "encode GICv3 list register",
                std::format!("physical IRQ {} does not fit PINTID", physical.raw()),
            )
        })?;
        fields = fields + ICH_LR_EL2::HW::SET + ICH_LR_EL2::PINTID.val(u64::from(pintid));
    }
    ich_lr_el2_write(index, fields);
    Ok(())
}

fn read_v3_list_register(
    index: usize,
    previous: Option<ListRegisterState>,
) -> Result<Option<ListRegisterState>, GicV3BackendError> {
    let raw = ich_lr_el2_get(index);
    let state = match raw.read(ICH_LR_EL2::STATE) {
        0 => return Ok(None),
        1 => InterruptState::Pending,
        2 => InterruptState::Active,
        3 => InterruptState::ActivePending,
        value => {
            return Err(GicV3BackendError::new(
                "decode GICv3 list register",
                std::format!("LR{index} has invalid state {value}"),
            ));
        }
    };
    let intid = decode_intid(index, raw.read(ICH_LR_EL2::VINTID) as u32, "GICv3")?;
    let priority = Priority::new(raw.read(ICH_LR_EL2::PRIORITY) as u8);
    if !raw.is_set(ICH_LR_EL2::HW) {
        require_software_backing(index, previous, "GICv3")?;
        return Ok(Some(ListRegisterState::new_software(
            intid,
            priority,
            state,
            if raw.is_set(ICH_LR_EL2::EOI) {
                arm_vgic::TriggerMode::Level
            } else {
                arm_vgic::TriggerMode::Edge
            },
        )));
    }
    let physical = PhysicalIrqId::new(raw.read(ICH_LR_EL2::PINTID));
    validate_physical_backing(index, previous, intid, physical, "GICv3")?;
    Ok(Some(ListRegisterState::new_physical(
        intid, priority, state, physical,
    )))
}

fn decode_intid(index: usize, raw: u32, version: &'static str) -> Result<IntId, GicV3BackendError> {
    IntId::new(raw).map_err(|error| {
        GicV3BackendError::new(
            "decode virtual list register",
            std::format!("{version} LR{index} contains invalid INTID {raw}: {error}"),
        )
    })
}

fn require_software_backing(
    index: usize,
    previous: Option<ListRegisterState>,
    version: &'static str,
) -> Result<(), GicV3BackendError> {
    if previous.is_some_and(|entry| matches!(entry.backing(), ListRegisterBacking::Physical(_))) {
        Err(GicV3BackendError::new(
            "decode virtual list register",
            std::format!("{version} LR{index} lost its physical backing"),
        ))
    } else {
        Ok(())
    }
}

fn validate_physical_backing(
    index: usize,
    previous: Option<ListRegisterState>,
    intid: IntId,
    physical: PhysicalIrqId,
    version: &'static str,
) -> Result<(), GicV3BackendError> {
    let previous = previous.ok_or_else(|| {
        GicV3BackendError::new(
            "decode virtual list register",
            std::format!("{version} LR{index} acquired unexpected physical backing"),
        )
    })?;
    if previous.intid() != intid || previous.backing() != ListRegisterBacking::Physical(physical) {
        return Err(GicV3BackendError::new(
            "decode virtual list register",
            std::format!(
                "{version} LR{index} changed physical identity from {:?}/{:?} to \
                 {intid:?}/{physical:?}",
                previous.intid(),
                previous.backing()
            ),
        ));
    }
    Ok(())
}

fn require_lr_count(
    saved: usize,
    available: usize,
    operation: &'static str,
) -> Result<(), GicV3BackendError> {
    if saved <= available {
        Ok(())
    } else {
        Err(GicV3BackendError::new(
            operation,
            std::format!("saved state has {saved} LRs, hardware exposes {available}"),
        ))
    }
}

fn require_current_vcpu(vcpu: GicVcpuId, operation: &'static str) -> Result<(), GicV3BackendError> {
    match crate::current_vcpu_id() {
        Some(current) if current == vcpu.raw() => Ok(()),
        Some(current) => Err(GicV3BackendError::new(
            operation,
            std::format!("requested vCPU {}, current vCPU is {current}", vcpu.raw()),
        )),
        None => Err(GicV3BackendError::new(
            operation,
            "no vCPU is current on this host CPU",
        )),
    }
}

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes architectural register effects on the
    // current CPU and does not access Rust memory.
    unsafe { std::arch::asm!("isb", options(nostack, preserves_flags)) };
}

fn data_sync_barrier() {
    // SAFETY: `dsb sy` only orders architectural register and memory effects
    // on the current CPU.
    unsafe { std::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}
