//! Physical SPI reservation, handoff, configuration, and restoration.

use alloc::collections::BTreeMap;

use arm_gic_driver::v3::{Affinity as PhysicalAffinity, Trigger as PhysicalTrigger};
use arm_vgic::{
    GicV3BackendError, PhysicalInterruptBinding, PhysicalInterruptConfiguration, PhysicalIrqId,
};
use ax_kspin::SpinNoIrq;
use ax_std::os::arceos::modules::ax_hal::irq::{
    self as host_irq, CpuId, HwIrq, IrqAffinity, IrqDomainId, IrqId,
};

use super::{
    AxvmGicV3Backend,
    physical_gic::{
        checked_physical_spi, instruction_sync_barrier, physical_trigger_mode, with_physical_gic,
    },
};

static PHYSICAL_IRQ_OWNERS: SpinNoIrq<BTreeMap<IrqId, PhysicalIrqOwner>> =
    SpinNoIrq::new(BTreeMap::new());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalIrqOwner {
    vm_id: usize,
    state: PhysicalIrqOwnershipState,
}

impl PhysicalIrqOwner {
    const fn reserved(vm_id: usize) -> Self {
        Self {
            vm_id,
            state: PhysicalIrqOwnershipState::Reserved,
        }
    }

    const fn guest_snapshot(self) -> Option<PhysicalSpiSnapshot> {
        match self.state {
            PhysicalIrqOwnershipState::GuestOwned(snapshot) => Some(snapshot),
            PhysicalIrqOwnershipState::Reserved | PhysicalIrqOwnershipState::Claiming => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhysicalIrqOwnershipState {
    Reserved,
    Claiming,
    GuestOwned(PhysicalSpiSnapshot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalSpiSnapshot {
    enabled: bool,
    pending: bool,
    active: bool,
    priority: u8,
    trigger: PhysicalTrigger,
    route: Option<PhysicalAffinity>,
    group1: bool,
    group_modifier: bool,
}

pub(super) fn bind(
    backend: &AxvmGicV3Backend,
    binding: PhysicalInterruptBinding,
) -> Result<(), GicV3BackendError> {
    let irq = decode_irq(binding.host())?;
    let route = backend.route(binding.target())?;
    if route.affinity != binding.affinity() {
        return Err(GicV3BackendError::new(
            "bind physical interrupt",
            alloc::format!(
                "binding affinity {:?} does not match vCPU {} fixed affinity {:?}",
                binding.affinity(),
                binding.target().raw(),
                route.affinity
            ),
        ));
    }
    reserve_irq(irq, backend.vm_id)
}

pub(super) fn configure(
    backend: &AxvmGicV3Backend,
    binding: PhysicalInterruptBinding,
    configuration: PhysicalInterruptConfiguration,
) -> Result<(), GicV3BackendError> {
    let irq = decode_irq(binding.host())?;
    claim_irq_for_guest(irq, backend.vm_id, "configure physical interrupt")?;
    with_physical_gic("configure physical interrupt", |gic| {
        let intid = checked_physical_spi(gic, irq, "configure physical interrupt")?;
        gic.set_irq_enable(intid, false);
        gic.set_pending(intid, false);
        gic.set_active(intid, false);
        gic.set_group(intid, true, false);
        gic.set_priority(intid, configuration.priority().raw());
        gic.set_cfg(intid, physical_trigger_mode(configuration.trigger()));
        gic.set_pending(intid, configuration.pending());
        gic.set_active(intid, configuration.active());
        instruction_sync_barrier();
        Ok(())
    })
}

pub(super) fn set_enabled(
    backend: &AxvmGicV3Backend,
    binding: PhysicalInterruptBinding,
    enabled: bool,
) -> Result<(), GicV3BackendError> {
    let irq = decode_irq(binding.host())?;
    let target_cpu = enabled
        .then(|| backend.route(binding.target()).map(|route| route.host_cpu))
        .transpose()?;
    claim_irq_for_guest(irq, backend.vm_id, "set physical interrupt enable state")?;
    if let Some(target_cpu) = target_cpu {
        host_irq::set_affinity(irq, IrqAffinity::Fixed(CpuId(target_cpu)))
            .map_err(|error| platform_error("route physical interrupt", irq, error))?;
    }
    host_irq::set_enable(irq, enabled)
        .map_err(|error| platform_error("set physical interrupt enable state", irq, error))
}

pub(super) fn unbind(
    backend: &AxvmGicV3Backend,
    binding: PhysicalInterruptBinding,
) -> Result<(), GicV3BackendError> {
    let irq = decode_irq(binding.host())?;
    let owner = require_owner(irq, backend.vm_id, "unbind physical interrupt")?;
    if matches!(owner.state, PhysicalIrqOwnershipState::Claiming) {
        return Err(GicV3BackendError::new(
            "unbind physical interrupt",
            alloc::format!("host IRQ {irq:?} ownership transition is still in progress"),
        ));
    }
    if let Some(snapshot) = owner.guest_snapshot() {
        restore_physical_spi(irq, snapshot)?;
    }
    release_irq(irq, backend.vm_id);
    Ok(())
}

pub(super) fn set_level(
    binding: PhysicalInterruptBinding,
    _asserted: bool,
) -> Result<(), GicV3BackendError> {
    Err(electrically_driven_error(
        "set physical interrupt level",
        binding,
    ))
}

pub(super) fn pulse(binding: PhysicalInterruptBinding) -> Result<(), GicV3BackendError> {
    Err(electrically_driven_error(
        "pulse physical interrupt",
        binding,
    ))
}

pub(super) fn resolve(intid: u32) -> Result<PhysicalIrqId, GicV3BackendError> {
    resolve_host_irq(intid).map(encode_irq)
}

pub(super) fn resolve_host_irq(intid: u32) -> Result<IrqId, GicV3BackendError> {
    let irq = host_irq::resolve_percpu_irq(HwIrq(intid))
        .map_err(|error| platform_error_for_intid("resolve physical interrupt", intid, error))?;
    if irq.hwirq != HwIrq(intid) {
        return Err(GicV3BackendError::new(
            "resolve physical interrupt",
            alloc::format!(
                "platform resolved GIC INTID {intid} to a different hardware line {:?}",
                irq.hwirq
            ),
        ));
    }
    Ok(irq)
}

fn claim_irq_for_guest(
    irq: IrqId,
    vm_id: usize,
    operation: &'static str,
) -> Result<(), GicV3BackendError> {
    let requires_snapshot = begin_handoff(irq, vm_id, operation)?;
    if !requires_snapshot {
        return Ok(());
    }
    let snapshot = match snapshot_physical_spi(irq) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            restore_reserved_ownership(irq, vm_id);
            return Err(error);
        }
    };
    finish_handoff(irq, vm_id, operation, snapshot)
}

fn begin_handoff(
    irq: IrqId,
    vm_id: usize,
    operation: &'static str,
) -> Result<bool, GicV3BackendError> {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    let owner = owners.get_mut(&irq).ok_or_else(|| {
        GicV3BackendError::new(operation, alloc::format!("host IRQ {irq:?} is not bound"))
    })?;
    if owner.vm_id != vm_id {
        return Err(wrong_owner_error(operation, irq, owner.vm_id, vm_id));
    }
    match owner.state {
        PhysicalIrqOwnershipState::Reserved => {
            owner.state = PhysicalIrqOwnershipState::Claiming;
            Ok(true)
        }
        PhysicalIrqOwnershipState::Claiming => Err(GicV3BackendError::new(
            operation,
            alloc::format!("host IRQ {irq:?} ownership transition is in progress"),
        )),
        PhysicalIrqOwnershipState::GuestOwned(_) => Ok(false),
    }
}

fn finish_handoff(
    irq: IrqId,
    vm_id: usize,
    operation: &'static str,
    snapshot: PhysicalSpiSnapshot,
) -> Result<(), GicV3BackendError> {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    let owner = owners.get_mut(&irq).ok_or_else(|| {
        GicV3BackendError::new(
            operation,
            alloc::format!("host IRQ {irq:?} ownership disappeared during handoff"),
        )
    })?;
    if owner.vm_id != vm_id || !matches!(owner.state, PhysicalIrqOwnershipState::Claiming) {
        return Err(GicV3BackendError::new(
            operation,
            alloc::format!(
                "host IRQ {irq:?} ownership changed unexpectedly to VM {} state {:?}",
                owner.vm_id,
                owner.state
            ),
        ));
    }
    owner.state = PhysicalIrqOwnershipState::GuestOwned(snapshot);
    Ok(())
}

fn snapshot_physical_spi(irq: IrqId) -> Result<PhysicalSpiSnapshot, GicV3BackendError> {
    with_physical_gic("snapshot physical interrupt", |gic| {
        let intid = checked_physical_spi(gic, irq, "snapshot physical interrupt")?;
        let (group1, group_modifier) = gic.group(intid);
        Ok(PhysicalSpiSnapshot {
            enabled: gic.is_irq_enable(intid),
            pending: gic.is_pending(intid),
            active: gic.is_active(intid),
            priority: gic.get_priority(intid),
            trigger: gic.get_cfg(intid),
            route: gic.get_target_cpu(intid),
            group1,
            group_modifier,
        })
    })
}

fn restore_physical_spi(
    irq: IrqId,
    snapshot: PhysicalSpiSnapshot,
) -> Result<(), GicV3BackendError> {
    with_physical_gic("restore physical interrupt", |gic| {
        let intid = checked_physical_spi(gic, irq, "restore physical interrupt")?;
        gic.set_irq_enable(intid, false);
        gic.set_pending(intid, false);
        gic.set_active(intid, false);
        gic.set_group(intid, snapshot.group1, snapshot.group_modifier);
        gic.set_priority(intid, snapshot.priority);
        gic.set_cfg(intid, snapshot.trigger);
        gic.set_target_cpu(intid, snapshot.route);
        gic.set_pending(intid, snapshot.pending);
        gic.set_active(intid, snapshot.active);
        gic.set_irq_enable(intid, snapshot.enabled);
        instruction_sync_barrier();
        Ok(())
    })
}

fn reserve_irq(irq: IrqId, vm_id: usize) -> Result<(), GicV3BackendError> {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    match owners.get(&irq).copied() {
        None => {
            owners.insert(irq, PhysicalIrqOwner::reserved(vm_id));
            Ok(())
        }
        Some(owner) if owner.vm_id == vm_id => Err(GicV3BackendError::new(
            "bind physical interrupt",
            alloc::format!("host IRQ {irq:?} is already bound by VM {vm_id}"),
        )),
        Some(owner) => Err(GicV3BackendError::new(
            "bind physical interrupt",
            alloc::format!("host IRQ {irq:?} is owned by VM {}", owner.vm_id),
        )),
    }
}

fn require_owner(
    irq: IrqId,
    vm_id: usize,
    operation: &'static str,
) -> Result<PhysicalIrqOwner, GicV3BackendError> {
    match PHYSICAL_IRQ_OWNERS.lock().get(&irq).copied() {
        Some(owner) if owner.vm_id == vm_id => Ok(owner),
        Some(owner) => Err(wrong_owner_error(operation, irq, owner.vm_id, vm_id)),
        None => Err(GicV3BackendError::new(
            operation,
            alloc::format!("host IRQ {irq:?} is not bound"),
        )),
    }
}

fn restore_reserved_ownership(irq: IrqId, vm_id: usize) {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    if let Some(owner) = owners.get_mut(&irq)
        && owner.vm_id == vm_id
        && matches!(owner.state, PhysicalIrqOwnershipState::Claiming)
    {
        owner.state = PhysicalIrqOwnershipState::Reserved;
    }
}

fn release_irq(irq: IrqId, vm_id: usize) {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    if owners.get(&irq).is_some_and(|owner| owner.vm_id == vm_id) {
        owners.remove(&irq);
    }
}

fn encode_irq(irq: IrqId) -> PhysicalIrqId {
    PhysicalIrqId::new((u64::from(irq.domain.0) << 32) | u64::from(irq.hwirq.0))
}

fn decode_irq(encoded: PhysicalIrqId) -> Result<IrqId, GicV3BackendError> {
    let raw = encoded.raw();
    if raw >> 48 != 0 {
        return Err(GicV3BackendError::new(
            "decode physical interrupt",
            alloc::format!("physical IRQ encoding {raw:#x} has reserved high bits"),
        ));
    }
    Ok(IrqId::new(
        IrqDomainId((raw >> 32) as u16),
        HwIrq(raw as u32),
    ))
}

fn electrically_driven_error(
    operation: &'static str,
    binding: PhysicalInterruptBinding,
) -> GicV3BackendError {
    GicV3BackendError::new(
        operation,
        alloc::format!(
            "host IRQ {:?} is electrically driven by its assigned physical device",
            binding.host()
        ),
    )
}

fn wrong_owner_error(
    operation: &'static str,
    irq: IrqId,
    owner_vm: usize,
    requested_vm: usize,
) -> GicV3BackendError {
    GicV3BackendError::new(
        operation,
        alloc::format!("host IRQ {irq:?} is owned by VM {owner_vm}, not VM {requested_vm}"),
    )
}

fn platform_error(
    operation: &'static str,
    irq: IrqId,
    error: host_irq::IrqError,
) -> GicV3BackendError {
    GicV3BackendError::new(operation, alloc::format!("host IRQ {irq:?}: {error:?}"))
}

fn platform_error_for_intid(
    operation: &'static str,
    intid: u32,
    error: host_irq::IrqError,
) -> GicV3BackendError {
    GicV3BackendError::new(operation, alloc::format!("GIC INTID {intid}: {error:?}"))
}
