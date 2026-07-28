use alloc::{boxed::Box, sync::Arc, task::Wake};
use core::{
    sync::atomic::{AtomicBool, AtomicUsize},
    task::{Context, Waker},
};

use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};
use rdif_vsock::{DriverGeneric, Interface, VsockAddr, VsockConnId, VsockError};

use super::*;

struct TestVsock {
    requested_rx: Arc<AtomicUsize>,
    poll_count: Arc<AtomicUsize>,
    always_poll_event: bool,
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
        self.requested_rx.store(buf.len(), Ordering::Release);
        if let Some(first) = buf.first_mut() {
            *first = 0x5a;
            Ok(1)
        } else {
            Ok(0)
        }
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
        if self.always_poll_event {
            self.poll_count.fetch_add(1, Ordering::AcqRel);
            Ok(Some(VsockEvent::Unknown))
        } else {
            Ok(None)
        }
    }
}

struct DeviceGateProbe {
    device_released: AtomicBool,
    manager_released: AtomicBool,
}

impl Wake for DeviceGateProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.device_released
            .store(VSOCK_DEVICE.try_lock().is_some(), Ordering::Release);
        self.manager_released
            .store(VSOCK_CONN_MANAGER.try_lock().is_some(), Ordering::Release);
    }
}

#[test]
fn received_event_releases_device_gate_before_waking_socket() {
    let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime = crate::test_runtime::install(&system, cpu.as_mut());

    let conn_id = VsockConnId {
        peer_addr: VsockAddr { cid: 2, port: 3 },
        local_port: 4,
    };
    let connection = VSOCK_CONN_MANAGER
        .lock()
        .create_connection(
            conn_id,
            VsockAddr { cid: 3, port: 4 },
            Some(conn_id.peer_addr),
            ConnectionState::Connected,
            VsockPollLease::inactive_for_test(),
        )
        .unwrap();
    let probe = Arc::new(DeviceGateProbe {
        device_released: AtomicBool::new(false),
        manager_released: AtomicBool::new(false),
    });
    let waker = Waker::from(probe.clone());
    connection.register_rx_poll(&mut Context::from_waker(&waker));

    let requested_rx = Arc::new(AtomicUsize::new(0));
    *VSOCK_DEVICE.lock() = Some(Box::new(TestVsock {
        requested_rx: requested_rx.clone(),
        poll_count: Arc::new(AtomicUsize::new(0)),
        always_poll_event: false,
    }));
    let mut worker = VsockPollWorker::new();

    assert_eq!(
        worker
            .handle_vsock_event(VsockEvent::Received(conn_id, 1))
            .unwrap(),
        EventDisposition::Consumed
    );
    assert_eq!(requested_rx.load(Ordering::Acquire), 1);
    assert!(probe.device_released.load(Ordering::Acquire));
    assert!(probe.manager_released.load(Ordering::Acquire));
    assert_eq!(connection.lock().rx_buffer_used(), 1);

    *VSOCK_DEVICE.lock() = None;
    VSOCK_CONN_MANAGER
        .lock()
        .remove_connection_if(conn_id, &connection);
}

#[test]
fn poll_iteration_has_a_fixed_event_budget() {
    let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime = crate::test_runtime::install(&system, cpu.as_mut());

    let poll_count = Arc::new(AtomicUsize::new(0));
    *VSOCK_DEVICE.lock() = Some(Box::new(TestVsock {
        requested_rx: Arc::new(AtomicUsize::new(0)),
        poll_count: poll_count.clone(),
        always_poll_event: true,
    }));
    let mut worker = VsockPollWorker::new();

    assert!(worker.poll_vsock_interfaces().unwrap());
    assert_eq!(
        poll_count.load(Ordering::Acquire),
        VSOCK_EVENT_BUDGET,
        "an always-ready device must return to the scheduler after one budget"
    );

    *VSOCK_DEVICE.lock() = None;
}
