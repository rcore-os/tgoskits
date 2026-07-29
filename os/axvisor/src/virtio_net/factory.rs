//! Device factory that builds the virtio-net adapter from an emulated-device
//! config row.
//!
//! The factory is constructed once per prepare generation by
//! [`crate::virtio_net::VirtioNetPrepareProfile`] with a [`Weak<AxVM>`] and the
//! generation number; it never reads a global slot, guesses the generation, or
//! calls back into AxVisor side-channels (plan 2.4, design §8.3). Each `build`
//! validates the config, resolves the edge IRQ through the build context,
//! constructs the accessor/backend/device, and returns a `DeviceBundle`
//! carrying the adapter. For a raw-uplink device it also claims the shared host
//! uplink runtime and attaches a switch port whose [`SwitchPortId`] embeds the
//! generation, so a reset (new generation) can never reuse a stale port.

use core::sync::atomic::{AtomicU16, Ordering};

use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceManagerError, DeviceRegistration,
};
use axdevice_base::{DeviceError, InterruptTriggerMode, Resource};
use axvirtio_net::{VirtioError, VirtioMmioNetDevice, VirtioNetConfig};
use axvirtio_switch::SwitchPortId;
use axvm::{AxVM, AxvmGuestMemoryAccessor, GuestPhysAddr};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::adapter::VirtioNetDeviceAdapter;
use super::backend::AxvisorNetworkBackend;
use super::config::{BackendSpec, VirtioNetDeviceSpec};
use super::raw_uplink::HostUplinkRuntime;
use super::worker::{VirtioNetEndpoint, VirtioNetEndpointKey};

/// Builds virtio-net MMIO device adapters for one VM at one prepare generation.
pub struct VirtioNetDeviceFactory {
    vm: Weak<AxVM>,
    generation: usize,
    /// Monotonic per-generation index assigning `device_index` to each virtio-net
    /// device built, so one VM may later own several NICs (design §3.1).
    next_device_index: AtomicU16,
}

impl VirtioNetDeviceFactory {
    /// Creates a factory whose built devices reach `vm` for guest memory and IRQ
    /// routing, and whose switch ports are scoped to `generation`.
    pub fn new(vm: Weak<AxVM>, generation: usize) -> Self {
        Self {
            vm,
            generation,
            next_device_index: AtomicU16::new(0),
        }
    }
}

impl DeviceFactory for VirtioNetDeviceFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioNet
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &DeviceBuildContext<'_>,
    ) -> Result<DeviceBundle, DeviceManagerError> {
        let spec = VirtioNetDeviceSpec::from_config(config)?;
        let irq = context.resolve_irq(spec.irq_id, InterruptTriggerMode::EdgeTriggered)?;
        let accessor = AxvmGuestMemoryAccessor::new(self.vm.clone());

        let (backend, attachment) = match spec.backend {
            BackendSpec::DeterministicPeer => {
                (AxvisorNetworkBackend::deterministic(spec.mac), None)
            }
            BackendSpec::RawUplink { mac } => {
                let vm_id = self.vm.upgrade().map(|vm| vm.id()).unwrap_or(0);
                let device_index = self.next_device_index.fetch_add(1, Ordering::Relaxed);
                let port_id = SwitchPortId::new(vm_id, self.generation, device_index);
                let runtime = HostUplinkRuntime::claim_or_get(mac).map_err(|detail| {
                    DeviceManagerError::InvalidConfig {
                        operation: "virtio-net raw uplink",
                        detail,
                    }
                })?;
                let (endpoint, attachment) =
                    runtime.attach_port(port_id, spec.mac).map_err(|detail| {
                        DeviceManagerError::InvalidConfig {
                            operation: "virtio-net switch port",
                            detail,
                        }
                    })?;
                let backend = AxvisorNetworkBackend::raw_uplink(endpoint);
                (backend, Some(attachment))
            }
        };
        let net_config = VirtioNetConfig::new(spec.mac);
        let device = Arc::new(
            VirtioMmioNetDevice::new(
                GuestPhysAddr::from(spec.base_gpa),
                spec.length,
                backend.clone(),
                net_config,
                accessor,
            )
            .map_err(construct_error)?,
        );
        let resources = Box::new([Resource::MmioRange {
            base: spec.base_gpa as u64,
            size: spec.length as u64,
        }]);
        let adapter = Arc::new(VirtioNetDeviceAdapter::new(
            spec.name, device, irq, backend, attachment, resources,
        ));
        let endpoint: Arc<VirtioNetEndpoint> = Arc::new(VirtioNetEndpoint::from_adapter(&adapter));
        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(adapter));
        bundle.with_service::<VirtioNetEndpointKey>(endpoint)
    }
}

fn construct_error(error: VirtioError) -> DeviceManagerError {
    DeviceManagerError::Device(DeviceError::InvalidInput {
        operation: "virtio-net construct",
        detail: alloc::format!("{error:?}"),
    })
}
