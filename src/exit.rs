use axaddrspace::{
    GuestPhysAddr, MappingFlags,
    device::{AccessWidth, Port, SysRegAddr},
};

#[allow(unused_imports)] // used in doc
use super::AxArchVCpu;

/// The result of [`AxArchVCpu::run`].
/// Can we reference or directly reuse content from [kvm-ioctls](https://github.com/rust-vmm/kvm-ioctls/blob/main/src/ioctls/vcpu.rs) ?
#[non_exhaustive]
#[derive(Debug)]
pub enum AxVCpuExitReason {
    /// The instruction executed by the vcpu performs a hypercall.
    Hypercall {
        /// The hypercall number.
        nr: u64,
        /// The arguments for the hypercall.
        args: [u64; 6],
    },
    /// The instruction executed by the vcpu performs a MMIO read operation.
    MmioRead {
        /// The physical address of the MMIO read.
        addr: GuestPhysAddr,
        /// The width of the MMIO read.
        width: AccessWidth,
        /// The index of reg to be read
        reg: usize,
        /// The width of the reg to be read
        reg_width: AccessWidth,
        /// Sign-extend the read value if true.
        signed_ext: bool,
    },
    /// The instruction executed by the vcpu performs a MMIO write operation.
    MmioWrite {
        /// The physical address of the MMIO write.
        addr: GuestPhysAddr,
        /// The width of the MMIO write.
        width: AccessWidth,
        /// The data to be written.
        data: u64,
    },
    /// The instruction executed by the vcpu performs a system register read operation.
    ///
    /// System register here refers `MSR`s in x86, `CSR`s in RISC-V, and `System registers` in Aarch64.
    SysRegRead {
        /// The address of the system register to be read.
        ///
        /// Under x86_64 and RISC-V, this field is the address.
        ///
        /// Under Aarch64, this field follows the ESR_EL2.ISS format: `<op0><op2><op1><CRn>00000<CRm>0`,
        /// which is consistent with the numbering scheme in the `aarch64_sysreg` crate.
        addr: SysRegAddr,
        /// The index of the GPR (general purpose register) where the value should be stored.
        ///
        /// Note that in x86_64, the destination register is always `[edx:eax]`, so this field is unused.
        reg: usize,
    },
    /// The instruction executed by the vcpu performs a system register write operation.
    ///
    /// System register here refers `MSR`s in x86, `CSR`s in RISC-V, and `System registers` in Aarch64.
    SysRegWrite {
        /// The address of the system register to be written.
        ///
        /// Under x86_64 and RISC-V, this field is the address.
        ///
        /// Under Aarch64, this field follows the ESR_EL2.ISS format: `<op0><op2><op1><CRn>00000<CRm>0`,
        /// which is consistent with the numbering scheme in the `aarch64_sysreg` crate.
        addr: SysRegAddr,
        /// Data to be written.
        value: u64,
    },
    /// The instruction executed by the vcpu performs a I/O read operation.
    ///
    /// It's unnecessary to specify the destination register because it's always `al`, `ax`, or `eax` (as port-I/O exists only in x86).
    IoRead {
        /// The port number of the I/O read.
        port: Port,
        /// The width of the I/O read.
        width: AccessWidth,
    },
    /// The instruction executed by the vcpu performs a I/O write operation.
    ///
    /// It's unnecessary to specify the source register because it's always `al`, `ax`, or `eax` (as port-I/O exists only in x86).
    IoWrite {
        /// The port number of the I/O write.
        port: Port,
        /// The width of the I/O write.
        width: AccessWidth,
        /// The data to be written.
        data: u64,
    },
    /// An external interrupt happened.
    ///
    /// Note that fields may be added in the future, use `..` to handle them.
    ExternalInterrupt {
        /// The interrupt vector.
        vector: u64,
    },
    /// A nested page fault happened. (EPT violation in x86)
    ///
    /// Note that fields may be added in the future, use `..` to handle them.
    NestedPageFault {
        /// The guest physical address of the fault.
        addr: GuestPhysAddr,
        /// The access flags of the fault.
        access_flags: MappingFlags,
    },
    /// The vcpu is halted.
    Halt,
    /// Try to bring up a secondary CPU.
    ///
    /// This is used to notify the hypervisor that the target vcpu
    /// is powered on and ready to run, generally used in the boot process
    /// of a multi-core VM.
    /// This VM exit reason is architecture-specific, may be triggered by
    /// * a PSCI call in ARM
    /// * a SIPI in x86
    /// * a sbi call in RISC-V
    CpuUp {
        /// The target vcpu id that is to be started.
        /// * for aarch64, it contains the affinity fields of the MPIDR register,
        /// * for x86_64, it contains the APIC ID of the secondary CPU,
        /// * for RISC-V, it contains the hartid of the secondary CPU.
        target_cpu: u64,
        /// Runtime-specified physical address of the secondary CPU's entry point, where the vcpu can start executing.
        entry_point: GuestPhysAddr,
        /// This argument passed as the first argument to the secondary CPU's.
        /// * for aarch64, it is the `arg` value that will be set in the `x0` register when the vcpu starts executing at `entry_point`.
        /// * for RISC-V, it will be set in the `a1` register when the hart starts executing at `entry_point`, and the `a0` register will be set to the hartid.
        /// * for x86_64, it is currently unused.
        arg: u64,
    },
    /// The vcpu is powered off.
    ///
    /// This vcpu may be resumed later.
    CpuDown {
        /// Currently unused.
        /// Maybe used for `PSCI_POWER_STATE` in the future.
        _state: u64,
    },
    /// The system should be powered off.
    ///
    /// This is used to notify the hypervisor that the whole system should be powered off.
    SystemDown,
    /// Nothing special happened, the vcpu has handled the exit itself.
    ///
    /// This exists to allow the caller to have a chance to check virtual devices/physical devices/virtual interrupts.
    Nothing,
    /// Something bad happened during VM entry, the vcpu could not be run due to unknown reasons.
    /// Further architecture-specific information is available in hardware_entry_failure_reason.
    /// Corresponds to `KVM_EXIT_FAIL_ENTRY`.
    FailEntry {
        /// Architecture related VM entry failure reasons.
        hardware_entry_failure_reason: u64,
    },
    /// The vcpu is trying to send an Inter-Processor Interrupt (IPI) to another CPU.
    ///
    /// This does not include SIPI, which is handled by [`AxVCpuExitReason::CpuUp`].
    SendIPI {
        /// The target CPU to send the IPI to. Invalid if any of `send_to_all` or `send_to_self` is
        /// true.
        target_cpu: u64,
        /// The auxiliary field for the target CPU list. Used currently only in aarch64.
        ///
        /// In aarch64, `target_cpu` contains Aff3.Aff2.Aff1.0, while this field contains a bitmask
        /// specifying the Aff0 values of the target CPUs.
        target_cpu_aux: u64,
        /// Specifies whether the IPI should be sent to all CPUs except the current one.
        send_to_all: bool,
        /// Specifies whether the IPI should be sent to the current CPU.
        send_to_self: bool,
        /// The IPI vector to be sent.
        vector: u64,
    },
}
