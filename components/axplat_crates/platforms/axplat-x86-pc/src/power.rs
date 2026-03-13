//! Power management.

use axplat::power::PowerIf;
use x86_64::instructions::port::PortWriteOnly;

struct PowerImpl;

#[impl_plat_interface]
impl PowerIf for PowerImpl {
    /// Bootstraps the given CPU core with the given initial stack (in physical
    /// address).
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform).
    #[cfg(feature = "smp")]
    fn cpu_boot(cpu_id: usize, stack_top_paddr: usize) {
        use axplat::mem::pa;
        crate::mp::start_secondary_cpu(cpu_id, pa!(stack_top_paddr))
    }

    /// Shutdown the whole system (in QEMU).
    ///
    /// See <https://wiki.osdev.org/Shutdown> for more information.
    fn system_off() -> ! {
        info!("Shutting down...");

        // For real hardware platforms, using port `0x604` to shutdown does not
        // work. Therefore we use port `0x64` to reboot the system instead.
        if cfg!(feature = "reboot-on-system-off") {
            axplat::console_println!("System will reboot, press any key to continue ...");
            while super::console::getchar().is_none() {}
            axplat::console_println!("Rebooting ...");
            unsafe { PortWriteOnly::new(0x64).write(0xfeu8) };
        } else {
            unsafe { PortWriteOnly::new(0x604).write(0x2000u16) };
        }

        axcpu::asm::halt();
        warn!("It should shutdown!");
        loop {
            axcpu::asm::halt();
        }
    }

    /// Get the number of CPU cores available on this platform.
    fn cpu_num() -> usize {
        crate::config::plat::MAX_CPU_NUM
    }
}
