use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_vsock::{VsockConnId, VsockDriverEvent, VsockDriverOps};
use virtio_drivers::{
    Hal,
    device::socket::{
        DisconnectReason, VirtIOSocket, VsockAddr, VsockConnectionManager as InnerDev,
        VsockEvent, VsockEventType,
    },
    transport::Transport,
};

use crate::as_dev_err;

const DEFAULT_RX_BUFFER_CAPACITY: u32 = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MappedConnId {
    peer_addr: VsockAddr,
    local_port: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranslatedEventKind {
    ConnectionRequest,
    Connected,
    Received(usize),
    Disconnected,
    CreditUpdate,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranslatedEvent {
    conn_id: VsockConnId,
    kind: TranslatedEventKind,
}

fn validate_port(port: u32) -> DevResult<()> {
    if port == 0 {
        return Err(DevError::InvalidParam);
    }
    Ok(())
}

fn validate_peer_addr(addr: &ax_driver_vsock::VsockAddr) -> DevResult<()> {
    if addr.cid == 0 {
        return Err(DevError::InvalidParam);
    }
    validate_port(addr.port)
}

fn validate_conn_id(cid: VsockConnId) -> DevResult<VsockConnId> {
    validate_peer_addr(&cid.peer_addr)?;
    validate_port(cid.local_port)?;
    Ok(cid)
}

fn map_peer_addr(addr: ax_driver_vsock::VsockAddr) -> VsockAddr {
    VsockAddr {
        cid: addr.cid,
        port: addr.port,
    }
}

fn map_conn_id_checked(cid: VsockConnId) -> DevResult<MappedConnId> {
    let cid = validate_conn_id(cid)?;
    Ok(MappedConnId {
        peer_addr: map_peer_addr(cid.peer_addr),
        local_port: cid.local_port,
    })
}

fn map_disconnect_reason(_reason: DisconnectReason) -> TranslatedEventKind {
    TranslatedEventKind::Disconnected
}

fn translate_event_kind(event_type: VsockEventType) -> TranslatedEventKind {
    match event_type {
        VsockEventType::ConnectionRequest => TranslatedEventKind::ConnectionRequest,
        VsockEventType::Connected => TranslatedEventKind::Connected,
        VsockEventType::Received { length } => TranslatedEventKind::Received(length),
        VsockEventType::Disconnected { reason } => map_disconnect_reason(reason),
        VsockEventType::CreditUpdate => TranslatedEventKind::CreditUpdate,
        _ => TranslatedEventKind::Unknown,
    }
}

/// The VirtIO socket device driver.
pub struct VirtIoSocketDev<H: Hal, T: Transport> {
    inner: InnerDev<H, T>,
}

unsafe impl<H: Hal, T: Transport> Send for VirtIoSocketDev<H, T> {}
unsafe impl<H: Hal, T: Transport> Sync for VirtIoSocketDev<H, T> {}

impl<H: Hal, T: Transport> VirtIoSocketDev<H, T> {
    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T) -> DevResult<Self> {
        let virtio_socket = VirtIOSocket::<H, _>::new(transport).map_err(as_dev_err)?;
        Ok(Self {
            inner: InnerDev::new_with_capacity(virtio_socket, DEFAULT_RX_BUFFER_CAPACITY),
        })
    }

    fn connect_mapped(&mut self, conn: MappedConnId) -> DevResult<()> {
        self.inner
            .connect(conn.peer_addr, conn.local_port)
            .map_err(as_dev_err)
    }

    fn send_on_mapped(&mut self, conn: MappedConnId, buf: &[u8]) -> DevResult<usize> {
        match self.inner.send(conn.peer_addr, conn.local_port, buf) {
            Ok(()) => Ok(buf.len()),
            Err(e) => Err(as_dev_err(e)),
        }
    }

    fn recv_on_mapped(&mut self, conn: MappedConnId, buf: &mut [u8]) -> DevResult<usize> {
        let res = self
            .inner
            .recv(conn.peer_addr, conn.local_port, buf)
            .map_err(as_dev_err);
        self.update_peer_credit(conn);
        res
    }

    fn recv_available_on_mapped(&mut self, conn: MappedConnId) -> DevResult<usize> {
        self.inner
            .recv_buffer_available_bytes(conn.peer_addr, conn.local_port)
            .map_err(as_dev_err)
    }

    fn shutdown_mapped(&mut self, conn: MappedConnId) -> DevResult<()> {
        self.inner
            .shutdown(conn.peer_addr, conn.local_port)
            .map_err(as_dev_err)
    }

    fn abort_mapped(&mut self, conn: MappedConnId) -> DevResult<()> {
        self.inner
            .force_close(conn.peer_addr, conn.local_port)
            .map_err(as_dev_err)
    }

    fn update_peer_credit(&mut self, conn: MappedConnId) {
        let _ = self.inner.update_credit(conn.peer_addr, conn.local_port);
    }

    fn poll_raw_event(&mut self) -> DevResult<Option<VsockEvent>> {
        self.inner.poll().map_err(as_dev_err)
    }

}

impl<H: Hal, T: Transport> BaseDriverOps for VirtIoSocketDev<H, T> {
    fn device_name(&self) -> &str {
        "virtio-socket"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Vsock
    }
}

#[cfg(test)]
fn map_conn_id(cid: VsockConnId) -> (VsockAddr, u32) {
    let mapped = map_conn_id_checked(cid).expect("vsock connection id should be valid");
    (mapped.peer_addr, mapped.local_port)
}

fn map_event_cid(event: &VsockEvent) -> VsockConnId {
    VsockConnId {
        peer_addr: ax_driver_vsock::VsockAddr {
            cid: event.source.cid as _,
            port: event.source.port as _,
        },
        local_port: event.destination.port,
    }
}

fn map_driver_event(event: TranslatedEvent) -> VsockDriverEvent {
    match event.kind {
        TranslatedEventKind::ConnectionRequest => VsockDriverEvent::ConnectionRequest(event.conn_id),
        TranslatedEventKind::Connected => VsockDriverEvent::Connected(event.conn_id),
        TranslatedEventKind::Received(length) => VsockDriverEvent::Received(event.conn_id, length),
        TranslatedEventKind::Disconnected => VsockDriverEvent::Disconnected(event.conn_id),
        TranslatedEventKind::CreditUpdate => VsockDriverEvent::CreditUpdate(event.conn_id),
        TranslatedEventKind::Unknown => VsockDriverEvent::Unknown,
    }
}

impl<H: Hal, T: Transport> VsockDriverOps for VirtIoSocketDev<H, T> {
    fn guest_cid(&self) -> u64 {
        self.inner.guest_cid()
    }

    fn listen(&mut self, src_port: u32) {
        self.inner.listen(src_port)
    }

    fn connect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let conn = map_conn_id_checked(cid)?;
        self.connect_mapped(conn)
    }

    fn send(&mut self, cid: VsockConnId, buf: &[u8]) -> DevResult<usize> {
        let conn = map_conn_id_checked(cid)?;
        self.send_on_mapped(conn, buf)
    }

    fn recv(&mut self, cid: VsockConnId, buf: &mut [u8]) -> DevResult<usize> {
        let conn = map_conn_id_checked(cid)?;
        self.recv_on_mapped(conn, buf)
    }

    fn recv_avail(&mut self, cid: VsockConnId) -> DevResult<usize> {
        let conn = map_conn_id_checked(cid)?;
        self.recv_available_on_mapped(conn)
    }

    fn disconnect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let conn = map_conn_id_checked(cid)?;
        self.shutdown_mapped(conn)
    }

    fn abort(&mut self, cid: VsockConnId) -> DevResult<()> {
        let conn = map_conn_id_checked(cid)?;
        self.abort_mapped(conn)
    }

    fn poll_event(&mut self) -> DevResult<Option<VsockDriverEvent>> {
        match self.poll_raw_event()? {
            None => Ok(None),
            Some(event) => Ok(Some(convert_vsock_event(event)?)),
        }
    }
}

fn convert_vsock_event(event: VsockEvent) -> DevResult<VsockDriverEvent> {
    let translated = TranslatedEvent {
        conn_id: map_event_cid(&event),
        kind: translate_event_kind(event.event_type),
    };
    Ok(map_driver_event(translated))
}

#[cfg(test)]
mod tests {
    use ax_driver_vsock::{VsockAddr as DriverVsockAddr, VsockConnId, VsockDriverEvent};
    use virtio_drivers::device::socket::{DisconnectReason, VsockAddr, VsockEvent, VsockEventType};

    use super::{convert_vsock_event, map_conn_id, map_event_cid};

    fn sample_conn_id() -> VsockConnId {
        VsockConnId {
            peer_addr: DriverVsockAddr { cid: 52, port: 2048 },
            local_port: 4096,
        }
    }

    fn sample_event(event_type: VsockEventType) -> VsockEvent {
        let mut event: VsockEvent = unsafe { core::mem::zeroed() };
        event.source = VsockAddr { cid: 33, port: 1025 };
        event.destination = VsockAddr { cid: 44, port: 2049 };
        event.event_type = event_type;
        event
    }

    #[test]
    fn map_conn_id_preserves_peer_and_local_port() {
        let conn_id = sample_conn_id();
        let (peer_addr, local_port) = map_conn_id(conn_id);
        assert_eq!(peer_addr.cid, conn_id.peer_addr.cid as _);
        assert_eq!(peer_addr.port, conn_id.peer_addr.port);
        assert_eq!(local_port, conn_id.local_port);
    }

    #[test]
    fn map_event_cid_uses_event_endpoints() {
        let event = sample_event(VsockEventType::Connected);
        let conn_id = map_event_cid(&event);
        assert_eq!(conn_id.peer_addr.cid, event.source.cid as _);
        assert_eq!(conn_id.peer_addr.port, event.source.port);
        assert_eq!(conn_id.local_port, event.destination.port);
    }

    #[test]
    fn convert_vsock_event_maps_connection_request() {
        let event = sample_event(VsockEventType::ConnectionRequest);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::ConnectionRequest(_)));
    }

    #[test]
    fn convert_vsock_event_maps_connected() {
        let event = sample_event(VsockEventType::Connected);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Connected(_)));
    }

    #[test]
    fn convert_vsock_event_maps_received_length() {
        let event = sample_event(VsockEventType::Received { length: 128 });
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Received(_, 128)));
    }

    #[test]
    fn convert_vsock_event_maps_disconnected() {
        let event = sample_event(VsockEventType::Disconnected {
            reason: DisconnectReason::Shutdown,
        });
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Disconnected(_)));
    }

    #[test]
    fn convert_vsock_event_maps_credit_update() {
        let event = sample_event(VsockEventType::CreditUpdate);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::CreditUpdate(_)));
    }

    #[test]
    fn convert_vsock_event_maps_credit_request_to_unknown() {
        let event = sample_event(VsockEventType::CreditRequest);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Unknown));
    }
}
