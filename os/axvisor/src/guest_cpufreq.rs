//! AxVisor adapter for run-scoped guest CPU-frequency hints.
//!
//! The adapter only publishes normalized requests into the shared ArceOS
//! cpufreq worker. Physical OPP selection, thermal limits and read-back remain
//! owned by `ax-runtime::cpufreq`.

use alloc::vec::Vec;

use ax_runtime::cpufreq::{self, FrequencyError};
use axvm::{GuestCpuFreqBackend, GuestCpuFreqError, VMId};

const _: () =
    assert!(axvm::VIRTUAL_CPUFREQ_MAX_PERFORMANCE == ax_runtime::cpufreq::GUEST_MAX_PERFORMANCE);

pub(crate) struct GuestCpuFreqService;

impl GuestCpuFreqService {
    pub(crate) const fn new() -> Self {
        Self
    }
}

fn map_frequency_error(error: FrequencyError) -> GuestCpuFreqError {
    match error {
        FrequencyError::NotSupported => {
            GuestCpuFreqError::Unavailable("host CPU frequency control is unsupported")
        }
        FrequencyError::NotReady => {
            GuestCpuFreqError::Unavailable("host CPU frequency worker is not ready")
        }
        FrequencyError::InvalidDomain => {
            GuestCpuFreqError::InvalidRequest("guest CPU frequency domain is invalid")
        }
        FrequencyError::OppUnavailable => {
            GuestCpuFreqError::Unavailable("no safe host OPP is available")
        }
        FrequencyError::HardwareFailure => {
            GuestCpuFreqError::HardwareFailure("host CPU frequency transition failed")
        }
    }
}

fn domain_overflow() -> GuestCpuFreqError {
    GuestCpuFreqError::InvalidRequest("guest CPU frequency domain overflows")
}

fn missing_domain() -> GuestCpuFreqError {
    GuestCpuFreqError::InvalidRequest("guest CPU frequency domain is not present on this host")
}

fn ambiguous_domain() -> GuestCpuFreqError {
    GuestCpuFreqError::InvalidRequest(
        "host CPU frequency domains are not contiguous; refusing an ambiguous guest domain",
    )
}

fn resolve_domain(domain: u32) -> Result<(usize, cpufreq::DomainInfo), GuestCpuFreqError> {
    let domain = usize::try_from(domain).map_err(|_| domain_overflow())?;
    let infos = cpufreq::domains().map_err(map_frequency_error)?;
    let info = infos.get(domain).cloned().ok_or_else(missing_domain)?;
    if usize::try_from(info.id.raw()).ok() != Some(domain) {
        return Err(ambiguous_domain());
    }
    Ok((domain, info))
}

impl GuestCpuFreqBackend for GuestCpuFreqService {
    fn bind(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::bind_guest_run(vm_id, run_generation);
    }

    fn performance_table(&self, domain: u32) -> Result<Vec<u32>, GuestCpuFreqError> {
        let (domain, info) = resolve_domain(domain)?;
        let snapshot = cpufreq::snapshot(info.id).map_err(map_frequency_error)?;
        if snapshot.available_opps.is_empty() {
            return Err(GuestCpuFreqError::Unavailable(
                "host CPU frequency domain has no currently safe OPP",
            ));
        }
        let mut opps = snapshot.all_opps;
        opps.sort_unstable_by_key(|opp| opp.frequency_hz);
        opps.dedup_by_key(|opp| opp.frequency_hz);
        if opps.is_empty() {
            return Err(GuestCpuFreqError::Unavailable(
                "host CPU frequency domain has no hardware OPP",
            ));
        }
        if opps.len() > 64 {
            return Err(GuestCpuFreqError::InvalidRequest(
                "host CPU frequency domain exposes more than 64 safe OPPs",
            ));
        }
        let table = cpufreq::guest_performance_table(&opps);
        let current_index = opps
            .iter()
            .rposition(|opp| opp.frequency_hz <= snapshot.current_opp.frequency_hz)
            .unwrap_or(0);
        cpufreq::seed_guest_domain_performance(domain, table[current_index])
            .map_err(map_frequency_error)?;
        Ok(table)
    }

    fn validate_placement(
        &self,
        domain: u32,
        host_cpus: &[usize],
    ) -> Result<(), GuestCpuFreqError> {
        let (_, info) = resolve_domain(domain)?;
        if host_cpus.is_empty() || host_cpus.iter().any(|cpu| !info.cpu_ids.contains(cpu)) {
            return Err(GuestCpuFreqError::InvalidRequest(
                "guest vCPU placement is outside the selected host CPU frequency domain",
            ));
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
    ) -> Result<(), GuestCpuFreqError> {
        let domain = usize::try_from(domain).map_err(|_| domain_overflow())?;
        if cpufreq::domain_count().is_none_or(|count| domain >= count) {
            return Err(missing_domain());
        }
        cpufreq::publish_guest_hint(vm_id, run_generation, vcpu_id, domain, performance)
            .map_err(map_frequency_error)
    }

    fn current_performance(
        &self,
        vm_id: VMId,
        run_generation: u64,
        vcpu_id: usize,
        domain: u32,
    ) -> Result<u32, GuestCpuFreqError> {
        let domain = usize::try_from(domain).map_err(|_| domain_overflow())?;
        if cpufreq::domain_count().is_none_or(|count| domain >= count) {
            return Err(missing_domain());
        }
        cpufreq::guest_hint_performance(vm_id, run_generation, vcpu_id, domain)
            .and_then(|performance| performance.ok_or(FrequencyError::NotReady))
            .map_err(map_frequency_error)
    }

    fn retire(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::retire_guest_run(vm_id, run_generation);
    }

    fn unbind(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::unbind_guest_run(vm_id, run_generation);
    }

    fn suspend(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::suspend_guest_hints(vm_id, run_generation);
    }

    fn resume(&self, vm_id: VMId, run_generation: u64) {
        cpufreq::resume_guest_hints(vm_id, run_generation);
    }
}
