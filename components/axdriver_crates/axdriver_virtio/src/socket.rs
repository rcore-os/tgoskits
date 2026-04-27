use ax_driver_base::{BaseDriverOps, DevResult, DeviceType};
use ax_driver_vsock::{VsockConnId, VsockDriverEvent, VsockDriverOps};
use virtio_drivers::{
    Hal,
    device::socket::{
        VirtIOSocket, VsockAddr, VsockConnectionManager as InnerDev, VsockEvent, VsockEventType,
    },
    transport::Transport,
};

use crate::as_dev_err;

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
            inner: InnerDev::new_with_capacity(virtio_socket, 32 * 1024), // 32KB buffer
        })
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

fn map_conn_id(cid: VsockConnId) -> (VsockAddr, u32) {
    (
        VsockAddr {
            cid: cid.peer_addr.cid as _,
            port: cid.peer_addr.port as _,
        },
        cid.local_port,
    )
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

impl<H: Hal, T: Transport> VsockDriverOps for VirtIoSocketDev<H, T> {
    fn guest_cid(&self) -> u64 {
        self.inner.guest_cid()
    }

    fn listen(&mut self, src_port: u32) {
        self.inner.listen(src_port)
    }

    fn connect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let (peer_addr, src_port) = map_conn_id(cid);
        self.inner.connect(peer_addr, src_port).map_err(as_dev_err)
    }

    fn send(&mut self, cid: VsockConnId, buf: &[u8]) -> DevResult<usize> {
        let (peer_addr, src_port) = map_conn_id(cid);
        match self.inner.send(peer_addr, src_port, buf) {
            Ok(()) => Ok(buf.len()),
            Err(e) => Err(as_dev_err(e)),
        }
    }

    fn recv(&mut self, cid: VsockConnId, buf: &mut [u8]) -> DevResult<usize> {
        let (peer_addr, src_port) = map_conn_id(cid);
        let res = self
            .inner
            .recv(peer_addr, src_port, buf)
            .map_err(as_dev_err);
        let _ = self.inner.update_credit(peer_addr, src_port);
        res
    }

    fn recv_avail(&mut self, cid: VsockConnId) -> DevResult<usize> {
        let (peer_addr, src_port) = map_conn_id(cid);
        self.inner
            .recv_buffer_available_bytes(peer_addr, src_port)
            .map_err(as_dev_err)
    }

    fn disconnect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let (peer_addr, src_port) = map_conn_id(cid);
        self.inner.shutdown(peer_addr, src_port).map_err(as_dev_err)
    }

    fn abort(&mut self, cid: VsockConnId) -> DevResult<()> {
        let (peer_addr, src_port) = map_conn_id(cid);
        self.inner
            .force_close(peer_addr, src_port)
            .map_err(as_dev_err)
    }

    fn poll_event(&mut self) -> DevResult<Option<VsockDriverEvent>> {
        match self.inner.poll() {
            Ok(None) => {
                // no event
                Ok(None)
            }
            Ok(Some(event)) => {
                // translate event
                let result = convert_vsock_event(event)?;
                Ok(Some(result))
            }
            Err(e) => {
                // error
                Err(as_dev_err(e))
            }
        }
    }
}

fn convert_vsock_event(event: VsockEvent) -> DevResult<VsockDriverEvent> {
    let cid = map_event_cid(&event);

    match event.event_type {
        VsockEventType::ConnectionRequest => Ok(VsockDriverEvent::ConnectionRequest(cid)),
        VsockEventType::Connected => Ok(VsockDriverEvent::Connected(cid)),
        VsockEventType::Received { length } => Ok(VsockDriverEvent::Received(cid, length)),
        VsockEventType::Disconnected { reason: _ } => Ok(VsockDriverEvent::Disconnected(cid)),
        VsockEventType::CreditUpdate => Ok(VsockDriverEvent::CreditUpdate(cid)),
        _ => Ok(VsockDriverEvent::Unknown),
    }
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
