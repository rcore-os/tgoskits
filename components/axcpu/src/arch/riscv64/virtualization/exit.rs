// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub use riscv::{
    CoreInterruptNumber, ExceptionNumber, InterruptNumber,
    interrupt::{Interrupt, Trap},
    result::{Error as RegisterError, Result as RegisterResult},
};

impl super::Exit {
    /// Decodes the saved cause without reading the current hart's trap CSR.
    /// Unknown interrupt or exception numbers remain register errors.
    pub fn cause(&self) -> RegisterResult<Trap<Interrupt, Exception>> {
        riscv::register::scause::Scause::from_bits(self.scause)
            .cause()
            .try_into()
    }
}

/// RISC-V supervisor and hypervisor exception numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Exception {
    /// Instruction Misaligned exception (0).
    InstructionMisaligned = 0,
    /// Instruction Fault exception (1).
    InstructionFault     = 1,
    /// Illegal Instruction exception (2).
    IllegalInstruction   = 2,
    /// Breakpoint exception (3).
    Breakpoint           = 3,
    /// Load Misaligned exception (4).
    LoadMisaligned       = 4,
    /// Load Fault exception (5).
    LoadFault            = 5,
    /// Store Misaligned exception (6).
    StoreMisaligned      = 6,
    /// Store Fault exception (7).
    StoreFault           = 7,
    /// User Env Call exception (8).
    UserEnvCall          = 8,
    /// Supervisor Env Call exception (9).
    SupervisorEnvCall    = 9,
    /// Virtual Supervisor Env Call exception (10).
    VirtualSupervisorEnvCall = 10,
    /// Instruction Page Fault exception (12).
    InstructionPageFault = 12,
    /// Load Page Fault exception (13).
    LoadPageFault        = 13,
    /// Store Page Fault exception (15).
    StorePageFault       = 15,
    /// Instruction Guest Page Fault exception (20).
    InstructionGuestPageFault = 20,
    /// Load Guest Page Fault exception (21).
    LoadGuestPageFault   = 21,
    /// Virtual Instruction exception (22).
    VirtualInstruction   = 22,
    /// Store Guest Page Fault exception (23).
    StoreGuestPageFault  = 23,
}

// SAFETY: `Exception` represents the standard RISC-V exceptions
unsafe impl ExceptionNumber for Exception {
    const MAX_EXCEPTION_NUMBER: usize = Self::StoreGuestPageFault as usize;

    #[inline]
    fn number(self) -> usize {
        self as usize
    }

    #[inline]
    fn from_number(value: usize) -> RegisterResult<Self> {
        match value {
            0 => Ok(Self::InstructionMisaligned),
            1 => Ok(Self::InstructionFault),
            2 => Ok(Self::IllegalInstruction),
            3 => Ok(Self::Breakpoint),
            4 => Ok(Self::LoadMisaligned),
            5 => Ok(Self::LoadFault),
            6 => Ok(Self::StoreMisaligned),
            7 => Ok(Self::StoreFault),
            8 => Ok(Self::UserEnvCall),
            9 => Ok(Self::SupervisorEnvCall),
            10 => Ok(Self::VirtualSupervisorEnvCall),
            12 => Ok(Self::InstructionPageFault),
            13 => Ok(Self::LoadPageFault),
            15 => Ok(Self::StorePageFault),
            20 => Ok(Self::InstructionGuestPageFault),
            21 => Ok(Self::LoadGuestPageFault),
            22 => Ok(Self::VirtualInstruction),
            23 => Ok(Self::StoreGuestPageFault),
            _ => Err(RegisterError::InvalidVariant(value)),
        }
    }
}
