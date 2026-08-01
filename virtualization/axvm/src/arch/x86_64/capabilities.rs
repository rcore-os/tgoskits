//! x86_64 implementations of AxVM platform capability hooks.

use super::X86_64Arch;
use crate::architecture::{GuestBootPlatform, HostTimePlatform, PhysicalSpiPlatform};

impl HostTimePlatform for X86_64Arch {}

impl PhysicalSpiPlatform for X86_64Arch {}

impl GuestBootPlatform for X86_64Arch {}
