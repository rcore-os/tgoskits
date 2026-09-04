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
//! Interrupt handling is always enabled as a baseline platform capability.
//!
//! - `smp`: Enable SMP (symmetric multiprocessing) support.
//! - `fp-simd`: Enable floating-point and SIMD support.
//! - `paging`: Enable page table manipulation.
//! - `tls`: Enable kernel space thread-local storage support.
//! - `rtc`: Enable real-time clock support.
//! - `uspace`: Enable user space support.
//!
//! [ArceOS]: https://github.com/arceos-org/arceos
//! [cargo test]: https://doc.rust-lang.org/cargo/guide/tests.html

#![no_std]

#[cfg(all(feature = "uspace", feature = "tls"))]
compile_error!("ax-hal features `uspace` and `tls` select incompatible register ownership modes");

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

pub mod irq;

#[cfg(feature = "paging")]
pub mod paging;

/// Console input and output.
pub mod console {
    pub use ax_plat::console::{
        ConsoleDeviceId, ConsoleDeviceIdError, ConsoleDeviceIdResult, ConsoleHandoffError,
        ConsoleHandoffResult, ConsoleIrqEvent, begin_runtime_handoff, commit_runtime_handoff,
        device_id, fail_runtime_handoff_closed, handle_irq, irq_num, read_bytes,
        rollback_runtime_handoff, set_input_irq_enabled, write_bytes, write_text_bytes,
    };
}

/// CPU power management.
pub mod power {
    #[cfg(feature = "smp")]
    pub use ax_plat::power::cpu_boot;
    pub use ax_plat::power::{system_off, system_reset};
}

/// CPU topology.
pub mod topology {
    /// Maps a firmware or hardware CPU ID to the runtime logical CPU index.
    #[cfg(any(test, feature = "host-test"))]
    pub const fn resolve_cpu_index(hardware_id: usize) -> Option<usize> {
        if hardware_id == 0 { Some(0) } else { None }
    }

    #[cfg(not(any(test, feature = "host-test")))]
    pub use ax_plat::cpu::resolve_cpu_index;

    #[cfg(test)]
    mod tests {
        use super::resolve_cpu_index;

        #[test]
        fn dummy_topology_only_maps_the_boot_cpu() {
            assert_eq!(resolve_cpu_index(0), Some(0));
            assert_eq!(resolve_cpu_index(1), None);
        }
    }
}

/// Trap handling.
pub mod trap {
    pub use ax_cpu::trap::{
        BreakpointHandler, DebugHandler, PageFaultFlags, breakpoint_handler, debug_handler,
        dispatch_irq, dispatch_page_fault, irq_handler, page_fault_handler, set_breakpoint_handler,
        set_debug_handler, set_irq_handler, set_page_fault_handler,
    };
}

/// CPU register states for context switching.
///
/// There are two types of context:
///
/// - [`TaskContext`][ax_cpu::TaskContext]: The context of a task.
/// - [`UserRegisters`][ax_cpu::UserRegisters]: User-owned registers saved at a trap boundary.
/// - [`KernelTrapFrame`][ax_cpu::KernelTrapFrame]: A CPU-pinned view of a kernel trap.
pub mod context {
    pub use ax_cpu::{KernelTlsBase, KernelTrapFrame, TaskContext, UserRegisters};
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
        use ax_lazyinit::LazyLock;

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

#[allow(unused_macros)]
macro_rules! addr_of_sym {
    ($e:ident) => {
        $e as *const () as usize
    };
}
#[cfg(feature = "tls")]
pub(crate) use addr_of_sym;
