use core::sync::atomic::{AtomicU64, Ordering};

use ax_plat::time::{NANOS_PER_MICROS, TimeIf};

static TICKS: AtomicU64 = AtomicU64::new(0);
static EPOCH_OFFSET_NANOS: AtomicU64 = AtomicU64::new(0);

struct TimeIfImpl;

#[impl_plat_interface]
impl TimeIf for TimeIfImpl {
    fn current_ticks() -> u64 {
        TICKS.fetch_add(1, Ordering::Relaxed)
    }

    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks.saturating_mul(NANOS_PER_MICROS)
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos / NANOS_PER_MICROS
    }

    fn epochoffset_nanos() -> u64 {
        EPOCH_OFFSET_NANOS.load(Ordering::Relaxed)
    }

    #[cfg(feature = "irq")]
    fn irq_num() -> ax_plat::irq::IrqId {
        ax_plat::irq::IrqNumber(0).expect("example timer IRQ is in range")
    }

    #[cfg(feature = "irq")]
    fn set_oneshot_timer(_deadline_ns: u64) {}
}

pub fn enable_timer_irq() {}

pub fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    EPOCH_OFFSET_NANOS
        .compare_exchange(0, epoch_time_nanos, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
