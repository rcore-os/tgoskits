#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    publisher::run();
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod publisher {
    use core::{cell::UnsafeCell, result::Result::Err, sync::atomic::AtomicU64};

    use ax_std::{
        os::arceos::modules::ax_hal::{
            irq,
            mem::{PhysAddr, VirtAddr, virt_to_phys},
        },
        println, thread,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{
        IVC_SLOT_PAYLOAD_SIZE, IvcMessageKind, IvcPeerEventWaiter, IvcRegion, record_peer_event,
    };

    const PUBLISH_COUNT: u64 = 5;
    static NOTIFY_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

    mod demo_config {
        pub const CHANNEL_KEY: usize = 0x4956_4301;
        pub const CHANNEL_SIZE: usize = 0x1_0000;
        pub const NOTIFY_IRQ: Option<usize> = Some(60);
        pub const PUBLISHER_VM_ID: usize = 1;
        pub const SUBSCRIBER_VM_ID: usize = 2;
    }

    pub fn run() {
        let shm_base_gpa = HyperCallOutputSlot::new(0);
        let shm_size = HyperCallOutputSlot::new(demo_config::CHANNEL_SIZE);
        let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
        let shm_size_ptr = shm_size.guest_phys_addr();

        if let Err(err) =
            ivc::publish_channel(demo_config::CHANNEL_KEY, shm_base_gpa_ptr, shm_size_ptr)
        {
            println!("ivc publish failed: {err}");
            return;
        }
        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "ivc publish failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        println!("ivc publish ok base={shm_base_gpa:#x} size={shm_size}");
        let Some(region) = shared_page_mut(shm_base_gpa, shm_size) else {
            println!("ivc publish failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        region.initialize(demo_config::PUBLISHER_VM_ID, demo_config::CHANNEL_KEY);
        let region: &'static IvcRegion = region;
        let irq_enabled = register_notify_irq();

        // Each task owns its waiter: `IvcPeerEventWaiter` tracks the observed
        // event count internally, so sharing one waiter would let one task
        // consume IRQ observations meant to wake the other.
        let sender = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            sender_task(region, &waiter);
        });
        let receiver = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            receiver_task(region, &waiter);
        });

        sender.join().expect("sender thread panicked");
        receiver.join().expect("receiver thread panicked");

        println!("ivc full-duplex demo complete");
    }

    #[allow(dead_code)]
    fn run_request_ack_demo(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut ack_payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        let mut subscriber_ready = false;
        for sequence in 1..=PUBLISH_COUNT {
            send_request_when_ready(region, sequence, subscriber_ready, waiter);
            wait_for_ack(region, sequence, &mut ack_payload, waiter);
            subscriber_ready = true;
        }
    }

    #[allow(dead_code)]
    fn send_request_when_ready(
        region: &'static IvcRegion,
        sequence: u64,
        subscriber_ready: bool,
        waiter: &IvcPeerEventWaiter<'_>,
    ) {
        loop {
            match region.send_request(sequence, b"hello from arceos publisher") {
                Ok(()) => {
                    println!("ivc send seq={sequence}");
                    if subscriber_ready {
                        notify_subscriber();
                    }
                    return;
                }
                Err(_) => waiter.wait_for_peer_event(),
            }
        }
    }

    #[allow(dead_code)]
    fn wait_for_ack(
        region: &'static IvcRegion,
        sequence: u64,
        payload: &mut [u8],
        waiter: &IvcPeerEventWaiter<'_>,
    ) {
        loop {
            match region.try_recv_ack(payload) {
                Ok(Some(message)) if message.sequence() == sequence => {
                    let text =
                        core::str::from_utf8(&payload[..message.len()]).unwrap_or("<non-utf8>");
                    println!("ivc ack seq={} msg={text}", message.sequence());
                    return;
                }
                Ok(Some(_)) | Ok(None) => waiter.wait_for_peer_event(),
                Err(err) => {
                    println!("ivc publish failed: recv ack error {err:?}");
                    return;
                }
            }
        }
    }

    /// Sends PUBLISH_COUNT requests without blocking on individual acks.
    /// Skips notification on the first send because the subscriber may not
    /// have completed `subscribe_channel` yet.
    fn sender_task(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut subscriber_ready = false;
        for seq in 1..=PUBLISH_COUNT {
            loop {
                match region.send_request(seq, b"hello from arceos publisher") {
                    Ok(()) => {
                        println!("ivc send seq={seq}");
                        if subscriber_ready {
                            notify_subscriber();
                        }
                        subscriber_ready = true;
                        break;
                    }
                    Err(_) => waiter.wait_for_peer_event(),
                }
            }
        }
    }

    /// Receives ring-B messages until PUBLISH_COUNT acks have arrived.
    /// Subscriber data messages (kind `Request`) share ring B with acks, so
    /// only `Ack` messages count toward request completion.
    fn receiver_task(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        let mut ack_count = 0u64;
        loop {
            match region.try_recv_ack(&mut payload) {
                Ok(Some(msg)) => {
                    let text = core::str::from_utf8(&payload[..msg.len()]).unwrap_or("<non-utf8>");
                    match msg.kind() {
                        IvcMessageKind::Ack => {
                            ack_count += 1;
                            println!("ivc ack seq={} msg={text}", msg.sequence());
                            if ack_count >= PUBLISH_COUNT {
                                return;
                            }
                        }
                        IvcMessageKind::Request => {
                            println!("ivc recv sub data seq={} msg={text}", msg.sequence());
                        }
                    }
                }
                Ok(None) => waiter.wait_for_peer_event(),
                Err(err) => {
                    println!("ivc recv ack error {err:?}");
                    return;
                }
            }
        }
    }

    fn notify_subscriber() {
        if let Err(err) = ivc::notify_channel(
            demo_config::PUBLISHER_VM_ID,
            demo_config::CHANNEL_KEY,
            demo_config::SUBSCRIBER_VM_ID,
        ) {
            println!("ivc notify warning: {err}");
        }
    }

    fn register_notify_irq() -> bool {
        let Some(raw_irq) = demo_config::NOTIFY_IRQ else {
            return false;
        };
        match notify_irq_id(raw_irq)
            .and_then(|irq_id| irq::request_shared_irq(irq_id, notify_irq_handler).map(|_| irq_id))
        {
            Ok(irq_id) => {
                println!("ivc notify irq enabled irq={irq_id:?}");
                true
            }
            Err(err) => {
                println!("ivc notify irq disabled raw={raw_irq} err={err:?}");
                false
            }
        }
    }

    fn notify_irq_handler(_ctx: irq::IrqContext) -> irq::IrqReturn {
        record_peer_event(&NOTIFY_IRQ_COUNT);
        irq::IrqReturn::Handled
    }

    fn notify_irq_id(raw_irq: usize) -> Result<irq::IrqId, irq::IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            let gsi = u32::try_from(raw_irq).map_err(|_| irq::IrqError::InvalidIrq)?;
            irq::resolve_irq_source(irq::IrqSource::AcpiGsi(gsi))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            irq::try_legacy_irq(raw_irq)
        }
    }

    struct HyperCallOutputSlot {
        value: UnsafeCell<usize>,
    }

    impl HyperCallOutputSlot {
        const fn new(value: usize) -> Self {
            Self {
                value: UnsafeCell::new(value),
            }
        }

        fn guest_phys_addr(&self) -> IvcGuestPhysAddr {
            let vaddr = VirtAddr::from_usize(self.value.get().addr());
            IvcGuestPhysAddr::new(virt_to_phys(vaddr).as_usize())
        }

        fn read(&self) -> usize {
            unsafe {
                // Axvisor writes this slot through the guest physical address
                // passed to the hypercall; use a volatile read to observe it.
                core::ptr::read_volatile(self.value.get())
            }
        }
    }

    fn shared_page_mut(shm_base_gpa: usize, shm_size: usize) -> Option<&'static mut IvcRegion> {
        let vaddr = ax_mm::iomap(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        unsafe {
            // Axvisor maps the returned GPA to one exclusive publisher view of
            // the shared region before subscribers can use the phase-2 rings.
            Some(&mut *(vaddr.as_mut_ptr() as *mut IvcRegion))
        }
    }
}
