//! Host physical interrupt pools used by architecture passthrough planning.

use alloc::string::String;
use core::ops::Range;

use axdevice_base::HostIrqId;

use super::{range::*, *};
use crate::DeviceManagerResult;

impl ResourcePools {
    /// Adds host IRQs eligible for explicit architecture allocation.
    pub fn add_auto_host_irqs(&mut self, range: Range<HostIrqId>) -> DeviceManagerResult {
        insert_range(
            &mut self.host_irqs.automatic,
            range.start.value()..range.end.value(),
            "automatic host IRQ",
        )
    }

    /// Allows fixed host IRQ identity requests inside `range`.
    pub fn allow_fixed_host_irqs(&mut self, range: Range<HostIrqId>) -> DeviceManagerResult {
        insert_range(
            &mut self.host_irqs.fixed,
            range.start.value()..range.end.value(),
            "fixed host IRQ",
        )
    }

    /// Reserves one host IRQ for architecture-internal use.
    pub fn reserve_host_irq(
        &mut self,
        owner: impl Into<String>,
        irq: HostIrqId,
    ) -> DeviceManagerResult {
        let end =
            irq.value()
                .checked_add(1)
                .ok_or_else(|| crate::DeviceManagerError::InvalidConfig {
                    operation: "reserve host IRQ",
                    detail: "host IRQ number overflows".into(),
                })?;
        reserve_range(
            &mut self.host_irqs.reserved,
            nonempty_owner(owner.into())?,
            irq.value()..end,
            "host IRQ",
        )
    }

    pub(crate) fn auto_host_irqs(&self) -> &[Range<usize>] {
        &self.host_irqs.automatic
    }

    pub(crate) fn fixed_host_irqs(&self) -> &[Range<usize>] {
        &self.host_irqs.fixed
    }

    pub(crate) fn reserved_host_irqs(&self) -> &[RangeOwner<usize>] {
        &self.host_irqs.reserved
    }
}
