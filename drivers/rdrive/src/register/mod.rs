use alloc::vec::Vec;
use core::ops::Deref;

pub use fdt::NodeType as Node;

use crate::probe::{acpi, fdt, pci, static_};
pub use crate::probe::{
    acpi::{AcpiInfo, ProbeAcpi},
    fdt::{FdtInfo, ProbeFdt},
    pci::{PciInfo, ProbePci},
};

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ProbePriority(pub usize);

impl ProbePriority {
    pub const CLK: ProbePriority = ProbePriority(6);
    pub const INTC: ProbePriority = ProbePriority(10);
    pub const TIMER: ProbePriority = ProbePriority(20);
    pub const MSI: ProbePriority = ProbePriority(30);
    pub const EARLY_DEVICE: ProbePriority = ProbePriority(128);
    pub const DEFAULT: ProbePriority = ProbePriority(256);
    pub const LAST: ProbePriority = ProbePriority(usize::MAX);
}

impl From<usize> for ProbePriority {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeLevel {
    PreKernel,
    PostKernel,
}

impl ProbeLevel {
    pub const fn new() -> Self {
        Self::PostKernel
    }
}

impl Default for ProbeLevel {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct DriverRegister {
    pub name: &'static str,
    pub level: ProbeLevel,
    pub priority: ProbePriority,
    pub probe_kinds: &'static [ProbeKind],
}

unsafe impl Send for DriverRegister {}
unsafe impl Sync for DriverRegister {}

pub enum ProbeKind {
    Static {
        on_probe: static_::FnOnProbe,
    },
    Fdt {
        compatibles: &'static [&'static str],
        on_probe: fdt::FnOnProbe,
    },
    Acpi {
        ids: &'static [acpi::AcpiId],
        on_probe: acpi::FnOnProbe,
    },
    Pci {
        on_probe: pci::FnOnProbe,
    },
}

#[repr(C)]
pub struct DriverRegisterSlice {
    data: *const u8,
    len: usize,
}

impl DriverRegisterSlice {
    pub fn from_raw(data: &'static [u8]) -> Self {
        Self {
            data: data.as_ptr(),
            len: data.len(),
        }
    }

    pub fn as_slice(&self) -> &[DriverRegister] {
        if self.len == 0 {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(self.data as _, self.len / size_of::<DriverRegister>())
        }
    }
    pub fn empty() -> Self {
        Self {
            data: core::ptr::null(),
            len: 0,
        }
    }
}

impl Deref for DriverRegisterSlice {
    type Target = [DriverRegister];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[derive(Default)]
pub struct RegisterContainer {
    registers: Vec<DriverRegister>,
}

impl RegisterContainer {
    pub const fn new() -> Self {
        Self {
            registers: Vec::new(),
        }
    }

    pub fn add(&mut self, register: DriverRegister) {
        self.registers.push(register);
    }

    pub fn append(&mut self, register: &[DriverRegister]) {
        for one in register {
            self.add(one.clone());
        }
    }

    pub fn unregistered(&self) -> Vec<DriverRegister> {
        self.registers.to_vec()
    }
}
