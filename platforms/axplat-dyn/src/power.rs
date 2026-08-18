#[cfg(feature = "smp")]
use core::hint::spin_loop;

use ax_plat::power::PowerIf;
#[cfg(feature = "smp")]
use somehal::power::{CpuOnError, SecondaryCpuStartupStatus};

#[cfg(feature = "smp")]
const CPU_ALIVE_TIMEOUT_SECONDS: usize = 10;

struct PowerImpl;

#[impl_plat_interface]
impl PowerIf for PowerImpl {
    /// Bootstraps the given CPU core with the given initial stack (in physical
    /// address).
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform).
    #[cfg(feature = "smp")]
    fn cpu_boot(cpu_id: usize, _stack_top_paddr: usize) {
        start_secondary_cpu(cpu_id)
            .unwrap_or_else(|error| panic!("failed to start logical CPU {cpu_id}: {error}"));
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        somehal::power::shutdown()
    }

    /// Reset the whole system.
    fn system_reset() -> ! {
        somehal::power::reset()
    }

    /// Get the number of CPU cores available on this platform.
    fn cpu_num() -> usize {
        somehal::smp::cpu_count()
    }
}

#[cfg(feature = "smp")]
fn start_secondary_cpu(cpu_id: usize) -> Result<(), CpuOnError> {
    let startup = somehal::power::start_secondary_cpu(cpu_id)?;
    let start = somehal::timer::ticks();
    let timeout_ticks = somehal::timer::freq().saturating_mul(CPU_ALIVE_TIMEOUT_SECONDS);

    loop {
        match startup.status() {
            SecondaryCpuStartupStatus::Alive => return startup.release(),
            SecondaryCpuStartupStatus::WaitingForAlive => {}
        }
        if somehal::timer::ticks().wrapping_sub(start) >= timeout_ticks {
            return Err(CpuOnError::AliveTimeout {
                logical_cpu: startup.logical_cpu(),
                hardware_id: startup.hardware_id(),
            });
        }
        spin_loop();
    }
}
