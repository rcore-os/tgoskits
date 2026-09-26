//! RK3588 CPU frequency domain adapter for the shared RDIF contract.
//!
//! The sibling `cpufreq` module owns all hardware state and transitions. This
//! adapter translates its domain model and maps FDT CPU hardware IDs to the
//! kernel's logical CPU indexes once the runtime has brought CPUs online.

use alloc::{vec, vec::Vec};

use rdif_cpufreq::{
    CpuFreq, DomainId, DomainInfo, FrequencyError, FrequencyLimits, Interface, OperatingPoint,
};
use rdrive::{DriverGeneric, PlatformDevice};
use rockchip_soc::rk3588::cpufreq::{self as soc_cpufreq, CpuDomainError};

use super::cpufreq::{self, FrequencyDomain};

const LITTLE_ID: DomainId = DomainId::new(0);
const BIG0_ID: DomainId = DomainId::new(1);
const BIG1_ID: DomainId = DomainId::new(2);

/// Publish the capability on the CPU node claimed by the RK3588 bootstrap
/// probe. Until that probe confirms boot-safe hardware state, calls return NotReady.
pub(super) fn register(platform_device: PlatformDevice) {
    platform_device.register(CpuFreq::new(Rk3588CpuFreq));
}

struct Rk3588CpuFreq;

impl DriverGeneric for Rk3588CpuFreq {
    fn name(&self) -> &str {
        "rk3588-cpufreq"
    }
}

impl Interface for Rk3588CpuFreq {
    fn domains(&self) -> Result<Vec<DomainInfo>, FrequencyError> {
        rdrive::with_fdt(|fdt| {
            let mut domains = vec![
                DomainInfo {
                    id: LITTLE_ID,
                    cpu_ids: Vec::new(),
                },
                DomainInfo {
                    id: BIG0_ID,
                    cpu_ids: Vec::new(),
                },
                DomainInfo {
                    id: BIG1_ID,
                    cpu_ids: Vec::new(),
                },
            ];
            let cpu_nodes = fdt.find_compatible(&["arm,cortex-a55", "arm,cortex-a76"]);
            let clock_phandle = cpu_nodes
                .first()
                .and_then(|node| node.clocks().into_iter().next())
                .map(|clock| clock.phandle)
                .ok_or(FrequencyError::NotReady)?;

            let nodes = cpu_nodes
                .into_iter()
                .map(|node| {
                    let hardware_id = node
                        .regs()
                        .into_iter()
                        .next()
                        .map(|reg| reg.address as usize)
                        .ok_or(FrequencyError::NotReady)?;
                    let clock_id = node
                        .clocks()
                        .into_iter()
                        .find(|clock| clock.phandle == clock_phandle)
                        .and_then(|clock| clock.specifier.first().copied())
                        .ok_or(FrequencyError::NotReady)?;
                    Ok((hardware_id, clock_id))
                })
                .collect::<Result<Vec<_>, FrequencyError>>()?;
            let mapped = soc_cpufreq::map_cpu_domains(nodes, axklib::cpu::resolve_logical_index)
                .map_err(|error| match error {
                    CpuDomainError::UnknownClock => FrequencyError::NotReady,
                    CpuDomainError::ConflictingCpu => FrequencyError::HardwareFailure,
                })?;
            for (domain, cpu_ids) in domains.iter_mut().zip(mapped) {
                domain.cpu_ids = cpu_ids;
            }
            domains.retain(|domain| !domain.cpu_ids.is_empty());
            if domains.is_empty() {
                return Err(FrequencyError::NotReady);
            }
            Ok(domains)
        })
        .ok_or(FrequencyError::NotReady)?
    }

    fn available_opps(&self, domain: DomainId) -> Result<Vec<OperatingPoint>, FrequencyError> {
        cpufreq::available_opps(to_domain(domain)?)
            .map(|opps| opps.into_iter().map(to_opp).collect())
            .map_err(to_error)
    }

    fn current_opp(&self, domain: DomainId) -> Result<OperatingPoint, FrequencyError> {
        cpufreq::current_opp(to_domain(domain)?)
            .map(to_opp)
            .map_err(to_error)
    }

    fn limits(&self, domain: DomainId) -> Result<FrequencyLimits, FrequencyError> {
        cpufreq::limits(to_domain(domain)?)
            .map(|limits| FrequencyLimits {
                min_hz: limits.min_hz,
                max_hz: limits.max_hz,
            })
            .map_err(to_error)
    }

    fn set_frequency(&mut self, domain: DomainId, frequency_hz: u64) -> Result<(), FrequencyError> {
        cpufreq::set_frequency(to_domain(domain)?, frequency_hz).map_err(to_error)
    }

    fn refresh_limits(&mut self) -> Result<(), FrequencyError> {
        cpufreq::refresh_limits().map_err(to_error)
    }
}

fn to_domain(id: DomainId) -> Result<FrequencyDomain, FrequencyError> {
    match id.raw() {
        0 => Ok(FrequencyDomain::Little),
        1 => Ok(FrequencyDomain::Big0),
        2 => Ok(FrequencyDomain::Big1),
        _ => Err(FrequencyError::InvalidDomain),
    }
}

fn to_opp(opp: cpufreq::OperatingPoint) -> OperatingPoint {
    OperatingPoint {
        frequency_hz: opp.frequency_hz,
        voltage_uv: Some(opp.voltage_uv),
    }
}

fn to_error(error: cpufreq::FrequencyError) -> FrequencyError {
    match error {
        cpufreq::FrequencyError::NotReady => FrequencyError::NotReady,
        cpufreq::FrequencyError::OppUnavailable => FrequencyError::OppUnavailable,
        cpufreq::FrequencyError::HardwareFailure => FrequencyError::HardwareFailure,
    }
}
