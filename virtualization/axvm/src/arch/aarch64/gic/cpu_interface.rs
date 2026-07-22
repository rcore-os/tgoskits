//! Checked ICH register save and restore.

use arm_gic_driver::v3::{
    ICC_CTLR_EL1, ICC_PMR_EL1, ICC_RPR_EL1, ICH_AP1R0_EL2, ICH_AP1R1_EL2, ICH_AP1R2_EL2,
    ICH_AP1R3_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VMCR_EL2, ICH_VTR_EL2, LocalRegisterCopy, Readable,
    Writeable, ich_lr_el2_get, ich_lr_el2_set, ich_lr_el2_write,
};
use arm_vcpu::ArmGicCpuInterfaceRegister;
use arm_vgic::{
    CpuInterfaceState, GicV3BackendError, GicVcpuId, IntId, InterruptState, ListRegisterBacking,
    ListRegisterState, Priority,
};

pub(super) fn load(vcpu: GicVcpuId, state: &CpuInterfaceState) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "load CPU interface")?;
    require_supported_lr_count(state.list_registers().len())?;
    let apr_count = hardware_apr_count()?;
    require_supported_apr_state(state.apr(), apr_count)?;

    // UIE and the other maintenance conditions must not become observable
    // until every register belongs to the vCPU being loaded.
    ICH_HCR_EL2.set(0);
    instruction_sync_barrier();
    ICH_VMCR_EL2.set(state.vmcr());
    write_apr(state.apr(), apr_count);
    for index in 0..hardware_list_register_count() {
        match state.list_registers().get(index).copied().flatten() {
            Some(entry) => write_list_register(index, entry)?,
            None => ich_lr_el2_set(index, LocalRegisterCopy::new(0)),
        }
    }
    data_sync_barrier();
    ICH_HCR_EL2.set(hardware_hcr_for_load(state.hcr()));
    instruction_sync_barrier();
    Ok(())
}

pub(super) fn save(
    vcpu: GicVcpuId,
    state: &mut CpuInterfaceState,
) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "save CPU interface")?;
    require_supported_lr_count(state.list_registers().len())?;
    let apr_count = hardware_apr_count()?;

    // Make guest GIC MMIO effects visible to the ICH system-register view
    // before harvesting LR and VMCR state, as required by the GICv3
    // context-switch protocol.
    data_sync_barrier();
    instruction_sync_barrier();
    let save_result = (|| {
        state.set_hcr(saved_hcr_from_hardware(ICH_HCR_EL2.get(), state.hcr()));
        state.set_vmcr(ICH_VMCR_EL2.get());
        for (index, value) in read_apr(apr_count).into_iter().enumerate() {
            if !state.set_apr(index, value) {
                return Err(GicV3BackendError::new(
                    "save CPU interface",
                    alloc::format!("APR index {index} is outside the saved state"),
                ));
            }
        }
        for (index, slot) in state.list_registers_mut().iter_mut().enumerate() {
            *slot = read_list_register(index, *slot)?;
        }
        Ok(())
    })();

    // The ICH state belongs to this vCPU only while its binding is loaded.
    // In particular, an HW-backed LR must not retain ownership of a physical
    // interrupt while the host handles the exit or schedules another task.
    disable_cpu_interface();
    save_result
}

fn disable_cpu_interface() {
    for index in 0..hardware_list_register_count() {
        ich_lr_el2_set(index, LocalRegisterCopy::new(0));
    }
    ICH_HCR_EL2.set(0);
    instruction_sync_barrier();
}

pub(super) fn hardware_list_register_count() -> usize {
    (ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1).min(16)
}

pub(super) fn read_common_register(
    vcpu: GicVcpuId,
    register: ArmGicCpuInterfaceRegister,
) -> Result<u64, GicV3BackendError> {
    require_current_vcpu(vcpu, "read GICv3 common CPU-interface register")?;
    Ok(match register {
        ArmGicCpuInterfaceRegister::Control => read_icc_ctlr_el1(),
        ArmGicCpuInterfaceRegister::PriorityMask => ICH_VMCR_EL2.read(ICH_VMCR_EL2::VPMR),
        ArmGicCpuInterfaceRegister::RunningPriority => read_icc_rpr_el1()?,
    })
}

pub(super) fn write_common_register(
    vcpu: GicVcpuId,
    register: ArmGicCpuInterfaceRegister,
    value: u64,
) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "write GICv3 common CPU-interface register")?;
    match register {
        ArmGicCpuInterfaceRegister::Control => write_icc_ctlr_el1(value),
        ArmGicCpuInterfaceRegister::PriorityMask => write_icc_pmr_el1(value),
        // ICC_RPR_EL1 is read-only and architecturally ignores trapped writes.
        ArmGicCpuInterfaceRegister::RunningPriority => {}
    }
    instruction_sync_barrier();
    Ok(())
}

fn hardware_hcr_for_load(saved_hcr: u64) -> u64 {
    // A pending LR can become active, and EOImode can change, without an EL2
    // exit. Keep one deactivation trap installed for the entire loaded
    // interval. CPUs without TDIR must use the common-register trap instead.
    let trap_bits = ICH_HCR_EL2::TC::SET.value | ICH_HCR_EL2::TDIR::SET.value;
    let deactivation_trap = if supports_dedicated_deactivation_trap() {
        ICH_HCR_EL2::TDIR::SET.value
    } else {
        ICH_HCR_EL2::TC::SET.value
    };
    (saved_hcr & !trap_bits) | deactivation_trap
}

fn saved_hcr_from_hardware(hardware_hcr: u64, previous_hcr: u64) -> u64 {
    // TC/TDIR selection is a property of the current pCPU. Preserve the
    // controller's shadow TDIR state without leaking a host trap choice into
    // the VM state that may later be loaded on a different pCPU.
    let adapter_traps = ICH_HCR_EL2::TC::SET.value | ICH_HCR_EL2::TDIR::SET.value;
    (hardware_hcr & !adapter_traps) | (previous_hcr & ICH_HCR_EL2::TDIR::SET.value)
}

fn supports_dedicated_deactivation_trap() -> bool {
    ICH_VTR_EL2.read(ICH_VTR_EL2::TDS) != 0
}

fn read_icc_ctlr_el1() -> u64 {
    let vmcr = ICH_VMCR_EL2.get();
    (ICC_CTLR_EL1::PRIBITS.val(ICH_VTR_EL2.read(ICH_VTR_EL2::PRIBITS))
        + ICC_CTLR_EL1::IDBITS.val(ICH_VTR_EL2.read(ICH_VTR_EL2::IDBITS))
        + ICC_CTLR_EL1::A3V.val(ICH_VTR_EL2.read(ICH_VTR_EL2::A3V))
        + ICC_CTLR_EL1::EOIMODE.val(ICH_VMCR_EL2::VEOIM.read(vmcr))
        + ICC_CTLR_EL1::CBPR.val(ICH_VMCR_EL2::VCBPR.read(vmcr)))
    .value
}

fn write_icc_ctlr_el1(value: u64) {
    let update = ICH_VMCR_EL2::VCBPR.val(ICC_CTLR_EL1::CBPR.read(value))
        + ICH_VMCR_EL2::VEOIM.val(ICC_CTLR_EL1::EOIMODE.read(value));
    ICH_VMCR_EL2.set(update.modify(ICH_VMCR_EL2.get()));
}

fn write_icc_pmr_el1(value: u64) {
    let priority = ICC_PMR_EL1::PRIORITY.read(value);
    let update = ICH_VMCR_EL2::VPMR.val(priority);
    ICH_VMCR_EL2.set(update.modify(ICH_VMCR_EL2.get()));
}

fn read_icc_rpr_el1() -> Result<u64, GicV3BackendError> {
    let apr_count = hardware_apr_count()?;
    let minimum_priority_shift = 8 - (ICH_VTR_EL2.read(ICH_VTR_EL2::PREBITS) as usize + 1);
    for (index, register) in read_apr(apr_count).into_iter().enumerate() {
        let active_priorities = register as u32;
        if active_priorities != 0 {
            let priority = index * u32::BITS as usize + active_priorities.trailing_zeros() as usize;
            return Ok(ICC_RPR_EL1::PRIORITY
                .val((priority << minimum_priority_shift) as u64)
                .value);
        }
    }
    Ok(ICC_RPR_EL1::PRIORITY.val(u8::MAX.into()).value)
}

fn require_current_vcpu(vcpu: GicVcpuId, operation: &'static str) -> Result<(), GicV3BackendError> {
    match crate::current_vcpu_id() {
        Some(current) if current == vcpu.raw() => Ok(()),
        Some(current) => Err(GicV3BackendError::new(
            operation,
            alloc::format!(
                "requested vCPU {}, but current vCPU is {current}",
                vcpu.raw()
            ),
        )),
        None => Err(GicV3BackendError::new(
            operation,
            "no vCPU is current on this host CPU",
        )),
    }
}

fn require_supported_lr_count(count: usize) -> Result<(), GicV3BackendError> {
    let available = hardware_list_register_count();
    if count <= available {
        Ok(())
    } else {
        Err(GicV3BackendError::new(
            "access CPU interface list registers",
            alloc::format!("saved state has {count} LRs, but hardware exposes {available}"),
        ))
    }
}

fn hardware_apr_count() -> Result<usize, GicV3BackendError> {
    let preemption_bits = ICH_VTR_EL2.read(ICH_VTR_EL2::PREBITS) as usize + 1;
    match preemption_bits {
        5 => Ok(1),
        6 => Ok(2),
        7 => Ok(4),
        _ => Err(GicV3BackendError::new(
            "inspect CPU interface active-priority registers",
            alloc::format!(
                "ICH_VTR_EL2 reports unsupported preemption-bit count {preemption_bits}"
            ),
        )),
    }
}

fn require_supported_apr_state(apr: &[u64; 4], available: usize) -> Result<(), GicV3BackendError> {
    if apr[available..].iter().all(|value| *value == 0) {
        Ok(())
    } else {
        Err(GicV3BackendError::new(
            "load CPU interface active-priority registers",
            alloc::format!(
                "saved state uses APR{available} or above, but hardware exposes {available} APRs"
            ),
        ))
    }
}

fn write_apr(apr: &[u64; 4], count: usize) {
    ICH_AP1R0_EL2.set(apr[0]);
    if count >= 2 {
        ICH_AP1R1_EL2.set(apr[1]);
    }
    if count == 4 {
        ICH_AP1R2_EL2.set(apr[2]);
        ICH_AP1R3_EL2.set(apr[3]);
    }
}

fn read_apr(count: usize) -> [u64; 4] {
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

fn write_list_register(index: usize, entry: ListRegisterState) -> Result<(), GicV3BackendError> {
    let state = match entry.state() {
        InterruptState::Inactive => ICH_LR_EL2::STATE::Invalid,
        InterruptState::Pending => ICH_LR_EL2::STATE::Pending,
        InterruptState::Active => ICH_LR_EL2::STATE::Active,
        InterruptState::ActivePending => ICH_LR_EL2::STATE::PendingAndActive,
    };
    let mut fields = ICH_LR_EL2::VINTID.val(entry.intid().raw() as u64)
        + ICH_LR_EL2::PRIORITY.val(entry.priority().raw() as u64)
        + ICH_LR_EL2::GROUP::SET
        + state;
    if let ListRegisterBacking::Physical(physical) = entry.backing() {
        let pintid = super::physical_spi::list_register_intid(physical)?;
        fields = fields + ICH_LR_EL2::HW::SET + ICH_LR_EL2::PINTID.val(u64::from(pintid));
    }
    ich_lr_el2_write(index, fields);
    Ok(())
}

fn read_list_register(
    index: usize,
    previous: Option<ListRegisterState>,
) -> Result<Option<ListRegisterState>, GicV3BackendError> {
    let value = ich_lr_el2_get(index);
    let state = match value.read(ICH_LR_EL2::STATE) {
        0 => return Ok(None),
        1 => InterruptState::Pending,
        2 => InterruptState::Active,
        3 => InterruptState::ActivePending,
        raw => {
            return Err(GicV3BackendError::new(
                "decode CPU interface list register",
                alloc::format!("LR{index} has invalid state {raw}"),
            ));
        }
    };
    let raw = value.read(ICH_LR_EL2::VINTID) as u32;
    let intid = IntId::new(raw).map_err(|error| {
        GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!("LR{index} contains invalid INTID {raw}: {error}"),
        )
    })?;
    let priority = Priority::new(value.read(ICH_LR_EL2::PRIORITY) as u8);
    if !value.is_set(ICH_LR_EL2::HW) {
        if previous.is_some_and(|entry| matches!(entry.backing(), ListRegisterBacking::Physical(_)))
        {
            return Err(GicV3BackendError::new(
                "decode CPU interface list register",
                alloc::format!("LR{index} lost its physical backing while still valid"),
            ));
        }
        return Ok(Some(ListRegisterState::new(intid, priority, state)));
    }
    let previous = previous.ok_or_else(|| {
        GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!("LR{index} acquired unexpected physical backing"),
        )
    })?;
    let ListRegisterBacking::Physical(physical) = previous.backing() else {
        return Err(GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!("LR{index} acquired unexpected physical backing"),
        ));
    };
    let expected_pintid = super::physical_spi::list_register_intid(physical)?;
    let actual_pintid = value.read(ICH_LR_EL2::PINTID) as u16;
    if previous.intid() != intid || expected_pintid != actual_pintid {
        return Err(GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!(
                "LR{index} physical identity changed from guest {:?}/host {expected_pintid} to \
                 guest {intid:?}/host {actual_pintid}",
                previous.intid()
            ),
        ));
    }
    Ok(Some(ListRegisterState::new_physical(
        intid, priority, state, physical,
    )))
}

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes architectural register effects on the
    // current CPU and neither dereferences memory nor changes Rust-visible state.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}

fn data_sync_barrier() {
    // SAFETY: `dsb sy` only orders architectural register and memory effects
    // on the current CPU and does not access any Rust object.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}
