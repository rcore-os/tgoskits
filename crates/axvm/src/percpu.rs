use crate::{
    arch::AxArchPerCpuState,
    AxVMHal,
};
use axerrno::{ax_err, AxResult};
use core::mem::MaybeUninit;

/// Host per-CPU states to run the guest.
///
/// Recommended usage:
/// - Define a per-CPU state in hypervisor:
///
///   ```rust
///   #[percpu::def_percpu]
///   pub static AXVM_PER_CPU: AxVMPerCpu<MyHal> = AxVMPerCpu::new_uninit();
///   ```
///
/// - Then initialize and enable virtualization on each CPU in the hypervisor initialization code:
///
///   ```rust
///   let percpu = unsafe {
///       AXVM_PER_CPU.current_ref_mut_raw()
///   };
///   percpu.init(0).expect("Failed to initialize percpu state");
///   percpu.hardware_enable().expect("Failed to enable virtualization");
///   ```
pub struct AxVMPerCpu<H: AxVMHal> {
    /// The id of the CPU. It's also used to check whether the per-CPU state is initialized.
    cpu_id: Option<usize>,
    /// The architecture-specific per-CPU state.
    arch: MaybeUninit<AxArchPerCpuState<H>>,
}

impl<H: AxVMHal + 'static> AxVMPerCpu<H> {
    /// Create a new, uninitialized per-CPU state.
    pub const fn new_uninit() -> Self {
        Self {
            cpu_id: None,
            arch: MaybeUninit::uninit(),
        }
    }

    /// Initialize the per-CPU state.
    pub fn init(&mut self, cpu_id: usize) -> AxResult {
        if self.cpu_id.is_some() {
            ax_err!(BadState, "per-CPU state is already initialized")
        } else {
            self.cpu_id = Some(cpu_id);
            self.arch.write(AxArchPerCpuState::<H>::new(cpu_id));
            Ok(())
        }
    }

    /// Return the architecture-specific per-CPU state. Panics if the per-CPU state is not initialized.
    pub fn arch_checked(&self) -> &AxArchPerCpuState<H> {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        // SAFETY: `cpu_id` is `Some` here, so `arch` must be initialized.
        unsafe { self.arch.assume_init_ref() }
    }

    /// Return the mutable architecture-specific per-CPU state. Panics if the per-CPU state is not initialized.
    pub fn arch_checked_mut(&mut self) -> &mut AxArchPerCpuState<H> {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        // SAFETY: `cpu_id` is `Some` here, so `arch` must be initialized.
        unsafe { self.arch.assume_init_mut() }
    }

    /// Whether the current CPU has hardware virtualization enabled.
    pub fn is_enabled(&self) -> bool {
        self.arch_checked().is_enabled()
    }

    /// Enable hardware virtualization on the current CPU.
    pub fn hardware_enable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_enable()
    }

    /// Disable hardware virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_disable()
    }

    // // Create a [`AxVMVcpu`], set the entry point to `entry`, set the nested
    // // page table root to `npt_root`.
    // pub fn create_vcpu(
    //     &self,
    //     entry: GuestPhysAddr,
    //     npt_root: HostPhysAddr,
    // ) -> AxResult<AxVMVcpu<H>> {
    //     if !self.is_enabled() {
    //         ax_err!(BadState, "virtualization is not enabled")
    //     } else {
    //         AxVMVcpu::new(entry, npt_root)
    //     }
    // }
}

// impl<H: AxVMHal> Drop for AxVMPerCpu<H> {
//     fn drop(&mut self) {
//         if self.is_enabled() {
//             self.hardware_disable().unwrap();
//         }
//     }
// }
