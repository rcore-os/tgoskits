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

use axaddrspace::{
    GuestPhysAddr, MappingFlags,
    device::{AccessWidth, Port, SysRegAddr},
};

#[allow(unused_imports)] // used in doc
use super::AxArchVCpu;

/// Reasons for VM-Exits returned by [AxArchVCpu::run].
///
/// When a guest virtual CPU executes, various conditions can cause control to be
/// transferred back to the hypervisor. This enum represents all possible exit reasons
/// that can occur during VCpu execution.
///
/// # VM Exit Categories
///
/// - **I/O Operations**: MMIO reads/writes, port I/O, system register access
/// - **System Events**: Hypercalls, interrupts, nested page faults
/// - **Power Management**: CPU power state changes, system shutdown
/// - **Multiprocessing**: IPI sending, secondary CPU bring-up
/// - **Error Conditions**: Entry failures, invalid states
///
/// # Compatibility Note
///
/// This enum draws inspiration from [kvm-ioctls](https://github.com/rust-vmm/kvm-ioctls/blob/main/src/ioctls/vcpu.rs)
/// for consistency with existing virtualization frameworks.
#[non_exhaustive]
#[derive(Debug)]
pub enum AxVCpuExitReason {
    /// A guest instruction triggered a hypercall to the hypervisor.
    ///
    /// Hypercalls are a mechanism for the guest OS to request services from
    /// the hypervisor, similar to system calls in a traditional OS.
    Hypercall {
        /// The hypercall number identifying the requested service
        nr: u64,
        /// Arguments passed to the hypercall (up to 6 parameters)
        args: [u64; 6],
    },

    /// The guest performed a Memory-Mapped I/O (MMIO) read operation.
    ///
    /// MMIO reads occur when the guest accesses device registers or other
    /// hardware-mapped memory regions that require hypervisor emulation.
    MmioRead {
        /// Guest physical address being read from
        addr: GuestPhysAddr,
        /// Width/size of the memory access (8, 16, 32, or 64 bits)
        width: AccessWidth,
        /// Index of the guest register that will receive the read value
        reg: usize,
        /// Width of the destination register  
        reg_width: AccessWidth,
        /// Whether to sign-extend the read value to fill the register
        signed_ext: bool,
    },

    /// The guest performed a Memory-Mapped I/O (MMIO) write operation.
    ///
    /// MMIO writes occur when the guest writes to device registers or other
    /// hardware-mapped memory regions that require hypervisor emulation.
    MmioWrite {
        /// Guest physical address being written to
        addr: GuestPhysAddr,
        /// Width/size of the memory access (8, 16, 32, or 64 bits)
        width: AccessWidth,
        /// Data being written to the memory location
        data: u64,
    },

    /// The guest performed a system register read operation.
    ///
    /// System registers are architecture-specific control and status registers:
    /// - **x86_64**: Model-Specific Registers (MSRs)
    /// - **RISC-V**: Control and Status Registers (CSRs)
    /// - **AArch64**: System registers accessible via MRS instruction
    SysRegRead {
        /// Address/identifier of the system register being read
        ///
        /// - **x86_64/RISC-V**: Direct register address
        /// - **AArch64**: ESR_EL2.ISS format (`<op0><op2><op1><CRn>00000<CRm>0`)
        ///   compatible with the `aarch64_sysreg` crate numbering scheme
        addr: SysRegAddr,
        /// Index of the guest register that will receive the read value
        ///
        /// **Note**: Unused on x86_64 where the result is always stored in `[edx:eax]`
        reg: usize,
    },

    /// The guest performed a system register write operation.
    ///
    /// System registers are architecture-specific control and status registers:
    /// - **x86_64**: Model-Specific Registers (MSRs)
    /// - **RISC-V**: Control and Status Registers (CSRs)
    /// - **AArch64**: System registers accessible via MSR instruction  
    SysRegWrite {
        /// Address/identifier of the system register being written
        ///
        /// - **x86_64/RISC-V**: Direct register address
        /// - **AArch64**: ESR_EL2.ISS format (`<op0><op2><op1><CRn>00000<CRm>0`)
        ///   compatible with the `aarch64_sysreg` crate numbering scheme
        addr: SysRegAddr,
        /// Data being written to the system register
        value: u64,
    },

    /// The guest performed a port-based I/O read operation.
    ///
    /// **Architecture**: x86-specific (other architectures don't have port I/O)
    ///
    /// The destination register is implicitly `al`/`ax`/`eax` based on the access width.
    IoRead {
        /// I/O port number being read from
        port: Port,
        /// Width of the I/O access (8, 16, or 32 bits)
        width: AccessWidth,
    },

    /// The guest performed a port-based I/O write operation.
    ///
    /// **Architecture**: x86-specific (other architectures don't have port I/O)
    ///
    /// The source register is implicitly `al`/`ax`/`eax` based on the access width.
    IoWrite {
        /// I/O port number being written to
        port: Port,
        /// Width of the I/O access (8, 16, or 32 bits)
        width: AccessWidth,
        /// Data being written to the I/O port
        data: u64,
    },

    /// An external interrupt was delivered to the VCpu.
    ///
    /// This represents hardware interrupts from external devices that need
    /// to be processed by the guest or hypervisor.
    ///
    /// **Note**: This enum may be extended with additional fields in the future.
    /// Use `..` in pattern matching to ensure forward compatibility.
    ExternalInterrupt {
        /// Hardware interrupt vector number
        vector: u64,
    },

    /// A nested page fault occurred during guest memory access.
    ///
    /// Also known as EPT violations on x86. These faults occur when:
    /// - Guest accesses unmapped memory regions
    /// - Access permissions are violated (e.g., writing to read-only pages)
    /// - Page table entries need to be populated or updated
    ///
    /// **Note**: This enum may be extended with additional fields in the future.
    /// Use `..` in pattern matching to ensure forward compatibility.
    NestedPageFault {
        /// Guest physical address that caused the fault
        addr: GuestPhysAddr,
        /// Type of access that was attempted (read/write/execute)
        access_flags: MappingFlags,
    },

    /// The guest VCpu has executed a halt instruction and is now idle.
    ///
    /// This typically occurs when the guest OS has no work to do and is
    /// waiting for interrupts or other events to wake it up.
    Halt,

    /// Request to bring up a secondary CPU core.
    ///
    /// This exit reason is used during the multi-core VM boot process when
    /// the primary CPU requests that a secondary CPU be started. The specific
    /// mechanism varies by architecture:
    ///
    /// - **ARM**: PSCI (Power State Coordination Interface) calls
    /// - **x86**: SIPI (Startup Inter-Processor Interrupt)
    /// - **RISC-V**: SBI (Supervisor Binary Interface) calls
    CpuUp {
        /// Target CPU identifier to be started
        ///
        /// Format varies by architecture:
        /// - **AArch64**: MPIDR register affinity fields  
        /// - **x86_64**: APIC ID of the target CPU
        /// - **RISC-V**: Hart ID of the target CPU
        target_cpu: u64,
        /// Guest physical address where the secondary CPU should begin execution
        entry_point: GuestPhysAddr,
        /// Argument to pass to the secondary CPU
        ///
        /// - **AArch64**: Value to set in `x0` register at startup
        /// - **RISC-V**: Value to set in `a1` register (`a0` gets the hartid)
        /// - **x86_64**: Currently unused
        arg: u64,
    },

    /// The guest VCpu has been powered down.
    ///
    /// This indicates the VCpu has executed a power-down instruction or
    /// hypercall and should be suspended. The VCpu may be resumed later.
    CpuDown {
        /// Power state information (currently unused)
        ///
        /// Reserved for future use with PSCI_POWER_STATE or similar mechanisms
        _state: u64,
    },

    /// The guest has requested system-wide shutdown.
    ///
    /// This indicates the entire virtual machine should be powered off,
    /// not just the current VCpu.
    SystemDown,

    /// No special handling required - the VCpu handled the exit internally.
    ///
    /// This provides an opportunity for the hypervisor to:
    /// - Check virtual device states
    /// - Process pending interrupts  
    /// - Handle background tasks
    /// - Perform scheduling decisions
    ///
    /// The VCpu can typically be resumed immediately after these checks.
    Nothing,

    /// VM entry failed due to invalid VCpu state or configuration.
    ///
    /// This corresponds to `KVM_EXIT_FAIL_ENTRY` in KVM and indicates that
    /// the hardware virtualization layer could not successfully enter the guest.
    ///
    /// The failure reason contains architecture-specific diagnostic information.
    FailEntry {
        /// Hardware-specific failure reason code
        ///
        /// Interpretation depends on the underlying virtualization technology
        /// and CPU architecture. Consult architecture documentation for details.
        hardware_entry_failure_reason: u64,
    },

    /// The guest is attempting to send an Inter-Processor Interrupt (IPI).
    ///
    /// IPIs are used for inter-CPU communication in multi-core systems.
    /// This does **not** include Startup IPIs (SIPI), which are handled
    /// by the [`AxVCpuExitReason::CpuUp`] variant.
    SendIPI {
        /// Target CPU identifier to receive the IPI
        ///
        /// This field is invalid if `send_to_all` or `send_to_self` is true.
        target_cpu: u64,
        /// Auxiliary field for complex target CPU specifications
        ///
        /// Currently used only on AArch64 where:
        /// - `target_cpu` contains `Aff3.Aff2.Aff1.0`
        /// - `target_cpu_aux` contains a bitmask for `Aff0` values
        target_cpu_aux: u64,
        /// Whether to broadcast the IPI to all CPUs except the sender
        send_to_all: bool,
        /// Whether to send the IPI to the current CPU (self-IPI)
        send_to_self: bool,
        /// IPI vector/interrupt number to deliver
        vector: u64,
    },
}
