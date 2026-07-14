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
    use core::{cell::UnsafeCell, result::Result::Err};

    use ax_std::{
        os::arceos::modules::ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys},
        println,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{IVC_SLOT_PAYLOAD_SIZE, IvcRegion};

    const PUBLISH_COUNT: u64 = 5;
    mod demo_config {
        pub const CHANNEL_KEY: usize = 0x4956_4301;
        pub const CHANNEL_SIZE: usize = 4096;
        pub const PUBLISHER_VM_ID: usize = 1;
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
        let Some(region) = shared_page_mut(shm_base_gpa) else {
            println!("ivc publish failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        region.initialize(demo_config::PUBLISHER_VM_ID, demo_config::CHANNEL_KEY);
        run_request_ack_demo(region);
    }

    fn run_request_ack_demo(region: &'static IvcRegion) {
        let mut ack_payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        for sequence in 1..=PUBLISH_COUNT {
            send_request_when_ready(region, sequence);
            wait_for_ack(region, sequence, &mut ack_payload);
        }
    }

    fn send_request_when_ready(region: &'static IvcRegion, sequence: u64) {
        loop {
            match region.send_request(sequence, b"hello from arceos publisher") {
                Ok(()) => {
                    println!("ivc send seq={sequence}");
                    return;
                }
                Err(_) => wait_for_subscriber_poll(),
            }
        }
    }

    fn wait_for_ack(region: &'static IvcRegion, sequence: u64, payload: &mut [u8]) {
        loop {
            match region.try_recv_ack(payload) {
                Ok(Some(message)) if message.sequence() == sequence => {
                    let text =
                        core::str::from_utf8(&payload[..message.len()]).unwrap_or("<non-utf8>");
                    println!("ivc ack seq={} msg={text}", message.sequence());
                    return;
                }
                Ok(Some(_)) | Ok(None) => wait_for_subscriber_poll(),
                Err(err) => {
                    println!("ivc publish failed: recv ack error {err:?}");
                    return;
                }
            }
        }
    }

    fn wait_for_subscriber_poll() {
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

    fn shared_page_mut(shm_base_gpa: usize) -> Option<&'static mut IvcRegion> {
        let vaddr = ax_mm::iomap(
            PhysAddr::from_usize(shm_base_gpa),
            demo_config::CHANNEL_SIZE,
        )
        .ok()?;
        unsafe {
            // Axvisor maps the returned GPA to one exclusive publisher view of
            // the shared region before subscribers can use the phase-2 rings.
            Some(&mut *(vaddr.as_mut_ptr() as *mut IvcRegion))
        }
    }
}
