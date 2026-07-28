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
use crate::{VgicError, VgicResult, consts::*, interrupt::VgicInt};

pub struct Vgicd {
    pub ctrlr: u32,
    pub typer: u32,
    pub iidr: u32,
    interrupt: [VgicInt; SPI_ID_MAX],
}

impl Vgicd {
    pub fn new() -> Self {
        let mut gic_int = [VgicInt::new(0, 0); SPI_ID_MAX];
        for (idx, item) in gic_int.iter_mut().enumerate() {
            *item = VgicInt::new(idx as u32, 0);
        }
        let typer = crate::api_reexp::read_vgicd_typer();
        let iidr = crate::api_reexp::read_vgicd_iidr();
        Self {
            ctrlr: 0,
            typer,
            iidr,
            interrupt: gic_int,
        }
    }

    pub fn vgicd_ctrlr_write(&mut self, ctrlr: usize) {
        self.ctrlr = ctrlr as u32;
    }

    pub fn vgicd_isenabler_read(&self, idx: u32) -> usize {
        let mut isenabler = 0;
        for i in 0..32 {
            if self
                .interrupt
                .get((idx * 32 + i) as usize)
                .is_some_and(VgicInt::get_enable)
            {
                isenabler |= 1 << i;
            }
        }
        isenabler
    }

    pub fn vgicd_isenabler_write(&mut self, idx: u32, isenabler: usize) {
        for i in 0..32 {
            if isenabler & (1 << i) != 0
                && let Some(interrupt) = self.interrupt.get_mut((idx * 32 + i) as usize)
            {
                interrupt.set_enable(true);
            }
        }
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.interrupt[irq as usize].inject_irq();
    // }

    pub fn fetch_irq(&self, idx: u32) -> VgicResult<VgicInt> {
        self.interrupt
            .get(idx as usize)
            .copied()
            .ok_or(VgicError::InvalidIrq {
                irq: idx as usize,
                max: SPI_ID_MAX,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmodelled_isenabler_registers_are_raz_wi() {
        let mut vgicd = Vgicd::new();

        assert_eq!(vgicd.vgicd_isenabler_read(16), 0);
        vgicd.vgicd_isenabler_write(16, usize::MAX);
        assert_eq!(vgicd.vgicd_isenabler_read(31), 0);
        vgicd.vgicd_isenabler_write(31, usize::MAX);
    }

    #[test]
    fn fetching_an_unmodelled_irq_returns_a_typed_error() {
        assert!(matches!(
            Vgicd::new().fetch_irq(512),
            Err(VgicError::InvalidIrq { irq: 512, max: 512 })
        ));
    }
}
