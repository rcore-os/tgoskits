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
//! - `smp`: Enable SMP (symmetric multiprocessing) support.
//! - `fs`: Enable filesystem support.
//! - `net`: Enable networking support.
//! - `display`: Enable graphics support.
//!
//! Interrupt handling and multi-task scheduling are mandatory runtime
//! capabilities. The listed features are optional and disabled by default.

#![cfg_attr(not(test), no_std)]
#![allow(missing_abi)]

#[macro_use]
extern crate ax_log;

extern crate ax_driver as _;

#[cfg(all(target_os = "none", not(feature = "std-compat"), not(test)))]
mod lang_items;
#[cfg(all(
    feature = "stack-protector",
    any(target_os = "none", target_env = "musl"),
    not(test)
))]
mod stack_protector;

#[cfg(feature = "smp")]
mod mp;

mod clock_event;
#[cfg(feature = "paging")]
mod kernel_mapping;
mod klib;
mod preempt;
mod raw_console;
mod structured_log;

pub mod console;
mod devices;
pub mod emergency_console;
mod error;
mod fs;
pub mod irq;
mod registers;
pub mod serial;
pub mod sync;

#[cfg(all(feature = "net", feature = "fs"))]
mod unix_ns;

pub use ax_hal as hal;
pub use error::{RuntimeError, RuntimeResult};

/// Drains task-console output before shutting down the whole system.
///
/// Fatal paths must bypass this task-context transaction and use the
/// emergency console plus [`ax_hal::power::system_off`] directly.
pub fn terminate() -> ! {
    if let Ok(output) = console::output() {
        let _ = output.drain();
    }
    ax_hal::power::system_off()
}

pub(crate) mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

#[cfg(feature = "smp")]
pub use self::mp::rust_main_secondary;

extern crate alloc;

#[cfg(feature = "fs")]
pub(crate) fn runtime_default_task_stack_size() -> usize {
    build_info::TASK_STACK_SIZE
}

fn ticks_per_sec() -> u64 {
    build_info::TICKS_PER_SEC as u64
}

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

fn ax_app_entry() {
    #[cfg(all(feature = "std-compat", not(test)))]
    {
        unsafe extern "C" {
            safe fn __axstd_std_check_entry();
        }
        __axstd_std_check_entry();
    }

    #[cfg(all(not(feature = "std-compat"), not(test)))]
    {
        unsafe extern "C" {
            /// Legacy application's entry point.
            safe fn main();
        }
        main();
    }
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
    fn try_publish(
        meta: ax_log::RecordMeta,
        args: core::fmt::Arguments<'_>,
    ) -> ax_log::PublishStatus {
        if let Some(status) = serial::try_publish_record(meta, args) {
            return status;
        }
        let context = structured_log::with_runtime_log_context(core::convert::identity)
            .unwrap_or_else(|_| structured_log::fallback_runtime_log_context(meta));
        if let Some(status) = console::try_publish_without_runtime(meta, context, args) {
            return status;
        }
        let mut writer = PlatformConsoleWriter::default();
        if structured_log::write_record(&mut writer, meta, context, args).is_ok() {
            ax_log::PublishStatus::Published
        } else {
            ax_log::PublishStatus::Dropped
        }
    }

    fn emergency_write(args: core::fmt::Arguments<'_>) -> usize {
        emergency_console::write_fmt(args)
    }
}

#[derive(Default)]
struct PlatformConsoleWriter {
    written: usize,
}

impl core::fmt::Write for PlatformConsoleWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        ax_hal::console::write_text_bytes(text.as_bytes());
        self.written = self.written.saturating_add(text.len());
        Ok(())
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
        build_info::ARCH,
        hal::platform_name(),
        build_info::TARGET,
        build_info::MODE,
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

    let virtual_address_space = ax_hal::mem::virtual_address_space()
        .unwrap_or_else(|error| panic!("unsupported platform virtual-address layout: {error}"));
    let user_space = virtual_address_space.user();
    let kernel_space = virtual_address_space.kernel();
    let kernel_space_start = kernel_space.start;
    let kernel_space_size = kernel_space.size();

    info!(
        "virtual address layout: user [{:#x}, {:#x}), kernel [{:#x}, {:#x})",
        user_space.start.as_usize(),
        user_space.end.as_usize(),
        kernel_space.start.as_usize(),
        kernel_space.end.as_usize(),
    );

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
    if rdrive::is_initialized() {
        registers::append_linker_registers();
        ax_hal::irq::init_boot_irqs(cpu_id)
            .unwrap_or_else(|err| panic!("failed to initialize boot IRQs: {err:?}"));
    } else {
        warn!("rdrive is not initialized; skip pre-kernel driver probe");
    }

    ax_task::init_scheduler();
    preempt::release_bootstrap();

    #[cfg(feature = "ipi")]
    {
        ax_ipi::init();
        ax_hal::irq::set_run_on_cpu_sync(ax_ipi_run_on_cpu_sync);
    }

    info!("Initialize interrupt handlers...");
    init_interrupt();

    devices::probe_all_devices();

    serial::init(cpu_id);

    match console::activate_before_smp() {
        console::ConsoleActivation::Active {
            runtime_index,
            tty_number,
        } => info!("runtime console active: serial{runtime_index}, ttyS{tty_number}"),
        console::ConsoleActivation::RawHal(reason) => {
            info!("no runtime console selected; keeping the HAL console: {reason:?}")
        }
        console::ConsoleActivation::FailedClosed(reason) => {
            warn!("runtime console unavailable; early console failed closed: {reason:?}")
        }
    }

    #[cfg(feature = "rtc")]
    ax_println!(
        "Boot at {}\n",
        chrono::DateTime::from_timestamp_nanos(ax_hal::time::wall_time_nanos() as _),
    );

    fs::init(ax_hal::boot::bootargs());

    #[cfg(feature = "display")]
    devices::init_display();

    #[cfg(feature = "input")]
    devices::init_input();

    #[cfg(feature = "vsock")]
    devices::init_vsock();

    #[cfg(feature = "smp")]
    self::mp::start_secondary_cpus(cpu_id);

    ax_ctor_bare::call_ctors();

    info!("Primary CPU {cpu_id} init OK.");
    INITED_CPUS.fetch_add(1, Ordering::Release);

    while !is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(feature = "ipi")]
    ax_ipi::wait_for_all_cpus_ready();

    #[cfg(all(feature = "smp", feature = "ipi"))]
    fs::online_smp();

    // Queue-level network IRQ ownership is selected from the complete online
    // CPU set.  Every target scheduler, IRQ CPU state, and synchronous IPI
    // route must therefore be ready before fixed-affinity workers handshake
    // and physical IRQ actions are registered.
    #[cfg(feature = "net")]
    devices::init_net();

    ax_app_entry();
    terminate();
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

fn init_interrupt() {
    init_percpu_irq(ax_hal::percpu::this_cpu_id());

    #[cfg(feature = "paging")]
    let tlb_preparation = ax_hal::cache::prepare_current_cpu_tlb()
        .expect("primary CPU failed to prepare TLB capability");

    // Enable IRQs before starting app
    ax_hal::asm::enable_irqs();

    #[cfg(feature = "ipi")]
    ax_ipi::mark_current_cpu_ready();

    #[cfg(feature = "paging")]
    ax_hal::cache::publish_current_cpu_tlb_ready(tlb_preparation)
        .expect("primary CPU failed to publish TLB readiness");
}

pub(crate) fn init_percpu_irq(cpu_id: usize) {
    ax_hal::irq::cpu_online(cpu_id).expect("failed to mark CPU online for IRQ framework");
    ax_hal::irq::init_common_irq_handler();

    if ax_hal::percpu::this_cpu_is_bsp() {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        ax_hal::irq::request_percpu_irq(ax_hal::time::irq_num(), cpus, timer_irq_handler)
            .expect("failed to register timer IRQ handler");

        #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
        ax_hal::irq::request_percpu_irq(ax_hal::irq::ipi_irq(), cpus, ipi_irq_handler)
            .expect("failed to register IPI IRQ handler");
    }

    init_timer();
}

#[cfg(feature = "ipi")]
unsafe fn ax_ipi_run_on_cpu_sync(
    cpu: usize,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ax_hal::irq::IrqError> {
    unsafe { ax_ipi::call_on_cpu(ax_hal::irq::CpuId(cpu), f, arg) }
}

fn periodic_interval_nanos() -> u64 {
    ax_hal::time::NANOS_PER_SEC / ticks_per_sec()
}

#[ax_percpu::def_percpu]
static NEXT_PERIODIC_DEADLINE_NANOS: u64 = 0;

#[ax_percpu::def_percpu]
static LOCAL_CLOCK_EVENT: clock_event::LocalClockEvent = clock_event::LocalClockEvent::offline();

fn with_periodic_deadline<R>(
    operation: impl for<'scope> FnOnce(&ax_percpu::CpuPin<'scope>) -> R,
) -> R {
    // SAFETY: every caller runs either during offline CPU initialization or in
    // the local timer IRQ path. Both contexts prevent migration for the whole
    // callback, and the CPU-local area was installed before runtime entry.
    unsafe { ax_percpu::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("timer CPU-local state is invalid: {error}"))
}

fn with_local_clock_event<R>(
    operation: impl for<'exclusive> FnOnce(&ax_percpu::ExclusiveCpu<'exclusive>) -> R,
) -> R {
    // SAFETY: callers exclude migration and local IRQ re-entry for the whole
    // transaction. The per-CPU area is installed before runtime entry.
    unsafe { ax_percpu::with_cpu_pin(|pin| ax_percpu::with_exclusive_cpu(pin, operation)) }
        .unwrap_or_else(|error| panic!("clockevent CPU-local state is invalid: {error}"))
}

fn commit_clock_event_action(action: clock_event::ClockEventAction) {
    if let clock_event::ClockEventAction::Program(deadline) = action {
        ax_hal::time::set_oneshot_timer(deadline);
    }
}

fn init_timer() {
    ax_task::init_timer_service();
    let now_ns = ax_hal::time::monotonic_time_nanos();
    with_periodic_deadline(|pin| {
        NEXT_PERIODIC_DEADLINE_NANOS
            .write_current(pin, now_ns.saturating_add(periodic_interval_nanos()));
    });
    let deadline = next_timer_deadline();
    let action = with_local_clock_event(|exclusive| {
        LOCAL_CLOCK_EVENT.with_current_mut(exclusive, |event| event.online(deadline))
    });
    commit_clock_event_action(action);
    ax_hal::time::enable_timer_irq();
}

fn advance_periodic_timer(now_ns: u64) -> bool {
    let mut deadline = with_periodic_deadline(|pin| NEXT_PERIODIC_DEADLINE_NANOS.read_current(pin));
    if deadline == 0 {
        with_periodic_deadline(|pin| {
            NEXT_PERIODIC_DEADLINE_NANOS
                .write_current(pin, now_ns.saturating_add(periodic_interval_nanos()));
        });
        return false;
    }
    if now_ns < deadline {
        return false;
    }

    while deadline <= now_ns {
        deadline = deadline.saturating_add(periodic_interval_nanos());
        if deadline == u64::MAX {
            break;
        }
    }
    with_periodic_deadline(|pin| NEXT_PERIODIC_DEADLINE_NANOS.write_current(pin, deadline));
    true
}

fn select_timer_deadline(
    periodic_deadline_nanos: u64,
    task_deadline_nanos: Option<u64>,
    now_nanos: u64,
    periodic_interval_nanos: u64,
) -> (u64, u64) {
    debug_assert_ne!(periodic_interval_nanos, 0);
    let periodic_deadline_nanos = if periodic_deadline_nanos <= now_nanos {
        let elapsed_intervals = (now_nanos - periodic_deadline_nanos) / periodic_interval_nanos;
        periodic_deadline_nanos.saturating_add(
            periodic_interval_nanos.saturating_mul(elapsed_intervals.saturating_add(1)),
        )
    } else {
        periodic_deadline_nanos
    };
    // A still-expired logical deadline means the bounded IRQ pass left work
    // behind. Publish a fresh edge so the hardware backend can apply its
    // minimum delta and continue draining without waiting for the next tick.
    let selected_deadline_nanos = task_deadline_nanos.map_or(periodic_deadline_nanos, |deadline| {
        let deadline = if deadline <= now_nanos {
            now_nanos.saturating_add(1)
        } else {
            deadline
        };
        core::cmp::min(periodic_deadline_nanos, deadline)
    });
    (periodic_deadline_nanos, selected_deadline_nanos)
}

fn next_timer_deadline() -> u64 {
    let mut periodic_deadline =
        with_periodic_deadline(|pin| NEXT_PERIODIC_DEADLINE_NANOS.read_current(pin));
    if periodic_deadline == 0 {
        let now_ns = ax_hal::time::monotonic_time_nanos();
        periodic_deadline = now_ns.saturating_add(periodic_interval_nanos());
        with_periodic_deadline(|pin| {
            NEXT_PERIODIC_DEADLINE_NANOS.write_current(pin, periodic_deadline)
        });
    }
    let task_deadline = ax_task::next_timer_deadline_nanos();
    let now_nanos = ax_hal::time::monotonic_time_nanos();
    let (next_periodic_deadline, deadline) = select_timer_deadline(
        periodic_deadline,
        task_deadline,
        now_nanos,
        periodic_interval_nanos(),
    );
    if next_periodic_deadline != periodic_deadline {
        // Timer callbacks and scheduler work can outlive the periodic deadline
        // selected at IRQ entry. Coalesce those ticks before rearming so the
        // hardware comparator is not programmed with an already elapsed value.
        with_periodic_deadline(|pin| {
            NEXT_PERIODIC_DEADLINE_NANOS.write_current(pin, next_periodic_deadline)
        });
    }

    deadline
}

struct ClockEventControlImpl;

#[ax_crate_interface::impl_interface]
impl ax_task::ClockEventControl for ClockEventControlImpl {
    fn request_local_reprogram(deadline_nanos: u64) {
        let _guard = ax_task::sync::PreemptIrqSaveGuard::new();
        let action = with_local_clock_event(|exclusive| {
            LOCAL_CLOCK_EVENT
                .with_current_mut(exclusive, |event| event.request_earlier(deadline_nanos))
        });
        commit_clock_event_action(action);
    }
}

fn timer_irq_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    let _ = ctx;
    let token = with_local_clock_event(|exclusive| {
        LOCAL_CLOCK_EVENT.with_current_mut(exclusive, |event| event.claim_irq())
    });
    // SAFETY: the local timer IRQ excludes migration and nested local
    // scheduler-clock publication for this complete stamp.
    unsafe { ax_hal::time::scheduler_clock_tick() }
        .expect("current CPU scheduler clock must be online before timer IRQs");
    let scheduler_tick = advance_periodic_timer(ax_hal::time::monotonic_time_nanos());
    ax_task::on_timer_irq(scheduler_tick);
    let deadline = next_timer_deadline();
    let action = with_local_clock_event(|exclusive| {
        LOCAL_CLOCK_EVENT.with_current_mut(exclusive, |event| match token {
            Some(token) => event.finish_irq(token, Some(deadline)),
            None => event.request_earlier(deadline),
        })
    });
    trace!(
        "clockevent IRQ CPU {}: token={token:?}, scheduler_tick={}, next_deadline={}, \
         action={action:?}",
        ax_hal::percpu::this_cpu_id(),
        scheduler_tick,
        deadline
    );
    commit_clock_event_action(action);
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(feature = "ipi")]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    ax_ipi::claim_current_delivery();
    #[cfg(feature = "smp")]
    ax_task::handle_ipi_reschedule();
    ax_ipi::drain_hard_calls()
        .unwrap_or_else(|error| panic!("failed to continue hard-call draining: {error:?}"));
    ax_ipi::legacy::drain_current_callbacks();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "wake-ipi", not(feature = "ipi")))]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(test)]
mod tests {
    #[test]
    fn timer_programming_catches_up_after_a_slow_irq() {
        let (periodic, selected) = super::select_timer_deadline(100, None, 150, 10);
        assert_eq!(periodic, 160);
        assert_eq!(selected, 160);
    }

    #[test]
    fn timer_programming_keeps_an_earlier_task_deadline() {
        let (periodic, selected) = super::select_timer_deadline(100, Some(155), 150, 10);
        assert_eq!(periodic, 160);
        assert_eq!(selected, 155);
    }

    #[test]
    fn timer_programming_advances_an_expired_budget_limited_deadline() {
        let (periodic, selected) = super::select_timer_deadline(100, Some(1), 150, 10);
        assert_eq!(periodic, 160);
        assert_eq!(selected, 151);
    }

    #[test]
    fn fs_init_accepts_bootargs_without_fs_feature() {
        crate::fs::init(Some("root=/dev/nvme0n1"));
    }
}
