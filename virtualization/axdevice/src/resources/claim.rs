//! One-shot claim and RAII lease lifecycle for planned resources.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::fmt;

use ax_kspin::SpinRaw;
use axdevice_base::{ControllerInputId, HostIrqId, InterruptControllerId};

use super::{resolved::*, *};
use crate::{DeviceManagerError, DeviceManagerResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimState {
    Planned,
    Issued,
    Leased,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ClaimKey {
    device_id: String,
    slot: ResourceSlot,
}

#[derive(Clone, Debug)]
struct ClaimRecord {
    resource: ResolvedResource,
    state: ClaimState,
}

#[derive(Debug)]
pub(super) struct ResourceClaimDomain {
    device_ids: BTreeSet<String>,
    records: SpinRaw<BTreeMap<ClaimKey, ClaimRecord>>,
}

impl ResourceClaimDomain {
    pub(super) fn new(devices: &BTreeMap<String, super::ResolvedDeviceResources>) -> Arc<Self> {
        let mut records = BTreeMap::new();
        for (device_id, resources) in devices {
            for (slot, resource) in resources.entries() {
                records.insert(
                    ClaimKey {
                        device_id: device_id.clone(),
                        slot: slot.clone(),
                    },
                    ClaimRecord {
                        resource: resource.clone(),
                        state: ClaimState::Planned,
                    },
                );
            }
        }
        Arc::new(Self {
            device_ids: devices.keys().cloned().collect(),
            records: SpinRaw::new(records),
        })
    }

    pub(super) fn issue_device(
        self: &Arc<Self>,
        device_id: &str,
    ) -> DeviceManagerResult<ResourceClaimSet> {
        if !self.device_ids.contains(device_id) {
            return Err(DeviceManagerError::ResourceNotFound {
                operation: "issue planned resource claims",
                resource: format!("planned device {device_id}"),
            });
        }

        let mut records = self.records.lock();
        let keys: Vec<ClaimKey> = records
            .keys()
            .filter(|key| key.device_id == device_id)
            .cloned()
            .collect();
        if let Some((key, state)) = keys.iter().find_map(|key| {
            let state = records.get(key)?.state;
            (state != ClaimState::Planned).then_some((key, state))
        }) {
            return Err(claim_state_error(
                "issue planned resource claims",
                key,
                state,
            ));
        }

        let mut claims = BTreeMap::new();
        for key in keys {
            let record = records
                .get_mut(&key)
                .expect("claim key was collected from the same locked map");
            record.state = ClaimState::Issued;
            claims.insert(key.slot, record.resource.clone());
        }
        Ok(ResourceClaimSet {
            device_id: device_id.into(),
            domain: self.clone(),
            claims,
        })
    }

    fn transition(
        &self,
        key: &ClaimKey,
        from: ClaimState,
        to: ClaimState,
        operation: &'static str,
    ) -> DeviceManagerResult {
        let mut records = self.records.lock();
        let record = records
            .get_mut(key)
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation,
                resource: format!("device {} slot {}", key.device_id, key.slot),
            })?;
        if record.state != from {
            return Err(claim_state_error(operation, key, record.state));
        }
        record.state = to;
        Ok(())
    }

    fn rollback(&self, key: &ClaimKey, from: ClaimState) {
        if let Some(record) = self.records.lock().get_mut(key)
            && record.state == from
        {
            record.state = ClaimState::Planned;
        }
    }

    pub(super) fn verify_leased(&self) -> DeviceManagerResult {
        if let Some((key, state)) = self.records.lock().iter().find_map(|(key, record)| {
            (record.state != ClaimState::Leased).then_some((key, record.state))
        }) {
            return Err(claim_state_error("commit VM resource plan", key, state));
        }
        Ok(())
    }

    pub(super) fn owner_of_controller_input(
        &self,
        controller: InterruptControllerId,
        input: ControllerInputId,
    ) -> Option<String> {
        self.records
            .lock()
            .iter()
            .find_map(|(key, record)| match record.resource {
                ResolvedResource::WiredIrq(irq)
                    if irq.controller() == controller && irq.input() == input =>
                {
                    Some(key.device_id.clone())
                }
                _ => None,
            })
    }

    pub(super) fn owner_of_host_irq(&self, irq: HostIrqId) -> Option<String> {
        self.records
            .lock()
            .iter()
            .find_map(|(key, record)| match record.resource {
                ResolvedResource::HostIrq(existing) if existing == irq => {
                    Some(key.device_id.clone())
                }
                _ => None,
            })
    }
}

/// The one-shot claims issued for one planned device.
pub struct ResourceClaimSet {
    device_id: String,
    domain: Arc<ResourceClaimDomain>,
    claims: BTreeMap<ResourceSlot, ResolvedResource>,
}

impl ResourceClaimSet {
    pub(crate) fn mmio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u64, u64)> {
        self.claim(slot)?
            .mmio()
            .ok_or_else(|| claim_kind_error(&self.device_id, slot, "MMIO"))
    }

    pub(crate) fn pio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u16, u16)> {
        self.claim(slot)?
            .pio()
            .ok_or_else(|| claim_kind_error(&self.device_id, slot, "PIO"))
    }

    pub(crate) fn wired_irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<ResolvedWiredIrq> {
        self.claim(slot)?
            .wired_irq()
            .ok_or_else(|| claim_kind_error(&self.device_id, slot, "wired IRQ"))
    }

    pub(crate) fn host_irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<HostIrqId> {
        self.claim(slot)?
            .host_irq()
            .ok_or_else(|| claim_kind_error(&self.device_id, slot, "host IRQ"))
    }

    pub(crate) fn msi(&self, slot: &ResourceSlot) -> DeviceManagerResult<ResolvedMsi> {
        self.claim(slot)?
            .msi()
            .ok_or_else(|| claim_kind_error(&self.device_id, slot, "MSI"))
    }

    /// Consumes one named claim and returns its lifetime lease.
    pub fn consume(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<ResourceLease> {
        let resource = self.claim(slot)?.clone();
        let key = ClaimKey {
            device_id: self.device_id.clone(),
            slot: slot.clone(),
        };
        self.domain.transition(
            &key,
            ClaimState::Issued,
            ClaimState::Leased,
            "consume planned resource claim",
        )?;
        self.claims
            .remove(slot)
            .expect("the transitioned claim came from the same set");
        Ok(ResourceLease {
            domain: self.domain.clone(),
            key,
            resource,
        })
    }

    /// Returns the number of claims not yet consumed.
    pub fn remaining(&self) -> usize {
        self.claims.len()
    }

    /// Rejects a build that did not consume every required slot.
    pub fn finish(&self) -> DeviceManagerResult {
        if let Some(slot) = self.claims.keys().next() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "finish planned device build",
                detail: format!("device {} did not consume slot {slot}", self.device_id),
            });
        }
        Ok(())
    }

    fn claim(&self, slot: &ResourceSlot) -> DeviceManagerResult<&ResolvedResource> {
        self.claims
            .get(slot)
            .ok_or_else(|| missing_claim_error(&self.device_id, slot))
    }
}

impl fmt::Debug for ResourceClaimSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceClaimSet")
            .field("device_id", &self.device_id)
            .field("remaining", &self.claims.len())
            .finish()
    }
}

impl Drop for ResourceClaimSet {
    fn drop(&mut self) {
        for slot in self.claims.keys() {
            self.domain.rollback(
                &ClaimKey {
                    device_id: self.device_id.clone(),
                    slot: slot.clone(),
                },
                ClaimState::Issued,
            );
        }
    }
}

/// A resource lifetime lease retained by a device registration.
pub struct ResourceLease {
    domain: Arc<ResourceClaimDomain>,
    key: ClaimKey,
    resource: ResolvedResource,
}

impl ResourceLease {
    /// Returns the stable device identifier owning this lease.
    pub fn device_id(&self) -> &str {
        &self.key.device_id
    }

    /// Returns the model-defined slot.
    pub const fn slot(&self) -> &ResourceSlot {
        &self.key.slot
    }

    /// Returns the leased MMIO window.
    pub fn mmio(&self) -> DeviceManagerResult<(u64, u64)> {
        self.resource
            .mmio()
            .ok_or_else(|| lease_kind_error(&self.key, "MMIO"))
    }

    /// Returns the leased port-I/O range.
    pub fn pio(&self) -> DeviceManagerResult<(u16, u16)> {
        self.resource
            .pio()
            .ok_or_else(|| lease_kind_error(&self.key, "PIO"))
    }

    /// Returns the leased wired interrupt.
    pub fn wired_irq(&self) -> DeviceManagerResult<ResolvedWiredIrq> {
        self.resource
            .wired_irq()
            .ok_or_else(|| lease_kind_error(&self.key, "wired IRQ"))
    }

    /// Returns the leased host physical interrupt.
    pub fn host_irq(&self) -> DeviceManagerResult<HostIrqId> {
        self.resource
            .host_irq()
            .ok_or_else(|| lease_kind_error(&self.key, "host IRQ"))
    }

    /// Returns the leased MSI range.
    pub fn msi(&self) -> DeviceManagerResult<ResolvedMsi> {
        self.resource
            .msi()
            .ok_or_else(|| lease_kind_error(&self.key, "MSI"))
    }
}

impl fmt::Debug for ResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceLease")
            .field("device_id", &self.key.device_id)
            .field("slot", &self.key.slot)
            .field("resource", &self.resource)
            .finish()
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.domain.rollback(&self.key, ClaimState::Leased);
    }
}

fn claim_state_error(
    operation: &'static str,
    key: &ClaimKey,
    state: ClaimState,
) -> DeviceManagerError {
    DeviceManagerError::ResourceConflict {
        operation,
        detail: format!(
            "device {} slot {} is in claim state {}",
            key.device_id,
            key.slot,
            claim_state_name(state)
        ),
    }
}

const fn claim_state_name(state: ClaimState) -> &'static str {
    match state {
        ClaimState::Planned => "planned",
        ClaimState::Issued => "issued",
        ClaimState::Leased => "leased",
    }
}

fn lease_kind_error(key: &ClaimKey, expected: &'static str) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "read leased device resource",
        detail: format!(
            "device {} slot {} is not a {expected} resource",
            key.device_id, key.slot
        ),
    }
}

fn claim_kind_error(
    device_id: &str,
    slot: &ResourceSlot,
    expected: &'static str,
) -> DeviceManagerError {
    lease_kind_error(
        &ClaimKey {
            device_id: device_id.into(),
            slot: slot.clone(),
        },
        expected,
    )
}

fn missing_claim_error(device_id: &str, slot: &ResourceSlot) -> DeviceManagerError {
    DeviceManagerError::ResourceNotFound {
        operation: "consume planned resource claim",
        resource: format!("device {device_id} slot {slot}"),
    }
}
