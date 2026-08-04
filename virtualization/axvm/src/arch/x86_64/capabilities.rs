//! x86_64 implementations of AxVM platform capability hooks.

use super::X86_64Arch;
use crate::architecture::{GuestBootPlatform, HostTimePlatform, MachinePlatform};

impl HostTimePlatform for X86_64Arch {}

impl GuestBootPlatform for X86_64Arch {}

impl MachinePlatform for X86_64Arch {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture =
        crate::machine::MachineArchitecture::X86_64;
}
