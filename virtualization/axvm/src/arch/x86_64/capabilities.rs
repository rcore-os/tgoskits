//! x86_64 implementations of AxVM platform capability hooks.

use super::X86_64Arch;
use crate::architecture::GuestBootPlatform;

impl GuestBootPlatform for X86_64Arch {}
