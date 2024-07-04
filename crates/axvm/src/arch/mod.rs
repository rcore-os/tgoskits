//! Architecture dependent structures.

use axerrno::AxResult;

use crate::HostPhysAddr;
cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use self::x86_64::*;
    } else {
        // Following are things for the new, unified code structure. Just a stub here.
        use crate::AxVMHal;
        use axerrno::AxResult;
        use crate::mm::{GuestPhysAddr, HostPhysAddr};

        /// The architecture dependent configuration of a `AxArchVCpu`.
        #[derive(Clone, Copy, Debug, Default)]
        pub struct AxArchVCpuConfig {}

        pub struct AxArchVCpu<H: AxVMHal> {
            _marker: core::marker::PhantomData<H>,
        }

        impl<H: AxVMHal> AxArchVCpu<H> {
            pub fn new(_config: AxArchVCpuConfig) -> AxResult<Self> {
                Ok(Self {
                    _marker: core::marker::PhantomData,
                })
            }

            pub fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
                unimplemented!()
            }

            pub fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
                unimplemented!()
            }

            pub fn run(&mut self) -> AxResult<crate::vcpu::AxArchVCpuExitReason> {
                unimplemented!()
            }

            pub fn bind(&mut self) -> AxResult {
                unimplemented!()
            }

            pub fn unbind(&mut self) -> AxResult {
                unimplemented!()
            }
        }

        pub struct AxArchPerCpuState<H: AxVMHal> {
            _marker: core::marker::PhantomData<H>,
        }

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
