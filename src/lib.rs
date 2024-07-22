//! System Real Time Clock (RTC) Drivers for aarch64 based on PL031.

#![cfg_attr(not(test), no_std)]

use core::ptr::{addr_of, addr_of_mut};

#[repr(C, align(4))]
struct Registers {
    /// Data register
    dr: u32,
    /// Match register
    mr: u32,
    /// Load register
    lr: u32,
    /// Control register
    cr: u8,
    _reserved0: [u8; 3],
    /// Interrupt Mask Set or Clear register
    imsc: u8,
    _reserved1: [u8; 3],
    /// Raw Interrupt Status
    ris: u8,
    _reserved2: [u8; 3],
    /// Masked Interrupt Status
    mis: u8,
    _reserved3: [u8; 3],
    /// Interrupt Clear Register
    icr: u8,
    _reserved4: [u8; 3],
}

/// The System Real Time Clock structure for aarch64 based on PL031.
pub struct Rtc {
    registers: *mut Registers,
}

impl Rtc {
    /// Constructs a new instance of the RTC driver for a PL031 device at the given base address.
    ///
    /// The base address may be obtained from the device tree.
    ///
    /// # Safety
    ///
    /// The given base address must point to the MMIO control registers of a PL031 device, which
    /// must be mapped into the address space of the process as device memory and not have any other
    /// aliases. It must be aligned to a 4 byte boundary.
    pub unsafe fn new(base_address: *mut u32) -> Self {
        Rtc {
            registers: base_address as _,
        }
    }

    /// Returns the current time in seconds since UNIX epoch.
    pub fn get_unix_timestamp(&self) -> u32 {
        // SAFETY: We know that self.registers points to the control registers
        // of a PL031 device which is appropriately mapped.
        unsafe { addr_of!((*self.registers).dr).read_volatile() }
    }

    /// Sets the current time in seconds since UNIX epoch.
    pub fn set_unix_timestamp(&mut self, unix_time: u32) {
        // SAFETY: We know that self.registers points to the control registers
        // of a PL031 device which is appropriately mapped.
        unsafe { addr_of_mut!((*self.registers).lr).write_volatile(unix_time) }
    }
}

// SAFETY: `Rtc` just contains a pointer to device memory, which can be accessed from any context.
unsafe impl Send for Rtc {}

// SAFETY: An `&Rtc` only allows reading device registers, which can safety be done from multiple
// places at once.
unsafe impl Sync for Rtc {}
