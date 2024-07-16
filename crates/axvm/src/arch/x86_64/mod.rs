mod lapic;
pub(crate) mod msr;

#[macro_use]
pub(crate) mod regs;

pub(crate) mod ept;

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vender;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};

        pub use vender::VmxArchVCpu as VenderArchVCpu;
        pub use vender::VmxArchPerCpuState as VenderArchPerCpuState;
    }
}

pub(crate) use vender::has_hardware_support;

pub use lapic::ApicTimer;
pub use regs::GeneralRegisters;
pub use vender::X64NestedPageTable;

pub use VenderArchPerCpuState as AxVMArchPerCpuImpl;
pub use VenderArchVCpu as AxArchVCpuImpl;
