#![allow(unused)]

use crate::consts::{PPI_ID_MAX, SGI_ID_MAX, SPI_ID_MAX};
use log::debug;

/// Interrupt trigger mode.
#[derive(Debug, Clone, Copy)]
pub enum TriggerMode {
    Edge = 0,
    Level = 1,
}

#[allow(clippy::upper_case_acronyms)]
/// Different types of interrupt that the GIC handles.
#[derive(Debug, Clone, Copy)]
pub enum InterruptType {
    SGI,
    PPI,
    SPI,
}

#[derive(Debug, Clone, Copy)]
pub enum InterruptStatus {
    Inactive,
    Pending,
    Active,
    ActivePending,
}

#[derive(Copy, Clone)]
pub struct Interrupt {
    interrupt_id: u32,
    vcpu_id: u32,
    priority: u32,
    status: InterruptStatus,
    enable: bool,
    trigger_mode: TriggerMode,
    interrupt_type: InterruptType,
}

impl Interrupt {
    fn new(interrupt_id: u32, vcpu_id: u32) -> Self {
        Interrupt {
            interrupt_id,
            vcpu_id,
            priority: 0,
            status: InterruptStatus::Inactive,
            enable: false,
            trigger_mode: TriggerMode::Edge,
            interrupt_type: InterruptType::SGI,
        }
    }
}

#[derive(Copy, Clone)]
pub struct VgicInt {
    inner: Interrupt,
}

impl VgicInt {
    pub(crate) fn new(interrupt_id: u32, vcpu_id: u32) -> Self {
        let interrupt_type = if interrupt_id < SGI_ID_MAX as u32 {
            InterruptType::SGI
        } else if interrupt_id < PPI_ID_MAX as u32 {
            InterruptType::PPI
        } else if interrupt_id < SPI_ID_MAX as u32 {
            InterruptType::SPI
        } else {
            panic!("Invalid interrupt id");
        };
        Self {
            inner: Interrupt::new(interrupt_id, vcpu_id),
        }
    }

    pub(crate) fn set_enable(&mut self, enable: bool) {
        self.inner.enable = enable;
        debug!(
            "Setting interrupt {} enable to {}",
            self.inner.interrupt_id, enable
        );
        // if !gicd.get_enable()
        // gicd.set_enable(self.interrupt_id, enable);
    }

    pub(crate) fn get_enable(&self) -> bool {
        self.inner.enable
    }

    pub(crate) fn set_priority(&mut self, priority: u32) {
        self.inner.priority = priority;
        // gicd.set_priority(self.interrupt_id, priority);
    }

    pub(crate) fn get_priority(&self) -> u32 {
        self.inner.priority
    }

    pub(crate) fn set_vcpu_id(&mut self, vcpu_id: u32) {
        self.inner.vcpu_id = vcpu_id;
    }

    pub(crate) fn get_vcpu_id(&self) -> u32 {
        self.inner.vcpu_id
    }

    pub(crate) fn set_status(&mut self, status: InterruptStatus) {
        self.inner.status = status;
    }

    pub(crate) fn get_status(&self) -> InterruptStatus {
        self.inner.status
    }

    pub(crate) fn set_trigger_mode(&mut self, trigger_mode: TriggerMode) {
        self.inner.trigger_mode = trigger_mode;
    }

    pub(crate) fn get_trigger_mode(&self) -> &TriggerMode {
        &self.inner.trigger_mode
    }

    pub(crate) fn get_interrupt_type(&self) -> &InterruptType {
        &self.inner.interrupt_type
    }
}
