// Copyright 2025 The Axvisor Team
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

extern crate alloc;

use alloc::{boxed::Box, format, sync::Arc};
use core::time::Duration;

use aarch64_sysreg::SystemRegType;
use ax_kspin::SpinNoPreempt;
use axdevice_base::{
    AccessWidth, BaseDeviceOps, DeviceAddrRange, DeviceError, DeviceResult, EmuDeviceType,
    SysRegAddr, SysRegAddrRange,
};
use axvm_types::MAX_VCPU_NUM;
use log::info;

use crate::host;

const PHYSICAL_TIMER_VIRTUAL_IRQ: usize = 30;

/// Generation-tagged registration state for one vCPU.
///
/// The enclosing spin lock serializes guest reprogramming, timer delivery, and
/// device teardown. Host cancellation and interrupt queuing always happen
/// after releasing that lock.
#[derive(Default)]
struct TimerTokenState {
    generation: usize,
    token: Option<usize>,
}

impl TimerTokenState {
    fn begin_rearm(&mut self) -> (usize, Option<usize>) {
        self.generation = next_generation(self.generation);
        (self.generation, self.token.take())
    }

    fn finish_registration(&mut self, generation: usize, token: usize) -> Result<(), usize> {
        if self.generation != generation || self.token.is_some() {
            return Err(token);
        }
        self.token = Some(token);
        Ok(())
    }

    fn take_for_delivery(&mut self, generation: usize) -> bool {
        if self.generation != generation {
            return false;
        }
        self.generation = next_generation(self.generation);
        self.token = None;
        true
    }

    fn abandon_registration(&mut self, generation: usize) {
        if self.generation == generation {
            self.generation = next_generation(self.generation);
            debug_assert!(self.token.is_none());
        }
    }
}

struct TimerTokenSlots {
    slots: [SpinNoPreempt<TimerTokenState>; MAX_VCPU_NUM],
}

impl TimerTokenSlots {
    fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| SpinNoPreempt::new(TimerTokenState::default())),
        }
    }

    fn begin_rearm(&self, vcpu_id: usize) -> Option<(usize, Option<usize>)> {
        self.slots
            .get(vcpu_id)
            .map(|slot| slot.lock().begin_rearm())
    }

    fn finish_registration(&self, vcpu_id: usize, generation: usize, token: usize) -> bool {
        self.slots
            .get(vcpu_id)
            .is_some_and(|slot| slot.lock().finish_registration(generation, token).is_ok())
    }

    fn abandon_registration(&self, vcpu_id: usize, generation: usize) {
        if let Some(slot) = self.slots.get(vcpu_id) {
            slot.lock().abandon_registration(generation);
        }
    }

    fn take_for_delivery(&self, vcpu_id: usize, generation: usize) -> bool {
        self.slots
            .get(vcpu_id)
            .is_some_and(|slot| slot.lock().take_for_delivery(generation))
    }

    fn cancel_all(&self) {
        for slot in &self.slots {
            let (_, token) = slot.lock().begin_rearm();
            if let Some(token) = token {
                host::cancel_timer(token);
            }
        }
    }
}

const fn next_generation(current: usize) -> usize {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

impl BaseDeviceOps<SysRegAddrRange> for SysCntpTvalEl0 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> SysRegAddrRange {
        SysRegAddrRange {
            start: SysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize),
            end: SysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize),
        }
    }

    fn handle_read(
        &self,
        _addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
    ) -> DeviceResult<usize> {
        todo!()
    }

    fn handle_write(
        &self,
        addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        info!("Write to emulator register: {addr:?}, value: {val}");
        let vm_id = host::current_vm_id();
        let vcpu_id = host::current_vcpu_id();
        let Some((generation, old_token)) = self.timer_tokens.begin_rearm(vcpu_id) else {
            return Err(DeviceError::InvalidInput {
                operation: "write CNTP_TVAL_EL0",
                detail: format!(
                    "vCPU ID {vcpu_id} exceeds the fixed timer slot count {MAX_VCPU_NUM}"
                ),
            });
        };
        if let Some(old_token) = old_token {
            host::cancel_timer(old_token);
        }

        let now = host::current_time_nanos();
        let deadline = now.saturating_add(val as u64);
        info!("Current time: {now}, deadline: {deadline}");
        let timer_tokens = Arc::clone(&self.timer_tokens);
        match host::register_timer(
            Duration::from_nanos(deadline),
            Box::new(move |_| {
                if timer_tokens.take_for_delivery(vcpu_id, generation) {
                    host::queue_virtual_interrupt(vm_id, vcpu_id, PHYSICAL_TIMER_VIRTUAL_IRQ);
                }
            }),
        ) {
            Some(token)
                if self
                    .timer_tokens
                    .finish_registration(vcpu_id, generation, token) =>
            {
                Ok(())
            }
            Some(stale_token) => {
                host::cancel_timer(stale_token);
                Ok(())
            }
            None => {
                self.timer_tokens.abandon_registration(vcpu_id, generation);
                Err(DeviceError::ResourceBusy {
                    operation: "arm CNTP_TVAL_EL0",
                    resource: "AxVM virtual timer registration slots".into(),
                })
            }
        }
    }
}

/// System register emulation for CNTP_TVAL_EL0.
///
/// Provides virtualization support for the physical timer value register.
pub struct SysCntpTvalEl0 {
    // The array is allocated once with the device and never grows. Cloning the
    // Arc for a callback keeps this fixed storage alive without allocating a
    // token node for each guest reprogramming operation.
    timer_tokens: Arc<TimerTokenSlots>,
}

impl Default for SysCntpTvalEl0 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SysCntpTvalEl0 {
    fn drop(&mut self) {
        self.timer_tokens.cancel_all();
    }
}

impl SysCntpTvalEl0 {
    /// Creates a new CNTP_TVAL_EL0 register emulator.
    pub fn new() -> Self {
        Self {
            timer_tokens: Arc::new(TimerTokenSlots::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rearm_returns_previous_token_and_rejects_its_generation() {
        let mut state = TimerTokenState::default();
        let (first_generation, old_token) = state.begin_rearm();
        assert_eq!(old_token, None);
        assert_eq!(state.finish_registration(first_generation, 17), Ok(()));

        let (second_generation, old_token) = state.begin_rearm();
        assert_eq!(old_token, Some(17));
        assert_ne!(second_generation, first_generation);
        assert!(!state.take_for_delivery(first_generation));
    }

    #[test]
    fn immediate_delivery_invalidates_late_token_publication() {
        let mut state = TimerTokenState::default();
        let (generation, _) = state.begin_rearm();

        assert!(state.take_for_delivery(generation));
        assert_eq!(state.finish_registration(generation, 23), Err(23));
    }
}
