//! SoC-specific power domain variants and configuration
//!
//! This module contains power domain definitions for different Rockchip SoCs,
//! including register offsets, bit masks, and domain metadata.

use alloc::collections::btree_map::BTreeMap;

use crate::RkBoard;

#[macro_use]
mod _macros;

mod rk3588;

/// Map of power domains to their configuration info
pub type DomainMap = BTreeMap<PowerDomain, RockchipDomainInfo>;

/// Power domain identifier
///
/// Represents a specific power domain in the SoC that can be
/// independently powered on or off.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PowerDomain(pub usize);

impl From<u32> for PowerDomain {
    fn from(value: u32) -> Self {
        PowerDomain(value as usize)
    }
}

/// PMU configuration for a specific SoC
///
/// Contains register offsets and domain information for power management.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct RockchipPmuInfo {
    /// Power control register offset
    pub pwr_offset: u32,
    /// Status register offset
    pub status_offset: u32,
    /// Request register offset
    pub req_offset: u32,
    /// Idle status register offset
    pub idle_offset: u32,
    /// Acknowledge register offset
    pub ack_offset: u32,
    /// Memory power control register offset
    pub mem_pwr_offset: u32,
    /// Chain status register offset
    pub chain_status_offset: u32,
    /// Memory status register offset
    pub mem_status_offset: u32,
    /// Repair status register offset
    pub repair_status_offset: u32,
    /// Clock ungate register offset
    pub clk_ungate_offset: u32,
    /// Memory shutdown register offset
    pub mem_sd_offset: u32,
    /// Core power count register offset
    pub core_pwrcnt_offset: u32,
    /// GPU power count register offset
    pub gpu_pwrcnt_offset: u32,
    /// Core power transition time
    pub core_power_transition_time: u32,
    /// GPU power transition time
    pub gpu_power_transition_time: u32,

    /// Map of all supported power domains
    pub domains: DomainMap,
}

impl RockchipPmuInfo {
    /// Create PMU info for the specified board type
    ///
    /// # Arguments
    ///
    /// * `board` - The Rockchip board type
    ///
    /// # Panics
    ///
    /// Panics if the board type is not implemented
    pub fn new(board: RkBoard) -> Self {
        match board {
            RkBoard::Rk3588 => rk3588::pmu_info(),
            RkBoard::Rk3568 => unimplemented!(),
        }
    }
}

/// Configuration information for a single power domain
///
/// Contains all the register masks and offsets needed to control
/// a specific power domain.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct RockchipDomainInfo {
    /// Domain name
    pub name: &'static str,
    /// Power control mask
    pub pwr_mask: i32,
    /// Status mask
    pub status_mask: i32,
    /// Request mask
    pub req_mask: i32,
    /// Idle status mask
    pub idle_mask: i32,
    /// Acknowledge mask
    pub ack_mask: i32,
    /// Active wakeup flag
    pub active_wakeup: bool,
    /// Power write-enable mask
    pub pwr_w_mask: i32,
    /// Request write-enable mask
    pub req_w_mask: i32,
    /// Memory status mask
    pub mem_status_mask: i32,
    /// Repair status mask
    pub repair_status_mask: i32,
    /// Clock ungate mask
    pub clk_ungate_mask: i32,
    /// Clock ungate write-enable mask
    pub clk_ungate_w_mask: i32,
    /// Memory block count
    pub mem_num: i32,
    /// Keep on at startup flag
    pub keepon_startup: bool,
    /// Always on flag
    pub always_on: bool,
    /// Power control register offset
    pub pwr_offset: u32,
    /// Memory power register offset
    pub mem_offset: u32,
    /// Request register offset
    pub req_offset: u32,
}
