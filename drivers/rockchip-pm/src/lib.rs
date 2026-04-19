#![no_std]
//! # RK3588 Power Management Driver
//!
//! This crate provides power management functionality for RK3588 series SoCs,
//! particularly for NPU power domain control.
//!
//! # Features
//!
//! - Dynamic power domain on/off control
//! - Support for multiple SoC variants (RK3588, RK3568)
//! - Device tree compatible string based auto-detection
//! - Safe register access and status checking
//!
//! # Example
//!
//! ```no_run
//! use core::ptr::NonNull;
//!
//! use rockchip_pm::{PowerDomain, RkBoard, RockchipPM};
//!
//! // Create driver instance with base address and board type
//! let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
//! let mut pm = RockchipPM::new(base, RkBoard::Rk3588);
//!
//! // Turn on NPU power domain
//! pm.power_domain_on(PowerDomain::NPU).unwrap();
//!
//! // Turn off NPU power domain
//! pm.power_domain_off(PowerDomain::NPU).unwrap();
//! ```

extern crate alloc;

use core::ptr::NonNull;

use mbarrier::mb;
use rdif_base::DriverGeneric;

use crate::{registers::PmuRegs, variants::RockchipPmuInfo};

mod registers;
mod variants;

pub use variants::PowerDomain;

/// Supported Rockchip SoC board types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RkBoard {
    /// RK3588 SoC
    Rk3588,
    /// RK3568 SoC
    Rk3568,
}

/// Power management operation errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmError {
    /// The specified power domain does not exist
    DomainNotFound,
    /// Timeout waiting for power domain status
    Timeout,
    /// Hardware error
    HardwareError,
}

/// Result type for power management operations
pub type PmResult<T> = Result<T, PmError>;

/// Rockchip Power Management Unit driver
///
/// This structure provides control over power domains for Rockchip SoCs,
/// allowing dynamic power gating of various IP blocks like GPU, NPU, VCODEC, etc.
pub struct RockchipPM {
    _board: RkBoard,
    reg: PmuRegs,
    info: RockchipPmuInfo,
}

impl RockchipPM {
    /// Create a new RockchipPM driver instance
    ///
    /// # Arguments
    ///
    /// * `base` - Base address of the PMU registers
    /// * `board` - The specific board type (RK3588 or RK3568)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use core::ptr::NonNull;
    ///
    /// use rockchip_pm::{RkBoard, RockchipPM};
    ///
    /// let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
    /// let pm = RockchipPM::new(base, RkBoard::Rk3588);
    /// ```
    pub fn new(base: NonNull<u8>, board: RkBoard) -> Self {
        Self {
            _board: board,
            info: RockchipPmuInfo::new(board),
            reg: PmuRegs::new(base),
        }
    }

    /// Create a new RockchipPM driver instance using device tree compatible string
    ///
    /// # Arguments
    ///
    /// * `base` - Base address of the PMU registers
    /// * `compatible` - Device tree compatible string (e.g., "rockchip,rk3588-power-controller")
    ///
    /// # Panics
    ///
    /// Panics if the compatible string is not supported
    ///
    /// # Example
    ///
    /// ```no_run
    /// use core::ptr::NonNull;
    ///
    /// use rockchip_pm::RockchipPM;
    ///
    /// let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
    /// let pm = RockchipPM::new_with_compatible(base, "rockchip,rk3588-power-controller");
    /// ```
    pub fn new_with_compatible(base: NonNull<u8>, compatible: &str) -> Self {
        let board = match compatible {
            "rockchip,rk3568-power-controller" => RkBoard::Rk3568,
            "rockchip,rk3588-power-controller" => RkBoard::Rk3588,
            _ => panic!("Unsupported compatible string: {compatible}"),
        };

        Self {
            _board: board,
            info: RockchipPmuInfo::new(board),
            reg: PmuRegs::new(base),
        }
    }

    /// Find a power domain by its name
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the power domain (e.g., "npu", "gpu", "vcodec")
    ///
    /// # Returns
    ///
    /// `Some(PowerDomain)` if found, `None` otherwise
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rockchip_pm::{RockchipPM, RkBoard, PowerDomain};
    /// # use core::ptr::NonNull;
    /// # let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
    /// # let pm = RockchipPM::new(base, RkBoard::Rk3588);
    /// let domain = pm.get_power_dowain_by_name("npu");
    /// assert_eq!(domain, Some(PowerDomain::NPU));
    /// ```
    pub fn get_power_dowain_by_name(&self, name: &str) -> Option<PowerDomain> {
        for (domain, info) in &self.info.domains {
            if info.name == name {
                return Some(*domain);
            }
        }
        None
    }

    /// Turn on the specified power domain
    ///
    /// This function enables power to the specified domain, initializing the
    /// associated hardware blocks.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to turn on
    ///
    /// # Errors
    ///
    /// Returns `PmError::DomainNotFound` if the domain does not exist
    /// Returns `PmError::Timeout` if the power domain fails to turn on within the timeout period
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rockchip_pm::{RockchipPM, RkBoard, PowerDomain};
    /// # use core::ptr::NonNull;
    /// # let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
    /// # let mut pm = RockchipPM::new(base, RkBoard::Rk3588);
    /// pm.power_domain_on(PowerDomain::NPU).unwrap();
    /// ```
    pub fn power_domain_on(&mut self, domain: PowerDomain) -> PmResult<()> {
        self.set_power_domain(domain, true)
    }

    /// Turn off the specified power domain
    ///
    /// This function cuts power to the specified domain, putting the associated
    /// hardware blocks into a low-power state.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to turn off
    ///
    /// # Errors
    ///
    /// Returns `PmError::DomainNotFound` if the domain does not exist
    /// Returns `PmError::Timeout` if the power domain fails to turn off within the timeout period
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rockchip_pm::{RockchipPM, RkBoard, PowerDomain};
    /// # use core::ptr::NonNull;
    /// # let base = unsafe { NonNull::new_unchecked(0xfd5d8000 as *mut u8) };
    /// # let mut pm = RockchipPM::new(base, RkBoard::Rk3588);
    /// pm.power_domain_off(PowerDomain::NPU).unwrap();
    /// ```
    pub fn power_domain_off(&mut self, domain: PowerDomain) -> PmResult<()> {
        self.set_power_domain(domain, false)
    }

    /// Set power domain state
    ///
    /// Internal function that handles the actual power control sequence.
    /// This includes writing to power control registers and waiting for
    /// the power domain to reach the desired state.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to control
    /// * `power_on` - `true` to turn on, `false` to turn off
    fn set_power_domain(&mut self, domain: PowerDomain, power_on: bool) -> PmResult<()> {
        let domain_info = self
            .info
            .domains
            .get(&domain)
            .ok_or(PmError::DomainNotFound)?;

        if domain_info.pwr_mask == 0 {
            return Ok(());
        }

        // Write power control register
        self.write_power_control(&domain, power_on)?;

        // Wait for power domain status to stabilize
        self.wait_power_domain_stable(&domain, power_on)?;

        Ok(())
    }

    /// Write to power control register
    ///
    /// Internal function that handles writing to the PMU power control registers.
    /// Supports both write-enable mask mode and read-modify-write mode.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to control
    /// * `power_on` - `true` to turn on, `false` to turn off
    fn write_power_control(&mut self, domain: &PowerDomain, power_on: bool) -> PmResult<()> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(PmError::DomainNotFound)?;
        let pwr_offset = self.info.pwr_offset + domain_info.pwr_offset;

        if domain_info.pwr_w_mask != 0 {
            // Use write-enable mask mode
            let value = if power_on {
                domain_info.pwr_w_mask
            } else {
                domain_info.pwr_mask | domain_info.pwr_w_mask
            };
            self.reg.write_u32(pwr_offset as usize, value as u32);
        } else {
            // Use read-modify-write mode
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

    /// Wait for power domain status to stabilize
    ///
    /// Polls the power domain status register until the expected state is reached
    /// or a timeout occurs.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to monitor
    /// * `expected_on` - The expected power state (`true` for on, `false` for off)
    ///
    /// # Errors
    ///
    /// Returns `PmError::Timeout` if the domain does not reach the expected state
    /// within the timeout period
    fn wait_power_domain_stable(&self, domain: &PowerDomain, expected_on: bool) -> PmResult<()> {
        for _ in 0..10000 {
            if self.is_domain_on(domain)? == expected_on {
                return Ok(());
            }
        }
        Err(PmError::Timeout)
    }

    /// Check if a power domain is powered on
    ///
    /// Reads the appropriate status register to determine if a power domain
    /// is currently powered on. Supports multiple status register types.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to check
    ///
    /// # Returns
    ///
    /// `true` if the domain is powered on, `false` otherwise
    fn is_domain_on(&self, domain: &PowerDomain) -> PmResult<bool> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(PmError::DomainNotFound)?;

        if domain_info.repair_status_mask != 0 {
            // Use repair status register
            let val = self.reg.read_u32(self.info.repair_status_offset as usize);
            // 1'b1: power on, 1'b0: power off
            return Ok((val & (domain_info.repair_status_mask as u32)) != 0);
        }

        if domain_info.status_mask == 0 {
            // Domain only has idle status
            return Ok(!self.is_domain_idle(domain)?);
        }

        let val = self.reg.read_u32(self.info.status_offset as usize);
        // 1'b0: power on, 1'b1: power off
        Ok((val & (domain_info.status_mask as u32)) == 0)
    }

    /// Check if a power domain is idle
    ///
    /// Reads the idle status register to determine if a power domain
    /// is in idle state.
    ///
    /// # Arguments
    ///
    /// * `domain` - The power domain to check
    ///
    /// # Returns
    ///
    /// `true` if the domain is idle, `false` otherwise
    fn is_domain_idle(&self, domain: &PowerDomain) -> PmResult<bool> {
        let domain_info = self
            .info
            .domains
            .get(domain)
            .ok_or(PmError::DomainNotFound)?;

        let val = self.reg.read_u32(self.info.idle_offset as usize);
        Ok((val & (domain_info.idle_mask as u32)) == (domain_info.idle_mask as u32))
    }
}

impl DriverGeneric for RockchipPM {
    fn name(&self) -> &str {
        "Rockchip-PM"
    }
}
