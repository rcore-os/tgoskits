//! Stable fingerprints binding planning and device construction.

use alloc::vec::Vec;
use core::fmt;

use axdevice_base::{InterruptSharing, InterruptTrigger};
use axvm_types::EmulatedDeviceConfig;

use crate::resources::{
    DeviceRequirement, DeviceRequirements, MsiResourceRequest, ResourceRequest,
};

/// Deterministic identity of one model configuration and its requirements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceModelFingerprint(u64);

impl DeviceModelFingerprint {
    pub(crate) fn for_model(
        config: &EmulatedDeviceConfig,
        requirements: &DeviceRequirements,
    ) -> Self {
        let mut hash = StableHash::new();
        hash.bytes(config.name.as_bytes());
        hash.usize(config.base_gpa);
        hash.usize(config.length);
        hash.usize(config.irq_id);
        hash.u8(config.emu_type as u8);
        hash.usize(config.cfg_list.len());
        for value in &config.cfg_list {
            hash.usize(*value);
        }
        hash_requirements(&mut hash, requirements);
        Self(hash.finish())
    }

    pub(crate) fn for_requirements(requirements: &DeviceRequirements) -> Self {
        let mut hash = StableHash::new();
        hash_requirements(&mut hash, requirements);
        Self(hash.finish())
    }

    /// Returns the stable numeric fingerprint.
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DeviceModelFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#018x}", self.0)
    }
}

fn hash_requirements(hash: &mut StableHash, requirements: &DeviceRequirements) {
    let mut entries: Vec<&DeviceRequirement> = requirements.entries().iter().collect();
    entries.sort_by_key(|entry| entry.slot());
    hash.usize(entries.len());
    for entry in entries {
        hash.bytes(entry.slot().as_str().as_bytes());
        match entry {
            DeviceRequirement::Mmio {
                size,
                alignment,
                request,
                ..
            } => {
                hash.u8(0);
                hash.u64(*size);
                hash.u64(*alignment);
                hash_u64_request(hash, *request);
            }
            DeviceRequirement::Pio {
                size,
                alignment,
                request,
                ..
            } => {
                hash.u8(1);
                hash.u16(*size);
                hash.u16(*alignment);
                hash_u16_request(hash, *request);
            }
            DeviceRequirement::WiredIrq {
                controller,
                trigger,
                sharing,
                request,
                ..
            } => {
                hash.u8(2);
                hash.usize(controller.value());
                hash.u8(trigger_tag(*trigger));
                hash.u8(sharing_tag(*sharing));
                match request {
                    ResourceRequest::Auto => hash.u8(0),
                    ResourceRequest::Fixed(input) => {
                        hash.u8(1);
                        hash.usize(input.value());
                    }
                }
            }
            DeviceRequirement::Msi { request, .. } => {
                hash.u8(3);
                hash_msi_request(hash, *request);
            }
        }
    }
}

fn hash_msi_request(hash: &mut StableHash, request: MsiResourceRequest) {
    hash.usize(request.controller().value());
    hash.u32(request.its().value());
    hash.u32(request.count());
    match request.device() {
        ResourceRequest::Auto => hash.u8(0),
        ResourceRequest::Fixed(device) => {
            hash.u8(1);
            hash.u32(device.value());
        }
    }
    match request.event() {
        ResourceRequest::Auto => hash.u8(0),
        ResourceRequest::Fixed(event) => {
            hash.u8(1);
            hash.u32(event.value());
        }
    }
    match request.lpi() {
        ResourceRequest::Auto => hash.u8(0),
        ResourceRequest::Fixed(lpi) => {
            hash.u8(1);
            hash.u32(lpi.value());
        }
    }
}

fn hash_u64_request(hash: &mut StableHash, request: ResourceRequest<u64>) {
    match request {
        ResourceRequest::Auto => hash.u8(0),
        ResourceRequest::Fixed(value) => {
            hash.u8(1);
            hash.u64(value);
        }
    }
}

fn hash_u16_request(hash: &mut StableHash, request: ResourceRequest<u16>) {
    match request {
        ResourceRequest::Auto => hash.u8(0),
        ResourceRequest::Fixed(value) => {
            hash.u8(1);
            hash.u16(value);
        }
    }
}

const fn trigger_tag(trigger: InterruptTrigger) -> u8 {
    match trigger {
        InterruptTrigger::EdgeTriggered => 0,
        InterruptTrigger::LevelTriggered => 1,
    }
}

const fn sharing_tag(sharing: InterruptSharing) -> u8 {
    match sharing {
        InterruptSharing::Exclusive => 0,
        InterruptSharing::Shared => 1,
    }
}

struct StableHash(u64);

impl StableHash {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    const fn finish(self) -> u64 {
        self.0
    }
}
