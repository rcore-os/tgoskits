//! Checked ICH register save and restore.

use arm_gic_driver::v3::{
    ICH_AP1R0_EL2, ICH_AP1R1_EL2, ICH_AP1R2_EL2, ICH_AP1R3_EL2, ICH_HCR_EL2, ICH_LR_EL2,
    ICH_VMCR_EL2, ICH_VTR_EL2, LocalRegisterCopy, Readable, Writeable, ich_lr_el2_get,
    ich_lr_el2_set, ich_lr_el2_write,
};
use arm_vgic::{
    CpuInterfaceState, GicV3BackendError, GicVcpuId, IntId, InterruptState, ListRegisterState,
    Priority,
};

pub(super) fn load(vcpu: GicVcpuId, state: &CpuInterfaceState) -> Result<(), GicV3BackendError> {
    require_current_vcpu(vcpu, "load CPU interface")?;
    require_supported_lr_count(state.list_registers().len())?;
    let apr_count = hardware_apr_count()?;
    require_supported_apr_state(state.apr(), apr_count)?;

    ICH_HCR_EL2.set(state.hcr());
    ICH_VMCR_EL2.set(state.vmcr());
    write_apr(state.apr(), apr_count);
    for index in 0..hardware_list_register_count() {
        match state.list_registers().get(index).copied().flatten() {
            Some(entry) => write_list_register(index, entry),
            None => ich_lr_el2_set(index, LocalRegisterCopy::new(0)),
        }
    }
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

    state.set_hcr(ICH_HCR_EL2.get());
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
        *slot = read_list_register(index)?;
    }
    Ok(())
}

pub(super) fn hardware_list_register_count() -> usize {
    (ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1).min(16)
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

fn write_list_register(index: usize, entry: ListRegisterState) {
    let state = match entry.state() {
        InterruptState::Inactive => ICH_LR_EL2::STATE::Invalid,
        InterruptState::Pending => ICH_LR_EL2::STATE::Pending,
        InterruptState::Active => ICH_LR_EL2::STATE::Active,
        InterruptState::ActivePending => ICH_LR_EL2::STATE::PendingAndActive,
    };
    ich_lr_el2_write(
        index,
        ICH_LR_EL2::VINTID.val(entry.intid().raw() as u64)
            + ICH_LR_EL2::PRIORITY.val(entry.priority().raw() as u64)
            + ICH_LR_EL2::GROUP::SET
            + state,
    );
}

fn read_list_register(index: usize) -> Result<Option<ListRegisterState>, GicV3BackendError> {
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
    if value.is_set(ICH_LR_EL2::HW) {
        return Err(GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!("LR{index} unexpectedly contains a hardware-backed interrupt"),
        ));
    }
    let raw = value.read(ICH_LR_EL2::VINTID) as u32;
    let intid = IntId::new(raw).map_err(|error| {
        GicV3BackendError::new(
            "decode CPU interface list register",
            alloc::format!("LR{index} contains invalid INTID {raw}: {error}"),
        )
    })?;
    Ok(Some(ListRegisterState::new(
        intid,
        Priority::new(value.read(ICH_LR_EL2::PRIORITY) as u8),
        state,
    )))
}

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes architectural register effects on the
    // current CPU and neither dereferences memory nor changes Rust-visible state.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}
