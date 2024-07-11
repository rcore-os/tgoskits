//! Architecture dependent structures.

use axerrno::AxResult;

use crate::HostPhysAddr;
cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use self::x86_64::*;
    } else {
        // Following are things for the new, unified code structure. It's just demonstration and won't compile.
        pub struct AxArchVCpuImpl<H: AxVMHal> {}
        impl axvcpu::AxArchVCpu for AxArchVCpuImpl<H> {
            /// ...implementation...
        }

        pub struct AxArchPerCpuState<H: AxVMHal> {}

        impl<H: AxVMHal> AxArchPerCpuState<H> {
            pub fn new(_cpu_id: usize) -> Self {
                Self {
                    _marker: core::marker::PhantomData,
                }
            }

            pub fn is_enabled(&self) -> bool {
                unimplemented!()
            }

            pub fn hardware_enable(&mut self) -> AxResult<()> {
                unimplemented!()
            }

            pub fn hardware_disable(&mut self) -> AxResult<()> {
                unimplemented!()
            }
        }
    }
}
