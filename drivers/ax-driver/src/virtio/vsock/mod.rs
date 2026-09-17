//! VirtIO vsock transport, connection, and IRQ capability adapter.

extern crate alloc;

use alloc::format;

use rdif_vsock::{
    VsockAddr as RdifVsockAddr, VsockConnId, VsockError, VsockEvent, VsockIrqEndpoints,
};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
use virtio_drivers::{
    Error as VirtIoError,
    device::socket::{
        DisconnectReason, SocketError, VirtIOSocket, VsockAddr, VsockConnectionManager,
        VsockEvent as RawVsockEvent, VsockEventType,
    },
    transport::{DeviceType, Transport},
};

use crate::{
    BindingInfo, binding_info_from_fdt, virtio::VirtIoHalImpl, vsock::PlatformDeviceVsock,
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

const DEFAULT_RX_BUFFER_CAPACITY: u32 = 32 * 1024;

mod irq;

use irq::SharedVsockTransport;

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO Socket",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport =
        crate::pci::take_virtio_transport_masked(probe.endpoint_mut(), DeviceType::Socket)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

pub fn register_fdt_transport<T: Transport + 'static>(
    info: &rdrive::register::FdtInfo<'_>,
    platform_device: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(platform_device, transport, binding_info_from_fdt(info)?)
}

pub fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = VirtIoVsock::new(transport).map_err(|err| {
        OnProbeError::other(format!("failed to initialize virtio-socket: {err:?}"))
    })?;
    plat_dev.register_vsock_with_info(dev, info)?;
    log::info!("registered virtio socket device with mandatory IRQ binding");
    Ok(())
}

struct VirtIoVsock<T: Transport + 'static> {
    inner: VsockConnectionManager<VirtIoHalImpl, SharedVsockTransport<T>>,
    pending_event: Option<Result<VsockEvent, VsockError>>,
    irq_endpoints: Option<VsockIrqEndpoints>,
}

unsafe impl<T: Transport + 'static> Send for VirtIoVsock<T> {}

impl<T: Transport + 'static> VirtIoVsock<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let (transport, irq_endpoints) = SharedVsockTransport::new(transport);
        let socket = VirtIOSocket::<VirtIoHalImpl, _>::new(transport)?;
        Ok(Self {
            inner: VsockConnectionManager::new_with_capacity(socket, DEFAULT_RX_BUFFER_CAPACITY),
            pending_event: None,
            irq_endpoints: Some(irq_endpoints),
        })
    }
}

impl<T: Transport + 'static> DriverGeneric for VirtIoVsock<T> {
    fn name(&self) -> &str {
        "virtio-socket"
    }
}

impl<T: Transport + 'static> rdif_vsock::Interface for VirtIoVsock<T> {
    fn guest_cid(&self) -> u64 {
        self.inner.guest_cid()
    }

    fn listen(&mut self, port: u32) -> Result<(), VsockError> {
        validate_port(port)?;
        self.inner.listen(port);
        Ok(())
    }

    fn connect(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .connect(peer, local_port)
            .map_err(map_vsock_error)?;
        Ok(())
    }

    fn send_capacity(&mut self, id: VsockConnId) -> Result<usize, VsockError> {
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .send_capacity(peer, local_port)
            .map_err(map_vsock_error)
    }

    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let capacity = self.send_capacity(id)?;
        // Let the manager request credit when exhausted. Returning early here would
        // leave a blocked writer waiting for an update that was never requested.
        let send_length = if capacity == 0 {
            buf.len()
        } else {
            buf.len().min(capacity)
        };
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .send(peer, local_port, &buf[..send_length])
            .map_err(map_vsock_error)?;
        Ok(send_length)
    }

    fn recv(&mut self, id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (peer, local_port) = map_conn_id(id)?;
        let read = self
            .inner
            .recv(peer, local_port, buf)
            .map_err(map_vsock_error)?;
        if read != 0 {
            let _ = self.inner.update_credit(peer, local_port);
        }
        Ok(read)
    }

    fn recv_avail(&mut self, id: VsockConnId) -> Result<usize, VsockError> {
        let (peer, local_port) = map_conn_id(id)?;
        let available = self
            .inner
            .recv_buffer_available_bytes(peer, local_port)
            .map_err(map_vsock_error)?;
        let _ = self.inner.update_credit(peer, local_port);
        Ok(available)
    }

    fn disconnect(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .shutdown(peer, local_port)
            .map_err(map_vsock_error)
    }

    fn abort(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .force_close(peer, local_port)
            .map_err(map_vsock_error)
    }

    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
        if let Some(event) = self.pending_event.take() {
            return event.map(Some);
        }
        let mut notification = None;
        let event = self
            .inner
            .poll_with_credit_update(|peer, local_port, event_type| {
                let connection = VsockConnId {
                    peer_addr: map_rdif_addr(peer),
                    local_port,
                };
                notification = Some(
                    if matches!(event_type, VsockEventType::Disconnected { .. }) {
                        VsockEvent::Disconnected(connection)
                    } else {
                        VsockEvent::CreditUpdate(connection)
                    },
                );
            });
        publish_polled_event(
            event
                .map(|event| event.map(map_event))
                .map_err(map_vsock_error),
            notification,
            &mut self.pending_event,
        )
    }

    fn take_irq_endpoints(&mut self) -> Result<VsockIrqEndpoints, VsockError> {
        self.irq_endpoints.take().ok_or(VsockError::NotAvailable)
    }
}

// Preserve state notifications even when a control response cannot be submitted.
// The task worker stops draining on errors, so return the notification first and
// retain the error for the next call. No socket callback runs under the device gate.
fn publish_polled_event(
    result: Result<Option<VsockEvent>, VsockError>,
    notification: Option<VsockEvent>,
    pending: &mut Option<Result<VsockEvent, VsockError>>,
) -> Result<Option<VsockEvent>, VsockError> {
    let Some(notification) = notification else {
        return result;
    };
    match result {
        Ok(Some(event)) => {
            if matches!(
                event,
                VsockEvent::Received(..) | VsockEvent::ConnectionRequest(..)
            ) {
                *pending = Some(Ok(notification));
            }
            Ok(Some(event))
        }
        Ok(None) => Ok(Some(notification)),
        Err(error) => {
            *pending = Some(Err(error));
            Ok(Some(notification))
        }
    }
}

fn validate_port(port: u32) -> Result<(), VsockError> {
    if port == 0 {
        return Err(VsockError::NotAvailable);
    }
    Ok(())
}

fn map_conn_id(id: VsockConnId) -> Result<(VsockAddr, u32), VsockError> {
    validate_port(id.peer_addr.port)?;
    validate_port(id.local_port)?;
    Ok((
        VsockAddr {
            cid: id.peer_addr.cid,
            port: id.peer_addr.port,
        },
        id.local_port,
    ))
}

fn map_rdif_addr(addr: VsockAddr) -> RdifVsockAddr {
    RdifVsockAddr {
        cid: addr.cid,
        port: addr.port,
    }
}

fn map_event_conn(event: &RawVsockEvent) -> VsockConnId {
    VsockConnId {
        peer_addr: map_rdif_addr(event.source),
        local_port: event.destination.port,
    }
}

fn map_event(event: RawVsockEvent) -> VsockEvent {
    let conn = map_event_conn(&event);
    match event.event_type {
        VsockEventType::ConnectionRequest => VsockEvent::ConnectionRequest(conn),
        VsockEventType::Connected => VsockEvent::Connected(conn),
        VsockEventType::Received { length } => VsockEvent::Received(conn, length),
        VsockEventType::Disconnected { reason } => {
            let _ = map_disconnect_reason(reason);
            VsockEvent::Disconnected(conn)
        }
        VsockEventType::CreditUpdate => VsockEvent::CreditUpdate(conn),
        VsockEventType::CreditRequest => VsockEvent::Unknown,
    }
}

fn map_disconnect_reason(reason: DisconnectReason) -> DisconnectReason {
    reason
}

fn map_vsock_error(err: VirtIoError) -> VsockError {
    match err {
        VirtIoError::Unsupported => VsockError::NotSupported,
        VirtIoError::QueueFull | VirtIoError::NotReady => VsockError::Retry,
        VirtIoError::AlreadyUsed => VsockError::AlreadyExists,
        VirtIoError::SocketDeviceError(SocketError::InsufficientBufferSpaceInPeer) => {
            VsockError::Retry
        }
        VirtIoError::SocketDeviceError(SocketError::ConnectionExists) => VsockError::AlreadyExists,
        VirtIoError::SocketDeviceError(
            SocketError::NotConnected | SocketError::PeerSocketShutdown,
        ) => VsockError::NotConnected,
        error => VsockError::Other(alloc::boxed::Box::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_credit_exhaustion_is_retryable_not_a_disconnect() {
        assert!(matches!(
            map_vsock_error(VirtIoError::SocketDeviceError(
                SocketError::InsufficientBufferSpaceInPeer
            )),
            VsockError::Retry
        ));
    }
}
