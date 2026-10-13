//! AxVisor adapter for run-scoped guest CPU-frequency hints.
//!
//! The adapter only publishes normalized requests into the shared ArceOS
//! cpufreq worker. Physical OPP selection, thermal limits and read-back remain
//! owned by `ax-runtime::cpufreq`.

use alloc::vec::Vec;

use ax_runtime::cpufreq::{self, FrequencyError};
use axvm::{GuestCpuFreqBackend, VMId};

pub(crate) struct GuestCpuFreqService;

impl GuestCpuFreqService {
    pub(crate) const fn new() -> Self {
        Self
    }
}

fn error_detail(error: FrequencyError) -> &'static str {
    match error {
        FrequencyError::NotSupported => "host CPU frequency control is unsupported",
        FrequencyError::NotReady => "host CPU frequency worker is not ready",
        FrequencyError::InvalidDomain => "guest CPU frequency domain is invalid",
        FrequencyError::OppUnavailable => "no safe host OPP is available",
        FrequencyError::HardwareFailure => "host CPU frequency transition failed",
    }
}

impl GuestCpuFreqBackend for GuestCpuFreqService {
    fn bind(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::bind_guest_run(vm_id, run_generation);
    }

    fn performance_table(&self, domain: u32) -> Result<Vec<u32>, &'static str> {
        let domain = usize::try_from(domain).map_err(|_| "guest CPU frequency domain overflows")?;
        let infos = cpufreq::domains().map_err(error_detail)?;
        let info = infos
            .get(domain)
            .ok_or("guest CPU frequency domain is not present on this host")?;
        if usize::try_from(info.id.raw()).ok() != Some(domain) {
            return Err(
                "host CPU frequency domains are not contiguous; refusing an ambiguous guest domain",
            );
        }
        let snapshot = cpufreq::snapshot(info.id).map_err(error_detail)?;
        if snapshot.available_opps.is_empty() {
            return Err("host CPU frequency domain has no currently safe OPP");
        }
        let mut opps = snapshot.all_opps;
        opps.sort_unstable_by_key(|opp| opp.frequency_hz);
        opps.dedup_by_key(|opp| opp.frequency_hz);
        if opps.is_empty() {
            return Err("host CPU frequency domain has no hardware OPP");
        }
        if opps.len() > 64 {
            return Err("host CPU frequency domain exposes more than 64 safe OPPs");
        }
        if opps.len() == 1 {
            cpufreq::seed_guest_domain_performance(domain, 1024).map_err(error_detail)?;
            return Ok(vec![1024]);
        }
        let current_index = opps
            .iter()
            .rposition(|opp| opp.frequency_hz <= snapshot.current_opp.frequency_hz)
            .unwrap_or(0);
        let current = 1 + ((current_index * 1023) / (opps.len() - 1)) as u32;
        cpufreq::seed_guest_domain_performance(domain, current).map_err(error_detail)?;
        Ok(opps
            .iter()
            .enumerate()
            .map(|(index, _)| {
                if index == 0 {
                    1
                } else {
                    1 + ((index * 1023) / (opps.len() - 1)) as u32
                }
            })
            .collect())
    }

    fn validate_placement(&self, domain: u32, host_cpus: &[usize]) -> Result<(), &'static str> {
        let domain = usize::try_from(domain).map_err(|_| "guest CPU frequency domain overflows")?;
        let infos = cpufreq::domains().map_err(error_detail)?;
        let info = infos
            .get(domain)
            .ok_or("guest CPU frequency domain is not present on this host")?;
        if usize::try_from(info.id.raw()).ok() != Some(domain) {
            return Err(
                "host CPU frequency domains are not contiguous; refusing an ambiguous guest domain",
            );
        }
        if host_cpus.is_empty() || host_cpus.iter().any(|cpu| !info.cpu_ids.contains(cpu)) {
            return Err("guest vCPU placement is outside the selected host CPU frequency domain");
        }
        Ok(())
    }

    fn set_hint(
        &self,
        vm_id: VMId,
        run_generation: u64,
        vcpu_id: usize,
        domain: u32,
        performance: u32,
    ) -> Result<(), &'static str> {
        let domain = usize::try_from(domain).map_err(|_| "guest CPU frequency domain overflows")?;
        if cpufreq::domain_count().is_none_or(|count| domain >= count) {
            return Err("guest CPU frequency domain is not present on this host");
        }
        cpufreq::publish_guest_hint(vm_id, run_generation, vcpu_id, domain, performance)
            .map_err(error_detail)
    }

    fn current_performance(
        &self,
        vm_id: VMId,
        run_generation: u64,
        vcpu_id: usize,
        domain: u32,
    ) -> Result<u32, &'static str> {
        let domain = usize::try_from(domain).map_err(|_| "guest CPU frequency domain overflows")?;
        if cpufreq::domain_count().is_none_or(|count| domain >= count) {
            return Err("guest CPU frequency domain is not present on this host");
        }
        cpufreq::guest_hint_performance(vm_id, run_generation, vcpu_id, domain)
            .and_then(|performance| performance.ok_or(FrequencyError::NotReady))
            .map_err(error_detail)
    }

    fn clear(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::clear_guest_hints(vm_id, run_generation);
    }

    fn suspend(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::suspend_guest_hints(vm_id, run_generation);
    }

    fn resume(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::resume_guest_hints(vm_id, run_generation);
    }
}
