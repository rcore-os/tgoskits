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

//! Runtime library of [ArceOS](https://github.com/arceos-org/arceos).
//!
//! Any application uses ArceOS should link this library. It does some
//! initialization work before entering the application's `main` function.
//!
//! # Cargo Features
//!
//! - `paging`: Enable page table manipulation support.
//! - `irq`: Enable interrupt handling support.
//! - `multitask`: Enable multi-threading support.
//! - `smp`: Enable SMP (symmetric multiprocessing) support.
//! - `fs`: Enable filesystem support.
//! - `net`: Enable networking support.
//! - `display`: Enable graphics support.
//!
//! All the features are optional and disabled by default.

#![feature(extern_item_impls)]
#![cfg_attr(not(test), no_std)]
#![allow(missing_abi)]

#[macro_use]
extern crate ax_log;

extern crate ax_driver as _;

#[cfg(all(target_os = "none", not(feature = "std-compat"), not(test)))]
mod lang_items;

#[cfg(feature = "smp")]
mod mp;

#[cfg(any(feature = "irq", feature = "paging"))]
mod klib;

#[cfg(any(feature = "fs", feature = "fs-ng", test))]
mod block;
mod devices;
#[cfg(feature = "irq")]
pub mod irq;
mod registers;

#[cfg(all(feature = "net", any(feature = "fs", feature = "fs-ng")))]
mod unix_ns;

pub use ax_hal as hal;

#[cfg(feature = "smp")]
pub use self::mp::rust_main_secondary;

extern crate alloc;

const LOGO: &str = r#"
       d8888                            .d88888b.   .d8888b.
      d88888                           d88P" "Y88b d88P  Y88b
     d88P888                           888     888 Y88b.
    d88P 888 888d888  .d8888b  .d88b.  888     888  "Y888b.
   d88P  888 888P"   d88P"    d8P  Y8b 888     888     "Y88b.
  d88P   888 888     888      88888888 888     888       "888
 d8888888888 888     Y88b.    Y8b.     Y88b. .d88P Y88b  d88P
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"
"#;

#[eii]
fn ax_app_entry() {
    #[cfg(not(test))]
    unsafe extern "C" {
        /// Legacy application's entry point.
        safe fn main();
    }
    // Default implementation
    #[cfg(not(test))]
    main();
}

struct LogIfImpl;

#[cfg(feature = "paging")]
fn runtime_page_fault_handler(
    addr: ax_memory_addr::VirtAddr,
    flags: ax_hal::trap::PageFaultFlags,
) -> bool {
    #[cfg(feature = "stack-guard-page")]
    if ax_task::diagnose_current_stack_guard_page_fault(addr) {
        return false;
    }

    ax_mm::kernel_aspace().lock().handle_page_fault(addr, flags)
}

#[ax_crate_interface::impl_interface]
impl ax_log::LogIf for LogIfImpl {
    fn console_write_str(s: &str) {
        ax_hal::console::write_text_bytes(s.as_bytes());
    }

    fn current_time() -> core::time::Duration {
        ax_hal::time::monotonic_time()
    }

    fn current_cpu_id() -> Option<usize> {
        #[cfg(feature = "smp")]
        if is_init_ok() {
            Some(ax_hal::percpu::this_cpu_id())
        } else {
            None
        }
        #[cfg(not(feature = "smp"))]
        Some(0)
    }

    fn current_task_id() -> Option<u64> {
        if is_init_ok() {
            #[cfg(feature = "multitask")]
            {
                ax_task::current_may_uninit().map(|curr| curr.id().as_u64())
            }
            #[cfg(not(feature = "multitask"))]
            None
        } else {
            None
        }
    }
}

use core::sync::atomic::{AtomicUsize, Ordering};

/// Number of CPUs that have completed initialization.
static INITED_CPUS: AtomicUsize = AtomicUsize::new(0);

fn is_init_ok() -> bool {
    INITED_CPUS.load(Ordering::Acquire) == ax_hal::cpu_num()
}

/// The main entry point of the ArceOS runtime.
///
/// It is called from the bootstrapping code in the specific platform crate (see
/// [`ax_plat::main`]).
///
/// `cpu_id` is the logic ID of the current CPU, and `arg` is passed from the
/// bootloader (typically the device tree blob address).
///
/// In multi-core environment, this function is called on the primary core, and
/// secondary cores call [`rust_main_secondary`].
#[cfg_attr(not(test), ax_plat::main)]
pub fn rust_main(cpu_id: usize, arg: usize) -> ! {
    #[cfg(not(feature = "plat-dyn"))]
    unsafe {
        ax_hal::mem::clear_bss()
    };
    ax_hal::percpu::init_primary(cpu_id);
    // After per-CPU init, before scheduler/IPI/IRQ paths can allocate.
    // This is a no-op for allocator backends that do not need per-CPU state.
    ax_alloc::init_percpu_slab(cpu_id);
    ax_hal::init_early(cpu_id, arg);
    let log_level = option_env!("AX_LOG").unwrap_or("info");

    ax_println!("{}", LOGO);
    ax_println!(
        indoc::indoc! {"
            arch = {}
            platform = {}
            target = {}
            build_mode = {}
            log_level = {}
            backtrace = {}
            smp = {}
        "},
        ax_config::ARCH,
        ax_config::PLATFORM,
        option_env!("AX_TARGET").unwrap_or(""),
        option_env!("AX_MODE").unwrap_or(""),
        log_level,
        axbacktrace::is_enabled(),
        ax_hal::cpu_num()
    );

    ax_log::init();
    ax_log::set_max_level(log_level); // no effect if set `log-level-*` features
    info!("Logging is enabled.");
    info!("Primary CPU {cpu_id} started, arg = {arg:#x}.");

    info!("Found physcial memory regions:");
    for r in ax_hal::mem::memory_regions() {
        info!(
            "  [{:x?}, {:x?}) {} ({:?})",
            r.paddr,
            r.paddr + r.size,
            r.name,
            r.flags
        );
    }

    init_allocator();

    let (kernel_space_start, kernel_space_size) = ax_hal::mem::kernel_aspace();

    {
        use core::ops::Range;

        unsafe extern "C" {
            safe static _stext: [u8; 0];
            safe static _etext: [u8; 0];
        }

        let fp_range_start = kernel_space_start.as_usize();
        let fp_range_end = fp_range_start.saturating_add(kernel_space_size);
        axbacktrace::init(
            Range {
                start: _stext.as_ptr() as usize,
                end: _etext.as_ptr() as usize,
            },
            Range {
                start: fp_range_start,
                end: fp_range_end,
            },
        );
    }

    info!(
        "kernel aspace: [{:#x?}, {:#x?})",
        kernel_space_start,
        kernel_space_start + kernel_space_size,
    );

    #[cfg(feature = "paging")]
    {
        ax_mm::init_memory_management();
        ax_hal::trap::set_page_fault_handler(runtime_page_fault_handler);
    }

    info!("Initialize platform devices...");
    ax_hal::init_later(cpu_id, arg);
    if cfg!(not(feature = "plat-dyn")) && !rdrive::is_initialized() {
        rdrive::init(rdrive::Platform::Static)
            .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    }
    if rdrive::is_initialized() {
        registers::append_linker_registers();
        rdrive::probe_pre_kernel()
            .unwrap_or_else(|err| panic!("failed to run pre-kernel driver probes: {err:?}"));
    } else {
        warn!("rdrive is not initialized; skip pre-kernel driver probe");
    }

    #[cfg(feature = "multitask")]
    ax_task::init_scheduler();

    #[cfg(feature = "ipi")]
    {
        ax_ipi::init();
        #[cfg(feature = "irq")]
        ax_hal::irq::set_run_on_cpu_sync(ax_ipi_run_on_cpu_sync);
    }

    #[cfg(feature = "irq")]
    {
        info!("Initialize interrupt handlers...");
        init_interrupt();
    }

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::mark_current_cpu_ready();

    devices::probe_all_devices();

    #[cfg(feature = "rtc")]
    ax_println!(
        "Boot at {}\n",
        chrono::DateTime::from_timestamp_nanos(ax_hal::time::wall_time_nanos() as _),
    );

    cfg_if::cfg_if! {
        if #[cfg(all(feature = "fs-ng", feature = "plat-dyn"))] {
            block::init_dyn_fs_ng(ax_hal::dtb::get_chosen_bootargs());
        } else if #[cfg(all(feature = "fs-ng", not(feature = "plat-dyn")))] {
            block::init_static_fs_ng();
        } else if #[cfg(all(feature = "fs", feature = "plat-dyn"))] {
            ax_fs::init_filesystems(
                devices::take_dyn_fs_block_devices(),
                ax_hal::dtb::get_chosen_bootargs(),
            );
        } else if #[cfg(all(feature = "fs", not(feature = "plat-dyn")))] {
            ax_fs::init_filesystems(devices::take_static_fs_block_devices(), None);
        }
    }

    #[cfg(all(feature = "display", feature = "plat-dyn"))]
    devices::init_dyn_display();

    #[cfg(all(feature = "display", not(feature = "plat-dyn")))]
    devices::init_static_display();

    #[cfg(all(feature = "input", feature = "plat-dyn"))]
    devices::init_dyn_input();

    #[cfg(all(feature = "input", not(feature = "plat-dyn")))]
    devices::init_static_input();

    cfg_if::cfg_if! {
        if #[cfg(all(feature = "net", feature = "plat-dyn"))] {
            devices::init_dyn_net();
        } else if #[cfg(all(feature = "net", not(feature = "plat-dyn")))] {
            devices::init_static_net();
        }
    }

    #[cfg(all(feature = "vsock", feature = "plat-dyn"))]
    devices::init_dyn_vsock();

    #[cfg(all(feature = "vsock", not(feature = "plat-dyn")))]
    devices::init_static_vsock();

    #[cfg(feature = "smp")]
    self::mp::start_secondary_cpus(cpu_id);

    #[cfg(all(feature = "tls", not(feature = "multitask")))]
    {
        info!("Initialize thread local storage...");
        init_tls();
    }

    ax_ctor_bare::call_ctors();

    info!("Primary CPU {cpu_id} init OK.");
    INITED_CPUS.fetch_add(1, Ordering::Release);

    while !is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::wait_for_all_cpus_ready();

    ax_app_entry();

    #[cfg(feature = "multitask")]
    ax_task::exit(0);
    #[cfg(not(feature = "multitask"))]
    {
        debug!("main task exited: exit_code={}", 0);
        ax_hal::power::system_off();
    }
}

fn init_allocator() {
    use ax_hal::mem::{MemRegionFlags, memory_regions, phys_to_virt};

    info!("Initialize global memory allocator...");
    info!("  use {} allocator.", ax_alloc::global_allocator().name());

    // The page allocator (which backs user-space page population via
    // `alloc_pages`) is initialized from a single contiguous region by
    // `global_init`; every other free region is handed to the byte/heap
    // allocator by `global_add_memory` (the bitmap page allocator does not
    // support `add_memory`). So the region chosen for `global_init` *is* the
    // entire pool available for user memory.
    //
    // Pick the LARGEST free region for the page allocator. Platforms with a
    // single contiguous RAM region (x86/aarch64/riscv64 qemu-virt) are
    // unaffected (largest == the only region). Platforms with disjoint regions
    // (loongarch64 qemu-virt: a small ~248 MB low region below the MMIO hole
    // plus the multi-GB high region at 0x8000_0000) previously picked the small
    // low region — the "first free region after .bss" heuristic — which capped
    // all user allocations at ~248 MB regardless of total RAM, OOM'ing large
    // workloads (e.g. the gradle build JVM) even with gigabytes free.
    let mut max_region_size = 0;
    let mut max_region_paddr = 0.into();

    for r in memory_regions() {
        if r.flags.contains(MemRegionFlags::FREE) && r.size > max_region_size {
            max_region_size = r.size;
            max_region_paddr = r.paddr;
        }
    }

    for r in memory_regions() {
        if r.flags.contains(MemRegionFlags::FREE) && r.paddr == max_region_paddr {
            ax_alloc::global_init(phys_to_virt(r.paddr).as_usize(), r.size)
                .expect("initialize global allocator failed");
            break;
        }
    }

    for r in memory_regions() {
        if r.flags.contains(MemRegionFlags::FREE) && r.paddr != max_region_paddr {
            ax_alloc::global_add_memory(phys_to_virt(r.paddr).as_usize(), r.size)
                .expect("add heap memory region failed");
        }
    }
}

#[cfg(feature = "irq")]
fn init_interrupt() {
    init_percpu_irq(ax_hal::percpu::this_cpu_id());

    // Enable IRQs before starting app
    ax_hal::asm::enable_irqs();
}

#[cfg(feature = "irq")]
pub(crate) fn init_percpu_irq(cpu_id: usize) {
    use core::ptr::NonNull;

    fn unit_data() -> NonNull<()> {
        NonNull::dangling()
    }

    ax_hal::irq::cpu_online(cpu_id).expect("failed to mark CPU online for IRQ framework");
    ax_hal::irq::init_common_irq_handler();

    if ax_hal::percpu::this_cpu_is_bsp() {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        ax_hal::irq::request_percpu_irq(
            ax_hal::time::irq_num(),
            cpus,
            timer_irq_handler,
            unit_data(),
        )
        .expect("failed to register timer IRQ handler");

        #[cfg(feature = "ipi")]
        ax_hal::irq::request_percpu_irq(ax_hal::irq::IPI_IRQ, cpus, ipi_irq_handler, unit_data())
            .expect("failed to register IPI IRQ handler");
    }

    update_timer(ax_hal::time::irq_num());
}

#[cfg(all(feature = "irq", feature = "ipi"))]
unsafe fn ax_ipi_run_on_cpu_sync(
    cpu: usize,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ax_hal::irq::IrqError> {
    unsafe { ax_ipi::run_on_cpu_sync_raw(cpu, f, arg) }
}

#[cfg(feature = "irq")]
const PERIODIC_INTERVAL_NANOS: u64 = ax_hal::time::NANOS_PER_SEC / ax_config::TICKS_PER_SEC as u64;

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static NEXT_DEADLINE: u64 = 0;

#[cfg(feature = "irq")]
fn update_timer(_irq_num: usize) {
    let now_ns = ax_hal::time::monotonic_time_nanos();
    // Safety: we have disabled preemption in IRQ handler.
    let mut deadline = unsafe { NEXT_DEADLINE.read_current_raw() };
    if now_ns >= deadline {
        deadline = now_ns + PERIODIC_INTERVAL_NANOS;
    }
    unsafe { NEXT_DEADLINE.write_current_raw(deadline + PERIODIC_INTERVAL_NANOS) };
    ax_hal::time::set_oneshot_timer(deadline);
}

#[cfg(feature = "irq")]
unsafe fn timer_irq_handler(
    ctx: ax_hal::irq::IrqContext,
    _data: core::ptr::NonNull<()>,
) -> ax_hal::irq::IrqReturn {
    update_timer(ctx.irq.0);
    #[cfg(feature = "multitask")]
    ax_task::on_timer_tick();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "irq", feature = "ipi"))]
unsafe fn ipi_irq_handler(
    _ctx: ax_hal::irq::IrqContext,
    _data: core::ptr::NonNull<()>,
) -> ax_hal::irq::IrqReturn {
    ax_ipi::ipi_handler();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "tls", not(feature = "multitask")))]
fn init_tls() {
    let main_tls = ax_hal::tls::TlsArea::alloc();
    unsafe { ax_hal::asm::write_thread_pointer(main_tls.tls_ptr() as usize) };
    core::mem::forget(main_tls);
}
