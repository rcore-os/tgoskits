//! Real driver/worker/scheduler integration with a controlled VirtIO peer.
//! The fixture owns only device queues; all connection and credit logic runs
//! in the production driver and ax-net. No physical interrupt routing is tested.

use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Wake, Waker},
    time::{Duration, Instant},
};

use ax_net::{
    ConnectStatus, NetError, PinnedNetIrqAction, PinnedNetIrqError, PinnedNetIrqRegistrar,
    PinnedNetIrqRegistration, SendOptions, SocketAddrEx, SocketOps, VsockDeviceInput,
    poll_socket_io,
    vsock::{VsockAddr, VsockSocket},
};
use ax_sync::{Mutex, SpinLock};
use axpoll::{IoEvents, PollRegistrar, Pollable, SharedObserver};
use irq_framework::IrqId;

mod peer;
use peer::{MemoryTransport, Peer};

const GUEST_CID: u64 = 3;
const HOST_CID: u64 = 2;
const HOST_PORT: u32 = 1234;
const HEADER_LEN: usize = 44;
const PAYLOAD: &[u8] = b"credit restored";
const TIMEOUT: Duration = Duration::from_secs(5);

pub fn run() -> crate::TestResult {
    let peer = Arc::new(SpinLock::new(Peer::default()));
    let registrar = TestRegistrar::default();
    let irq = IrqId::new(irq_framework::IrqDomainId(0), irq_framework::HwIrq(1));
    let platform = rdrive::PlatformDevice {
        descriptor: rdrive::Descriptor::new(),
    };
    ax_driver::virtio::vsock::register_transport_with_info(
        platform,
        MemoryTransport(peer.clone()),
        ax_driver::BindingInfo::with_irq_id(Some(irq)),
    )
    .expect("register production vsock adapter");
    let devices = ax_driver::vsock::take_vsock_devices().expect("take vsock adapter");
    let inputs = devices
        .into_iter()
        .map(|device| VsockDeviceInput {
            name: device.name,
            device: device.device,
            irq,
            endpoints: device.endpoints,
        })
        .collect();
    ax_net::init_vsock(
        inputs,
        &registrar,
        ax_task::sched::active_cpu_set().unwrap(),
    )
    .expect("start real vsock worker");

    let socket = Arc::new(VsockSocket::new());
    socket
        .start_connect(SocketAddrEx::Vsock(VsockAddr {
            cid: HOST_CID,
            port: HOST_PORT,
        }))
        .expect("start connection");
    registrar.trigger();
    ax_task::executor::block_on_timeout(
        TIMEOUT,
        poll_socket_io(&*socket, IoEvents::OUT, false, || {
            match socket.connect_status()? {
                ConnectStatus::Connected => Ok(()),
                ConnectStatus::InProgress => Err(NetError::WouldBlock),
            }
        }),
    )
    .expect("connection timeout")
    .expect("connect");
    assert!(
        !socket.poll().contains(IoEvents::OUT),
        "zero window reported writable"
    );
    assert_eq!(
        socket.try_send(PAYLOAD, &mut SendOptions::default()),
        Err(NetError::WouldBlock)
    );

    let observer = Arc::new(ReadinessWake {
        socket: socket.clone(),
        woke: AtomicBool::new(false),
    });
    let waker = Waker::from(observer.clone());
    let mut registration = PollRegistrar::<SharedObserver>::new(&waker);
    // SAFETY: the registrar owns its registrations and is dropped before the
    // socket; the callback rechecks published readiness in ordinary task context.
    unsafe { socket.register_shared(&mut registration, IoEvents::OUT) };

    let sleeping = Arc::new(AtomicBool::new(false));
    let writer_socket = socket.clone();
    let writer_sleeping = sleeping.clone();
    let writer = ax_task::thread::ThreadBuilder::new("vsock-writer".into())
        .spawn(move || {
            let mut send = pin!(poll_socket_io(
                &*writer_socket,
                IoEvents::OUT,
                false,
                || { writer_socket.try_send(PAYLOAD, &mut SendOptions::default()) }
            ));
            let notified = Arc::new(AtomicBool::new(false));
            let sent = ax_task::executor::block_on_timeout(
                TIMEOUT,
                poll_fn(|cx| {
                    let waker = Waker::from(Arc::new(WriterWake {
                        inner: cx.waker().clone(),
                        notified: notified.clone(),
                    }));
                    let result = send.as_mut().poll(&mut Context::from_waker(&waker));
                    if result.is_pending() {
                        writer_sleeping.store(true, Ordering::Release);
                    }
                    result
                }),
            )
            .expect("blocked writer was not woken")
            .expect("send after credit request");
            assert!(
                notified.load(Ordering::Acquire),
                "writer only resumed through timeout polling"
            );
            assert_eq!(sent, PAYLOAD.len());
        })
        .expect("spawn writer");
    let deadline = Instant::now() + TIMEOUT;
    while !sleeping.load(Ordering::Acquire)
        || writer.state() != ax_task::thread::ThreadState::Blocked
    {
        assert!(
            Instant::now() < deadline,
            "writer did not enter pending state"
        );
        std::thread::yield_now();
    }
    // No other peer packet follows the handshake: only CREDIT_REQUEST carries
    // the new window. Leave enough capacity to stay writable after the send.
    {
        let mut peer = peer.lock();
        assert!(
            peer.credit_requests > 0,
            "zero-window sender never requested peer credit"
        );
        peer.inject(7, 4096);
    }
    registrar.trigger();
    assert_eq!(writer.join().unwrap(), 0);
    assert!(
        observer.woke.load(Ordering::Acquire),
        "POLLOUT observer not notified"
    );
    assert!(socket.poll().contains(IoEvents::OUT));
    assert_eq!(peer.lock().received, PAYLOAD);
    std::println!("vsock CREDIT_REQUEST restored writer and POLLOUT");
    Ok(())
}

// Record a real socket notification separately from executor timeout repolls.
struct WriterWake {
    inner: Waker,
    notified: Arc<AtomicBool>,
}
impl Wake for WriterWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::Release);
        self.inner.wake_by_ref();
    }
}

struct ReadinessWake {
    socket: Arc<VsockSocket>,
    woke: AtomicBool,
}
impl Wake for ReadinessWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        // Reentering poll also takes the production device gate. A lock-held
        // wake deadlocks here and the QEMU runner reports failure.
        assert!(self.socket.poll().contains(IoEvents::OUT));
        self.woke.store(true, Ordering::Release);
    }
}

// There is no external IRQ source: only trigger() invokes the callback, under
// this mutex. The worker and driver still use their production IRQ endpoints.
#[derive(Default)]
struct TestRegistrar {
    action: Arc<Mutex<Option<PinnedNetIrqAction>>>,
}
impl TestRegistrar {
    fn trigger(&self) {
        self.action
            .lock()
            .as_mut()
            .expect("registered action")
            .run();
    }
}
struct Registration {
    cpu: usize,
}
impl PinnedNetIrqRegistration for Registration {
    fn owner_cpu(&self) -> usize {
        self.cpu
    }
    fn enable(&self) -> Result<(), PinnedNetIrqError> {
        Ok(())
    }
    fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError> {
        Ok(())
    }
}
impl PinnedNetIrqRegistrar for TestRegistrar {
    fn register(
        &self,
        _: String,
        _: IrqId,
        cpu: usize,
        action: PinnedNetIrqAction,
    ) -> Result<Box<dyn PinnedNetIrqRegistration>, PinnedNetIrqError> {
        assert!(self.action.lock().replace(action).is_none());
        Ok(Box::new(Registration { cpu }))
    }
}
