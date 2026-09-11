// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::arch::aarch64::policy::{ArmVcpuError, ArmVcpuResult};

const ENABLE: u32 = 1 << 0;
const IMASK: u32 = 1 << 1;
const ISTATUS: u32 = 1 << 2;
const WRITABLE_CONTROL: u32 = ENABLE | IMASK;

/// One architectural generic-timer instance owned by a vCPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmTimerKind {
    /// The virtual timer backed by `CNTV_*`.
    Virtual,
    /// The physical timer exposed to the guest through trapped `CNTP_*` accesses.
    Physical,
}

/// Immutable generic-timer configuration shared by every vCPU in one VM.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmTimerVmConfig {
    frequency: u64,
    virtual_offset: u64,
    physical_offset: u64,
}

impl ArmTimerVmConfig {
    /// Creates a VM timer configuration.
    pub const fn new(
        frequency: u64,
        virtual_offset: u64,
        physical_offset: u64,
    ) -> ArmVcpuResult<Self> {
        if frequency == 0 {
            return Err(ArmVcpuError::InvalidInput);
        }
        Ok(Self {
            frequency,
            virtual_offset,
            physical_offset,
        })
    }

    /// Validates one nonzero counter frequency shared by all target CPUs.
    pub fn uniform_frequency(frequencies: &[u64]) -> ArmVcpuResult<u64> {
        let Some((&frequency, remaining)) = frequencies.split_first() else {
            return Err(ArmVcpuError::InvalidInput);
        };
        if frequency == 0 || remaining.iter().any(|candidate| *candidate != frequency) {
            return Err(ArmVcpuError::InvalidInput);
        }
        Ok(frequency)
    }

    /// Returns the counter frequency visible to the guest.
    pub const fn frequency(self) -> u64 {
        self.frequency
    }

    const fn offset(self, kind: ArmTimerKind) -> u64 {
        match kind {
            ArmTimerKind::Virtual => self.virtual_offset,
            ArmTimerKind::Physical => self.physical_offset,
        }
    }

    /// Converts a host physical counter value into one guest counter domain.
    pub const fn guest_counter(self, kind: ArmTimerKind, physical_counter: u64) -> u64 {
        physical_counter.wrapping_sub(self.offset(kind))
    }
}

/// Canonical writable state of one vCPU timer.
///
/// `ISTATUS` is deliberately absent. It is derived from the current counter
/// and `compare_value` whenever the timer is observed.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArmTimerContext {
    compare_value: u64,
    control: u32,
}

impl ArmTimerContext {
    /// Returns the absolute compare value in the timer's guest counter domain.
    pub const fn compare_value(self) -> u64 {
        self.compare_value
    }

    /// Reads `CTL`, deriving `ISTATUS` from the supplied guest counter.
    pub const fn read_control(self, guest_counter: u64) -> u32 {
        let status = if self.expired(guest_counter) {
            ISTATUS
        } else {
            0
        };
        self.control | status
    }

    /// Reads `TVAL` as the architectural low 32 bits of `CVAL - counter`.
    pub const fn read_tval(self, guest_counter: u64) -> u32 {
        self.compare_value.wrapping_sub(guest_counter) as u32
    }

    /// Updates the writable `CTL` bits.
    pub fn write_control(&mut self, control: u32) {
        self.control = control & WRITABLE_CONTROL;
    }

    /// Updates `CVAL`.
    pub fn write_compare(&mut self, compare_value: u64) {
        self.compare_value = compare_value;
    }

    /// Updates `TVAL`, sign-extending the architectural 32-bit value.
    pub fn write_tval(&mut self, guest_counter: u64, timer_value: u32) {
        self.compare_value = guest_counter.wrapping_add((timer_value as i32 as i64) as u64);
    }

    /// Returns whether the timer's level output is asserted.
    pub const fn irq_asserted(self, guest_counter: u64) -> bool {
        self.delivery_enabled() && self.expired(guest_counter)
    }

    /// Returns the host-counter deadline while the timer can wake a blocked vCPU.
    pub const fn host_deadline(
        self,
        kind: ArmTimerKind,
        config: ArmTimerVmConfig,
        guest_counter: u64,
    ) -> Option<u64> {
        if !self.delivery_enabled() || self.expired(guest_counter) {
            return None;
        }
        Some(self.compare_value.wrapping_add(config.offset(kind)))
    }

    const fn delivery_enabled(self) -> bool {
        self.control & WRITABLE_CONTROL == ENABLE
    }

    const fn expired(self, guest_counter: u64) -> bool {
        self.control & ENABLE != 0 && (guest_counter.wrapping_sub(self.compare_value) as i64) >= 0
    }
}

/// Immutable vCPU timer state sampled after a world switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmTimerSnapshot {
    config: ArmTimerVmConfig,
    virtual_timer: ArmTimerContext,
    physical_timer: ArmTimerContext,
}

impl ArmTimerSnapshot {
    /// Returns one timer context.
    pub const fn context(self, kind: ArmTimerKind) -> ArmTimerContext {
        match kind {
            ArmTimerKind::Virtual => self.virtual_timer,
            ArmTimerKind::Physical => self.physical_timer,
        }
    }

    /// Returns whether one timer currently asserts its PPI level.
    pub const fn irq_asserted(self, kind: ArmTimerKind, physical_counter: u64) -> bool {
        self.context(kind)
            .irq_asserted(self.config.guest_counter(kind, physical_counter))
    }

    /// Returns the earliest host-counter deadline that can wake this vCPU.
    pub const fn earliest_deadline(self, physical_counter: u64) -> Option<u64> {
        let virtual_counter = physical_counter.wrapping_sub(self.config.virtual_offset);
        let physical_guest_counter = physical_counter.wrapping_sub(self.config.physical_offset);
        let virtual_deadline =
            self.virtual_timer
                .host_deadline(ArmTimerKind::Virtual, self.config, virtual_counter);
        let physical_deadline = self.physical_timer.host_deadline(
            ArmTimerKind::Physical,
            self.config,
            physical_guest_counter,
        );
        match (virtual_deadline, physical_deadline) {
            (Some(virtual_deadline), Some(physical_deadline)) => {
                let virtual_distance = virtual_deadline.wrapping_sub(physical_counter);
                let physical_distance = physical_deadline.wrapping_sub(physical_counter);
                if virtual_distance <= physical_distance {
                    Some(virtual_deadline)
                } else {
                    Some(physical_deadline)
                }
            }
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        }
    }
}

/// Timer state owned by one vCPU.
#[repr(C)]
#[derive(Debug)]
pub struct ArmVcpuTimer {
    config: ArmTimerVmConfig,
    virtual_timer: ArmTimerContext,
    physical_timer: ArmTimerContext,
    guest_hypervisor_control: u64,
    guest_kernel_control: u64,
}

impl ArmVcpuTimer {
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn prepare_machine(
        &self,
    ) -> ArmVcpuResult<ax_cpu::virtualization::VirtualTimerState> {
        if !self.is_configured() {
            return Err(ArmVcpuError::BadState);
        }
        Ok(ax_cpu::virtualization::VirtualTimerState {
            offset: self.config.virtual_offset,
            compare: self.virtual_timer.compare_value,
            control: self.virtual_timer.control,
            hypervisor_control: self.guest_hypervisor_control,
            kernel_control: self.guest_kernel_control,
        })
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn finish_machine(&mut self, state: ax_cpu::virtualization::VirtualTimerState) {
        self.virtual_timer.compare_value = state.compare;
        self.virtual_timer.control = state.control;
        self.guest_kernel_control = state.kernel_control;
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn unconfigured() -> Self {
        Self {
            config: ArmTimerVmConfig {
                frequency: 0,
                virtual_offset: 0,
                physical_offset: 0,
            },
            virtual_timer: ArmTimerContext {
                compare_value: 0,
                control: 0,
            },
            physical_timer: ArmTimerContext {
                compare_value: 0,
                control: 0,
            },
            guest_hypervisor_control: 0,
            guest_kernel_control: 0,
        }
    }

    /// Creates reset timer state for one vCPU.
    pub const fn new(config: ArmTimerVmConfig, guest_hypervisor_control: u64) -> Self {
        Self {
            config,
            virtual_timer: ArmTimerContext {
                compare_value: 0,
                control: 0,
            },
            physical_timer: ArmTimerContext {
                compare_value: 0,
                control: 0,
            },
            guest_hypervisor_control,
            guest_kernel_control: 0,
        }
    }

    /// Returns an immutable snapshot while the timer is not loaded.
    pub fn snapshot(&self) -> ArmVcpuResult<ArmTimerSnapshot> {
        Ok(ArmTimerSnapshot {
            config: self.config,
            virtual_timer: self.virtual_timer,
            physical_timer: self.physical_timer,
        })
    }

    /// Returns a mutable timer context while the timer is not loaded.
    pub fn context_mut(&mut self, kind: ArmTimerKind) -> ArmVcpuResult<&mut ArmTimerContext> {
        Ok(match kind {
            ArmTimerKind::Virtual => &mut self.virtual_timer,
            ArmTimerKind::Physical => &mut self.physical_timer,
        })
    }

    /// Returns the VM-wide timer configuration.
    pub const fn config(&self) -> ArmTimerVmConfig {
        self.config
    }

    /// Reads one guest counter from the host physical counter.
    pub fn guest_counter(&self, kind: ArmTimerKind, physical_counter: u64) -> ArmVcpuResult<u64> {
        Ok(self.config.guest_counter(kind, physical_counter))
    }

    /// Reads one timer's control register.
    pub fn read_control(&self, kind: ArmTimerKind, physical_counter: u64) -> ArmVcpuResult<u32> {
        let guest_counter = self.guest_counter(kind, physical_counter)?;
        Ok(match kind {
            ArmTimerKind::Virtual => self.virtual_timer,
            ArmTimerKind::Physical => self.physical_timer,
        }
        .read_control(guest_counter))
    }

    /// Reads one timer's timer-value register.
    pub fn read_tval(&self, kind: ArmTimerKind, physical_counter: u64) -> ArmVcpuResult<u32> {
        let guest_counter = self.guest_counter(kind, physical_counter)?;
        Ok(match kind {
            ArmTimerKind::Virtual => self.virtual_timer,
            ArmTimerKind::Physical => self.physical_timer,
        }
        .read_tval(guest_counter))
    }

    /// Reads one timer's compare-value register.
    pub fn read_compare(&self, kind: ArmTimerKind) -> ArmVcpuResult<u64> {
        Ok(match kind {
            ArmTimerKind::Virtual => self.virtual_timer,
            ArmTimerKind::Physical => self.physical_timer,
        }
        .compare_value())
    }

    /// Writes one timer's control register.
    pub fn write_control(&mut self, kind: ArmTimerKind, value: u32) -> ArmVcpuResult {
        self.context_mut(kind)?.write_control(value);
        Ok(())
    }

    /// Writes one timer's timer-value register.
    pub fn write_tval(
        &mut self,
        kind: ArmTimerKind,
        physical_counter: u64,
        value: u32,
    ) -> ArmVcpuResult {
        let guest_counter = self.guest_counter(kind, physical_counter)?;
        self.context_mut(kind)?.write_tval(guest_counter, value);
        Ok(())
    }

    /// Writes one timer's compare-value register.
    pub fn write_compare(&mut self, kind: ArmTimerKind, value: u64) -> ArmVcpuResult {
        self.context_mut(kind)?.write_compare(value);
        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn is_configured(&self) -> bool {
        self.config.frequency != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_status_is_derived_and_wrap_safe() {
        let mut context = ArmTimerContext::default();
        context.write_compare(u64::MAX - 1);
        context.write_control(ENABLE);

        assert_eq!(context.read_control(u64::MAX - 2), ENABLE);
        assert_eq!(context.read_control(0), ENABLE | ISTATUS);
        assert!(context.irq_asserted(0));
    }

    #[test]
    fn tval_write_sign_extends_the_architectural_value() {
        let mut context = ArmTimerContext::default();
        context.write_tval(100, (-4_i32) as u32);

        assert_eq!(context.compare_value(), 96);
        assert_eq!(context.read_tval(100), (-4_i32) as u32);
    }

    #[test]
    fn masked_or_disabled_timers_never_assert_or_wake() {
        let config = ArmTimerVmConfig::new(24_000_000, 0x1000, 0).unwrap();
        let mut masked = ArmTimerContext::default();
        masked.write_compare(100);
        masked.write_control(ENABLE | IMASK);
        let mut disabled = ArmTimerContext::default();
        disabled.write_compare(100);
        disabled.write_control(0);

        for context in [masked, disabled] {
            assert!(!context.irq_asserted(101));
            assert_eq!(
                context.host_deadline(ArmTimerKind::Virtual, config, 99),
                None
            );
        }
    }

    #[test]
    fn snapshot_selects_the_nearest_deliverable_deadline_across_both_timers() {
        let config = ArmTimerVmConfig::new(24_000_000, 100, 200).unwrap();
        let mut virtual_timer = ArmTimerContext::default();
        virtual_timer.write_compare(950);
        virtual_timer.write_control(ENABLE);
        let mut physical_timer = ArmTimerContext::default();
        physical_timer.write_compare(830);
        physical_timer.write_control(ENABLE);
        let snapshot = ArmTimerSnapshot {
            config,
            virtual_timer,
            physical_timer,
        };

        assert_eq!(snapshot.earliest_deadline(1_000), Some(1_030));
    }

    #[test]
    fn vm_frequency_requires_identical_nonzero_target_cpu_counters() {
        assert_eq!(
            ArmTimerVmConfig::uniform_frequency(&[24_000_000, 24_000_000]),
            Ok(24_000_000)
        );
        assert_eq!(
            ArmTimerVmConfig::uniform_frequency(&[24_000_000, 25_000_000]),
            Err(ArmVcpuError::InvalidInput)
        );
        assert_eq!(
            ArmTimerVmConfig::uniform_frequency(&[0]),
            Err(ArmVcpuError::InvalidInput)
        );
        assert_eq!(
            ArmTimerVmConfig::uniform_frequency(&[]),
            Err(ArmVcpuError::InvalidInput)
        );
    }
}
