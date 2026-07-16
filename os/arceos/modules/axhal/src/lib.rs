//! [ArceOS] hardware abstraction layer, provides unified APIs for
//! platform-specific operations.
//!
//! It does the bootstrapping and initialization process for the specified
//! platform, and provides useful operations on the hardware.
//!
//! Currently supported platforms (specify by cargo features):
//!
//! - Runtime-discovered platform support through `axplat-dyn`.
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

#[path = "platform.rs"]
mod platform_select;
pub use platform_select::selected as platform;

mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

pub mod boot;
pub mod cache;
pub mod dtb;
pub mod mem;
pub mod percpu;
pub mod pmu;
pub mod time;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "irq")]
pub mod irq;

#[cfg(feature = "paging")]
pub mod paging;

/// Console input and output.
pub mod console {
    pub use ax_plat::console::{
        ConsoleDeviceId, ConsoleDeviceIdError, ConsoleDeviceIdResult, claim_runtime_output,
        device_id, read_bytes, write_bytes, write_text_bytes,
    };
    #[cfg(feature = "irq")]
    pub use ax_plat::console::{ConsoleIrqEvent, handle_irq, irq_num, set_input_irq_enabled};
}

/// CPU power management.
pub mod power {
    #[cfg(feature = "smp")]
    pub use ax_plat::power::cpu_boot;
    pub use ax_plat::power::{system_off, system_reset};
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

pub use ax_cpu as cpu;
pub use ax_cpu::asm;
#[cfg(feature = "uspace")]
pub use ax_cpu::uspace;
#[cfg(feature = "smp")]
pub use ax_plat::init::init_later_secondary;
pub use ax_plat::{init::init_later, platform::platform_name};

/// Initializes the platform and boot argument.
/// This function should be called as early as possible.
pub fn init_early(cpu_id: usize, arg: usize) {
    dtb::init(arg);
    ax_cpu::init::init_trap();
    ax_plat::init::init_early(cpu_id, arg);
}

/// Initializes the CPU trap vector and platform early state for a secondary CPU.
#[cfg(feature = "smp")]
pub fn init_early_secondary(cpu_id: usize) {
    ax_cpu::init::init_trap();
    ax_plat::init::init_early_secondary(cpu_id);
}

/// Gets the number of CPUs running in the system.
///
/// When SMP is disabled, this function always returns 1.
///
/// When SMP is enabled, it's the smaller one between the platform-declared CPU
/// number [`ax_plat::power::cpu_num`] and the build-time CPU capacity.
///
/// This value is determined during the BSP initialization phase.
pub fn cpu_num() -> usize {
    #[cfg(feature = "smp")]
    {
        use spin::LazyLock;

        /// The number of CPUs in the system. Based on the number declared by the
        /// platform crate and limited by the configured maximum CPU number.
        static CPU_NUM: LazyLock<usize> = LazyLock::new(|| {
            let max_cpu_num = build_info::CPU_CAPACITY;
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

/// Returns the firmware or architecture hardware ID of one logical host CPU.
pub fn cpu_hardware_id(cpu_id: usize) -> Option<usize> {
    ax_plat::power::cpu_hardware_id(cpu_id)
}

#[allow(unused_macros)]
macro_rules! addr_of_sym {
    ($e:ident) => {
        $e as *const () as usize
    };
}
#[cfg(feature = "tls")]
pub(crate) use addr_of_sym;
