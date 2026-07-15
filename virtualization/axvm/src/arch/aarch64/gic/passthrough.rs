//! Physical GIC resource ownership and direct-delivery operations.

use alloc::collections::BTreeMap;

use arm_gic_driver::v3::{ICC_SGI1R_EL1, Writeable};
use arm_vgic::{
    GicAffinity, GicV3BackendError, GicVcpuId, PhysicalInterruptBinding, PhysicalIrqId,
    PhysicalMsiBinding, SgiId,
};
use ax_kspin::SpinNoIrq;
use ax_std::os::arceos::modules::ax_hal::irq::{
    self as host_irq, CpuId, HwIrq, IrqAffinity, IrqDomainId, IrqId,
};
use rdif_msi::{
    Msi, MsiAllocation, MsiDeviceId, MsiEventId, MsiReservationRequest, MsiVectorIndex,
};
use rdrive::DeviceId;

use super::AxvmGicV3Backend;

static PHYSICAL_IRQ_OWNERS: SpinNoIrq<BTreeMap<IrqId, usize>> = SpinNoIrq::new(BTreeMap::new());
static PHYSICAL_MSI_OWNERS: SpinNoIrq<BTreeMap<PhysicalMsiKey, PhysicalMsiOwner>> =
    SpinNoIrq::new(BTreeMap::new());

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PhysicalMsiKey {
    device: u32,
    event: u32,
    lpi: u32,
}

#[derive(Clone)]
enum PhysicalMsiOwner {
    Reserving {
        vm_id: usize,
    },
    Bound {
        vm_id: usize,
        provider: DeviceId,
        allocation: MsiAllocation,
    },
}

impl PhysicalMsiOwner {
    const fn vm_id(&self) -> usize {
        match self {
            Self::Reserving { vm_id } | Self::Bound { vm_id, .. } => *vm_id,
        }
    }
}

pub(super) fn bind_interrupt(
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
    reserve_irq(irq, backend.vm_id)?;
    let result = host_irq::set_affinity(irq, IrqAffinity::Fixed(CpuId(route.host_cpu)))
        .and_then(|()| host_irq::set_enable(irq, true));
    if let Err(error) = result {
        release_irq(irq, backend.vm_id);
        return Err(platform_error("bind physical interrupt", irq, error));
    }
    Ok(())
}

pub(super) fn unbind_interrupt(
    backend: &AxvmGicV3Backend,
    binding: PhysicalInterruptBinding,
) -> Result<(), GicV3BackendError> {
    let irq = decode_irq(binding.host())?;
    require_owner(irq, backend.vm_id)?;
    host_irq::set_enable(irq, false)
        .map_err(|error| platform_error("unbind physical interrupt", irq, error))?;
    release_irq(irq, backend.vm_id);
    Ok(())
}

pub(super) fn set_interrupt_level(
    binding: PhysicalInterruptBinding,
    _asserted: bool,
) -> Result<(), GicV3BackendError> {
    Err(GicV3BackendError::new(
        "set physical interrupt level",
        alloc::format!(
            "host IRQ {:?} is electrically driven by its assigned physical device",
            binding.host()
        ),
    ))
}

pub(super) fn pulse_interrupt(binding: PhysicalInterruptBinding) -> Result<(), GicV3BackendError> {
    Err(GicV3BackendError::new(
        "pulse physical interrupt",
        alloc::format!(
            "host IRQ {:?} is electrically driven by its assigned physical device",
            binding.host()
        ),
    ))
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

pub(super) fn bind_msi(
    backend: &AxvmGicV3Backend,
    binding: PhysicalMsiBinding,
) -> Result<(), GicV3BackendError> {
    let route = backend.route(binding.target())?;
    if route.affinity != binding.affinity() {
        return Err(GicV3BackendError::new(
            "bind physical MSI",
            alloc::format!(
                "binding affinity {:?} does not match vCPU {} fixed affinity {:?}",
                binding.affinity(),
                binding.target().raw(),
                route.affinity
            ),
        ));
    }

    let key = physical_msi_key(binding);
    reserve_msi_key(key, backend.vm_id)?;
    let result = reserve_msi_translation(binding, route.host_cpu);
    match result {
        Ok((provider, allocation)) => {
            PHYSICAL_MSI_OWNERS.lock().insert(
                key,
                PhysicalMsiOwner::Bound {
                    vm_id: backend.vm_id,
                    provider,
                    allocation,
                },
            );
            Ok(())
        }
        Err(error) => {
            PHYSICAL_MSI_OWNERS.lock().remove(&key);
            Err(error)
        }
    }
}

pub(super) fn signal_msi(
    _backend: &AxvmGicV3Backend,
    binding: PhysicalMsiBinding,
) -> Result<(), GicV3BackendError> {
    Err(GicV3BackendError::new(
        "signal physical MSI",
        alloc::format!(
            "physical MSI ({}, {}) must originate from its assigned device",
            binding.device().raw(),
            binding.event().raw()
        ),
    ))
}

pub(super) fn unbind_msi(
    backend: &AxvmGicV3Backend,
    binding: PhysicalMsiBinding,
) -> Result<(), GicV3BackendError> {
    let key = physical_msi_key(binding);
    let owner = PHYSICAL_MSI_OWNERS
        .lock()
        .get(&key)
        .cloned()
        .ok_or_else(|| {
            GicV3BackendError::new(
                "unbind physical MSI",
                alloc::format!("physical MSI {key:?} is not bound"),
            )
        })?;
    if owner.vm_id() != backend.vm_id {
        return Err(GicV3BackendError::new(
            "unbind physical MSI",
            alloc::format!(
                "physical MSI {key:?} is owned by VM {}, not VM {}",
                owner.vm_id(),
                backend.vm_id
            ),
        ));
    }
    let PhysicalMsiOwner::Bound {
        provider,
        allocation,
        ..
    } = owner
    else {
        return Err(GicV3BackendError::new(
            "unbind physical MSI",
            alloc::format!("physical MSI {key:?} reservation is incomplete"),
        ));
    };
    let msi = rdrive::get::<Msi>(provider).map_err(|error| {
        GicV3BackendError::new(
            "unbind physical MSI",
            alloc::format!("ITS provider {provider:?} is unavailable: {error:?}"),
        )
    })?;
    msi.lock()
        .map_err(|error| {
            GicV3BackendError::new(
                "unbind physical MSI",
                alloc::format!("failed to lock ITS provider {provider:?}: {error:?}"),
            )
        })?
        .free(allocation)
        .map_err(|error| {
            GicV3BackendError::new(
                "unbind physical MSI",
                alloc::format!("ITS provider rejected {key:?} release: {error:?}"),
            )
        })?;
    PHYSICAL_MSI_OWNERS.lock().remove(&key);
    Ok(())
}

fn reserve_msi_translation(
    binding: PhysicalMsiBinding,
    host_cpu: usize,
) -> Result<(DeviceId, MsiAllocation), GicV3BackendError> {
    let parent_irq = host_irq::resolve_percpu_irq(HwIrq(binding.lpi().raw())).map_err(|error| {
        platform_error_for_intid("resolve physical LPI", binding.lpi().raw(), error)
    })?;
    if parent_irq.hwirq != HwIrq(binding.lpi().raw()) {
        return Err(GicV3BackendError::new(
            "resolve physical LPI",
            alloc::format!(
                "platform resolved LPI {} to a different hardware line {:?}",
                binding.lpi().raw(),
                parent_irq.hwirq
            ),
        ));
    }
    let provider = rdrive::get_one::<Msi>().ok_or_else(|| {
        GicV3BackendError::new(
            "bind physical MSI",
            "no physical MSI provider is registered",
        )
    })?;
    let provider_id = provider.descriptor().device_id();
    let request = MsiReservationRequest::new(
        MsiDeviceId(binding.device().raw()),
        MsiVectorIndex(0),
        MsiEventId(binding.event().raw()),
        parent_irq,
    )
    .affinity(IrqAffinity::Fixed(CpuId(host_cpu)));
    let allocation = provider
        .lock()
        .map_err(|error| {
            GicV3BackendError::new(
                "bind physical MSI",
                alloc::format!("failed to lock ITS provider {provider_id:?}: {error:?}"),
            )
        })?
        .reserve(request)
        .map_err(|error| {
            GicV3BackendError::new(
                "bind physical MSI",
                alloc::format!(
                    "ITS provider rejected device {}, event {}, LPI {}: {error:?}",
                    binding.device().raw(),
                    binding.event().raw(),
                    binding.lpi().raw()
                ),
            )
        })?;
    Ok((provider_id, allocation))
}

fn reserve_msi_key(key: PhysicalMsiKey, vm_id: usize) -> Result<(), GicV3BackendError> {
    let mut owners = PHYSICAL_MSI_OWNERS.lock();
    match owners.get(&key) {
        None => {
            owners.insert(key, PhysicalMsiOwner::Reserving { vm_id });
            Ok(())
        }
        Some(owner) => Err(GicV3BackendError::new(
            "bind physical MSI",
            alloc::format!(
                "physical MSI {key:?} is already owned by VM {}",
                owner.vm_id()
            ),
        )),
    }
}

fn physical_msi_key(binding: PhysicalMsiBinding) -> PhysicalMsiKey {
    PhysicalMsiKey {
        device: binding.device().raw(),
        event: binding.event().raw(),
        lpi: binding.lpi().raw(),
    }
}

pub(super) fn resolve_physical_irq(intid: u32) -> Result<PhysicalIrqId, GicV3BackendError> {
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

fn reserve_irq(irq: IrqId, vm_id: usize) -> Result<(), GicV3BackendError> {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    match owners.get(&irq).copied() {
        None => {
            owners.insert(irq, vm_id);
            Ok(())
        }
        Some(owner) if owner == vm_id => Err(GicV3BackendError::new(
            "bind physical interrupt",
            alloc::format!("host IRQ {irq:?} is already bound by VM {vm_id}"),
        )),
        Some(owner) => Err(GicV3BackendError::new(
            "bind physical interrupt",
            alloc::format!("host IRQ {irq:?} is owned by VM {owner}"),
        )),
    }
}

fn require_owner(irq: IrqId, vm_id: usize) -> Result<(), GicV3BackendError> {
    match PHYSICAL_IRQ_OWNERS.lock().get(&irq).copied() {
        Some(owner) if owner == vm_id => Ok(()),
        Some(owner) => Err(GicV3BackendError::new(
            "unbind physical interrupt",
            alloc::format!("host IRQ {irq:?} is owned by VM {owner}, not VM {vm_id}"),
        )),
        None => Err(GicV3BackendError::new(
            "unbind physical interrupt",
            alloc::format!("host IRQ {irq:?} is not bound"),
        )),
    }
}

fn release_irq(irq: IrqId, vm_id: usize) {
    let mut owners = PHYSICAL_IRQ_OWNERS.lock();
    if owners.get(&irq) == Some(&vm_id) {
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

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes the SGI system-register write on the
    // current CPU and neither dereferences memory nor changes Rust-visible state.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}
