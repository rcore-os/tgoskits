//! Test-only host scheduler observations for the AArch64 idle-poll A/B case.

use core::time::Duration;

const OBSERVED_CPUS: [usize; 2] = [1, 2];
const EXPECTED_VCPU_CPU_SETS: [usize; 2] = [0b10, 0b100];
const SAME_CORE_WORKER_CPU: usize = OBSERVED_CPUS[0];
const SAMPLE_COUNT: usize = 6_000;
const PERIOD: Duration = Duration::from_millis(1);

#[derive(Clone, Copy)]
struct CpuSnapshot {
    busy_ticks: u64,
    context_switches: u64,
}

impl CpuSnapshot {
    fn capture(cpu: usize) -> Self {
        use ax_std::os::arceos::modules::ax_task;

        Self {
            busy_ticks: ax_task::cpu_busy_ticks(cpu),
            context_switches: ax_task::cpu_context_switches(cpu),
        }
    }

    fn delta_from(self, start: Self) -> Self {
        Self {
            busy_ticks: self.busy_ticks.saturating_sub(start.busy_ticks),
            context_switches: self.context_switches.saturating_sub(start.context_switches),
        }
    }
}

pub(crate) fn start() {
    if !has_expected_vcpu_affinity() {
        error!(
            "AXVISOR_RT_HOST_SCHED_FAILED expected vCPU masks {:#x?}",
            EXPECTED_VCPU_CPU_SETS
        );
        return;
    }
    println!(
        "AXVISOR_RT_HOST_AFFINITY vcpu0_mask={:#x} vcpu1_mask={:#x}",
        EXPECTED_VCPU_CPU_SETS[0], EXPECTED_VCPU_CPU_SETS[1]
    );
    std::thread::Builder::new()
        .name("axvisor-rt-observe".into())
        .spawn(run_same_core_worker)
        .unwrap_or_else(|error| panic!("failed to start realtime observer: {error}"));
}

fn has_expected_vcpu_affinity() -> bool {
    let Some(vm) = crate::manager::AxvmManager::vm_by_id(1) else {
        return false;
    };
    let snapshots = vm.vcpu_snapshots();
    snapshots.len() == EXPECTED_VCPU_CPU_SETS.len()
        && snapshots
            .iter()
            .zip(EXPECTED_VCPU_CPU_SETS)
            .all(|(vcpu, expected_mask)| vcpu.phys_cpu_set == Some(expected_mask))
}

fn run_same_core_worker() {
    use ax_std::os::arceos::modules::ax_task;

    if !ax_task::set_current_affinity(ax_task::AxCpuMask::one_shot(SAME_CORE_WORKER_CPU)) {
        error!("AXVISOR_RT_HOST_SCHED_FAILED unable to bind worker to CPU{SAME_CORE_WORKER_CPU}");
        return;
    }

    let start = std::time::Instant::now();
    let start_cpu1 = CpuSnapshot::capture(OBSERVED_CPUS[0]);
    let start_cpu2 = CpuSnapshot::capture(OBSERVED_CPUS[1]);
    let mut max_delay = Duration::ZERO;
    for _ in 0..SAMPLE_COUNT {
        let requested = std::time::Instant::now();
        std::thread::sleep(PERIOD);
        max_delay = max_delay.max(requested.elapsed().saturating_sub(PERIOD));
    }
    let cpu1 = CpuSnapshot::capture(OBSERVED_CPUS[0]).delta_from(start_cpu1);
    let cpu2 = CpuSnapshot::capture(OBSERVED_CPUS[1]).delta_from(start_cpu2);
    println!(
        "AXVISOR_RT_HOST_SCHED window_ns={} same_core_cpu={} max_worker_delay_ns={} pCPU1_busy_ticks={} pCPU1_context_switches={} pCPU2_busy_ticks={} pCPU2_context_switches={}",
        start.elapsed().as_nanos(),
        SAME_CORE_WORKER_CPU,
        max_delay.as_nanos(),
        cpu1.busy_ticks,
        cpu1.context_switches,
        cpu2.busy_ticks,
        cpu2.context_switches,
    );
}
