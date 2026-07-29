extern crate alloc;

use alloc::format;

use rdif_vsock::{VsockAddr as RdifVsockAddr, VsockConnId, VsockError, VsockEvent};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::socket::{
        DisconnectReason, SocketError, VirtIOSocket, VsockAddr, VsockConnectionManager,
        VsockEvent as RawVsockEvent, VsockEventType,
    },
    transport::Transport,
};

use crate::{BindingInfo, virtio::VirtIoHalImpl, vsock::PlatformDeviceVsock};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

const DEFAULT_RX_BUFFER_CAPACITY: u32 = 32 * 1024;

mod credit;

use credit::TxCreditBook;

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
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Optional)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

pub fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = VirtIoVsock::new(transport).map_err(|err| {
        OnProbeError::other(format!("failed to initialize virtio-socket: {err:?}"))
    })?;
    let irq = plat_dev.register_vsock_with_info(dev, info);
    log::info!("registered virtio socket device irq={irq:?}");
    Ok(())
}

struct VirtIoVsock<T: Transport + 'static> {
    inner: VsockConnectionManager<VirtIoHalImpl, T>,
    tx_credits: TxCreditBook,
}

unsafe impl<T: Transport + 'static> Send for VirtIoVsock<T> {}

impl<T: Transport + 'static> VirtIoVsock<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let socket = VirtIOSocket::<VirtIoHalImpl, _>::new(transport)?;
        Ok(Self {
            inner: VsockConnectionManager::new_with_capacity(socket, DEFAULT_RX_BUFFER_CAPACITY),
            tx_credits: TxCreditBook::default(),
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
        self.tx_credits.open(id);
        Ok(())
    }

    fn send_capacity(&mut self, id: VsockConnId) -> Result<usize, VsockError> {
        let _validated = map_conn_id(id)?;
        self.tx_credits
            .available(id, DEFAULT_RX_BUFFER_CAPACITY)
            .ok_or(VsockError::NotConnected)
    }

    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let capacity = self
            .tx_credits
            .available(id, DEFAULT_RX_BUFFER_CAPACITY)
            .ok_or(VsockError::NotConnected)?;
        if capacity == 0 {
            return Err(VsockError::Retry);
        }
        let send_length = buf.len().min(capacity);
        let length = u32::try_from(send_length).map_err(|_| VsockError::NotSupported)?;
        let (peer, local_port) = map_conn_id(id)?;
        self.inner
            .send(peer, local_port, &buf[..send_length])
            .map_err(map_vsock_error)?;
        self.tx_credits.record_sent(id, length);
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
            .map_err(map_vsock_error)?;
        self.tx_credits.close(id);
        Ok(())
    }

    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
        let Some(event) = self.inner.poll().map_err(map_vsock_error)? else {
            return Ok(None);
        };
        let connection = map_event_conn(&event);
        self.tx_credits.update_peer(
            connection,
            event.buffer_status.buffer_allocation,
            event.buffer_status.forward_count,
        );
        let disconnected = matches!(event.event_type, VsockEventType::Disconnected { .. });
        let event = map_event(event);
        if disconnected {
            self.tx_credits.close(connection);
        }
        Ok(Some(event))
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
