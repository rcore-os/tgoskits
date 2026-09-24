#![no_std]
#![no_main]

extern crate ax_std as std;

use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::ax_hal,
    },
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use ax_driver::soc::set_test_cpu_temperature_mc;
use ax_runtime::cpufreq::{
    self, DomainId, DomainInfo, DomainSnapshot, FrequencyError, Governor, OperatingPoint,
};

const SAMPLE_TICKS_MS: u64 = 40;
const MAX_DEVIATION_PERCENT: u64 = 12;

fn pin_to(cpu: usize) {
    assert!(ax_hal::irq::is_cpu_online(cpu), "CPU {cpu} is offline");
    ax_set_current_affinity(AxCpuMask::one_shot(cpu)).unwrap();
    for _ in 0..256 {
        if ax_hal::percpu::this_cpu_id() == cpu {
            return;
        }
        std::thread::yield_now();
    }
    assert_eq!(ax_hal::percpu::this_cpu_id(), cpu, "affinity failed");
}

fn measure_delivered_hz(cpu: usize) -> u64 {
    use ax_cpu::{interrupt, pmu, timer};

    pin_to(cpu);
    let timer_hz = timer::counter_frequency();
    assert!(timer_hz > 0, "generic timer frequency is zero");
    let window_ticks = timer_hz * SAMPLE_TICKS_MS / 1_000;
    assert!(window_ticks > 0);

    let irqs_were_enabled = interrupt::irqs_enabled();
    interrupt::disable_irqs();
    let (cycles, elapsed_ticks) = {
        // SAFETY: this standalone board test has no perf or guest owner. The
        // task is pinned, local IRQs are masked, and the session never sleeps.
        let mut pmu = unsafe { pmu::Pmu::current() }.expect("PMUv3 unavailable");
        // SAFETY: every local PMU counter belongs to this standalone test.
        unsafe { pmu.reset() };
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
        pmu.enable(cycle).unwrap();
        pmu.start();
        let begin_ticks = timer::virtual_counter();
        let begin_cycles = pmu.read(cycle).unwrap();
        let mut work = 1u64;
        while timer::virtual_counter().wrapping_sub(begin_ticks) < window_ticks {
            work = work.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        core::hint::black_box(work);
        let end_cycles = pmu.read(cycle).unwrap();
        let end_ticks = timer::virtual_counter();
        pmu.disable(cycle).unwrap();
        pmu.stop();
        (
            end_cycles.wrapping_sub(begin_cycles),
            end_ticks.wrapping_sub(begin_ticks),
        )
    };
    if irqs_were_enabled {
        interrupt::enable_irqs();
    }
    assert!(elapsed_ticks >= window_ticks);
    let hz = (u128::from(cycles) * u128::from(timer_hz) / u128::from(elapsed_ticks)) as u64;
    std::println!(
        "CPU_CPUFREQ_SAMPLE cpu={cpu} cycles={cycles} ticks={elapsed_ticks} timer_hz={timer_hz} \
         delivered_hz={hz}"
    );
    hz
}

fn assert_delivered(cpu: usize, domain: DomainId, opp: OperatingPoint) -> u16 {
    let delivered = measure_delivered_hz(cpu);
    let part = ax_cpu::capability::Midr::read().part_number();
    assert!(
        matches!(part, 0xd05 | 0xd0b),
        "CPU {cpu} has unexpected MIDR"
    );
    let error = delivered.abs_diff(opp.frequency_hz);
    let deviation = u128::from(error) * 100 / u128::from(opp.frequency_hz);
    std::println!(
        "CPU_CPUFREQ_OPP cpu={cpu} part={part:#x} domain={domain:?} target_hz={} voltage_uv={:?} \
         measured_hz={} deviation_pct={deviation}",
        opp.frequency_hz,
        opp.voltage_uv,
        delivered,
    );
    assert!(
        u128::from(error) * 100 <= u128::from(opp.frequency_hz) * u128::from(MAX_DEVIATION_PERCENT),
        "CPU {cpu} {domain:?} delivered {delivered} Hz, OPP requested {} Hz",
        opp.frequency_hz
    );
    part
}

fn assert_domain(domain: &DomainInfo, opp: OperatingPoint) {
    let snapshot = cpufreq::snapshot(domain.id).unwrap();
    assert_eq!(snapshot.info, *domain);
    assert_eq!(
        snapshot.current_opp, opp,
        "driver state disagrees with requested OPP"
    );
    let mut domain_part = None;
    for &cpu in &domain.cpu_ids {
        let part = assert_delivered(cpu, domain.id, opp);
        assert_eq!(
            domain_part.get_or_insert(part),
            &part,
            "mixed CPU types in one domain"
        );
    }
    let expected_count = match domain_part.expect("domain has no CPU") {
        0xd05 => 4, // Cortex-A55 is one shared clock domain.
        0xd0b => 2, // Each Cortex-A76 pair has an independent clock domain.
        _ => unreachable!(),
    };
    assert_eq!(domain.cpu_ids.len(), expected_count);
}

fn assert_domain_membership(domains: &[DomainInfo]) {
    assert_eq!(domains.len(), 3, "RK3588 must expose three domains");
    let mut seen = [false; 8];
    let mut little_domains = 0;
    let mut big_domains = 0;
    for domain in domains {
        assert!(!domain.cpu_ids.is_empty(), "empty domain {domain:?}");
        let mut domain_part = None;
        for &cpu in &domain.cpu_ids {
            assert!(cpu < seen.len(), "domain {domain:?} has invalid CPU {cpu}");
            assert!(!seen[cpu], "CPU {cpu} appears in multiple domains");
            assert!(ax_hal::irq::is_cpu_online(cpu), "CPU {cpu} is offline");
            pin_to(cpu);
            let part = ax_cpu::capability::Midr::read().part_number();
            assert!(
                matches!(part, 0xd05 | 0xd0b),
                "CPU {cpu} has unexpected MIDR"
            );
            assert_eq!(
                domain_part.get_or_insert(part),
                &part,
                "mixed CPU types in one domain"
            );
            seen[cpu] = true;
        }
        match domain_part.expect("domain has no CPU") {
            0xd05 => {
                assert_eq!(domain.cpu_ids.len(), 4, "A55 cluster must share a domain");
                little_domains += 1;
            }
            0xd0b => {
                assert_eq!(domain.cpu_ids.len(), 2, "A76 pairs need separate domains");
                big_domains += 1;
            }
            _ => unreachable!(),
        }
    }
    assert!(
        seen.into_iter().all(|member| member),
        "not all CPUs are mapped"
    );
    assert_eq!((little_domains, big_domains), (1, 2));
}

fn assert_concurrent_requests(domain: &DomainInfo, lower: OperatingPoint, upper: OperatingPoint) {
    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let mut workers = std::vec::Vec::new();
    for (target, &cpu) in [lower.frequency_hz, upper.frequency_hz]
        .into_iter()
        .zip(&domain.cpu_ids)
    {
        let ready = ready.clone();
        let start = start.clone();
        let id = domain.id;
        workers.push(std::thread::spawn(move || {
            pin_to(cpu);
            ready.fetch_add(1, Ordering::Release);
            while !start.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            cpufreq::set_fixed_frequency(id, target)
        }));
    }
    let ready_start = ax_cpu::timer::virtual_counter();
    let ready_timeout = ax_cpu::timer::counter_frequency() * 5;
    while ready.load(Ordering::Acquire) != workers.len() {
        assert!(
            ax_cpu::timer::virtual_counter().wrapping_sub(ready_start) < ready_timeout,
            "concurrent request workers did not reach the start gate"
        );
        std::thread::yield_now();
    }
    start.store(true, Ordering::Release);
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    let current = cpufreq::snapshot(domain.id).unwrap().current_opp;
    assert!(
        current == lower || current == upper,
        "concurrent requests left an unrelated OPP"
    );
    assert_domain(domain, current);
}

fn wait_for_snapshots(
    domains: &[&DomainInfo],
    ready: impl Fn(&[DomainSnapshot]) -> bool,
) -> std::vec::Vec<DomainSnapshot> {
    let start = ax_cpu::timer::virtual_counter();
    let timeout = ax_cpu::timer::counter_frequency() * 5;
    loop {
        let snapshots: std::vec::Vec<_> = domains
            .iter()
            .map(|domain| cpufreq::snapshot(domain.id).unwrap())
            .collect();
        if ready(&snapshots) {
            return snapshots;
        }
        assert!(
            ax_cpu::timer::virtual_counter().wrapping_sub(start) < timeout,
            "CPU frequency thermal transition did not settle"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_controlled_thermal_limits(domains: &[&DomainInfo]) {
    let normal = wait_for_snapshots(domains, |_| true);
    let caps = [1_608_000_000, 2_208_000_000, 2_208_000_000];
    for (domain, snapshot) in domains.iter().zip(&normal) {
        assert!(
            snapshot.limits.max_hz > caps[domain.id.raw() as usize],
            "domain {:?} has no high OPP for the hot-limit test",
            domain.id
        );
    }

    set_test_cpu_temperature_mc(Some(86_000));
    let hot = wait_for_snapshots(domains, |snapshots| {
        snapshots.iter().zip(&normal).all(|(now, before)| {
            now.limits.max_hz < before.limits.max_hz
                && now.current_opp.frequency_hz <= now.limits.max_hz
        })
    });
    for (domain, snapshot) in domains.iter().zip(&hot) {
        assert!(snapshot.limits.max_hz <= caps[domain.id.raw() as usize]);
        assert_domain(domain, snapshot.current_opp);
    }
    set_test_cpu_temperature_mc(Some(80_000));
    std::thread::sleep(Duration::from_millis(150));
    for snapshot in wait_for_snapshots(domains, |_| true) {
        assert!(snapshot.limits.max_hz < normal[snapshot.info.id.raw() as usize].limits.max_hz);
    }
    set_test_cpu_temperature_mc(Some(79_000));
    let restored = wait_for_snapshots(domains, |snapshots| {
        snapshots.iter().zip(&normal).all(|(now, before)| {
            now.limits.max_hz == before.limits.max_hz
                && now.current_opp.frequency_hz == before.limits.max_hz
        })
    });
    for (domain, snapshot) in domains.iter().zip(&restored) {
        assert_domain(domain, snapshot.current_opp);
    }

    for domain in domains.iter().filter(|domain| domain.cpu_ids.len() == 2) {
        let lowest = cpufreq::available_opps(domain.id).unwrap()[0];
        cpufreq::set_fixed_frequency(domain.id, lowest.frequency_hz).unwrap();
    }
    for domain in domains.iter().filter(|domain| domain.cpu_ids.len() == 4) {
        let lowest = cpufreq::available_opps(domain.id).unwrap()[0];
        cpufreq::set_fixed_frequency(domain.id, lowest.frequency_hz).unwrap();
    }
    let low_baseline = wait_for_snapshots(domains, |_| true);
    assert!(low_baseline.iter().all(|snapshot| {
        snapshot.current_opp.frequency_hz == 408_000_000
            && snapshot
                .current_opp
                .voltage_uv
                .is_some_and(|uv| uv < 750_000)
    }));
    set_test_cpu_temperature_mc(Some(9_000));
    let cold = wait_for_snapshots(domains, |snapshots| {
        snapshots
            .iter()
            .all(|snapshot| snapshot.current_opp.voltage_uv == Some(750_000))
    });
    for (domain, snapshot) in domains.iter().zip(&cold) {
        assert_domain(domain, snapshot.current_opp);
    }
    set_test_cpu_temperature_mc(Some(15_000));
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_for_snapshots(domains, |_| true)
            .iter()
            .all(|snapshot| snapshot.current_opp.voltage_uv == Some(750_000))
    );
    set_test_cpu_temperature_mc(Some(16_000));
    wait_for_snapshots(domains, |snapshots| {
        snapshots
            .iter()
            .zip(&low_baseline)
            .all(|(now, before)| now.current_opp.voltage_uv == before.current_opp.voltage_uv)
    });
    set_test_cpu_temperature_mc(None);
    std::println!("CPU_CPUFREQ_THERMAL_OK");
}

#[unsafe(no_mangle)]
fn main() {
    assert_eq!(ax_hal::cpu_num(), 8, "all RK3588 CPUs must be online");
    let domains = cpufreq::domains().unwrap();
    assert_domain_membership(&domains);
    let first_performance = cpufreq::set_governor(Governor::Performance);
    for domain in &domains {
        let Ok(opps) = cpufreq::available_opps(domain.id) else {
            continue;
        };
        if let Some(highest) = opps.last() {
            assert_domain(domain, *highest);
        }
    }
    for domain in domains.iter().filter(|domain| domain.cpu_ids.len() == 2) {
        if let Ok(opps) = cpufreq::available_opps(domain.id) {
            cpufreq::set_fixed_frequency(domain.id, opps[0].frequency_hz).unwrap();
        }
    }
    let mut ready_domains = std::vec::Vec::new();
    let mut unready_domains = 0;

    for domain in &domains {
        let opps = match cpufreq::available_opps(domain.id) {
            Ok(opps) => opps,
            Err(FrequencyError::NotReady) => {
                unready_domains += 1;
                for &cpu in &domain.cpu_ids {
                    let hz = measure_delivered_hz(cpu);
                    std::println!(
                        "CPU_CPUFREQ_UNREADY cpu={cpu} domain={:?} delivered_hz={hz}",
                        domain.id
                    );
                }
                continue;
            }
            Err(error) => panic!("domain {:?} OPP query failed: {error}", domain.id),
        };
        let limits = cpufreq::limits(domain.id).unwrap();
        let lowest = *opps.first().expect("domain has no available OPP");
        let highest = *opps.last().expect("domain has no available OPP");
        assert!(
            lowest.frequency_hz < highest.frequency_hz,
            "domain {domain:?} has no adjustable OPP range"
        );
        assert_eq!(limits.min_hz, lowest.frequency_hz);
        assert_eq!(limits.max_hz, highest.frequency_hz);
        std::println!("CPU_CPUFREQ_DOMAIN domain={domain:?} opps={opps:?} limits={limits:?}");
        cpufreq::set_fixed_frequency(domain.id, highest.frequency_hz).unwrap();
        assert_domain(domain, highest);
        ready_domains.push(domain);

        cpufreq::set_fixed_frequency(domain.id, lowest.frequency_hz).unwrap();
        assert_domain(domain, lowest);
        cpufreq::set_fixed_frequency(domain.id, highest.frequency_hz).unwrap();
        assert_domain(domain, highest);
        assert_concurrent_requests(domain, lowest, highest);
        cpufreq::set_fixed_frequency(domain.id, lowest.frequency_hz).unwrap();
    }

    assert!(!ready_domains.is_empty(), "no CPU domain is adjustable");
    match (first_performance, unready_domains) {
        (Ok(()), 0) | (Err(FrequencyError::NotReady), 1..) => {}
        (result, count) => panic!("performance result {result:?} with {count} unready domains"),
    }

    let second_performance = cpufreq::set_governor(Governor::Performance);
    match (second_performance, unready_domains) {
        (Ok(()), 0) | (Err(FrequencyError::NotReady), 1..) => {}
        (result, count) => panic!("performance result {result:?} with {count} unready domains"),
    }
    for domain in &ready_domains {
        let highest = *cpufreq::available_opps(domain.id)
            .unwrap()
            .last()
            .expect("domain has no available OPP");
        assert_domain(domain, highest);
    }
    if unready_domains == 0 {
        assert_controlled_thermal_limits(&ready_domains);
    } else {
        std::println!("CPU_CPUFREQ_THERMAL_SKIP_UNREADY domains={unready_domains}");
    }
    cpufreq::set_governor(Governor::Ondemand).unwrap();
    std::println!("CPU_CPUFREQ_OK");
    std::process::exit(0);
}
