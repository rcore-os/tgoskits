//! Configured VirtIO MMIO network devices connected by an internal L2 switch.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::{
    boxed::Box,
    collections::{BTreeSet, VecDeque},
    format,
    string::String,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
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
    switch::{EgressOutcome, SwitchPort, SwitchPortId, SwitchPortRegistration, VirtualSwitch},
};
use axvm_types::GuestPhysAddr;
use axvmconfig::VirtualDeviceRequest;

use super::EMULATED_MMIO_PLACEMENT_ALIGNMENT;
use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MMIO_SLOT: &str = "mmio";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x200;
const INGRESS_CAPACITY: usize = 64;

static NEXT_PORT_ID: AtomicUsize = AtomicUsize::new(0);
static INTERNAL_SWITCH: Mutex<Option<Arc<VirtualSwitch>>> = Mutex::new(None);

/// MACs owned by a live guest port, so the physical-ingress glue can drop its
/// own transmitted frames instead of looping guest broadcasts back into the
/// switch. Only task contexts (device build/teardown and the protocol
/// executor's ingress check) touch it, never a vCPU MMIO write or a hard IRQ.
static GUEST_MACS: Mutex<BTreeSet<[u8; 6]>> = Mutex::new(BTreeSet::new());

/// Host NIC MACs a guest port must never claim, so the host interface address
/// stays unambiguous on the LAN.
static HOST_MACS: Mutex<BTreeSet<[u8; 6]>> = Mutex::new(BTreeSet::new());

/// Prefix of the AxVM-managed auto-assignment range. The first octet sets the
/// locally-administered bit (`0x02`) and clears the multicast bit, so every
/// derived address is a valid, nonzero unicast NIC address.
const AUTO_MAC_PREFIX: [u8; 3] = [0x02, 0x41, 0x58];

/// Number of auto-assignable addresses in the managed range (`2^24`).
const AUTO_MAC_RANGE: u32 = 1 << 24;

/// Rotating start hint for the auto-assignment search, so consecutive builds do
/// not all rescan the same low slots. Correctness never depends on this value:
/// the reservation lock makes the candidate check and insert atomic.
static NEXT_AUTO_MAC: AtomicU32 = AtomicU32::new(0);

/// Hypervisor-side adapter for one physical host uplink, implemented by the
/// platform glue that owns the host network stack. It is the only channel by
/// which a guest egress frame reaches that selected interface.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum PhysicalUplinkError {
    /// The bounded host queue cannot accept the frame at this instant.
    #[error("physical uplink queue is full or busy")]
    Backpressure,
    /// The adapter rejected a frame that cannot be transmitted as Ethernet.
    #[error("physical uplink rejected an invalid frame")]
    InvalidFrame,
}

pub trait PhysicalUplink: Send + Sync {
    /// Submits one guest-originated frame for physical transmission.
    ///
    /// Returns an error when the bounded host queue cannot accept the frame.
    /// Must not block or allocate, because it runs in the guest's vCPU
    /// MMIO-write context.
    fn submit_guest_egress(&self, frame: &[u8]) -> Result<(), PhysicalUplinkError>;
}

/// Physical uplink adapter, published exactly once before any guest can send.
///
/// `OnceLock` keeps the guest TX path a lock-free load, so a vCPU MMIO write
/// never blocks on the uplink and host tests without a kernel lock runtime can
/// still transmit through this module.
static PHYSICAL_UPLINK: OnceLock<Arc<dyn PhysicalUplink>> = OnceLock::new();

/// Publishes the physical uplink adapter; returns `false` if one is installed.
pub fn install_physical_uplink(uplink: Arc<dyn PhysicalUplink>) -> bool {
    PHYSICAL_UPLINK.set(uplink).is_ok()
}

/// Returns whether this switch fabric already owns a physical uplink.
pub fn physical_uplink_installed() -> bool {
    PHYSICAL_UPLINK.get().is_some()
}

/// Reserves a host-owned MAC that no guest port may claim.
pub fn reserve_host_mac(mac: [u8; 6]) {
    HOST_MACS
        .lock()
        .expect("virtio-net host MAC set poisoned")
        .insert(mac);
}

/// Returns whether `mac` is reserved for the host interface.
fn is_reserved_host_mac(mac: &[u8; 6]) -> bool {
    HOST_MACS
        .lock()
        .expect("virtio-net host MAC set poisoned")
        .contains(mac)
}

/// Derives the auto MAC for `index` in `0..AUTO_MAC_RANGE`.
fn auto_guest_mac(index: u32) -> [u8; 6] {
    [
        AUTO_MAC_PREFIX[0],
        AUTO_MAC_PREFIX[1],
        AUTO_MAC_PREFIX[2],
        (index >> 16) as u8,
        (index >> 8) as u8,
        index as u8,
    ]
}

/// Returns whether `mac` is the shape every auto-assigned address must have: a
/// nonzero, unicast address with the locally-administered bit set.
fn is_local_unicast_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac[0] & 1 == 0 && mac[0] & 2 != 0
}

/// Returns the first index at or after `start` (wrapping within
/// [`AUTO_MAC_RANGE`]) whose auto MAC is neither host-reserved nor already owned
/// by a live guest port. `None` means the managed range is exhausted.
fn next_free_auto_mac_index(
    start: u32,
    host_macs: &BTreeSet<[u8; 6]>,
    guest_macs: &BTreeSet<[u8; 6]>,
) -> Option<u32> {
    let start = start % AUTO_MAC_RANGE;
    (0..AUTO_MAC_RANGE).find_map(|step| {
        let index = (start + step) % AUTO_MAC_RANGE;
        let mac = auto_guest_mac(index);
        (!host_macs.contains(&mac) && !guest_macs.contains(&mac)).then_some(index)
    })
}

/// Renders `mac` as the conventional `xx:xx:xx:xx:xx:xx` text used in logs.
fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Removes a guest MAC when its port is torn down so a VM restart cannot leak
/// stale L2 ownership.
struct GuestMacReservation {
    mac: [u8; 6],
}

impl Drop for GuestMacReservation {
    fn drop(&mut self) {
        GUEST_MACS
            .lock()
            .expect("virtio-net guest MAC set poisoned")
            .remove(&self.mac);
    }
}

/// Why a configured or auto-assigned guest MAC could not be reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum GuestMacReservationError {
    /// The address is reserved for a host interface.
    #[error("guest MAC {0:02x?} is reserved for the host interface")]
    HostOwned([u8; 6]),
    /// Another live guest port already owns the address.
    #[error("guest MAC {0:02x?} is already owned by a live guest port")]
    Duplicate([u8; 6]),
    /// Every address in the managed range is host-reserved or in use.
    #[error("no free locally-administered unicast MAC remains")]
    Exhausted,
}

/// Reserves an explicitly configured `mac` for a live guest port, recording
/// ownership in the same atomic step. A host-owned or already-claimed address
/// fails here, before the switch port is registered, so ownership is never
/// silently overwritten after registration.
fn reserve_guest_mac(mac: [u8; 6]) -> Result<GuestMacReservation, GuestMacReservationError> {
    let mut guest_macs = GUEST_MACS
        .lock()
        .expect("virtio-net guest MAC set poisoned");
    if is_reserved_host_mac(&mac) {
        return Err(GuestMacReservationError::HostOwned(mac));
    }
    // `insert` reports whether the address was absent: one atomic step both
    // detects a duplicate and claims the slot.
    if !guest_macs.insert(mac) {
        return Err(GuestMacReservationError::Duplicate(mac));
    }
    Ok(GuestMacReservation { mac })
}

/// Reserves the next free auto MAC for a live guest port. Holding the guest set
/// lock across the candidate search and the insert is what keeps two concurrent
/// builds from ever choosing the same address.
fn reserve_auto_guest_mac() -> Result<GuestMacReservation, GuestMacReservationError> {
    let mut guest_macs = GUEST_MACS
        .lock()
        .expect("virtio-net guest MAC set poisoned");
    let host_macs = HOST_MACS.lock().expect("virtio-net host MAC set poisoned");
    let start = NEXT_AUTO_MAC.fetch_add(1, Ordering::Relaxed);
    let index = next_free_auto_mac_index(start, &host_macs, &guest_macs)
        .ok_or(GuestMacReservationError::Exhausted)?;
    let mac = auto_guest_mac(index);
    debug_assert!(is_local_unicast_mac(mac));
    guest_macs.insert(mac);
    Ok(GuestMacReservation { mac })
}

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
    let guest_mac = parse_optional_guest_mac(request)?;
    if let Some(mac) = guest_mac
        && is_reserved_host_mac(&mac)
    {
        return Err(invalid_options(
            request,
            "`guest_mac` must differ from the host interface MAC".into(),
        ));
    }
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
    });
    let mut node = DeviceNodeSpec::virtual_device(id, model);
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

/// Parses the optional `guest_mac`. Omitting the key leaves the final address to
/// be auto-assigned at build time; a present key keeps full validation.
fn parse_optional_guest_mac(
    request: &VirtualDeviceRequest,
) -> Result<Option<[u8; 6]>, ConfiguredDeviceError> {
    if request.options.contains_key("guest_mac") {
        parse_mac(request, "guest_mac").map(Some)
    } else {
        Ok(None)
    }
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
    /// Configured guest MAC, or `None` to auto-assign one at build time.
    guest_mac: Option<[u8; 6]>,
    controller: axdevice_base::InterruptControllerId,
}

impl DeviceModel for VirtioNetModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(MMIO_SLOT)?,
                MMIO_SIZE,
                EMULATED_MMIO_PLACEMENT_ALIGNMENT,
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
        let auto_assigned = self.guest_mac.is_none();
        // Reserve the guest MAC in the same step that records ownership, before
        // the port becomes visible to the switch. A configured address that
        // collides with the host or another live guest fails here instead of
        // silently overwriting the ownership set after registration, and the
        // reservation lock keeps two concurrent auto-assignments from picking the
        // same address. Any later failure drops the guard and releases the slot.
        let mac_reservation = match self.guest_mac {
            Some(mac) => reserve_guest_mac(mac),
            None => reserve_auto_guest_mac(),
        }
        .map_err(|error| DeviceManagerError::InvalidConfig {
            operation: "reserve virtio-net guest MAC",
            detail: format!("{error}"),
        })?;
        let guest_mac = mac_reservation.mac;
        let endpoint = PortEndpoint::new(
            port_id,
            guest_mac,
            switch.clone(),
            Arc::new(AxvmWakeTarget {
                port: crate::services::DeviceWorkPort::from_device_signal(
                    context
                        .work_port()
                        .ok_or_else(|| DeviceManagerError::InvalidConfig {
                            operation: "bind virtio-net work port",
                            detail: "runtime work capability is unavailable".into(),
                        })?,
                    context.work_device_id(),
                ),
            }),
        );
        let registration = switch.register_owned(endpoint.clone()).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "register virtio-net switch port",
                detail: format!("{error:?}"),
            }
        })?;
        info!(
            "virtio-net device {} {} guest MAC {}",
            context.work_device_id().as_u32(),
            if auto_assigned {
                "auto-assigned"
            } else {
                "configured with"
            },
            format_mac(&guest_mac),
        );
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
                VirtioNetConfig::new(guest_mac),
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
            _mac_reservation: mac_reservation,
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

/// Returns the process-wide internal L2 switch, creating it on first use.
fn internal_switch() -> Arc<VirtualSwitch> {
    let mut slot = INTERNAL_SWITCH
        .lock()
        .expect("virtio-net switch mutex poisoned");
    slot.get_or_insert_with(VirtualSwitch::new).clone()
}

/// Delivers one physical RX frame into the internal L2 switch.
///
/// Frames whose source MAC is owned by a live guest port are dropped, since
/// they are our own transmitted guest traffic reflected back by the LAN and
/// must not be looped into the fabric a second time.
pub fn switch_from_physical_rx(frame: &[u8]) {
    if frame.len() >= 12 {
        let mut source = [0u8; 6];
        source.copy_from_slice(&frame[6..12]);
        if GUEST_MACS
            .lock()
            .expect("virtio-net guest MAC set poisoned")
            .contains(&source)
        {
            return;
        }
    }
    internal_switch().switch_from_uplink(frame);
}

#[derive(Clone)]
struct SwitchBackend {
    endpoint: Arc<PortEndpoint>,
    switch: Arc<VirtualSwitch>,
}

impl NetworkBackend for SwitchBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        if let EgressOutcome::Forwarded { uplink: true } =
            self.switch.switch_from_port(self.endpoint.id(), frame)
        {
            // Frames that must leave the virtual fabric go to the physical
            // uplink when one is installed. Without a bridge there is no host
            // destination, so the switch still drops the frame locally.
            if let Some(uplink) = PHYSICAL_UPLINK.get() {
                uplink.submit_guest_egress(frame).map_err(|error| {
                    warn!("physical uplink rejected guest egress: {error}");
                    NetworkBackendError::TransmitFailed
                })?;
            }
        }
        Ok(())
    }

    fn rx_queue_notified(&self) {
        // A kick means the guest published more RX buffers, so a frame that
        // `poll_dma` retained after `RxOutcome::NoGuestBuffer` can now be
        // delivered; wake only while such a frame is pending so an idle kick
        // does not force a pointless vCPU exit. The ingress lock must be
        // released before the wake: `notify` publishes a poll request whose
        // later `poll_dma` takes the same lock, so holding it would invert the
        // lock order.
        if self.endpoint.has_retained_ingress() {
            self.endpoint.notify_ingress();
        }
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
    port: crate::services::DeviceWorkPort,
}

impl WakeTarget for AxvmWakeTarget {
    fn notify(&self) {
        // Publish the poll request before kicking vCPU0. Polling synchronously
        // from the sender's device access would let two VM device runtimes
        // re-enter each other.
        if let Err(error) = self.port.notify() {
            debug!("virtio-net RX work port rejected notification: {error}");
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

    /// Reports whether a retained frame waits for a guest RX buffer. Releases
    /// the ingress lock before returning so the caller can wake lock-free.
    fn has_retained_ingress(&self) -> bool {
        !self.lock_ingress().is_empty()
    }
}

#[cfg(test)]
impl PortEndpoint {
    /// Non-blocking ingress probe used by tests to prove the wake path does not
    /// observe the endpoint's ingress lock as held.
    fn try_lock_ingress(&self) -> Result<MutexGuard<'_, VecDeque<Vec<u8>>>, ()> {
        self.ingress.try_lock().map_err(|_| ())
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
    /// Removes this guest MAC from the loop-guard set when the device drops.
    _mac_reservation: GuestMacReservation,
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

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use super::*;

    /// `WakeTarget` that records the wake count and re-enters the endpoint from
    /// inside `notify`, mirroring what a real wake does: `notify_vm` publishes a
    /// device poll request whose later `poll_dma` run takes the ingress lock
    /// again.
    ///
    /// The endpoint is held weakly so the endpoint -> wake-target -> endpoint
    /// ownership chain stays acyclic.
    #[derive(Default)]
    struct RecordingWakeTarget {
        notifications: AtomicUsize,
        ingress_lock_free_in_notify: AtomicBool,
        endpoint: Mutex<Option<Weak<PortEndpoint>>>,
    }

    impl WakeTarget for RecordingWakeTarget {
        fn notify(&self) {
            let endpoint = {
                let slot = self.endpoint.lock().expect("wake endpoint slot poisoned");
                slot.as_ref().and_then(Weak::upgrade)
            };
            if let Some(endpoint) = endpoint {
                // `try_lock` rather than `lock` keeps a lock-order regression
                // from hanging the test thread; it fails the assertion instead.
                if endpoint.try_lock_ingress().is_ok() {
                    self.ingress_lock_free_in_notify
                        .store(true, Ordering::Release);
                }
            }
            self.notifications.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Builds an active port whose wake target records notifications and can
    /// re-enter the endpoint, plus the backend the guest device would drive.
    fn backend_with_recorder() -> (Arc<PortEndpoint>, Arc<RecordingWakeTarget>, SwitchBackend) {
        let switch = VirtualSwitch::new();
        let wake_target = Arc::new(RecordingWakeTarget::default());
        let endpoint = PortEndpoint::new(
            SwitchPortId::new(0, 0, 1),
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            switch.clone(),
            wake_target.clone(),
        );
        *wake_target
            .endpoint
            .lock()
            .expect("wake endpoint slot poisoned") = Some(Arc::downgrade(&endpoint));
        endpoint.activate();
        let backend = SwitchBackend {
            endpoint: endpoint.clone(),
            switch,
        };
        (endpoint, wake_target, backend)
    }

    #[test]
    fn rx_queue_notify_with_empty_ingress_does_not_wake() {
        let (_endpoint, wake_target, backend) = backend_with_recorder();

        backend.rx_queue_notified();

        assert_eq!(wake_target.notifications.load(Ordering::Acquire), 0);
    }

    #[test]
    fn rx_queue_notify_with_retained_frame_wakes_once_and_keeps_frame_in_order() {
        let (endpoint, wake_target, backend) = backend_with_recorder();
        assert!(endpoint.deliver_ingress(&[0xde, 0xad, 0xbe, 0xef]));

        // One RX queue notify edge wakes exactly once: the count is per call,
        // not an artificial de-duplication of repeated kicks.
        backend.rx_queue_notified();

        assert_eq!(wake_target.notifications.load(Ordering::Acquire), 1);
        // The edge only re-publishes the poll request; `poll_dma` stays the
        // sole consumer and still sees the retained frame first.
        assert_eq!(
            endpoint.pop_ingress().as_deref(),
            Some(&[0xde, 0xad, 0xbe, 0xef][..])
        );

        // Once the frame is consumed, the ingress is empty again and a further
        // idle kick must not wake the VM.
        backend.rx_queue_notified();
        assert_eq!(wake_target.notifications.load(Ordering::Acquire), 1);
    }

    #[test]
    fn rx_queue_notify_releases_ingress_lock_before_waking() {
        let (endpoint, wake_target, backend) = backend_with_recorder();
        assert!(endpoint.deliver_ingress(&[0x01, 0x02, 0x03, 0x04]));

        backend.rx_queue_notified();

        assert_eq!(wake_target.notifications.load(Ordering::Acquire), 1);
        assert!(
            wake_target
                .ingress_lock_free_in_notify
                .load(Ordering::Acquire),
            "WakeTarget::notify re-entered the endpoint, so the ingress lock must already be free"
        );
    }

    #[test]
    fn auto_mac_is_nonzero_local_unicast() {
        for index in [0u32, 1, 0x00_00ff, 0x00_ffff, 0x41_5859, AUTO_MAC_RANGE - 1] {
            let mac = auto_guest_mac(index);
            assert!(
                is_local_unicast_mac(mac),
                "auto MAC {mac:02x?} must be a valid nonzero local unicast address"
            );
            assert_eq!(&mac[..3], &AUTO_MAC_PREFIX);
        }
    }

    #[test]
    fn auto_mac_search_skips_host_and_live_guest_addresses() {
        let mut host = BTreeSet::new();
        host.insert(auto_guest_mac(0));
        host.insert(auto_guest_mac(2));
        let mut guest = BTreeSet::new();
        guest.insert(auto_guest_mac(1));

        // 0 is host-owned, 1 is owned by a live guest and 2 is host-owned, so
        // the first free slot is index 3.
        assert_eq!(next_free_auto_mac_index(0, &host, &guest), Some(3));
    }

    #[test]
    fn consecutive_auto_assignments_do_not_repeat() {
        let host = BTreeSet::new();
        let mut guest = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut start = 0u32;
        for _ in 0..64 {
            let index = next_free_auto_mac_index(start, &host, &guest).expect("range is not full");
            let mac = auto_guest_mac(index);
            assert!(seen.insert(mac), "auto MAC {mac:02x?} was assigned twice");
            guest.insert(mac);
            start = index + 1;
        }
        assert_eq!(seen.len(), 64);
    }

    #[test]
    fn auto_mac_search_wraps_within_the_managed_range() {
        let host = BTreeSet::new();
        let mut guest = BTreeSet::new();
        // Occupy the tail so a search that starts at the last slot wraps to base.
        guest.insert(auto_guest_mac(AUTO_MAC_RANGE - 1));
        assert_eq!(
            next_free_auto_mac_index(AUTO_MAC_RANGE - 1, &host, &guest),
            Some(0)
        );
    }

    #[test]
    fn released_auto_mac_is_reusable() {
        let host = BTreeSet::new();
        let mut guest = BTreeSet::new();
        let first = auto_guest_mac(next_free_auto_mac_index(0, &host, &guest).unwrap());
        guest.insert(first);
        let second = auto_guest_mac(next_free_auto_mac_index(1, &host, &guest).unwrap());
        assert_ne!(first, second);

        // Once the first port is torn down its slot is the lowest free one again.
        guest.remove(&first);
        let reused = auto_guest_mac(next_free_auto_mac_index(0, &host, &guest).unwrap());
        assert_eq!(reused, first);
    }

    #[test]
    fn explicit_reservation_rejects_duplicates_and_releases_on_drop() {
        // Outside the auto range so this test cannot race the auto allocator.
        let mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        let first = reserve_guest_mac(mac).expect("a fresh explicit MAC reserves");
        assert!(matches!(
            reserve_guest_mac(mac),
            Err(GuestMacReservationError::Duplicate(_))
        ));

        drop(first);
        let again = reserve_guest_mac(mac).expect("a released MAC reserves again");
        drop(again);
    }

    #[test]
    fn concurrent_auto_reservations_are_unique() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 32;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                std::thread::spawn(|| {
                    let mut held = Vec::new();
                    for _ in 0..PER_THREAD {
                        held.push(
                            reserve_auto_guest_mac().expect("the auto MAC range is not full"),
                        );
                    }
                    held
                })
            })
            .collect();

        let mut reservations = Vec::new();
        for handle in handles {
            reservations.extend(handle.join().expect("auto reservation thread panicked"));
        }

        let macs: BTreeSet<_> = reservations.iter().map(|held| held.mac).collect();
        assert_eq!(
            macs.len(),
            THREADS * PER_THREAD,
            "concurrent auto reservations must never share an address"
        );
        assert!(macs.iter().all(|mac| is_local_unicast_mac(*mac)));

        // Dropping the guards releases every slot for later tests.
        drop(reservations);
    }
}
