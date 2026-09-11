//! Host callbacks required by the OS-neutral x86 vCPU implementation.

use x86_vlapic::X86VlapicHostOps;

use crate::arch::x86_64::policy::{X86GuestPhysAddr, X86VcpuResult};

/// Guest memory, time, and interrupt policy required by the x86 VMM.
pub trait X86HostOps: X86VlapicHostOps {
    /// Read one byte from guest physical memory.
    fn read_guest_u8(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8>;

    /// Convert nanoseconds to host ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;

    /// Services an interrupt that remains pending after an SVM exit.
    ///
    /// SVM does not acknowledge the interrupt during VM exit. The caller keeps
    /// the vCPU pinned and enters with local IRQs disabled; the implementation
    /// may briefly enable IRQs, but must restore the disabled state before it
    /// returns.
    fn service_pending_host_interrupt();

    /// Dispatches and completes a host interrupt acknowledged by VMX.
    ///
    /// VMX has already transferred the vector into VM-exit state, so the host
    /// must synchronously run the matching IRQ action and controller EOI before
    /// this call returns. The caller keeps local IRQs disabled and the vCPU
    /// pinned for the complete operation.
    fn dispatch_acknowledged_host_interrupt(vector: u8);
}

pub(crate) fn read_guest_u8<H: X86HostOps>(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8> {
    H::read_guest_u8(paddr)
}

pub(crate) fn nanos_to_ticks<H: X86HostOps>(nanos: u64) -> u64 {
    H::nanos_to_ticks(nanos)
}
