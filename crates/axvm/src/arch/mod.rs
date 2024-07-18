//! Architecture dependent structures.

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use self::x86_64::*;
    } else if #[cfg(target_arch = "riscv64")] {
        mod riscv64;
        pub use self::riscv64::*;
    } else {
        // Following are things for the new, unified code structure. It's just demonstration and won't compile.
        pub struct AxArchVCpuImpl<H: AxVMHal> {}
        impl<H: AxVMHal> axvcpu::AxArchVCpu for AxArchVCpuImpl<H> {
            // ...implementation...
        }

        pub struct AxArchPerCpuState<H: AxVMHal> {}

        impl<H: AxVMHal> AxVMArchPerCpu for AxVMArchPerCpuImpl<H> {
            // ...implementation...
        }

        pub struct AxArchDeviceList<H: AxVMHal> {}

        impl<H: AxVMHal> AxArchDeviceList<H> {
            pub fn new() -> Self {
                // ...implementation...
            }
        }

        pub fn has_hardware_support() -> bool {
            // ...implementation...
        }
    }
}
