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
#[cfg(all(
    feature = "stack-protector",
    any(target_os = "none", target_env = "musl"),
    not(test)
))]
mod stack_protector;

#[cfg(feature = "smp")]
mod mp;

mod guard;
#[cfg(feature = "paging")]
mod kernel_mapping;
mod klib;

#[cfg(feature = "irq")]
mod clock_event;
mod devices;
mod fs;
#[cfg(feature = "irq")]
pub mod irq;
mod registers;
#[cfg(feature = "serial")]
pub mod serial;

#[cfg(feature = "multitask")]
pub mod task;

#[cfg(all(feature = "net", feature = "fs"))]
mod unix_ns;

#[cfg(feature = "aic8800-wifi")]
mod wifi_glue;

pub use ax_hal as hal;

pub(crate) mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

/// Maximum logical CPU count represented by runtime-sized CPU masks.
#[cfg(feature = "smp")]
pub const CPU_CAPACITY: usize = build_info::CPU_CAPACITY;

/// A uniprocessor runtime represents only CPU zero.
#[cfg(not(feature = "smp"))]
pub const CPU_CAPACITY: usize = 1;

#[cfg(feature = "smp")]
pub use self::mp::rust_main_secondary;

extern crate alloc;

#[cfg(feature = "fs")]
pub(crate) fn runtime_default_task_stack_size() -> usize {
    build_info::TASK_STACK_SIZE
}

#[cfg(feature = "irq")]
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
    if task::diagnose_current_stack_guard_page_fault(addr) {
        return false;
    }

    ax_mm::kernel_aspace().lock().handle_page_fault(addr, flags)
}

#[ax_crate_interface::impl_interface]
impl ax_log::LogIf for LogIfImpl {
    fn console_write_str(s: &str) {
        #[cfg(feature = "serial")]
        if serial::route_console_bytes(s.as_bytes()).is_some() {
            return;
        }
        ax_hal::console::write_text_bytes(s.as_bytes());
    }

    fn try_write_log_record(record: &str) -> bool {
        #[cfg(feature = "serial")]
        {
            serial::route_console_bytes(record.as_bytes()).is_some()
        }
        #[cfg(not(feature = "serial"))]
        {
            let _ = record;
            false
        }
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
                task::current_thread_id().ok().map(|id| id.as_u64())
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
    ax_hal::percpu::init_primary(cpu_id);
    guard::assert_boot_guards_released();
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

    #[cfg(all(feature = "tls", feature = "multitask"))]
    task::initialize_early_bootstrap_tls().expect("failed to initialize primary bootstrap TLS");
    #[cfg(all(feature = "tls", not(feature = "multitask")))]
    init_tls();

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
    if rdrive::is_initialized() {
        registers::append_linker_registers();
        #[cfg(feature = "irq")]
        ax_hal::irq::init_boot_irqs(cpu_id)
            .unwrap_or_else(|err| panic!("failed to initialize boot IRQs: {err:?}"));
        #[cfg(not(feature = "irq"))]
        rdrive::probe_pre_kernel()
            .unwrap_or_else(|err| panic!("failed to run pre-kernel driver probes: {err:?}"));
    } else {
        warn!("rdrive is not initialized; skip pre-kernel driver probe");
    }

    #[cfg(feature = "multitask")]
    task::initialize_primary(cpu_id).expect("failed to initialize primary task scheduler");

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

    #[cfg(feature = "multitask")]
    let online_cpu =
        task::publish_current_cpu_online().expect("failed to publish primary scheduler CPU");

    #[cfg(all(feature = "irq", feature = "multitask"))]
    enable_irqs_after_scheduler_online(online_cpu);
    #[cfg(all(feature = "irq", not(feature = "multitask")))]
    ax_hal::asm::enable_irqs();
    #[cfg(all(feature = "multitask", not(feature = "irq")))]
    let _ = online_cpu;

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::mark_current_cpu_ready();

    #[cfg(feature = "multitask")]
    task::start_deferred_task_work_service()
        .expect("failed to start deferred scheduler task-work service");

    // Install the ArceOS runtime glue into the OS-independent Wi-Fi driver
    // cores (aic8800 / sdhci-cv1800) *before* probing, since the FDT probe
    // brings the chip up and that needs timing/task capabilities. The cores
    // declare no ArceOS dependency themselves; this is the adapter layer (see
    // `wifi_glue`).
    #[cfg(feature = "aic8800-wifi")]
    wifi_glue::install_runtime();

    devices::probe_all_devices();

    #[cfg(feature = "serial")]
    serial::init(cpu_id);

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

    #[cfg(feature = "net")]
    devices::init_net();

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

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::wait_for_all_cpus_ready();

    #[cfg(all(feature = "smp", feature = "ipi"))]
    fs::online_smp();

    ax_app_entry();

    #[cfg(feature = "multitask")]
    task::exit_current(0);
    #[cfg(not(feature = "multitask"))]
    {
        debug!("main task exited: exit_code={}", 0);
        #[cfg(feature = "irq")]
        take_current_clock_event_offline();
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
}

#[cfg(feature = "irq")]
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

#[cfg(all(feature = "irq", feature = "ipi"))]
unsafe fn ax_ipi_run_on_cpu_sync(
    cpu: usize,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ax_hal::irq::IrqError> {
    unsafe { ax_ipi::run_on_cpu_sync_raw(cpu, f, arg) }
}

#[cfg(feature = "irq")]
fn periodic_interval_nanos() -> u64 {
    (ax_hal::time::NANOS_PER_SEC / ticks_per_sec()).max(1)
}

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static LOCAL_CLOCK_EVENT: clock_event::LocalClockEvent = clock_event::LocalClockEvent::offline();

#[cfg(feature = "irq")]
fn with_local_clock_event_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut clock_event::LocalClockEvent) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable clockevent access requires local IRQ exclusion"
    );
    // SAFETY: every caller is either offline initialization or the local timer
    // IRQ/scheduler path with IRQs disabled. The clockevent has no remote
    // mutable endpoint, so this excludes every conflicting access.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                LOCAL_CLOCK_EVENT.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("clockevent CPU-local state is invalid: {error}"))
}

#[cfg(feature = "irq")]
fn apply_clock_event_action(action: clock_event::ClockEventAction) {
    match action {
        clock_event::ClockEventAction::None => {}
        clock_event::ClockEventAction::Stop => ax_hal::time::cancel_oneshot_timer(),
        clock_event::ClockEventAction::Program(deadline) => {
            ax_hal::time::set_oneshot_timer(deadline.as_nanos());
        }
    }
}

#[cfg(feature = "irq")]
pub(crate) fn take_current_clock_event_offline() {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || {
            (
                (),
                with_local_clock_event_mut(clock_event::LocalClockEvent::take_offline),
            )
        },
        apply_clock_event_action,
    );
}

#[cfg(feature = "irq")]
fn run_clock_event_transaction<R, Action, Guard>(
    acquire_irq: impl FnOnce() -> Guard,
    access: impl FnOnce() -> (R, Action),
    apply: impl FnOnce(Action),
) -> R {
    run_clock_event_irq_scope(acquire_irq, || {
        let (result, action) = access();
        apply(action);
        result
    })
}

#[cfg(feature = "irq")]
fn run_clock_event_irq_scope<R, Guard>(
    acquire_irq: impl FnOnce() -> Guard,
    service: impl FnOnce() -> R,
) -> R {
    // One IRQ-save must cover clockevent state transitions, bounded task work,
    // and the physical commit so an IRQ cannot observe a split transaction.
    let irq_guard = acquire_irq();
    let result = service();
    drop(irq_guard);
    result
}

#[cfg(all(feature = "irq", feature = "multitask"))]
fn commit_local_clock_event<R>(
    operation: impl for<'value> FnOnce(
        &'value mut clock_event::LocalClockEvent,
    ) -> (R, clock_event::ClockEventAction),
) -> R {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || with_local_clock_event_mut(operation),
        apply_clock_event_action,
    )
}

#[cfg(all(feature = "irq", feature = "multitask"))]
fn enable_irqs_after_scheduler_online(_online: task::PublishedCpuOnline) {
    ax_hal::asm::enable_irqs();
}

#[cfg(feature = "irq")]
struct ClockEventFiringGuard {
    active: bool,
}

#[cfg(feature = "irq")]
impl ClockEventFiringGuard {
    fn begin(now_ns: u64) -> Self {
        with_local_clock_event_mut(|clockevent| {
            clockevent.begin_firing();
            clockevent.advance_periodic(now_ns, periodic_interval_nanos());
        });
        Self { active: true }
    }

    #[cfg(feature = "multitask")]
    fn begin_if_due(now_ns: u64) -> Option<Self> {
        let active = with_local_clock_event_mut(|clockevent| {
            if !clockevent.begin_firing_if_due(now_ns) {
                return false;
            }
            clockevent.advance_periodic(now_ns, periodic_interval_nanos());
            true
        });
        active.then_some(Self { active: true })
    }

    fn finish(
        mut self,
        #[cfg(feature = "multitask")] task_update: Option<ax_task::runtime::TaskDeadlineUpdate>,
    ) {
        let action = with_local_clock_event_mut(|clockevent| {
            #[cfg(feature = "multitask")]
            if let Some(update) = task_update {
                let _ = clockevent.publish_task(
                    update.generation(),
                    update
                        .deadline()
                        .map(ax_task::runtime::MonotonicDeadline::as_nanos),
                    update.deferred_work(),
                );
            }
            clockevent.finish_firing()
        });
        self.active = false;
        apply_clock_event_action(action);
    }
}

#[cfg(feature = "irq")]
impl Drop for ClockEventFiringGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let action = with_local_clock_event_mut(clock_event::LocalClockEvent::recover_firing);
        apply_clock_event_action(action);
    }
}

#[cfg(all(feature = "irq", feature = "multitask"))]
fn local_clock_event_has_immediate_work(now_ns: u64) -> bool {
    commit_local_clock_event(|clockevent| {
        (
            clockevent.has_immediate_work(now_ns),
            clock_event::ClockEventAction::None,
        )
    })
}

#[cfg(all(feature = "irq", feature = "multitask"))]
fn recover_overdue_local_clock_event(now_ns: u64) -> bool {
    let Some(firing) = ClockEventFiringGuard::begin_if_due(now_ns) else {
        return false;
    };
    let task_update = task::recover_clock_event(now_ns);
    firing.finish(task_update);
    true
}

#[cfg(all(feature = "irq", feature = "multitask"))]
fn publish_local_task_deadline(
    update: ax_task::runtime::TaskDeadlineUpdate,
) -> ax_task::runtime::RuntimeStatus {
    commit_local_clock_event(|clockevent| {
        (
            (),
            clockevent.publish_task(
                update.generation(),
                update
                    .deadline()
                    .map(ax_task::runtime::MonotonicDeadline::as_nanos),
                update.deferred_work(),
            ),
        )
    });
    ax_task::runtime::RuntimeStatus::Success
}

#[cfg(feature = "irq")]
fn init_timer() {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || {
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let periodic = initial_periodic_deadline(now_ns, periodic_interval_nanos());
            let action = with_local_clock_event_mut(|clockevent| clockevent.online(periodic));
            ((), action)
        },
        apply_clock_event_action,
    );
}

#[cfg(any(feature = "irq", test))]
const fn initial_periodic_deadline(
    now_ns: u64,
    interval_ns: u64,
) -> Option<clock_event::ClockDeadline> {
    match now_ns.checked_add(interval_ns) {
        Some(deadline_ns) => clock_event::ClockDeadline::from_nanos(deadline_ns),
        None => None,
    }
}

#[cfg(any(feature = "irq", test))]
const fn next_periodic_deadline(deadline_ns: u64, now_ns: u64, interval_ns: u64) -> Option<u64> {
    if now_ns == u64::MAX {
        return None;
    }
    if deadline_ns > now_ns {
        return Some(deadline_ns);
    }

    let interval_ns = if interval_ns == 0 { 1 } else { interval_ns };
    let elapsed_ns = (now_ns - deadline_ns) as u128;
    let interval_ns = interval_ns as u128;
    let periods = elapsed_ns / interval_ns + 1;
    let next = deadline_ns as u128 + periods * interval_ns;
    if next >= u64::MAX as u128 {
        None
    } else {
        Some(next as u64)
    }
}

#[cfg(any(feature = "multitask", test))]
pub(crate) const fn timer_resolution_from_frequency(frequency_hz: u64) -> u64 {
    if frequency_hz == 0 {
        return ax_hal::time::NANOS_PER_SEC;
    }
    let nanos_per_second = ax_hal::time::NANOS_PER_SEC as u128;
    let frequency_hz = frequency_hz as u128;
    let resolution_ns = nanos_per_second.div_ceil(frequency_hz);
    if resolution_ns == 0 {
        1
    } else {
        resolution_ns as u64
    }
}

#[cfg(feature = "irq")]
fn timer_irq_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    run_clock_event_irq_scope(ax_kernel_guard::IrqSave::new, || {
        let _ = ctx;
        let now_ns = ax_hal::time::monotonic_time_nanos();
        let firing = ClockEventFiringGuard::begin(now_ns);
        #[cfg(feature = "multitask")]
        let task_update = task::on_clock_event(now_ns);
        firing.finish(
            #[cfg(feature = "multitask")]
            task_update,
        );
        ax_hal::irq::IrqReturn::Handled
    })
}

#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
fn dispatch_shared_ipi(
    drain_callbacks: impl FnOnce(),
    consume_scheduler_delivery: impl FnOnce() -> bool,
    acknowledge_scheduler_delivery: impl FnOnce(),
) {
    if consume_scheduler_delivery() {
        acknowledge_scheduler_delivery();
    }
    drain_callbacks();
}

#[cfg(all(feature = "irq", feature = "ipi"))]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    dispatch_shared_ipi(
        ax_ipi::ipi_handler,
        || {
            #[cfg(feature = "multitask")]
            {
                task::consume_scheduler_ipi_doorbell()
            }
            #[cfg(not(feature = "multitask"))]
            {
                false
            }
        },
        || {
            #[cfg(feature = "multitask")]
            task::on_scheduler_ipi();
        },
    );
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "irq", feature = "wake-ipi", not(feature = "ipi")))]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    dispatch_shared_ipi(
        || {},
        || {
            #[cfg(feature = "multitask")]
            {
                task::consume_scheduler_ipi_doorbell()
            }
            #[cfg(not(feature = "multitask"))]
            {
                false
            }
        },
        || {
            #[cfg(feature = "multitask")]
            task::on_scheduler_ipi();
        },
    );
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "tls", not(feature = "multitask")))]
fn init_tls() {
    let main_tls = ax_hal::tls::TlsArea::alloc();
    let kernel_tls = ax_hal::context::KernelTlsBase::new(main_tls.tls_ptr() as usize);
    unsafe { ax_hal::asm::write_thread_pointer(kernel_tls) };
    core::mem::forget(main_tls);
}

#[cfg(test)]
mod tests {
    use core::cell::{Cell, RefCell};

    #[cfg(feature = "irq")]
    struct TestIrqGuard<'state> {
        irq_enabled: &'state Cell<bool>,
        restore_enabled: bool,
    }

    #[cfg(feature = "irq")]
    impl Drop for TestIrqGuard<'_> {
        fn drop(&mut self) {
            self.irq_enabled.set(self.restore_enabled);
        }
    }

    #[cfg(feature = "irq")]
    #[test]
    fn clockevent_transaction_holds_irq_exclusion_through_hardware_commit() {
        let irq_enabled = Cell::new(true);
        let hardware_committed = Cell::new(false);
        let deadline = crate::clock_event::ClockDeadline::from_nanos(100).unwrap();
        let mut clockevent = crate::clock_event::LocalClockEvent::offline();

        let result = super::run_clock_event_transaction(
            || {
                let restore_enabled = irq_enabled.replace(false);
                TestIrqGuard {
                    irq_enabled: &irq_enabled,
                    restore_enabled,
                }
            },
            || {
                assert!(
                    !irq_enabled.get(),
                    "clockevent state mutation requires local IRQ exclusion"
                );
                (7, clockevent.online(Some(deadline)))
            },
            |action| {
                assert!(
                    !irq_enabled.get(),
                    "clockevent hardware commit requires the same IRQ exclusion window"
                );
                assert_eq!(
                    action,
                    crate::clock_event::ClockEventAction::Program(deadline)
                );
                hardware_committed.set(true);
            },
        );

        assert_eq!(result, 7);
        assert_eq!(
            clockevent.phase(),
            crate::clock_event::ClockEventPhase::Armed
        );
        assert_eq!(clockevent.armed_deadline(), Some(deadline));
        assert!(hardware_committed.get());
        assert!(irq_enabled.get(), "the caller's IRQ state must be restored");
    }

    #[cfg(feature = "irq")]
    #[test]
    fn timer_irq_scope_establishes_local_irq_exclusion() {
        let irq_enabled = Cell::new(true);

        let handled = super::run_clock_event_irq_scope(
            || {
                let restore_enabled = irq_enabled.replace(false);
                TestIrqGuard {
                    irq_enabled: &irq_enabled,
                    restore_enabled,
                }
            },
            || {
                assert!(
                    !irq_enabled.get(),
                    "timer IRQ service must establish its own local IRQ exclusion"
                );
                true
            },
        );

        assert!(handled);
        assert!(irq_enabled.get(), "the caller's IRQ state must be restored");
    }

    #[test]
    fn fs_init_accepts_bootargs_without_fs_feature() {
        crate::fs::init(Some("root=/dev/nvme0n1"));
    }

    #[test]
    fn shared_ipi_dispatch_consumes_scheduler_delivery_before_callback_drain() {
        let events = RefCell::new(alloc::vec::Vec::new());

        super::dispatch_shared_ipi(
            || events.borrow_mut().push("callbacks"),
            || {
                events.borrow_mut().push("consume");
                true
            },
            || events.borrow_mut().push("acknowledge"),
        );

        assert_eq!(*events.borrow(), ["consume", "acknowledge", "callbacks"]);
    }

    #[test]
    fn shared_ipi_callback_can_publish_a_fresh_scheduler_epoch() {
        let scheduler_epoch_claimed = Cell::new(true);

        super::dispatch_shared_ipi(
            || {
                assert!(
                    !scheduler_epoch_claimed.get(),
                    "the delivered scheduler epoch must be released at IPI entry"
                );
                scheduler_epoch_claimed.set(true);
            },
            || true,
            || scheduler_epoch_claimed.set(false),
        );

        assert!(
            scheduler_epoch_claimed.get(),
            "a scheduler delivery published during callback drain must remain pending"
        );
    }

    #[test]
    fn unrelated_shared_ipi_does_not_acknowledge_scheduler_delivery() {
        let events = RefCell::new(alloc::vec::Vec::new());

        super::dispatch_shared_ipi(
            || events.borrow_mut().push("callbacks"),
            || {
                events.borrow_mut().push("consume");
                false
            },
            || events.borrow_mut().push("acknowledge"),
        );

        assert_eq!(*events.borrow(), ["consume", "callbacks"]);
    }

    #[test]
    fn periodic_deadline_catches_up_without_accumulating_drift() {
        assert_eq!(super::next_periodic_deadline(100, 100, 25), Some(125));
        assert_eq!(super::next_periodic_deadline(100, 149, 25), Some(150));
        assert_eq!(super::next_periodic_deadline(100, 150, 25), Some(175));
    }

    #[test]
    fn initial_periodic_deadline_becomes_idle_at_the_monotonic_limit() {
        assert_eq!(super::initial_periodic_deadline(u64::MAX - 1, 2), None);
        assert_eq!(super::initial_periodic_deadline(u64::MAX - 1, 1), None);
    }

    #[test]
    fn periodic_deadline_saturates_at_the_monotonic_limit() {
        assert_eq!(
            super::next_periodic_deadline(u64::MAX - 5, u64::MAX - 1, 10),
            None
        );
        assert_eq!(
            super::next_periodic_deadline(u64::MAX - 5, u64::MAX, 10),
            None
        );
    }
}
