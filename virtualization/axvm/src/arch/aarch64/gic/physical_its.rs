//! Physical ITS resource ownership for assigned device translations.

use alloc::collections::BTreeMap;

use arm_vgic::{GicV3BackendError, PhysicalMsiBinding};
use ax_kspin::SpinNoIrq;
use ax_std::os::arceos::modules::ax_hal::irq::{self as host_irq, CpuId, HwIrq, IrqAffinity};
use rdif_msi::{
    Msi, MsiAllocation, MsiDeviceId, MsiEventId, MsiReservationRequest, MsiVectorIndex,
};
use rdrive::DeviceId;

use super::AxvmGicV3Backend;

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
    match reserve_msi_translation(binding, route.host_cpu) {
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

fn platform_error_for_intid(
    operation: &'static str,
    intid: u32,
    error: host_irq::IrqError,
) -> GicV3BackendError {
    GicV3BackendError::new(operation, alloc::format!("GIC INTID {intid}: {error:?}"))
}
