//! VM-owned LVZ exit interpretation, guest firmware and device state.

pub(super) use ax_cpu::virtualization::GuestContext as LoongArchContextFrame;
mod exception;
mod guest_addr;
mod guest_csr;
mod host;
mod host_cpu;
mod iocsr;
mod mmio;
mod registers;
mod trap;
mod types;
mod vcpu;

pub(super) use host::LoongArchHostOps;
pub(super) use iocsr::LoongArchIocsrState;
pub(super) use types::*;
pub(super) use vcpu::{LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig, LoongArchVcpu};
