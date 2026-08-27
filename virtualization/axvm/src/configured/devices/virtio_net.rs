//! Configured VirtIO MMIO network devices connected by an internal L2 switch.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::String,
    sync::{Arc, Mutex, MutexGuard},
    vec::Vec,
};

use axdevice::*;
use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DmaGrant, InterruptSharing,
    InterruptTrigger, IrqLine, Resource,
};
use axvirtio_common::{GuestMemory, NoGuestMemoryAccessor, VirtioError};
use axvirtio_net::{
    DeviceEvent, NetworkBackend, NetworkBackendError, RxOutcome, VirtioMmioNetDevice,
    VirtioNetConfig,
    switch::{SwitchPort, SwitchPortId, SwitchPortRegistration, VirtualSwitch},
};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x200;
const INGRESS_CAPACITY: usize = 64;

static NEXT_PORT_ID: AtomicUsize = AtomicUsize::new(0);
static INTERNAL_SWITCH: Mutex<Option<Arc<VirtualSwitch>>> = Mutex::new(None);

/// Catalog entry for `[[devices.virtual]] model = "virtio-net"`.
pub const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-net",
    create: create_device_node,
};

pub(super) fn register(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), ConfiguredDeviceError> {
    catalog.register(module_path!(), REGISTRATION)
}

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let guest_mac = parse_mac(request, "guest_mac")?;
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "virtio-net requires a wired interrupt controller".into(),
            })?;
    let model: Arc<dyn DeviceModel> = Arc::new(VirtioNetModel {
        guest_mac,
        controller,
        vm_id: context
            .vm_id()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "virtio-net requires a VM identity".into(),
            })?,
    });
    let mut node = DeviceNodeSpec::virtual_device(id, model);
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

fn parse_mac(request: &VirtualDeviceRequest, key: &str) -> Result<[u8; 6], ConfiguredDeviceError> {
    let values = request
        .options
        .get(key)
        .and_then(toml::Value::as_array)
        .ok_or_else(|| invalid_options(request, format!("missing six-octet array `{key}`")))?;
    if values.len() != 6 {
        return Err(invalid_options(
            request,
            format!("`{key}` must contain exactly six octets"),
        ));
    }
    let mut mac = [0u8; 6];
    for (octet, value) in mac.iter_mut().zip(values) {
        *octet = value
            .as_integer()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| invalid_options(request, format!("`{key}` contains a non-u8 octet")))?;
    }
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(invalid_options(
            request,
            format!("`{key}` must be a nonzero unicast MAC address"),
        ));
    }
    Ok(mac)
}

fn invalid_options(request: &VirtualDeviceRequest, detail: String) -> ConfiguredDeviceError {
    ConfiguredDeviceError::InvalidOptions {
        device: request.id.clone(),
        model: request.model.clone(),
        detail,
    }
}

struct VirtioNetModel {
    guest_mac: [u8; 6],
    controller: axdevice_base::InterruptControllerId,
    vm_id: usize,
}

impl DeviceModel for VirtioNetModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(MMIO_SLOT)?,
                MMIO_SIZE,
                MMIO_SIZE,
                ResourceRequest::Auto,
            )?
            .with_wired_irq(
                ResourceSlot::new(IRQ_SLOT)?,
                self.controller,
                InterruptTrigger::EdgeTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Auto,
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let registers = ResourceSlot::new(MMIO_SLOT).expect("static slot is valid");
        let interrupt = ResourceSlot::new(IRQ_SLOT).expect("static slot is valid");
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("virtio_mmio")
                    .with_compatible("virtio,mmio")
                    .with_register(registers.clone())
                    .with_interrupt(interrupt.clone())
                    .with_empty_property("dma-coherent"),
            )]),
            Some(std::vec![AcpiContributionSpec::Conventional(
                AcpiDeviceSpec::new_indexed("VN", "LNRO0005")
                    .with_register(registers)
                    .with_interrupt(interrupt),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = context.mmio(MMIO_SLOT)?;
        let irq = context.irq(IRQ_SLOT)?;
        let irq_id = irq.input().value() as u32;
        let switch = internal_switch();
        let port_id = SwitchPortId::new(NEXT_PORT_ID.fetch_add(1, Ordering::Relaxed), 0, 0);
        let endpoint = PortEndpoint::new(
            port_id,
            self.guest_mac,
            switch.clone(),
            Arc::new(AxvmWakeTarget { vm_id: self.vm_id }),
        );
        let registration = switch.register_owned(endpoint.clone()).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "register virtio-net switch port",
                detail: format!("{error:?}"),
            }
        })?;
        endpoint.activate();

        let backend = SwitchBackend {
            endpoint: endpoint.clone(),
            switch,
        };
        let model = Arc::new(
            VirtioMmioNetDevice::new(
                GuestPhysAddr::from(base as usize),
                size as usize,
                backend,
                VirtioNetConfig::new(self.guest_mac),
                NoGuestMemoryAccessor,
            )
            .map_err(|error| DeviceManagerError::InvalidConfig {
                operation: "construct virtio-net device",
                detail: format!("{error:?}"),
            })?,
        );
        let grant = DmaGrant::new();
        let device = Arc::new(VirtioNetRuntimeDevice {
            model,
            irq,
            grant: grant.clone(),
            endpoint,
            _registration: registration,
            resources: std::vec![
                Resource::MmioRange { base, size },
                Resource::IrqLine {
                    line: irq_id,
                    trigger: InterruptTrigger::EdgeTriggered,
                },
            ]
            .into_boxed_slice(),
        });
        let mut bundle = DeviceBundle::new();
        bundle.add_dma_pollable_device(device.clone(), device, grant);
        Ok(bundle)
    }
}

fn internal_switch() -> Arc<VirtualSwitch> {
    let mut slot = INTERNAL_SWITCH
        .lock()
        .expect("virtio-net switch mutex poisoned");
    slot.get_or_insert_with(VirtualSwitch::new).clone()
}

#[derive(Clone)]
struct SwitchBackend {
    endpoint: Arc<PortEndpoint>,
    switch: Arc<VirtualSwitch>,
}

impl NetworkBackend for SwitchBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        let _ = self.switch.switch_from_port(self.endpoint.id(), frame);
        Ok(())
    }
}

struct PortEndpoint {
    id: SwitchPortId,
    mac: [u8; 6],
    ingress: Mutex<VecDeque<Vec<u8>>>,
    active: AtomicBool,
    wake_target: Arc<dyn WakeTarget>,
    _switch: Arc<VirtualSwitch>,
}

trait WakeTarget: Send + Sync {
    fn notify(&self);
}

struct AxvmWakeTarget {
    vm_id: usize,
}

impl WakeTarget for AxvmWakeTarget {
    fn notify(&self) {
        // Wake only; vCPU0 polls DMA devices at the top of its next run-loop
        // iteration. Polling synchronously from the sender's device access
        // would let two VM device runtimes re-enter each other.
        if let Err(error) = crate::notify_vm_vcpu(self.vm_id, 0) {
            warn!(
                "failed to notify VM[{}] for virtio-net RX: {error:#}",
                self.vm_id
            );
        }
    }
}

impl PortEndpoint {
    fn new(
        id: SwitchPortId,
        mac: [u8; 6],
        switch: Arc<VirtualSwitch>,
        wake_target: Arc<dyn WakeTarget>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            mac,
            ingress: Mutex::new(VecDeque::new()),
            active: AtomicBool::new(false),
            wake_target,
            _switch: switch,
        })
    }

    fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    fn pop_ingress(&self) -> Option<Vec<u8>> {
        self.lock_ingress().pop_front()
    }

    fn requeue_ingress(&self, frame: Vec<u8>) {
        self.lock_ingress().push_front(frame);
    }

    fn lock_ingress(&self) -> MutexGuard<'_, VecDeque<Vec<u8>>> {
        self.ingress
            .lock()
            .expect("virtio-net ingress mutex poisoned")
    }
}

impl SwitchPort for PortEndpoint {
    fn id(&self) -> SwitchPortId {
        self.id
    }

    fn guest_mac(&self) -> [u8; 6] {
        self.mac
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn deliver_ingress(&self, frame: &[u8]) -> bool {
        let mut ingress = self.lock_ingress();
        if !self.is_active() || ingress.len() >= INGRESS_CAPACITY {
            return false;
        }
        ingress.push_back(frame.into());
        true
    }

    fn notify_ingress(&self) {
        self.wake_target.notify();
    }
}

struct ScopedDeviceMemory<'a> {
    context: &'a mut dyn DeviceContext,
    grant: &'a DmaGrant,
}

impl GuestMemory for ScopedDeviceMemory<'_> {
    fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> Result<(), VirtioError> {
        self.context
            .read_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> Result<(), VirtioError> {
        self.context
            .write_guest_memory(self.grant, guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

struct VirtioNetRuntimeDevice {
    model: Arc<VirtioMmioNetDevice<SwitchBackend, NoGuestMemoryAccessor>>,
    irq: IrqLine,
    grant: DmaGrant,
    endpoint: Arc<PortEndpoint>,
    _registration: SwitchPortRegistration,
    resources: Box<[Resource]>,
}

impl Device for VirtioNetRuntimeDevice {
    fn name(&self) -> &str {
        "virtio-net"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        self.model
            .mmio_read(
                GuestPhysAddr::from(access.address() as usize),
                access.width(),
            )
            .map(|value| value as u64)
            .map_err(map_virtio_error)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let mut memory = ScopedDeviceMemory {
            context,
            grant: &self.grant,
        };
        let event = self
            .model
            .mmio_write_with_memory(
                GuestPhysAddr::from(access.address() as usize),
                access.width(),
                value as usize,
                &mut memory,
            )
            .map_err(map_virtio_error)?;
        self.pulse_if_pending(event)?;
        Ok(())
    }
}

impl DmaPollableDeviceOps for VirtioNetRuntimeDevice {
    fn poll_dma(
        &self,
        _now_ns: u64,
        context: &mut dyn DeviceContext,
        grant: &DmaGrant,
    ) -> DeviceManagerResult {
        let mut memory = ScopedDeviceMemory { context, grant };
        while let Some(frame) = self.endpoint.pop_ingress() {
            match self.model.receive_frame_with_memory(&frame, &mut memory) {
                Ok(RxOutcome::Delivered { notify, .. }) => {
                    if notify {
                        self.irq
                            .pulse()
                            .map_err(|error| DeviceManagerError::InvalidState {
                                operation: "pulse virtio-net RX interrupt",
                                detail: format!("{error}"),
                            })?;
                    }
                }
                Ok(RxOutcome::NoGuestBuffer) => {
                    self.endpoint.requeue_ingress(frame);
                    break;
                }
                Err(error) => {
                    warn!("virtio-net drops an ingress frame: {error:?}");
                }
            }
        }
        Ok(())
    }
}

impl VirtioNetRuntimeDevice {
    fn pulse_if_pending(&self, event: DeviceEvent) -> Result<(), DeviceError> {
        if event == DeviceEvent::InterruptPending {
            self.irq.pulse().map_err(|error| DeviceError::Backend {
                operation: "pulse virtio-net interrupt",
                detail: format!("{error}"),
            })?;
        }
        Ok(())
    }
}

fn map_virtio_error(error: VirtioError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access virtio-net MMIO transport",
        detail: format!("{error:?}"),
    }
}
