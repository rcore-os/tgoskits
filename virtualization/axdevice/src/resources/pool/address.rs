//! MMIO and port-I/O pool configuration.

use alloc::string::String;
use core::ops::Range;

use super::{range::*, *};
use crate::DeviceManagerResult;

impl ResourcePools {
    /// Adds an MMIO range used only for automatic allocation.
    pub fn add_auto_mmio(&mut self, range: Range<u64>) -> DeviceManagerResult {
        insert_range(&mut self.addresses.automatic.mmio, range, "automatic MMIO")
    }

    /// Allows fixed MMIO requests inside `range`.
    pub fn allow_fixed_mmio(&mut self, range: Range<u64>) -> DeviceManagerResult {
        insert_range(&mut self.addresses.fixed.mmio, range, "fixed MMIO")
    }

    /// Reserves an MMIO range before device allocation.
    pub fn reserve_mmio(
        &mut self,
        owner: impl Into<String>,
        range: Range<u64>,
    ) -> DeviceManagerResult {
        reserve_range(
            &mut self.addresses.reserved.mmio,
            nonempty_owner(owner.into())?,
            range,
            "MMIO",
        )
    }

    /// Adds a port-I/O range used only for automatic allocation.
    pub fn add_auto_pio(&mut self, range: Range<u16>) -> DeviceManagerResult {
        insert_range(&mut self.addresses.automatic.pio, range, "automatic PIO")
    }

    /// Allows fixed port-I/O requests inside `range`.
    pub fn allow_fixed_pio(&mut self, range: Range<u16>) -> DeviceManagerResult {
        insert_range(&mut self.addresses.fixed.pio, range, "fixed PIO")
    }

    /// Reserves a port-I/O range before device allocation.
    pub fn reserve_pio(
        &mut self,
        owner: impl Into<String>,
        range: Range<u16>,
    ) -> DeviceManagerResult {
        reserve_range(
            &mut self.addresses.reserved.pio,
            nonempty_owner(owner.into())?,
            range,
            "PIO",
        )
    }

    pub(crate) fn auto_mmio(&self) -> &[Range<u64>] {
        &self.addresses.automatic.mmio
    }

    pub(crate) fn fixed_mmio(&self) -> &[Range<u64>] {
        &self.addresses.fixed.mmio
    }

    pub(crate) fn reserved_mmio(&self) -> &[RangeOwner<u64>] {
        &self.addresses.reserved.mmio
    }

    pub(crate) fn auto_pio(&self) -> &[Range<u16>] {
        &self.addresses.automatic.pio
    }

    pub(crate) fn fixed_pio(&self) -> &[Range<u16>] {
        &self.addresses.fixed.pio
    }

    pub(crate) fn reserved_pio(&self) -> &[RangeOwner<u16>] {
        &self.addresses.reserved.pio
    }
}
