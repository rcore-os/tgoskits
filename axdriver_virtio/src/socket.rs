use axdriver_base::{BaseDriverOps, DevResult, DeviceType};
use axdriver_vsock::{VsockConnId, VsockDriverEvent, VsockDriverOps};
use virtio_drivers::{
    device::socket::{
        VirtIOSocket, VsockAddr, VsockConnectionManager as InnerDev, VsockEvent, VsockEventType,
    },
    transport::Transport,
    Hal,
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
            inner: InnerDev::new(virtio_socket),
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
        self.inner
            .recv(peer_addr, src_port, buf)
            .map_err(as_dev_err)
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

    fn poll_event(&mut self, buf: &mut [u8]) -> DevResult<Option<VsockDriverEvent>> {
        match self.inner.poll() {
            Ok(None) => {
                // no event
                Ok(None)
            }
            Ok(Some(event)) => {
                // translate event
                let result = convert_vsock_event(event, &mut self.inner, buf)?;
                Ok(Some(result))
            }
            Err(e) => {
                // error
                Err(as_dev_err(e))
            }
        }
    }
}

fn convert_vsock_event<H: Hal, T: Transport>(
    event: VsockEvent,
    inner: &mut InnerDev<H, T>,
    buf: &mut [u8],
) -> DevResult<VsockDriverEvent> {
    let cid = VsockConnId {
        peer_addr: axdriver_vsock::VsockAddr {
            cid: event.source.cid as _,
            port: event.source.port as _,
        },
        local_port: event.destination.port,
    };

    match event.event_type {
        VsockEventType::ConnectionRequest => Ok(VsockDriverEvent::ConnectionRequest(cid)),
        VsockEventType::Connected => Ok(VsockDriverEvent::Connected(cid)),
        VsockEventType::Received { length } => {
            let read = inner
                .recv(event.source, event.destination.port, &mut buf[..length])
                .map_err(as_dev_err)?;
            Ok(VsockDriverEvent::Received(cid, read))
        }
        VsockEventType::Disconnected { reason: _ } => Ok(VsockDriverEvent::Disconnected(cid)),
        _ => Ok(VsockDriverEvent::Unknown),
    }
}
