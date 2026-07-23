#![no_std]

extern crate alloc;

mod addr;
mod error;
mod event;
mod interface;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for vsock connection ids, events, and interface defaults.
pub mod axtest;

pub use addr::*;
pub use error::*;
pub use event::*;
pub use interface::*;
pub use rdif_base::{DriverGeneric, KError, io};

#[cfg(test)]
mod tests {
    use super::*;

    struct TestVsock;

    impl DriverGeneric for TestVsock {
        fn name(&self) -> &str {
            "test-vsock"
        }
    }

    impl Interface for TestVsock {
        fn guest_cid(&self) -> u64 {
            3
        }

        fn listen(&mut self, _port: u32) -> Result<(), VsockError> {
            Ok(())
        }

        fn connect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn send(&mut self, _id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
            Ok(buf.len())
        }

        fn recv(&mut self, _id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError> {
            if !buf.is_empty() {
                buf[0] = 7;
            }
            Ok(buf.len().min(1))
        }

        fn recv_avail(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
            Ok(1)
        }

        fn disconnect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn abort(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
            Ok(Some(VsockEvent::Connected(VsockConnId::listening(1024))))
        }
    }

    #[test]
    fn vsock_interface_exposes_connections_and_events() {
        let mut vsock = TestVsock;
        let id = VsockConnId::listening(1024);
        assert_eq!(vsock.guest_cid(), 3);
        assert_eq!(vsock.send(id, &[1, 2, 3]).unwrap(), 3);

        let mut buf = [0; 4];
        assert_eq!(vsock.recv(id, &mut buf).unwrap(), 1);
        assert_eq!(buf[0], 7);
        assert_eq!(vsock.poll_event().unwrap(), Some(VsockEvent::Connected(id)));
        assert_eq!(vsock.handle_irq(), Event::none());
    }

    #[test]
    fn already_exists_maps_to_not_available() {
        let kind: io::ErrorKind = VsockError::AlreadyExists.into();

        assert!(matches!(kind, io::ErrorKind::NotAvailable));
    }
}
