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

//! Architecture dependent vcpu implementations.

cfg_if::cfg_if! {
    if #[cfg(all(target_arch = "x86_64", any(feature = "vmx", feature = "svm")))] {
        pub use x86_vcpu::X86ArchVCpu as AxArchVCpuImpl;
        pub use x86_vcpu::X86ArchPerCpuState as AxVMArchPerCpuImpl;
        pub use x86_vcpu::X86VCpuSetupConfig as AxVCpuSetupConfig;
        pub use x86_vcpu::has_hardware_support;
        #[allow(dead_code)]
        pub type AxVCpuCreateConfig = ();

        /// x86_64 EPT always uses 4-level page tables.
        pub fn max_guest_page_table_levels() -> usize { 4 }

        // Note:
        // According to the requirements of `x86_vcpu`,
        // users of the `x86_vcpu` crate need to implement the `PhysFrameIf` trait for it with the help of `crate_interface`.
        //
        // Since in our hypervisor architecture, `axvm` is not responsible for OS-related resource management,
        // we leave the `PhysFrameIf` implementation to `vmm_app`.
    } else if #[cfg(target_arch = "x86_64")] {
        mod x86_no_backend {
            use ax_errno::{AxResult, ax_err};
            use axaddrspace::{GuestPhysAddr, HostPhysAddr};
            use axvcpu::{AxArchPerCpu, AxArchVCpu, AxVCpuExitReason};
            use axvisor_api::types::{VCpuId, VMId};

            pub struct NoBackendPerCpu;

            impl AxArchPerCpu for NoBackendPerCpu {
                fn new(_cpu_id: usize) -> AxResult<Self> {
                    Ok(Self)
                }

                fn is_enabled(&self) -> bool {
                    false
                }

                fn hardware_enable(&mut self) -> AxResult {
                    ax_err!(Unsupported, "x86 virtualization backend is not enabled")
                }

                fn hardware_disable(&mut self) -> AxResult {
                    Ok(())
                }
            }

            pub struct NoBackendVCpu;

            impl AxArchVCpu for NoBackendVCpu {
                type CreateConfig = ();
                type SetupConfig = x86_vcpu::X86VCpuSetupConfig;

                fn new(_vm_id: VMId, _vcpu_id: VCpuId, _config: Self::CreateConfig) -> AxResult<Self> {
                    Ok(Self)
                }

                fn set_entry(&mut self, _entry: GuestPhysAddr) -> AxResult {
                    Ok(())
                }

                fn set_ept_root(&mut self, _ept_root: HostPhysAddr) -> AxResult {
                    Ok(())
                }

                fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
                    ax_err!(Unsupported, "x86 virtualization backend is not enabled")
                }

                fn run(&mut self) -> AxResult<AxVCpuExitReason> {
                    ax_err!(Unsupported, "x86 virtualization backend is not enabled")
                }

                fn bind(&mut self) -> AxResult {
                    ax_err!(Unsupported, "x86 virtualization backend is not enabled")
                }

                fn unbind(&mut self) -> AxResult {
                    Ok(())
                }

                fn set_gpr(&mut self, _reg: usize, _val: usize) {}

                fn inject_interrupt(&mut self, _vector: usize) -> AxResult {
                    ax_err!(Unsupported, "x86 virtualization backend is not enabled")
                }

                fn set_return_value(&mut self, _val: usize) {}
            }
        }

        pub use self::x86_no_backend::NoBackendPerCpu as AxVMArchPerCpuImpl;
        pub use self::x86_no_backend::NoBackendVCpu as AxArchVCpuImpl;
        pub use x86_vcpu::X86VCpuSetupConfig as AxVCpuSetupConfig;
        #[allow(dead_code)]
        pub type AxVCpuCreateConfig = ();

        pub fn has_hardware_support() -> bool { false }

        /// x86_64 EPT always uses 4-level page tables.
        pub fn max_guest_page_table_levels() -> usize { 4 }
    } else if #[cfg(target_arch = "riscv64")] {
        pub use riscv_vcpu::RISCVVCpu as AxArchVCpuImpl;
        pub use riscv_vcpu::RISCVPerCpu as AxVMArchPerCpuImpl;
        pub use riscv_vcpu::RISCVVCpuCreateConfig as AxVCpuCreateConfig;
        pub use riscv_vcpu::has_hardware_support;

        /// RISC-V uses Sv39 (3 levels) or Sv48 (4 levels) for guest page tables.
        /// Default to 4 levels (Sv48) for maximum address space.
        pub fn max_guest_page_table_levels() -> usize { 4 }
    } else if #[cfg(target_arch = "loongarch64")] {
        pub use loongarch_vcpu::LoongArchPerCpu as AxVMArchPerCpuImpl;
        pub use loongarch_vcpu::LoongArchVCpu as AxArchVCpuImpl;
        pub use loongarch_vcpu::LoongArchVCpuCreateConfig as AxVCpuCreateConfig;
        pub use loongarch_vcpu::LoongArchVCpuSetupConfig as AxVCpuSetupConfig;
        pub use loongarch_vcpu::has_hardware_support;

        /// LoongArch guests currently use 4-level page tables.
        pub fn max_guest_page_table_levels() -> usize { 4 }
    } else if #[cfg(target_arch = "aarch64")] {
        pub use arm_vcpu::Aarch64VCpu as AxArchVCpuImpl;
        pub use arm_vcpu::Aarch64PerCpu as AxVMArchPerCpuImpl;
        pub use arm_vcpu::Aarch64VCpuCreateConfig as AxVCpuCreateConfig;
        pub use arm_vcpu::Aarch64VCpuSetupConfig as AxVCpuSetupConfig;
        pub use arm_vcpu::has_hardware_support;
        pub use arm_vcpu::max_guest_page_table_levels;

        pub use arm_vgic::vtimer::get_sysreg_device;
    }
}
