//! [ArceOS] hardware abstraction layer, provides unified APIs for
//! platform-specific operations.
//!
//! It does the bootstrapping and initialization process for the specified
//! platform, and provides useful operations on the hardware.
//!
//! Currently supported platforms (specify by cargo features):
//!
//! - `x86-pc`: Standard PC with x86_64 ISA.
//! - `riscv64-qemu-virt`: QEMU virt machine with RISC-V ISA.
//! - `aarch64-qemu-virt`: QEMU virt machine with AArch64 ISA.
//! - `aarch64-raspi`: Raspberry Pi with AArch64 ISA.
//! - `dummy`: If none of the above platform is selected, the dummy platform
//!   will be used. In this platform, most of the operations are no-op or
//!   `unimplemented!()`. This platform is mainly used for [cargo test].
//!
//! # Cargo Features
//!
//! - `smp`: Enable SMP (symmetric multiprocessing) support.
//! - `fp-simd`: Enable floating-point and SIMD support.
//! - `paging`: Enable page table manipulation.
//! - `irq`: Enable interrupt handling support.
//! - `tls`: Enable kernel space thread-local storage support.
//! - `rtc`: Enable real-time clock support.
//! - `uspace`: Enable user space support.
//!
//! [ArceOS]: https://github.com/arceos-org/arceos
//! [cargo test]: https://doc.rust-lang.org/cargo/guide/tests.html

#![no_std]

#[allow(unused_imports)]
#[macro_use]
extern crate log;

#[allow(unused_imports)]
#[macro_use]
extern crate ax_memory_addr;

cfg_if::cfg_if! {
    if #[cfg(feature = "myplat")] {
        // link the custom platform crate in your application.
    }
    else if #[cfg(plat_dyn)] {
        extern crate axplat_dyn;
    }
    else if #[cfg(all(target_os = "none", feature = "defplat"))] {
        #[cfg(target_arch = "x86_64")]
        extern crate ax_plat_x86_pc;
        #[cfg(target_arch = "aarch64")]
        extern crate ax_plat_aarch64_qemu_virt;
        #[cfg(target_arch = "riscv64")]
        extern crate ax_plat_riscv64_qemu_virt;
        #[cfg(target_arch = "loongarch64")]
        extern crate ax_plat_loongarch64_qemu_virt;
    } else {
        // Link the dummy platform implementation to pass cargo test.
        mod dummy;
    }
}

pub mod dtb;
pub mod mem;
pub mod percpu;
pub mod time;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "irq")]
pub mod irq;

#[cfg(feature = "paging")]
pub mod paging;

/// Console input and output.
pub mod console {
    #[cfg(feature = "irq")]
    pub use ax_plat::console::irq_num;
    pub use ax_plat::console::{read_bytes, write_bytes};
}

/// CPU power management.
pub mod power {
    #[cfg(feature = "smp")]
    pub use ax_plat::power::cpu_boot;
    pub use ax_plat::power::system_off;
}

/// Trap handling.
pub mod trap {
    #[cfg(target_arch = "x86_64")]
    pub use ax_cpu::trap::debug_handler;
    pub use ax_cpu::trap::{
        PageFaultFlags, breakpoint_handler, dispatch_irq, dispatch_page_fault, irq_handler,
        page_fault_handler, set_irq_handler, set_page_fault_handler,
    };
}

/// CPU register states for context switching.
///
/// There are two types of context:
///
/// - [`TaskContext`][ax_cpu::TaskContext]: The context of a task.
/// - [`TrapFrame`][ax_cpu::TrapFrame]: The context of an interrupt or an exception.
pub mod context {
    pub use ax_cpu::{TaskContext, TrapFrame};
}

pub use ax_cpu::asm;
#[cfg(feature = "uspace")]
pub use ax_cpu::uspace;
pub use ax_plat::init::init_later;
#[cfg(feature = "smp")]
pub use ax_plat::init::{init_early_secondary, init_later_secondary};

/// Initializes the platform and boot argument.
/// This function should be called as early as possible.
pub fn init_early(cpu_id: usize, arg: usize) {
    dtb::init(arg);
    ax_plat::init::init_early(cpu_id, arg);
}

/// Gets the number of CPUs running in the system.
///
/// When SMP is disabled, this function always returns 1.
///
/// When SMP is enabled, it's the smaller one between the platform-declared CPU
/// number [`ax_plat::power::cpu_num`] and the configured maximum CPU number
/// `ax_config::plat::MAX_CPU_NUM`.
///
/// This value is determined during the BSP initialization phase.
pub fn cpu_num() -> usize {
    #[cfg(feature = "smp")]
    {
        use spin::Lazy;

        /// The number of CPUs in the system. Based on the number declared by the
        /// platform crate and limited by the configured maximum CPU number.
        static CPU_NUM: Lazy<usize> = Lazy::new(|| {
            let max_cpu_num = ax_config::plat::MAX_CPU_NUM;
            let plat_cpu_num = ax_plat::power::cpu_num();
            let cpu_num = plat_cpu_num.min(max_cpu_num);

            info!("CPU number: max = {max_cpu_num}, platform = {plat_cpu_num}, use = {cpu_num}");

            if plat_cpu_num > max_cpu_num {
                warn!(
                    "platform declares more CPUs ({plat_cpu_num}) than configured max \
                     ({max_cpu_num}), only the first {max_cpu_num} CPUs will be used."
                );
            }

            cpu_num
        });

        *CPU_NUM
    }
    #[cfg(not(feature = "smp"))]
    {
        1
    }
}

#[allow(unused_macros)]
macro_rules! addr_of_sym {
    ($e:ident) => {
        $e as *const () as usize
    };
}
pub(crate) use addr_of_sym;
