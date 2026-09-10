#![no_std]
#![no_main]
extern crate ax_std as std;

mod overflow;

fn check_current_cpu() -> ax_cpu::pmu::PmuInfo {
    use ax_cpu::{interrupt, pmu};
    let enabled = interrupt::irqs_enabled();
    interrupt::disable_irqs();
    let info;
    {
        // SAFETY: this test owns the PMU, keeps IRQs disabled, and never migrates
        // or calls another PMU user while the session is live.
        let mut pmu = unsafe { pmu::Pmu::current() }.expect("test CPU must implement PMUv3");
        info = pmu.info();
        assert!(pmu.info().num_counters > 0);
        let counter = pmu.counter(0).unwrap();
        assert!(pmu.counter(pmu.info().num_counters).is_err());
        // SAFETY: no perf or guest exists in this test image; all counters are ours.
        unsafe { pmu.reset() };
        // SAFETY: the test owns every PMU slot and the callback is bounded.
        unsafe {
            pmu.with_counting_paused(|paused| {
                assert!(!paused.is_running());
                paused.write(counter, 0x9876).unwrap();
            });
        }
        assert!(
            !pmu.is_running(),
            "snapshot enabled an initially stopped PMU"
        );
        pmu.start();
        pmu.disable(counter).unwrap();
        pmu.write(counter, 0x1234_5678).unwrap();
        // Enabling global counting must not reset an already configured counter.
        pmu.start();
        assert_eq!(
            pmu.read(counter).unwrap(),
            0x1234_5678,
            "global PMU enable destroyed a counter"
        );
        // SAFETY: global ownership is unchanged and no callback can migrate.
        unsafe {
            pmu.with_counting_paused(|paused| {
                assert!(!paused.is_running());
                assert_eq!(paused.read(counter).unwrap(), 0x1234_5678);
            });
        }
        assert!(
            pmu.is_running(),
            "snapshot failed to restore global counting"
        );
        pmu.write(counter, 0).unwrap();
        let cycle = pmu::CounterId::CYCLE;
        pmu.configure(
            cycle,
            pmu::EventConfig {
                event: 0x11,
                exclude_user: false,
                exclude_kernel: false,
                include_hypervisor: false,
            },
        )
        .unwrap();
        pmu.preload(cycle, 10_000).unwrap();
        pmu.enable(cycle).unwrap();
        for _ in 0..1_000_000 {
            core::hint::black_box(());
        }
        pmu.disable(cycle).unwrap();
        assert_ne!(
            pmu.overflow_status() & (1 << 31),
            0,
            "cycle counter failed to overflow"
        );
        pmu.clear_overflow(1 << 31);
        assert_eq!(pmu.overflow_status() & (1 << 31), 0);
        pmu.stop();
        // Counter authorization is local to this exclusive, IRQ-excluded test.
        // Leave one readable counter's value intact and revoke every other
        // counter before returning control to a future EL0 owner.
        let instructions = pmu::CounterId::INSTRUCTIONS;
        if pmu.info().has_instruction_counter {
            assert_eq!(pmu.width(instructions).unwrap(), 64);
            pmu.disable(instructions).unwrap();
            pmu.write(instructions, 0x1234_5678_9abc_def0).unwrap();
            assert_eq!(pmu.read(instructions).unwrap(), 0x1234_5678_9abc_def0);
        } else {
            assert_eq!(pmu.read(instructions), Err(pmu::PmuError::InvalidCounter));
        }
        pmu.write(counter, 0xabcdef).unwrap();
        pmu.write(cycle, 0x123456).unwrap();
        // SAFETY: all non-readable counters are stopped and exclusively
        // unassigned; the test grants no other context access during this scope.
        unsafe { pmu.enable_user_access(1 << counter.index()) }.unwrap();
        pmu.disable_user_access();
        assert_eq!(pmu.read(counter).unwrap(), 0xabcdef);
        if pmu.info().version < 9 {
            assert_eq!(pmu.read(cycle).unwrap(), 0, "unowned cycle contents leaked");
            if pmu.info().has_instruction_counter {
                assert_eq!(
                    pmu.read(instructions).unwrap(),
                    0,
                    "unowned instruction contents leaked"
                );
            }
        } else {
            assert_eq!(
                pmu.read(cycle).unwrap(),
                0x123456,
                "per-counter authorization must preserve other owners' state"
            );
        }
        // SAFETY: the same exclusive scope remains active; validation must
        // reject a nonexistent counter before changing user authorization.
        assert_eq!(
            unsafe { pmu.enable_user_access(1 << 63) },
            Err(pmu::PmuError::InvalidCounter)
        );
    }
    if enabled {
        interrupt::enable_irqs();
    }
    info
}

fn pin_to(cpu: usize) {
    use std::os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::ax_hal,
    };
    for _ in 0..300 {
        if ax_hal::irq::is_cpu_online(cpu) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ax_hal::irq::is_cpu_online(cpu), "CPU did not become online");
    ax_set_current_affinity(AxCpuMask::one_shot(cpu)).unwrap();
    for _ in 0..256 {
        if ax_hal::percpu::this_cpu_id() == cpu {
            return;
        }
        std::thread::yield_now();
    }
    assert_eq!(ax_hal::percpu::this_cpu_id(), cpu);
}

#[unsafe(no_mangle)]
fn main() {
    use std::os::arceos::modules::ax_hal;
    let cpus = ax_hal::cpu_num();
    for cpu in 0..cpus {
        pin_to(cpu);
        let identity = ax_cpu::capability::Midr::read();
        let info = check_current_cpu();
        let enabled = ax_cpu::interrupt::irqs_enabled();
        ax_cpu::interrupt::disable_irqs();
        // SAFETY: this test owns every local slot, IRQs are masked, and affinity
        // pins the task to this CPU for the complete bounded register session.
        unsafe {
            let mut pmu = ax_cpu::pmu::Pmu::current().unwrap();
            let counter = pmu.counter(0).unwrap();
            pmu.disable(counter).unwrap();
            pmu.write(counter, 0xbeef_0000 | cpu as u64).unwrap();
        }
        if enabled {
            ax_cpu::interrupt::enable_irqs();
        }
        std::println!(
            "CPU_PMU_CORE cpu={cpu} midr={:#x} part={:#x} info={info:?}",
            identity.raw(),
            identity.part_number()
        );
    }
    overflow::run(cpus);
    for cpu in 0..cpus {
        pin_to(cpu);
        let enabled = ax_cpu::interrupt::irqs_enabled();
        ax_cpu::interrupt::disable_irqs();
        // SAFETY: no other PMU consumer exists and this scope cannot migrate.
        unsafe {
            let mut pmu = ax_cpu::pmu::Pmu::current().unwrap();
            let counter = pmu.counter(0).unwrap();
            assert_eq!(
                pmu.read(counter).unwrap(),
                0xbeef_0000 | cpu as u64,
                "configuring another CPU destroyed local PMU state"
            );
            pmu.write(counter, 0).unwrap();
        }
        if enabled {
            ax_cpu::interrupt::enable_irqs();
        }
    }
    std::println!("CPU_PMU_OK");
    std::process::exit(0);
}
