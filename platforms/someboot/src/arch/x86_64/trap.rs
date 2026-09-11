use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use ax_cpu::capability::{CpuId, Hypervisor};
use page_table_generic::PhysAddr;

const PIT_CHANNEL2_PORT: u16 = 0x42;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CONTROL_PORT: u16 = 0x61;
const PIT_CHANNEL2_GATE: u8 = 0x01;
const PIT_SPEAKER_ENABLE: u8 = 0x02;
const PIT_CHANNEL2_OUT: u8 = 0x20;
const PIT_MODE0_CHANNEL2: u8 = 0xb0;
const PIT_TICK_RATE_HZ: u64 = 1_193_182;
const TSC_PIT_CALIBRATION_MS: u64 = 50;
const TSC_PIT_MAX_POLL_COUNT: usize = 5_000_000;
const MIN_VALID_TSC_FREQ_HZ: u64 = 10_000_000;
const MAX_VALID_TSC_FREQ_HZ: u64 = 10_000_000_000;

static TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);
static HAS_INVARIANT_TSC: AtomicBool = AtomicBool::new(false);
static HAS_TSC_ADJUST: AtomicBool = AtomicBool::new(false);
static HAS_RELIABLE_VIRTUAL_TSC: AtomicBool = AtomicBool::new(false);
static TSC_INFO_STATE: AtomicU8 = AtomicU8::new(0);
static TSC_ADJUST_REFERENCE_STATE: AtomicU8 = AtomicU8::new(0);
static TSC_ADJUST_REFERENCE: AtomicU64 = AtomicU64::new(0);
static TSC_STABILITY: AtomicU8 = AtomicU8::new(0);
pub fn setup() {
    // SAFETY: BSP/AP boot paths use the same code selector, masked IRQs and
    // permanent boot stacks. The callback only uses boot diagnostics.
    unsafe { ax_cpu::boot::install_boot_vectors() };
}

pub fn trap_addr() -> usize {
    ax_cpu::boot::current_vector_table()
}

struct BootTrap;
#[trait_ffi::impl_extern_trait]
impl ax_cpu::trap::boot::BootTrapHandler for BootTrap {
    fn handle(exception: &ax_cpu::trap::boot::BootException) {
        if exception.vector == 3 {
            println!("x86_64 breakpoint: {exception:#x?}");
            return;
        }
        panic!("x86_64 boot exception: {exception:#x?}");
    }
}

pub fn init_local() {
    // The BSP and AP entry paths must install the complete CR0 state before
    // reaching this point. Check rather than repairing it late so an alternate
    // or regressed entry path cannot run with write protection or caches off.
    // SAFETY: boot runs at CPL0 before local IRQ enable or task creation.
    unsafe { ax_cpu::boot::assert_kernel_cr0_state() };
    mask_legacy_pic();
    // SAFETY: this CPU has no task-owned xstate and uses NX-capable mappings.
    unsafe {
        ax_cpu::boot::enable_execute_disable();
        ax_cpu::boot::enable_xsave_features();
    }
    init_tsc_freq();
    validate_local_tsc();
}

pub fn tsc_freq() -> usize {
    let freq = TSC_FREQ_HZ.load(Ordering::Acquire);
    if freq == 0 {
        panic!("x86_64 TSC frequency is not initialized");
    }
    freq as usize
}

pub fn scheduler_counter_stability() -> crate::timer::CounterStability {
    if TSC_STABILITY.load(Ordering::Acquire) == 1 {
        crate::timer::CounterStability::Stable
    } else {
        crate::timer::CounterStability::Unstable
    }
}

fn validate_local_tsc() {
    let cpuid = CpuId::new();
    let invariant_tsc = cpuid
        .get_advanced_power_mgmt_info()
        .is_some_and(|info| info.has_invariant_tsc());
    let cpu_count = crate::smp::cpu_count();
    let reliable_virtual_tsc = HAS_RELIABLE_VIRTUAL_TSC.load(Ordering::Acquire);
    let rate_stable = invariant_tsc || reliable_virtual_tsc;
    let synchronization_trusted = if HAS_TSC_ADJUST.load(Ordering::Acquire) {
        tsc_adjust_matches_reference()
    } else {
        cpu_count == 1 || reliable_virtual_tsc
    };
    if classify_scheduler_counter(rate_stable, cpu_count, synchronization_trusted)
        == crate::timer::CounterStability::Unstable
    {
        TSC_STABILITY.store(2, Ordering::Release);
        return;
    }

    let _ = TSC_STABILITY.compare_exchange(0, 1, Ordering::Release, Ordering::Relaxed);
}

fn tsc_adjust_matches_reference() -> bool {
    let current_adjust = unsafe { ax_cpu::timer::read_adjustment() };
    if TSC_ADJUST_REFERENCE_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        TSC_ADJUST_REFERENCE.store(current_adjust, Ordering::Relaxed);
        TSC_ADJUST_REFERENCE_STATE.store(2, Ordering::Release);
        return true;
    }
    while TSC_ADJUST_REFERENCE_STATE.load(Ordering::Acquire) != 2 {
        spin_loop();
    }
    current_adjust == TSC_ADJUST_REFERENCE.load(Ordering::Acquire)
}

const fn classify_scheduler_counter(
    rate_stable: bool,
    cpu_count: usize,
    synchronization_trusted: bool,
) -> crate::timer::CounterStability {
    if rate_stable && (cpu_count == 1 || synchronization_trusted) {
        crate::timer::CounterStability::Stable
    } else {
        crate::timer::CounterStability::Unstable
    }
}

fn init_tsc_freq() {
    if TSC_INFO_STATE.load(Ordering::Acquire) == 2 {
        return;
    }
    if TSC_INFO_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        while TSC_INFO_STATE.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
        return;
    }

    let cpuid = CpuId::new();
    let freq_hz = hypervisor_tsc_freq_hz(&cpuid)
        .or_else(|| cpuid_tsc_freq_hz(&cpuid))
        .or_else(pit_calibrate_tsc_freq_hz)
        .or_else(|| processor_base_freq_hz(&cpuid))
        .unwrap_or_else(|| {
            let fallback = 1_000_000_000u64;
            warn!("x86_64 TSC frequency unavailable, fallback to {fallback} Hz");
            fallback
        });
    let has_invariant_tsc = cpuid
        .get_advanced_power_mgmt_info()
        .is_some_and(|info| info.has_invariant_tsc());
    let has_tsc_adjust = cpuid
        .get_extended_feature_info()
        .is_some_and(|info| info.has_tsc_adjust_msr());
    let has_reliable_virtual_tsc = cpuid.get_hypervisor_info().is_some_and(|hypervisor| {
        matches!(hypervisor.identify(), Hypervisor::KVM | Hypervisor::QEMU)
    });

    HAS_INVARIANT_TSC.store(has_invariant_tsc, Ordering::Release);
    HAS_TSC_ADJUST.store(has_tsc_adjust, Ordering::Release);
    HAS_RELIABLE_VIRTUAL_TSC.store(has_reliable_virtual_tsc, Ordering::Release);

    TSC_FREQ_HZ.store(freq_hz, Ordering::Release);
    TSC_INFO_STATE.store(2, Ordering::Release);
}

fn valid_tsc_freq_hz(freq: u64) -> Option<u64> {
    (MIN_VALID_TSC_FREQ_HZ..=MAX_VALID_TSC_FREQ_HZ)
        .contains(&freq)
        .then_some(freq)
}

fn hypervisor_tsc_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_hypervisor_info()
        .and_then(|hv| hv.tsc_frequency())
        .map(|khz| khz as u64 * 1_000)
        .and_then(valid_tsc_freq_hz)
}

fn cpuid_tsc_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_tsc_info()
        .and_then(|info| {
            if let Some(freq) = info.tsc_frequency().and_then(valid_tsc_freq_hz) {
                return Some(freq);
            }

            let numerator = info.numerator();
            let denominator = info.denominator();
            if numerator == 0 || denominator == 0 {
                return None;
            }

            let base_hz = processor_base_freq_hz(cpuid)? as u128;
            let crystal_hz = base_hz * denominator as u128 / numerator as u128;
            Some((crystal_hz * numerator as u128 / denominator as u128) as u64)
        })
        .and_then(valid_tsc_freq_hz)
}

fn processor_base_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_processor_frequency_info()
        .map(|pinfo| pinfo.processor_base_frequency() as u64 * 1_000_000)
        .and_then(valid_tsc_freq_hz)
}

fn pit_calibrate_tsc_freq_hz() -> Option<u64> {
    let latch = ((PIT_TICK_RATE_HZ * TSC_PIT_CALIBRATION_MS) / 1_000) as u16;

    unsafe {
        let control = x86::io::inb(PIT_CONTROL_PORT);
        x86::io::outb(
            PIT_CONTROL_PORT,
            (control & !PIT_SPEAKER_ENABLE) | PIT_CHANNEL2_GATE,
        );
        x86::io::outb(PIT_COMMAND_PORT, PIT_MODE0_CHANNEL2);
        x86::io::outb(PIT_CHANNEL2_PORT, (latch & 0xff) as u8);
        x86::io::outb(PIT_CHANNEL2_PORT, (latch >> 8) as u8);
    }

    let start = ax_cpu::timer::read_counter();
    let mut end = start;
    let mut done = false;
    for _ in 0..TSC_PIT_MAX_POLL_COUNT {
        if unsafe { x86::io::inb(PIT_CONTROL_PORT) } & PIT_CHANNEL2_OUT != 0 {
            end = ax_cpu::timer::read_counter();
            done = true;
            break;
        }
        end = ax_cpu::timer::read_counter();
        spin_loop();
    }

    if !done {
        return None;
    }

    end.wrapping_sub(start)
        .checked_mul(1_000)?
        .checked_div(TSC_PIT_CALIBRATION_MS)
        .and_then(valid_tsc_freq_hz)
}

fn mask_legacy_pic() {
    unsafe {
        x86::io::outb(0x21, 0xff);
        x86::io::outb(0xa1, 0xff);
    }
}

pub fn current_cr3() -> PhysAddr {
    ax_cpu::mmu::read_kernel_page_table()
}

pub fn set_cr3(addr: PhysAddr) {
    // SAFETY: the boot mapping owner retains the root and executing mappings.
    unsafe {
        ax_cpu::mmu::write_kernel_page_table(addr);
    }
}

#[cfg(test)]
mod tests {
    use super::classify_scheduler_counter;
    use crate::timer::CounterStability;

    #[test]
    fn stable_tsc_rate_uses_stable_path_only_with_trusted_synchronization() {
        assert_eq!(
            classify_scheduler_counter(true, 1, true),
            CounterStability::Stable
        );
        assert_eq!(
            classify_scheduler_counter(false, 1, true),
            CounterStability::Unstable
        );
        assert_eq!(
            classify_scheduler_counter(true, 2, true),
            CounterStability::Stable
        );
        assert_eq!(
            classify_scheduler_counter(true, 1, false),
            CounterStability::Unstable
        );
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TimerOperation {
        Mask,
        Unmask,
        Serialize,
        Clear,
    }

    struct RecordingTimerHardware {
        operations: Cell<[Option<TimerOperation>; 4]>,
        len: Cell<usize>,
    }

    impl RecordingTimerHardware {
        const fn new() -> Self {
            Self {
                operations: Cell::new([None; 4]),
                len: Cell::new(0),
            }
        }

        fn record(&self, operation: TimerOperation) {
            let len = self.len.get();
            let mut operations = self.operations.get();
            operations[len] = Some(operation);
            self.operations.set(operations);
            self.len.set(len + 1);
        }

        fn snapshot(&self) -> ([Option<TimerOperation>; 4], usize) {
            (self.operations.get(), self.len.get())
        }
    }

    impl OneShotTimerHardware for RecordingTimerHardware {
        fn set_irq_masked(&self, masked: bool) {
            self.record(if masked {
                TimerOperation::Mask
            } else {
                TimerOperation::Unmask
            });
        }

        fn serialize_lvt_update(&self) {
            self.record(TimerOperation::Serialize);
        }

        fn clear_comparator(&self) {
            self.record(TimerOperation::Clear);
        }
    }

    #[test]
    fn timer_shutdown_masks_source_before_clearing_comparator() {
        let timer = RecordingTimerHardware::new();

        stop_oneshot(&timer);

        let (operations, len) = timer.snapshot();
        assert_eq!(
            &operations[..len],
            &[Some(TimerOperation::Mask), Some(TimerOperation::Clear)]
        );
    }

    #[test]
    fn timer_unmask_is_serialized_before_comparator_programming() {
        let timer = RecordingTimerHardware::new();

        enable_timer_irq(&timer);

        let (operations, len) = timer.snapshot();
        assert_eq!(
            &operations[..len],
            &[
                Some(TimerOperation::Unmask),
                Some(TimerOperation::Serialize),
            ]
        );
    }

    #[test]
    fn only_xapic_tsc_deadline_transition_requires_mfence() {
        assert!(requires_lvt_deadline_fence(ApicMode::XApic, true));
        assert!(!requires_lvt_deadline_fence(ApicMode::XApic, false));
        assert!(!requires_lvt_deadline_fence(ApicMode::X2Apic, true));
        assert!(!requires_lvt_deadline_fence(ApicMode::X2Apic, false));
    }
}
