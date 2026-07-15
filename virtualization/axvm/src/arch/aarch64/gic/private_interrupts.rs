//! Per-vCPU SGI/PPI context switching for passthrough guests.

use alloc::collections::BTreeMap;

use arm_gic_driver::{
    checked_intid,
    v3::{Gic as PhysicalGicV3, ICC_SGI1R_EL1, Writeable},
};
use arm_vgic::{
    GicAffinity, GicV3BackendError, GicVcpuId, IntId, Priority, PrivateInterruptMask,
    PrivateInterruptState, SgiId,
};

use super::{
    AxvmGicV3Backend,
    physical_gic::{
        instruction_sync_barrier, physical_trigger, physical_trigger_mode, vgic_state_error,
        with_physical_gic,
    },
};

pub(super) fn load(
    backend: &AxvmGicV3Backend,
    vcpu: GicVcpuId,
    owned: PrivateInterruptMask,
    guest: &PrivateInterruptState,
) -> Result<PrivateInterruptState, GicV3BackendError> {
    require_current_route(backend, vcpu, "load physical private interrupts")?;
    with_physical_gic("load physical private interrupts", |gic| {
        let host = snapshot(gic)?;
        let cpu = gic.cpu_interface();
        for raw in 0..32u32 {
            let (_, physical) = private_intids(raw, "load physical private interrupts")?;
            if owned.raw() & (1 << raw) == 0 {
                cpu.set_irq_enable(physical, false);
            }
        }
        apply(gic, owned, guest)?;
        instruction_sync_barrier();
        Ok(host)
    })
}

pub(super) fn save(
    backend: &AxvmGicV3Backend,
    vcpu: GicVcpuId,
    owned: PrivateInterruptMask,
    guest: &mut PrivateInterruptState,
    host: &PrivateInterruptState,
) -> Result<(), GicV3BackendError> {
    require_current_route(backend, vcpu, "save physical private interrupts")?;
    with_physical_gic("save physical private interrupts", |gic| {
        let physical_guest = snapshot(gic)?;
        merge(guest, &physical_guest, owned)?;
        apply(gic, owned, host)?;

        // A host-owned timer may become pending while the guest is running.
        // Restore its enable bit without clearing that newly latched state.
        let host_owned = PrivateInterruptMask::ALL.raw() & !owned.raw();
        let cpu = gic.cpu_interface();
        for raw in 0..32u32 {
            if host_owned & (1 << raw) == 0 {
                continue;
            }
            let (_, physical) = private_intids(raw, "restore host private interrupts")?;
            cpu.set_irq_enable(physical, host.enabled_mask() & (1 << raw) != 0);
        }
        instruction_sync_barrier();
        Ok(())
    })
}

pub(super) fn synchronize(
    backend: &AxvmGicV3Backend,
    vcpu: GicVcpuId,
    owned: PrivateInterruptMask,
    guest: &mut PrivateInterruptState,
) -> Result<(), GicV3BackendError> {
    require_current_route(backend, vcpu, "synchronize physical private interrupts")?;
    with_physical_gic("synchronize physical private interrupts", |gic| {
        let physical_guest = snapshot(gic)?;
        merge(guest, &physical_guest, owned)
    })
}

pub(super) fn update(
    backend: &AxvmGicV3Backend,
    vcpu: GicVcpuId,
    owned: PrivateInterruptMask,
    guest: &PrivateInterruptState,
) -> Result<(), GicV3BackendError> {
    require_current_route(backend, vcpu, "update physical private interrupts")?;
    with_physical_gic("update physical private interrupts", |gic| {
        apply(gic, owned, guest)?;
        instruction_sync_barrier();
        Ok(())
    })
}

pub(super) fn send_sgi(
    backend: &AxvmGicV3Backend,
    source: GicVcpuId,
    sgi: SgiId,
    targets: &[GicAffinity],
) -> Result<(), GicV3BackendError> {
    backend.route(source)?;
    if crate::current_vcpu_id() != Some(source.raw()) {
        return Err(GicV3BackendError::new(
            "send physical SGI",
            alloc::format!("vCPU {} is not current on this host CPU", source.raw()),
        ));
    }
    for affinity in targets {
        if !backend
            .routes
            .values()
            .any(|route| route.affinity == *affinity)
        {
            return Err(GicV3BackendError::new(
                "send physical SGI",
                alloc::format!("affinity {affinity:?} is not owned by this VM"),
            ));
        }
    }

    let mut groups: BTreeMap<(u8, u8, u8, u8), u16> = BTreeMap::new();
    for affinity in targets {
        let selector = affinity.aff0() / 16;
        let bit = affinity.aff0() % 16;
        *groups
            .entry((affinity.aff3(), affinity.aff2(), affinity.aff1(), selector))
            .or_default() |= 1 << bit;
    }
    for ((aff3, aff2, aff1, selector), target_list) in groups {
        ICC_SGI1R_EL1.write(
            ICC_SGI1R_EL1::TARGETLIST.val(target_list as u64)
                + ICC_SGI1R_EL1::AFF1.val(aff1 as u64)
                + ICC_SGI1R_EL1::INTID.val(sgi.raw() as u64)
                + ICC_SGI1R_EL1::AFF2.val(aff2 as u64)
                + ICC_SGI1R_EL1::RS.val(selector as u64)
                + ICC_SGI1R_EL1::AFF3.val(aff3 as u64),
        );
    }
    instruction_sync_barrier();
    Ok(())
}

fn require_current_route(
    backend: &AxvmGicV3Backend,
    vcpu: GicVcpuId,
    operation: &'static str,
) -> Result<(), GicV3BackendError> {
    let route = backend.route(vcpu)?;
    if crate::current_vcpu_id() != Some(vcpu.raw()) {
        return Err(GicV3BackendError::new(
            operation,
            alloc::format!("vCPU {} is not current on this host CPU", vcpu.raw()),
        ));
    }
    let current_cpu = ax_std::os::arceos::modules::ax_hal::percpu::this_cpu_id();
    if current_cpu != route.host_cpu {
        return Err(GicV3BackendError::new(
            operation,
            alloc::format!(
                "vCPU {} is fixed to host CPU {}, but is running on CPU {current_cpu}",
                vcpu.raw(),
                route.host_cpu
            ),
        ));
    }
    Ok(())
}

fn snapshot(gic: &PhysicalGicV3) -> Result<PrivateInterruptState, GicV3BackendError> {
    let cpu = gic.cpu_interface();
    let mut snapshot = PrivateInterruptState::new();
    for raw in 0..32u32 {
        let (virtual_id, physical_id) = private_intids(raw, "snapshot private interrupts")?;
        let (group1, modifier) = cpu.group(physical_id);
        snapshot
            .set_enabled(virtual_id, cpu.is_irq_enable(physical_id))
            .map_err(vgic_state_error)?;
        snapshot
            .set_pending(virtual_id, cpu.is_pending(physical_id))
            .map_err(vgic_state_error)?;
        snapshot
            .set_active(virtual_id, cpu.is_active(physical_id))
            .map_err(vgic_state_error)?;
        snapshot
            .set_group1(virtual_id, group1)
            .map_err(vgic_state_error)?;
        snapshot
            .set_group_modifier(virtual_id, modifier)
            .map_err(vgic_state_error)?;
        snapshot
            .set_trigger(virtual_id, physical_trigger(cpu.get_cfg(physical_id)))
            .map_err(vgic_state_error)?;
        snapshot
            .set_priority(virtual_id, Priority::new(cpu.get_priority(physical_id)))
            .map_err(vgic_state_error)?;
    }
    Ok(snapshot)
}

fn apply(
    gic: &PhysicalGicV3,
    owned: PrivateInterruptMask,
    state: &PrivateInterruptState,
) -> Result<(), GicV3BackendError> {
    let cpu = gic.cpu_interface();
    for raw in 0..32u32 {
        if owned.raw() & (1 << raw) == 0 {
            continue;
        }
        let (_, physical) = private_intids(raw, "apply private interrupts")?;
        cpu.set_irq_enable(physical, false);
        cpu.set_pending(physical, false);
        cpu.set_active(physical, false);
        cpu.set_group(
            physical,
            state.group1_mask() & (1 << raw) != 0,
            state.group_modifier_mask() & (1 << raw) != 0,
        );
        cpu.set_priority(physical, state.priorities()[raw as usize]);
        let trigger = if state.edge_triggered_mask() & (1 << raw) != 0 {
            arm_vgic::TriggerMode::Edge
        } else {
            arm_vgic::TriggerMode::Level
        };
        cpu.set_cfg(physical, physical_trigger_mode(trigger));
        cpu.set_pending(physical, state.pending_mask() & (1 << raw) != 0);
        cpu.set_active(physical, state.active_mask() & (1 << raw) != 0);
        cpu.set_irq_enable(physical, state.enabled_mask() & (1 << raw) != 0);
    }
    Ok(())
}

fn merge(
    destination: &mut PrivateInterruptState,
    source: &PrivateInterruptState,
    owned: PrivateInterruptMask,
) -> Result<(), GicV3BackendError> {
    for raw in 0..32u32 {
        if owned.raw() & (1 << raw) == 0 {
            continue;
        }
        let (intid, _) = private_intids(raw, "merge private interrupts")?;
        destination
            .set_enabled(intid, source.enabled_mask() & (1 << raw) != 0)
            .map_err(vgic_state_error)?;
        destination
            .set_pending(intid, source.pending_mask() & (1 << raw) != 0)
            .map_err(vgic_state_error)?;
        destination
            .set_active(intid, source.active_mask() & (1 << raw) != 0)
            .map_err(vgic_state_error)?;
        destination
            .set_group1(intid, source.group1_mask() & (1 << raw) != 0)
            .map_err(vgic_state_error)?;
        destination
            .set_group_modifier(intid, source.group_modifier_mask() & (1 << raw) != 0)
            .map_err(vgic_state_error)?;
        destination
            .set_trigger(
                intid,
                if source.edge_triggered_mask() & (1 << raw) != 0 {
                    arm_vgic::TriggerMode::Edge
                } else {
                    arm_vgic::TriggerMode::Level
                },
            )
            .map_err(vgic_state_error)?;
        destination
            .set_priority(intid, Priority::new(source.priorities()[raw as usize]))
            .map_err(vgic_state_error)?;
    }
    Ok(())
}

fn private_intids(
    raw: u32,
    operation: &'static str,
) -> Result<(IntId, arm_gic_driver::IntId), GicV3BackendError> {
    let virtual_id = IntId::new(raw).map_err(vgic_state_error)?;
    let physical_id = checked_intid(raw, 32).map_err(|error| {
        GicV3BackendError::new(
            operation,
            alloc::format!("invalid physical private INTID {raw}: {error:?}"),
        )
    })?;
    Ok((virtual_id, physical_id))
}
