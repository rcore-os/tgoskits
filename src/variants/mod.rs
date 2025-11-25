use alloc::collections::btree_map::BTreeMap;

use crate::RkBoard;

#[macro_use]
mod _macros;

mod rk3588;

pub type DomainMap = BTreeMap<PD, RockchipDomainInfo>;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PD(pub usize);

impl From<u32> for PD {
    fn from(value: u32) -> Self {
        PD(value as usize)
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct RockchipPmuInfo {
    pub pwr_offset: u32,
    pub status_offset: u32,
    pub req_offset: u32,
    pub idle_offset: u32,
    pub ack_offset: u32,
    pub mem_pwr_offset: u32,
    pub chain_status_offset: u32,
    pub mem_status_offset: u32,
    pub repair_status_offset: u32,
    pub clk_ungate_offset: u32,
    pub mem_sd_offset: u32,
    pub core_pwrcnt_offset: u32,
    pub gpu_pwrcnt_offset: u32,
    pub core_power_transition_time: u32,
    pub gpu_power_transition_time: u32,

    pub domains: DomainMap,
}

impl RockchipPmuInfo {
    pub fn new(board: RkBoard) -> Self {
        match board {
            RkBoard::Rk3588 => rk3588::pmu_info(),
            RkBoard::Rk3568 => unimplemented!(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct RockchipDomainInfo {
    pub name: &'static str,
    pub pwr_mask: i32,
    pub status_mask: i32,
    pub req_mask: i32,
    pub idle_mask: i32,
    pub ack_mask: i32,
    pub active_wakeup: bool,
    pub pwr_w_mask: i32,
    pub req_w_mask: i32,
    pub mem_status_mask: i32,
    pub repair_status_mask: i32,
    pub clk_ungate_mask: i32,
    pub clk_ungate_w_mask: i32,
    pub mem_num: i32,
    pub keepon_startup: bool,
    pub always_on: bool,
    pub pwr_offset: u32,
    pub mem_offset: u32,
    pub req_offset: u32,
}
