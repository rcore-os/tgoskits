#![no_std]
//! # RK3588 电源管理驱动
//!
//! 本库提供了针对 RK3588 系列 SoC 的电源管理功能，特别是 NPU 电源域的控制。
//!

extern crate alloc;

use mbarrier::mb;
use rdif_base::DriverGeneric;

use crate::{registers::PmuRegs, variants::RockchipPmuInfo};
use core::ptr::NonNull;

mod registers;
mod variants;

pub use variants::PD;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RkBoard {
    Rk3588,
    Rk3568,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpuError {
    /// 电源域不存在
    DomainNotFound,
    /// 超时错误
    Timeout,
    /// 硬件错误
    HardwareError,
}

pub type NpuResult<T> = Result<T, NpuError>;

pub struct RockchipPM {
    _board: RkBoard,
    reg: PmuRegs,
    info: RockchipPmuInfo,
}

impl RockchipPM {
    pub fn new(base: NonNull<u8>, board: RkBoard) -> Self {
        Self {
            _board: board,
            info: RockchipPmuInfo::new(board),
            reg: PmuRegs::new(base),
        }
    }

    /// 开启指定电源域
    pub fn power_domain_on(&mut self, domain: PD) -> NpuResult<()> {
        self.set_power_domain(domain, true)
    }

    /// 关闭指定电源域
    pub fn power_domain_off(&mut self, domain: PD) -> NpuResult<()> {
        self.set_power_domain(domain, false)
    }

    /// 设置电源域状态（简化版本）
    fn set_power_domain(&mut self, domain: PD, power_on: bool) -> NpuResult<()> {
        let domain_info = self
            .info
            .domains
            .get(&domain)
            .ok_or(NpuError::DomainNotFound)?;

        if domain_info.pwr_mask == 0 {
            return Ok(());
        }

        // 写入电源控制寄存器
        self.write_power_control(&domain, power_on)?;

        // 等待电源域状态稳定
        self.wait_power_domain_stable(&domain, power_on)?;

        Ok(())
    }

    /// 写入电源控制寄存器
    fn write_power_control(&mut self, domain: &PD, power_on: bool) -> NpuResult<()> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(NpuError::DomainNotFound)?;
        let pwr_offset = self.info.pwr_offset + domain_info.pwr_offset;

        if domain_info.pwr_w_mask != 0 {
            // 使用写使能掩码方式
            let value = if power_on {
                domain_info.pwr_w_mask
            } else {
                domain_info.pwr_mask | domain_info.pwr_w_mask
            };
            self.reg.write_u32(pwr_offset as usize, value as u32);
        } else {
            // 使用读改写方式
            let current = self.reg.read_u32(pwr_offset as usize);
            let new_value = if power_on {
                current & !(domain_info.pwr_mask as u32)
            } else {
                current | (domain_info.pwr_mask as u32)
            };
            self.reg.write_u32(pwr_offset as usize, new_value);
        }

        mb();

        Ok(())
    }

    /// 等待电源域状态稳定
    fn wait_power_domain_stable(&self, domain: &PD, expected_on: bool) -> NpuResult<()> {
        for _ in 0..10000 {
            if self.is_domain_on(domain)? == expected_on {
                return Ok(());
            }
        }
        Err(NpuError::Timeout)
    }

    /// 检查电源域是否开启
    fn is_domain_on(&self, domain: &PD) -> NpuResult<bool> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(NpuError::DomainNotFound)?;

        if domain_info.repair_status_mask != 0 {
            // 使用修复状态寄存器
            let val = self.reg.read_u32(self.info.repair_status_offset as usize);
            // 1'b1: power on, 1'b0: power off
            return Ok((val & (domain_info.repair_status_mask as u32)) != 0);
        }

        if domain_info.status_mask == 0 {
            // 仅检查空闲状态的域
            return Ok(!self.is_domain_idle(domain)?);
        }

        let val = self.reg.read_u32(self.info.status_offset as usize);
        // 1'b0: power on, 1'b1: power off
        Ok((val & (domain_info.status_mask as u32)) == 0)
    }

    /// 检查电源域是否空闲
    fn is_domain_idle(&self, domain: &PD) -> NpuResult<bool> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(NpuError::DomainNotFound)?;

        let val = self.reg.read_u32(self.info.idle_offset as usize);
        Ok((val & (domain_info.idle_mask as u32)) == (domain_info.idle_mask as u32))
    }
}

impl DriverGeneric for RockchipPM {
    fn open(&mut self) -> Result<(), rdif_base::KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), rdif_base::KError> {
        Ok(())
    }
}
