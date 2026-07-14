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

    use arceos_ivc_demo::{CHANNEL_KEY, CHANNEL_SIZE, IvcDemoPage, PUBLISHER_VM_ID};
    use ax_std::{
        os::arceos::modules::ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys},
        println,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};

    const PUBLISH_COUNT: u64 = 16;

    pub fn run() {
        let shm_base_gpa = HyperCallOutputSlot::new(0);
        let shm_size = HyperCallOutputSlot::new(CHANNEL_SIZE);
        let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
        let shm_size_ptr = shm_size.guest_phys_addr();

        if let Err(err) = ivc::publish_channel(CHANNEL_KEY, shm_base_gpa_ptr, shm_size_ptr) {
            println!("ivc publish failed: {err}");
            return;
        }
        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcDemoPage>() {
            println!(
                "ivc publish failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcDemoPage>()
            );
            return;
        }

        println!("ivc publish ok base={shm_base_gpa:#x} size={shm_size}");
        let Some(page) = shared_page_mut(shm_base_gpa) else {
            println!("ivc publish failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        page.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);
        publish_counter_messages(page);
    }

    fn publish_counter_messages(page: &mut IvcDemoPage) {
        for sequence in 1..=PUBLISH_COUNT {
            page.publish_message(sequence, "hello from arceos publisher");
            wait_for_subscriber_poll();
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

    fn shared_page_mut(shm_base_gpa: usize) -> Option<&'static mut IvcDemoPage> {
        let vaddr = ax_mm::iomap(PhysAddr::from_usize(shm_base_gpa), CHANNEL_SIZE).ok()?;
        unsafe {
            // Axvisor maps the returned GPA to one exclusive publisher view of
            // the shared page. The publisher is the only writer in phase 1.
            Some(&mut *(vaddr.as_mut_ptr() as *mut IvcDemoPage))
        }
    }
}
