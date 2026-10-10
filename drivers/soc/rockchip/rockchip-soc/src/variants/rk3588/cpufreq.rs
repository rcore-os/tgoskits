//! RK3588 CPU frequency constraints and OPP transition ordering.
//!
//! The platform adapter supplies verified regulator, GRF, and SCMI operations.
//! The SoC driver owns the order in which those operations may execute.

use alloc::vec::Vec;

/// An invalid SCMI CPU clock or conflicting kernel logical CPU mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuDomainError {
    UnknownClock,
    ConflictingCpu,
}

/// Resolve FDT CPU hardware IDs into the scheduler's logical CPU numbering.
/// Offline CPUs are omitted; the DT node order never determines domain members.
pub fn map_cpu_domains(
    nodes: impl IntoIterator<Item = (usize, u32)>,
    resolve: impl Fn(usize) -> Option<usize>,
) -> Result<[Vec<usize>; 3], CpuDomainError> {
    let mut domains = [Vec::new(), Vec::new(), Vec::new()];
    for (hardware_id, clock_id) in nodes {
        let index = match clock_id {
            0 => 0,
            2 => 1,
            3 => 2,
            _ => return Err(CpuDomainError::UnknownClock),
        };
        let Some(logical_cpu) = resolve(hardware_id) else {
            continue;
        };
        if domains
            .iter()
            .enumerate()
            .any(|(other, cpus)| other != index && cpus.contains(&logical_cpu))
        {
            return Err(CpuDomainError::ConflictingCpu);
        }
        if !domains[index].contains(&logical_cpu) {
            domains[index].push(logical_cpu);
        }
    }
    for cpus in &mut domains {
        cpus.sort_unstable();
    }
    Ok(domains)
}

/// The BSP lowers the DSU floor to the nearest 100 MHz below 80% of a big
/// cluster's requested frequency.
pub const fn dsu_minimum_hz(big_frequency_hz: u64) -> u64 {
    (big_frequency_hz.saturating_mul(4) / 5 / 100_000_000) * 100_000_000
}

/// Thermal state of one CPU frequency domain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThermalState {
    pub valid: bool,
    pub low: bool,
    pub high: bool,
}

impl ThermalState {
    pub const fn from_bits(bits: u8) -> Self {
        Self {
            valid: bits & 4 != 0,
            low: bits & 1 != 0,
            high: bits & 2 != 0,
        }
    }

    pub const fn bits(self) -> u8 {
        (self.low as u8) | ((self.high as u8) << 1) | ((self.valid as u8) << 2)
    }

    /// Apply the board's 10/15 C and 85/80 C hysteresis thresholds.
    /// An unavailable sensor invalidates high OPPs and retains the 750 mV
    /// low-temperature voltage floor.
    pub fn update(self, temperature_mc: Option<i32>) -> Self {
        let Some(temperature_mc) = temperature_mc else {
            return Self {
                valid: false,
                low: true,
                high: false,
            };
        };
        Self {
            valid: true,
            low: if self.low {
                temperature_mc <= 15_000
            } else {
                temperature_mc < 10_000
            },
            high: if self.high {
                temperature_mc >= 80_000
            } else {
                temperature_mc > 85_000
            },
        }
    }

    /// Voltage required by this OPP under the active low-temperature rule.
    pub const fn effective_voltage_uv(self, nominal_uv: u32) -> u32 {
        if !self.valid || self.low {
            if nominal_uv < 750_000 {
                750_000
            } else {
                nominal_uv
            }
        } else {
            nominal_uv
        }
    }

    /// Thermal ceiling, including the confirmed boot-rate fallback when the
    /// temperature input cannot be trusted.
    pub const fn maximum_hz(self, little: bool) -> u64 {
        if !self.valid {
            if little { 1_008_000_000 } else { 1_200_000_000 }
        } else if self.high {
            if little { 1_608_000_000 } else { 2_208_000_000 }
        } else {
            u64::MAX
        }
    }
}

/// A read-back-confirmed operation in one CPU OPP transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionStep {
    /// Establish the regulator voltage and GRF read margin for the target.
    SupplyAndMargin,
    /// Establish the target SCMI clock rate.
    Clock,
}

/// Execute an OPP transition without committing software state on partial
/// failure. An upward move secures supply before frequency; a downward move
/// lowers frequency before supply. The adapter must verify each step before
/// returning `Ok(())` from its callback.
pub fn transition<E>(
    upward: bool,
    mut apply: impl FnMut(TransitionStep) -> Result<(), E>,
) -> Result<(), E> {
    let order = if upward {
        [TransitionStep::SupplyAndMargin, TransitionStep::Clock]
    } else {
        [TransitionStep::Clock, TransitionStep::SupplyAndMargin]
    };
    for step in order {
        apply(step)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_hysteresis_and_missing_sensor() {
        let mut state = ThermalState::default();
        state = state.update(Some(9_000));
        assert!(state.low);
        assert_eq!(state.effective_voltage_uv(675_000), 750_000);
        assert!(state.update(Some(15_000)).low);
        state = state.update(Some(15_001));
        assert!(!state.low);
        state = state.update(Some(85_001));
        assert!(state.high);
        assert_eq!(state.maximum_hz(false), 2_208_000_000);
        assert!(state.update(Some(80_000)).high);
        state = state.update(Some(79_999));
        assert!(!state.high);
        state = state.update(None);
        assert_eq!(state.maximum_hz(true), 1_008_000_000);
        assert_eq!(state.effective_voltage_uv(675_000), 750_000);
    }

    #[test]
    fn transition_stops_at_each_failed_step() {
        for upward in [true, false] {
            let mut steps = alloc::vec::Vec::new();
            let first = if upward {
                TransitionStep::SupplyAndMargin
            } else {
                TransitionStep::Clock
            };
            let second = if upward {
                TransitionStep::Clock
            } else {
                TransitionStep::SupplyAndMargin
            };
            assert_eq!(
                transition(upward, |step| {
                    steps.push(step);
                    Err::<(), _>(())
                }),
                Err(())
            );
            assert_eq!(steps, [first]);
            steps.clear();
            assert_eq!(
                transition(upward, |step| {
                    steps.push(step);
                    if step == second { Err(()) } else { Ok(()) }
                }),
                Err(())
            );
            assert_eq!(steps, [first, second]);
        }
    }

    #[test]
    fn dsu_floor_matches_bsp_rounding() {
        assert_eq!(dsu_minimum_hz(2_352_000_000), 1_800_000_000);
        assert_eq!(dsu_minimum_hz(2_208_000_000), 1_700_000_000);
    }

    #[test]
    fn cpu_domains_follow_kernel_logical_ids_instead_of_fdt_order() {
        // A passthrough guest boots hardware CPU 0x400 as logical CPU 0 even
        // though its FDT lists the A55 CPU first.
        let nodes = [(0x000, 0), (0x400, 2), (0x600, 3)];
        let resolve = |hardware_id| match hardware_id {
            0x400 => Some(0),
            0x000 => Some(1),
            _ => None,
        };
        assert_eq!(
            map_cpu_domains(nodes, resolve),
            Ok([alloc::vec![1], alloc::vec![0], alloc::vec![]])
        );
        assert_eq!(
            map_cpu_domains([(0x000, 0), (0x400, 2)], |_| Some(0)),
            Err(CpuDomainError::ConflictingCpu)
        );
    }
}
