//! Immutable resources produced by the planner.

use alloc::{collections::BTreeMap, format, string::ToString};

use axdevice_base::{
    ControllerInputId, HostIrqId, InterruptControllerId, InterruptSharing, InterruptTrigger, ItsId,
    LpiId, MsiDeviceId, MsiEventId,
};

use super::ResourceSlot;
use crate::{DeviceManagerError, DeviceManagerResult};

/// A resolved wired controller input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedWiredIrq {
    controller: InterruptControllerId,
    input: ControllerInputId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
}

impl ResolvedWiredIrq {
    pub(crate) const fn new(
        controller: InterruptControllerId,
        input: ControllerInputId,
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
    ) -> Self {
        Self {
            controller,
            input,
            trigger,
            sharing,
        }
    }

    /// Returns the controller namespace.
    pub const fn controller(self) -> InterruptControllerId {
        self.controller
    }

    /// Returns the allocated controller input.
    pub const fn input(self) -> ControllerInputId {
        self.input
    }

    /// Returns the planned trigger semantics.
    pub const fn trigger(self) -> InterruptTrigger {
        self.trigger
    }

    /// Returns the planned sharing policy.
    pub const fn sharing(self) -> InterruptSharing {
        self.sharing
    }
}

/// A resolved contiguous MSI event/LPI range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedMsi {
    controller: InterruptControllerId,
    its: ItsId,
    device: MsiDeviceId,
    event: MsiEventId,
    lpi: LpiId,
    count: u32,
}

impl ResolvedMsi {
    pub(crate) const fn new(
        controller: InterruptControllerId,
        its: ItsId,
        device: MsiDeviceId,
        event: MsiEventId,
        lpi: LpiId,
        count: u32,
    ) -> Self {
        Self {
            controller,
            its,
            device,
            event,
            lpi,
            count,
        }
    }

    /// Returns the controller namespace.
    pub const fn controller(self) -> InterruptControllerId {
        self.controller
    }

    /// Returns the ITS namespace.
    pub const fn its(self) -> ItsId {
        self.its
    }

    /// Returns the allocated DeviceID.
    pub const fn device(self) -> MsiDeviceId {
        self.device
    }

    /// Returns the first allocated EventID.
    pub const fn event(self) -> MsiEventId {
        self.event
    }

    /// Returns the first allocated LPI.
    pub const fn lpi(self) -> LpiId {
        self.lpi
    }

    /// Returns the number of consecutive EventID/LPI pairs.
    pub const fn count(self) -> u32 {
        self.count
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedResource {
    Mmio { base: u64, size: u64 },
    Pio { base: u16, size: u16 },
    WiredIrq(ResolvedWiredIrq),
    HostIrq(HostIrqId),
    Msi(ResolvedMsi),
}

/// Named immutable resources produced for one device.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedDeviceResources {
    entries: BTreeMap<ResourceSlot, ResolvedResource>,
}

impl ResolvedDeviceResources {
    /// Returns all resolved slots in stable order.
    pub fn slots(&self) -> alloc::vec::Vec<ResourceSlot> {
        self.entries.keys().cloned().collect()
    }

    /// Returns a resolved MMIO window.
    pub fn mmio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u64, u64)> {
        match self.resource(slot)? {
            ResolvedResource::Mmio { base, size } => Ok((*base, *size)),
            _ => Err(resource_kind_error(slot, "MMIO")),
        }
    }

    /// Iterates all resolved MMIO slots in stable slot order.
    pub fn mmio_ranges(&self) -> impl Iterator<Item = (&ResourceSlot, u64, u64)> {
        self.entries
            .iter()
            .filter_map(|(slot, resource)| match resource {
                ResolvedResource::Mmio { base, size } => Some((slot, *base, *size)),
                _ => None,
            })
    }

    /// Returns a resolved PIO range.
    pub fn pio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u16, u16)> {
        match self.resource(slot)? {
            ResolvedResource::Pio { base, size } => Ok((*base, *size)),
            _ => Err(resource_kind_error(slot, "PIO")),
        }
    }

    /// Iterates all resolved PIO slots in stable slot order.
    pub fn pio_ranges(&self) -> impl Iterator<Item = (&ResourceSlot, u16, u16)> {
        self.entries
            .iter()
            .filter_map(|(slot, resource)| match resource {
                ResolvedResource::Pio { base, size } => Some((slot, *base, *size)),
                _ => None,
            })
    }

    /// Returns a resolved wired interrupt.
    pub fn wired_irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<ResolvedWiredIrq> {
        match self.resource(slot)? {
            ResolvedResource::WiredIrq(irq) => Ok(*irq),
            _ => Err(resource_kind_error(slot, "wired IRQ")),
        }
    }

    /// Returns a resolved host physical interrupt.
    pub fn host_irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<HostIrqId> {
        match self.resource(slot)? {
            ResolvedResource::HostIrq(irq) => Ok(*irq),
            _ => Err(resource_kind_error(slot, "host IRQ")),
        }
    }

    /// Returns a resolved MSI range.
    pub fn msi(&self, slot: &ResourceSlot) -> DeviceManagerResult<ResolvedMsi> {
        match self.resource(slot)? {
            ResolvedResource::Msi(msi) => Ok(*msi),
            _ => Err(resource_kind_error(slot, "MSI")),
        }
    }

    /// Returns a deterministic fingerprint of all resolved slot values.
    pub fn fingerprint(&self) -> u64 {
        let mut value = FNV_OFFSET_BASIS;
        for (slot, resource) in &self.entries {
            hash_bytes(&mut value, slot.as_str().as_bytes());
            hash_resource(&mut value, resource);
        }
        value
    }

    pub(crate) fn insert(
        &mut self,
        slot: ResourceSlot,
        resource: ResolvedResource,
    ) -> DeviceManagerResult {
        if self.entries.insert(slot.clone(), resource).is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "store resolved device resource",
                detail: format!("slot {slot} is resolved twice"),
            });
        }
        Ok(())
    }

    pub(crate) fn entries(&self) -> &BTreeMap<ResourceSlot, ResolvedResource> {
        &self.entries
    }

    fn resource(&self, slot: &ResourceSlot) -> DeviceManagerResult<&ResolvedResource> {
        self.entries
            .get(slot)
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "read resolved device resource",
                resource: slot.to_string(),
            })
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_bytes(value: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *value ^= u64::from(*byte);
        *value = value.wrapping_mul(FNV_PRIME);
    }
}

fn hash_u64(value: &mut u64, field: u64) {
    hash_bytes(value, &field.to_le_bytes());
}

fn hash_resource(value: &mut u64, resource: &ResolvedResource) {
    let fields = match resource {
        ResolvedResource::Mmio { base, size } => [0, *base, *size, 0, 0, 0, 0],
        ResolvedResource::Pio { base, size } => [1, u64::from(*base), u64::from(*size), 0, 0, 0, 0],
        ResolvedResource::WiredIrq(irq) => [
            2,
            irq.controller().value() as u64,
            irq.input().value() as u64,
            irq.trigger() as u64,
            irq.sharing() as u64,
            0,
            0,
        ],
        ResolvedResource::HostIrq(irq) => [3, irq.value() as u64, 0, 0, 0, 0, 0],
        ResolvedResource::Msi(msi) => [
            4,
            msi.controller().value() as u64,
            u64::from(msi.its().value()),
            u64::from(msi.device().value()),
            u64::from(msi.event().value()),
            u64::from(msi.lpi().value()),
            u64::from(msi.count()),
        ],
    };
    for field in fields {
        hash_u64(value, field);
    }
}

fn resource_kind_error(slot: &ResourceSlot, expected: &'static str) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "read resolved device resource",
        detail: format!("slot {slot} is not a {expected} resource"),
    }
}
