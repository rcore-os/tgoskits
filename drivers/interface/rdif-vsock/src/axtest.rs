use axtest::prelude::*;

use crate::{DriverGeneric, Event, Interface, VsockAddr, VsockConnId, VsockError, VsockEvent, io};

struct TestVsock {
    irq_enabled: bool,
}

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

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        Event {
            connection_changed: true,
            data_available: true,
        }
    }
}

#[axtest]
fn rdif_vsock_addresses_events_and_interface_rules_hold() {
    let listen_id = VsockConnId::listening(1024);
    ax_assert_eq!(listen_id.peer_addr, VsockAddr { cid: 0, port: 0 });
    ax_assert_eq!(listen_id.local_port, 1024);

    let events = [
        VsockEvent::ConnectionRequest(listen_id),
        VsockEvent::Connected(listen_id),
        VsockEvent::Received(listen_id, 8),
        VsockEvent::Disconnected(listen_id),
        VsockEvent::CreditUpdate(listen_id),
        VsockEvent::Unknown,
    ];
    ax_assert_eq!(events[2], VsockEvent::Received(listen_id, 8));

    let mut vsock = TestVsock { irq_enabled: false };
    ax_assert_eq!(vsock.guest_cid(), 3);
    vsock.listen(1024).unwrap();
    vsock.connect(listen_id).unwrap();
    ax_assert_eq!(vsock.send(listen_id, &[1, 2, 3]).unwrap(), 3);

    let mut buf = [0; 4];
    ax_assert_eq!(vsock.recv(listen_id, &mut buf).unwrap(), 1);
    ax_assert_eq!(buf[0], 7);
    ax_assert_eq!(vsock.recv_avail(listen_id).unwrap(), 1);
    ax_assert_eq!(
        vsock.poll_event().unwrap(),
        Some(VsockEvent::Connected(listen_id))
    );

    vsock.enable_irq();
    ax_assert!(vsock.is_irq_enabled());
    ax_assert_eq!(
        vsock.handle_irq(),
        Event {
            connection_changed: true,
            data_available: true
        }
    );
    vsock.disable_irq();
    ax_assert!(!vsock.is_irq_enabled());
    vsock.disconnect(listen_id).unwrap();
    vsock.abort(listen_id).unwrap();
}

#[axtest]
fn rdif_vsock_error_mapping_and_default_event_hold() {
    ax_assert_eq!(
        Event::none(),
        Event {
            connection_changed: false,
            data_available: false
        }
    );
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::Retry),
        io::ErrorKind::Interrupted
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::NotConnected),
        io::ErrorKind::BrokenPipe
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::AlreadyExists),
        io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(VsockError::Other("vsock backend".into())),
        io::ErrorKind::Other(_)
    ));
}

#[axtest]
fn rdif_vsock_minimal_interface_uses_default_irq_methods() {
    struct MinimalVsock;

    impl DriverGeneric for MinimalVsock {
        fn name(&self) -> &str {
            "minimal-vsock"
        }
    }

    impl Interface for MinimalVsock {
        fn guest_cid(&self) -> u64 {
            9
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

        fn recv(&mut self, _id: VsockConnId, _buf: &mut [u8]) -> Result<usize, VsockError> {
            Ok(0)
        }

        fn recv_avail(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
            Ok(0)
        }

        fn disconnect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn abort(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
            Ok(None)
        }
    }

    let mut vsock = MinimalVsock;
    ax_assert_eq!(vsock.guest_cid(), 9);
    vsock.enable_irq();
    ax_assert!(!vsock.is_irq_enabled());
    ax_assert_eq!(vsock.handle_irq(), Event::none());
    vsock.disable_irq();
}
