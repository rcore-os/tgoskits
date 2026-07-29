#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    subscriber::run();
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod subscriber {
    use core::{
        cell::UnsafeCell,
        option::Option::{None, Some},
        result::Result::{Err, Ok},
        sync::atomic::{AtomicU64, Ordering},
    };

    use ax_std::{
        os::arceos::modules::ax_hal::{
            irq,
            mem::{PhysAddr, VirtAddr, virt_to_phys},
        },
        println, thread,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{IVC_SLOT_PAYLOAD_SIZE, IvcPeerEventWaiter, IvcRegion, record_peer_event};

    const MAX_SUBSCRIBE_ATTEMPTS: usize = 80;
    const PASS_SEQUENCE: u64 = 5;
    const SUBSCRIBE_DATA_COUNT: u64 = 3;
    static NOTIFY_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
    /// Highest publisher sequence received so far. Publisher sequences are
    /// contiguous, so one monotonic counter covers every pending ack.
    static HIGHEST_RECV_SEQ: AtomicU64 = AtomicU64::new(0);

    mod demo_config {
        pub const CHANNEL_KEY: usize = 0x4956_4301;
        pub const NOTIFY_IRQ: Option<usize> = Some(60);
        pub const PUBLISHER_VM_ID: usize = 1;
        pub const SUBSCRIBER_VM_ID: usize = 2;
    }

    pub fn run() {
        let irq_enabled = register_notify_irq();
        let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
        let Some((shm_base_gpa, shm_size)) = subscribe_with_retry(&waiter) else {
            println!("ivc subscribe failed: retry limit reached");
            return;
        };

        println!(
            "ivc subscribe ok subscriber={} base={shm_base_gpa:#x} size={shm_size}",
            demo_config::SUBSCRIBER_VM_ID
        );
        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "ivc subscribe failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        let Some(region) = shared_region(shm_base_gpa, shm_size) else {
            println!("ivc subscribe failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        if !region.channel_header_matches(demo_config::PUBLISHER_VM_ID, demo_config::CHANNEL_KEY) {
            println!(
                "ivc subscribe failed: unexpected header publisher/key for base={shm_base_gpa:#x}"
            );
            return;
        }
        if !region.protocol_header_matches() {
            println!("ivc subscribe failed: unsupported phase-2 protocol header");
            return;
        }

        let region: &'static IvcRegion = region;

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

        println!("ivc subscriber full-duplex demo complete");
    }

    fn subscribe_with_retry(waiter: &IvcPeerEventWaiter<'_>) -> Option<(usize, usize)> {
        for attempt in 1..=MAX_SUBSCRIBE_ATTEMPTS {
            let shm_base_gpa = HyperCallOutputSlot::new(0);
            let shm_size = HyperCallOutputSlot::new(0);
            let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
            let shm_size_ptr = shm_size.guest_phys_addr();

            match ivc::subscribe_channel(
                demo_config::PUBLISHER_VM_ID,
                demo_config::CHANNEL_KEY,
                shm_base_gpa_ptr,
                shm_size_ptr,
            ) {
                Ok(()) => return Some((shm_base_gpa.read(), shm_size.read())),
                Err(err) => {
                    if attempt == 1 || attempt % 10 == 0 {
                        println!("ivc subscribe retry attempt={attempt} err={err}");
                    }
                    waiter.wait_for_peer_event();
                }
            }
        }
        None
    }

    #[allow(dead_code)]
    fn run_request_ack_demo(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        loop {
            match region.try_recv_request(&mut payload) {
                Ok(Some(message)) => {
                    let text =
                        core::str::from_utf8(&payload[..message.len()]).unwrap_or("<non-utf8>");
                    println!("ivc recv seq={} msg={text}", message.sequence());
                    send_ack_when_ready(region, message.sequence(), waiter);
                    if message.sequence() >= PASS_SEQUENCE {
                        println!("ivc demo pass");
                        return;
                    }
                }
                Ok(None) => waiter.wait_for_peer_event(),
                Err(err) => {
                    println!("ivc subscribe failed: recv request error {err:?}");
                    return;
                }
            }
        }
    }

    #[allow(dead_code)]
    fn send_ack_when_ready(
        region: &'static IvcRegion,
        sequence: u64,
        waiter: &IvcPeerEventWaiter<'_>,
    ) {
        loop {
            match region.send_ack(sequence, b"ack from arceos subscriber") {
                Ok(()) => {
                    notify_publisher();
                    return;
                }
                Err(_) => waiter.wait_for_peer_event(),
            }
        }
    }

    /// Sends subscriber data messages and acks of publisher data on ring B.
    /// Skips notification on the first data send because the publisher may not
    /// be ready to receive the notify yet. Acks every publisher sequence seen
    /// in `HIGHEST_RECV_SEQ` in order, so no ack is lost when several
    /// publisher messages arrive between two sender iterations.
    fn sender_task(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut sent_data = 0u64;
        let mut last_acked_seq = 0u64;
        let mut publisher_ready = false;

        loop {
            if sent_data < SUBSCRIBE_DATA_COUNT {
                match region.send_data_to_publisher(sent_data + 1, b"hello from arceos subscriber")
                {
                    Ok(()) => {
                        sent_data += 1;
                        println!("ivc send data seq={sent_data}");
                        if publisher_ready {
                            notify_publisher();
                        }
                        publisher_ready = true;
                    }
                    Err(_) => { /* ring full, will retry */ }
                }
            }

            let highest = HIGHEST_RECV_SEQ.load(Ordering::Acquire);
            let mut acked_any = false;
            while last_acked_seq < highest {
                match region.send_ack(last_acked_seq + 1, b"ack from arceos subscriber") {
                    Ok(()) => {
                        last_acked_seq += 1;
                        acked_any = true;
                        println!("ivc ack pub seq={last_acked_seq}");
                    }
                    Err(_) => break, // ring full, will retry
                }
            }
            if acked_any {
                notify_publisher();
            }

            if sent_data >= SUBSCRIBE_DATA_COUNT && last_acked_seq >= PASS_SEQUENCE {
                return;
            }

            waiter.wait_for_peer_event();
        }
    }

    /// Receives publisher data messages from ring A.
    fn receiver_task(region: &'static IvcRegion, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        loop {
            match region.try_recv_request(&mut payload) {
                Ok(Some(msg)) => {
                    let text = core::str::from_utf8(&payload[..msg.len()]).unwrap_or("<non-utf8>");
                    println!("ivc recv pub data seq={} msg={text}", msg.sequence());
                    HIGHEST_RECV_SEQ.store(msg.sequence(), Ordering::Release);
                    if msg.sequence() >= PASS_SEQUENCE {
                        return;
                    }
                }
                Ok(None) => waiter.wait_for_peer_event(),
                Err(err) => {
                    println!("ivc recv error {err:?}");
                    return;
                }
            }
        }
    }

    fn notify_publisher() {
        if let Err(err) = ivc::notify_channel(
            demo_config::PUBLISHER_VM_ID,
            demo_config::CHANNEL_KEY,
            demo_config::PUBLISHER_VM_ID,
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

    fn shared_region(shm_base_gpa: usize, shm_size: usize) -> Option<&'static IvcRegion> {
        let vaddr = ax_mm::iomap(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        unsafe {
            // Axvisor maps the returned GPA to the publisher's shared region.
            // Phase 2 uses atomic ring ownership for subscriber writes.
            Some(&*(vaddr.as_ptr() as *const IvcRegion))
        }
    }
}
