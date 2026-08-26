use rdif_vsock::{
    DriverGeneric, Interface, VsockAddr, VsockConnId, VsockError, VsockEvent, VsockHardIrqEndpoint,
    VsockHardIrqHandler, VsockHardIrqResult, VsockIrqEndpoints, VsockPollIrqControl,
    VsockRearmResult, io,
};

struct TestHardIrq;

impl VsockHardIrqHandler for TestHardIrq {
    fn handle_irq(&mut self) -> VsockHardIrqResult {
        VsockHardIrqResult::Schedule
    }
}

struct TestIrqControl;

impl VsockPollIrqControl for TestIrqControl {
    fn quiesce(&mut self) -> Result<(), VsockError> {
        Ok(())
    }

    fn rearm_and_check(&mut self) -> Result<VsockRearmResult, VsockError> {
        Ok(VsockRearmResult::Idle)
    }

    fn shutdown(&mut self) -> Result<(), VsockError> {
        Ok(())
    }
}

struct TestVsock {
    irq_endpoints: Option<VsockIrqEndpoints>,
}

impl TestVsock {
    fn new() -> Self {
        Self {
            irq_endpoints: Some(VsockIrqEndpoints::new(
                VsockHardIrqEndpoint::new(Box::new(TestHardIrq)),
                Box::new(TestIrqControl),
            )),
        }
    }
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

    fn send_capacity(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
        Ok(usize::MAX)
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

    fn take_irq_endpoints(&mut self) -> Result<VsockIrqEndpoints, VsockError> {
        self.irq_endpoints.take().ok_or(VsockError::NotAvailable)
    }
}

#[test]
fn public_interface_transfers_mandatory_irq_capabilities_once() {
    let mut vsock = TestVsock::new();
    let connection = VsockConnId {
        peer_addr: VsockAddr { cid: 2, port: 3 },
        local_port: 4,
    };

    assert_eq!(vsock.send(connection, &[1, 2, 3]).unwrap(), 3);
    let endpoints = vsock.take_irq_endpoints().unwrap();
    assert!(matches!(
        vsock.take_irq_endpoints(),
        Err(VsockError::NotAvailable)
    ));
    let (mut hard_irq, mut control) = endpoints.into_parts();
    assert_eq!(hard_irq.handle_irq(), VsockHardIrqResult::Schedule);
    control.quiesce().unwrap();
    assert_eq!(control.rearm_and_check().unwrap(), VsockRearmResult::Idle);
    control.shutdown().unwrap();

    assert!(matches!(
        io::ErrorKind::from(VsockError::NotConnected),
        io::ErrorKind::BrokenPipe
    ));
}
