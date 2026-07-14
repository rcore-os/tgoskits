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
    };

    use arceos_ivc_demo::{
        CHANNEL_KEY, CHANNEL_SIZE, IvcDemoPage, MESSAGE_CAPACITY, PUBLISHER_VM_ID, SUBSCRIBER_VM_ID,
    };
    use ax_std::{
        os::arceos::modules::ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys},
        println,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};

    const MAX_SUBSCRIBE_ATTEMPTS: usize = 80;
    const PASS_SEQUENCE: u64 = 5;

    pub fn run() {
        let Some((shm_base_gpa, shm_size)) = subscribe_with_retry() else {
            println!("ivc subscribe failed: retry limit reached");
            return;
        };

        println!(
            "ivc subscribe ok subscriber={} base={shm_base_gpa:#x} size={shm_size}",
            SUBSCRIBER_VM_ID
        );
        let Some(page) = shared_page(shm_base_gpa) else {
            println!("ivc subscribe failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        if !page.header_matches(PUBLISHER_VM_ID, CHANNEL_KEY) {
            println!(
                "ivc subscribe failed: unexpected header publisher/key for base={shm_base_gpa:#x}"
            );
            return;
        }
        poll_messages(page);
    }

    fn subscribe_with_retry() -> Option<(usize, usize)> {
        for attempt in 1..=MAX_SUBSCRIBE_ATTEMPTS {
            let shm_base_gpa = HyperCallOutputSlot::new(0);
            let shm_size = HyperCallOutputSlot::new(0);
            let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
            let shm_size_ptr = shm_size.guest_phys_addr();

            match ivc::subscribe_channel(
                PUBLISHER_VM_ID,
                CHANNEL_KEY,
                shm_base_gpa_ptr,
                shm_size_ptr,
            ) {
                Ok(()) => return Some((shm_base_gpa.read(), shm_size.read())),
                Err(err) => {
                    if attempt == 1 || attempt % 10 == 0 {
                        println!("ivc subscribe retry attempt={attempt} err={err}");
                    }
                    wait_for_publisher();
                }
            }
        }
        None
    }

    fn poll_messages(page: &'static IvcDemoPage) {
        let mut last_sequence = 0;
        let mut buffer = [0u8; MESSAGE_CAPACITY];
        loop {
            let snapshot = page.read_message(&mut buffer);
            if snapshot.sequence() > last_sequence {
                last_sequence = snapshot.sequence();
                let message =
                    core::str::from_utf8(&buffer[..snapshot.len()]).unwrap_or("<non-utf8>");
                println!("ivc recv seq={} msg={message}", snapshot.sequence());
                if snapshot.sequence() >= PASS_SEQUENCE {
                    println!("ivc demo pass");
                    return;
                }
            }
            wait_for_publisher();
        }
    }

    fn wait_for_publisher() {
        for _ in 0..100_000 {
            core::hint::spin_loop();
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

    fn shared_page(shm_base_gpa: usize) -> Option<&'static IvcDemoPage> {
        let vaddr = ax_mm::iomap(PhysAddr::from_usize(shm_base_gpa), CHANNEL_SIZE).ok()?;
        unsafe {
            // Axvisor maps the returned GPA to the publisher's shared frame.
            // The subscriber only reads the page in phase 1.
            Some(&*(vaddr.as_ptr() as *const IvcDemoPage))
        }
    }
}
