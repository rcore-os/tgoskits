#![allow(dead_code)]

use raw_cpuid::CpuId;
use x86::controlregs::{Xcr0, xcr0 as xcr0_read, xcr0_write};
use x86_64::registers::control::{Cr4, Cr4Flags};

use crate::msr::Msr;

/// Availability of extended processor state features.
#[derive(Debug, Clone, Copy)]
pub struct XAvailable {
    pub xsave: bool,
    pub xsaves: bool,
}

impl XAvailable {
    pub fn new() -> Self {
        let xsave = xsave_available();
        let xsaves = xsave && xsaves_available();
        Self { xsave, xsaves }
    }
}

/// XCR0 and IA32_XSS values that need switching between host and guest.
#[derive(Debug, Clone, Copy)]
pub struct XRegs {
    pub xcr0: u64,
    pub xss: u64,
}

impl XRegs {
    pub fn new(avail: XAvailable) -> Self {
        let xcr0 = if avail.xsave {
            unsafe { xcr0_read().bits() }
        } else {
            0
        };
        let xss = if avail.xsaves {
            Msr::IA32_XSS.read()
        } else {
            0
        };
        Self { xcr0, xss }
    }

    pub fn load(&self, avail: XAvailable) {
        unsafe {
            if avail.xsave {
                xcr0_write(Xcr0::from_bits_unchecked(self.xcr0));
                if avail.xsaves {
                    Msr::IA32_XSS.write(self.xss);
                }
            }
        }
    }

    pub fn save(&mut self, avail: XAvailable) {
        unsafe {
            if avail.xsave {
                self.xcr0 = xcr0_read().bits();
                if avail.xsaves {
                    self.xss = Msr::IA32_XSS.read();
                }
            }
        }
    }
}

/// Extended processor state switched around guest execution.
#[derive(Debug)]
pub struct XState {
    pub host: XRegs,
    pub guest: XRegs,
    pub avail: XAvailable,
}

impl XState {
    pub fn new() -> Self {
        let avail = XAvailable::new();
        let host = XRegs::new(avail);
        let guest = host;
        Self { host, guest, avail }
    }

    pub fn switch_to_guest(&mut self) {
        self.host.save(self.avail);
        self.guest.load(self.avail);
    }

    pub fn switch_to_host(&mut self) {
        self.guest.save(self.avail);
        self.host.load(self.avail);
    }
}

pub fn xsave_available() -> bool {
    CpuId::new()
        .get_feature_info()
        .map(|features| features.has_xsave())
        .unwrap_or(false)
}

pub fn xsaves_available() -> bool {
    CpuId::new()
        .get_extended_state_info()
        .map(|features| features.has_xsaves_xrstors())
        .unwrap_or(false)
}

pub fn enable_xsave() {
    if xsave_available() {
        unsafe {
            Cr4::write(Cr4::read() | Cr4Flags::OSXSAVE);
        }
    }
}
