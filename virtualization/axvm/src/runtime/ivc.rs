//! Inter-VM communication (IVC) channels owned by a VM manager instance.
//!
//! The host-side channel table, the shared-page backing, and the guest aperture
//! services live in submodules. The manager instance owns the table instead of a
//! process-global map, and the table is keyed by the publisher's [`VmKey`] so a
//! recycled numeric VM ID cannot alias a stale channel. The guest-visible ABI is
//! unchanged: the numeric publisher VM ID and channel key header bytes are still
//! written into the shared region.
//!
//! [`VmKey`]: crate::VmKey

mod aperture;
mod backing;
mod manager;

pub(crate) use aperture::{
    IvcApertureAllocatorKey, IvcAperturePool, IvcNotifyEndpointKey, WiredIvcNotifyEndpoint,
};
#[cfg(test)]
use axdevice::{DeviceManagerResult, DeviceRuntime};
pub(crate) use manager::{
    IvcBinding, IvcDetach, IvcEndpointPlan, IvcManager, MAX_IVC_CHANNEL_SIZE,
};

#[cfg(test)]
use crate::GuestPhysAddr;

/// Allocates one guest binding inside a VM's IVC aperture service.
///
/// Retained for the configured-device regression test, which resolves the IVC
/// aperture through the device runtime instead of through a manager attach
/// token.
#[cfg(test)]
pub(crate) fn alloc_guest_binding(
    devices: &DeviceRuntime,
    size: usize,
) -> DeviceManagerResult<GuestPhysAddr> {
    devices.service::<IvcApertureAllocatorKey>()?.allocate(size)
}
