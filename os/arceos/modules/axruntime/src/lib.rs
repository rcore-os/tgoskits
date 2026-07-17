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
mod klib;

pub mod console;

#[cfg(feature = "block")]
pub mod block;
mod devices;
mod fs;
#[cfg(feature = "irq")]
pub mod irq;
mod registers;

pub mod workqueue;

#[cfg(feature = "multitask")]
pub mod task;

#[cfg(all(feature = "net", feature = "fs"))]
mod unix_ns;

#[cfg(feature = "aic8800-wifi")]
mod wifi_glue;

pub use ax_hal as hal;

fn current_backtrace_stack_bounds() -> Option<axbacktrace::StackBounds> {
    #[cfg(feature = "multitask")]
    if let Some(bounds) = task::current_kernel_stack_bounds() {
        return Some(bounds);
    }

    #[cfg(feature = "smp")]
    let cpu = ax_hal::percpu::this_cpu_id();
    #[cfg(not(feature = "smp"))]
    let cpu = 0;
    let (base, size) = ax_hal::mem::boot_stack_bounds(cpu);
    let start = base.as_usize();
    let end = start.checked_add(size)?;
    (start < end).then(|| axbacktrace::StackBounds::new(start, end))
}

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
    ax_mm::kernel_aspace().lock().handle_page_fault(addr, flags)
}

#[ax_crate_interface::impl_interface]
impl ax_log::LogIf for LogIfImpl {
    fn console_write_str(s: &str) {
        console::write_text_bytes(s.as_bytes());
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
                ax_task::current_thread_id().ok().map(|id| id.as_u64())
            }
            #[cfg(not(feature = "multitask"))]
            None
        } else {
            None
        }
    }
}

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// CPUs whose scheduler and local IRQ paths are ready for blocking services.
static CPU_RUNTIME_ONLINE: [AtomicBool; CPU_CAPACITY] =
    [const { AtomicBool::new(false) }; CPU_CAPACITY];
static ONLINE_RUNTIME_CPUS: AtomicUsize = AtomicUsize::new(0);

/// Global device/filesystem initialization boundary observed by applications.
static SYSTEM_READY: AtomicBool = AtomicBool::new(false);

#[cfg(any(feature = "smp", feature = "workqueue", test))]
const fn configured_runtime_cpu_count(
    discovered_cpus: usize,
    cpu_capacity: usize,
    smp_enabled: bool,
) -> usize {
    assert!(cpu_capacity > 0, "runtime CPU capacity must be non-zero");
    if smp_enabled {
        let bounded = if discovered_cpus < cpu_capacity {
            discovered_cpus
        } else {
            cpu_capacity
        };
        if bounded == 0 { 1 } else { bounded }
    } else {
        1
    }
}

#[cfg(any(feature = "smp", feature = "workqueue"))]
pub(crate) fn runtime_cpu_count() -> usize {
    configured_runtime_cpu_count(ax_hal::cpu_num(), CPU_CAPACITY, cfg!(feature = "smp"))
}

#[cfg(feature = "fs")]
/// Returns whether one CPU has scheduler and local IRQ service available.
pub fn cpu_runtime_online(cpu: usize) -> bool {
    CPU_RUNTIME_ONLINE
        .get(cpu)
        .is_some_and(|online| online.load(Ordering::Acquire))
}

fn mark_current_cpu_runtime_online(cpu: usize) {
    let online = CPU_RUNTIME_ONLINE
        .get(cpu)
        .unwrap_or_else(|| panic!("runtime CPU {cpu} exceeds capacity {CPU_CAPACITY}"));
    assert!(
        !online.swap(true, Ordering::AcqRel),
        "runtime CPU {cpu} published online twice"
    );
    ONLINE_RUNTIME_CPUS.fetch_add(1, Ordering::Release);
}

#[cfg(feature = "fs")]
/// Returns whether the caller may enter a scheduler-backed blocking service.
pub fn current_cpu_can_block() -> bool {
    let cpu = ax_hal::percpu::this_cpu_id();
    cpu_runtime_online(cpu) && !guard::in_atomic_context() && task::current_thread_id().is_ok()
}

fn is_init_ok() -> bool {
    SYSTEM_READY.load(Ordering::Acquire)
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

        // SAFETY: the provider returns either the current RuntimeStack's exact
        // usable allocation or this CPU's exact someboot stack. Both remain
        // mapped for the synchronous walk and exclude guard pages, unrelated
        // kernel VA holes, and user-controlled ranges.
        unsafe {
            axbacktrace::init_with_stack_provider(
                Range {
                    start: _stext.as_ptr() as usize,
                    end: _etext.as_ptr() as usize,
                },
                current_backtrace_stack_bounds,
            )
        };
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
        // SAFETY: ax-ipi's synchronous lifecycle either completes the thunk or
        // cancels it before returning and never retains the raw argument. The
        // immutable hook is installed after the local queue exists and before
        // interrupt handlers or online scheduler CPUs expose it to consumers.
        unsafe {
            ax_hal::irq::set_run_on_cpu_sync(ax_ipi_run_on_cpu_sync)
        };
    }

    #[cfg(feature = "irq")]
    {
        info!("Initialize interrupt handlers...");
        init_interrupt();
    }

    #[cfg(feature = "multitask")]
    task::publish_current_cpu_online().expect("failed to publish primary scheduler CPU");

    #[cfg(feature = "multitask")]
    task::start_deferred_task_work_service()
        .expect("failed to start deferred scheduler task-work service");

    mark_current_cpu_runtime_online(cpu_id);

    #[cfg(feature = "smp")]
    self::mp::start_secondary_cpus(cpu_id);

    #[cfg(feature = "workqueue")]
    workqueue::initialize().expect("failed to initialize shared per-CPU worker pools");

    // Install the ArceOS runtime glue into the OS-independent Wi-Fi driver
    // cores (aic8800 / sdhci-cv1800) *before* probing, since the FDT probe
    // brings the chip up and that needs timing/task capabilities. The cores
    // declare no ArceOS dependency themselves; this is the adapter layer (see
    // `wifi_glue`).
    #[cfg(feature = "aic8800-wifi")]
    wifi_glue::install_runtime();

    devices::probe_all_devices();

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

    ax_ctor_bare::call_ctors();

    info!("Primary CPU {cpu_id} init OK.");
    SYSTEM_READY.store(true, Ordering::Release);

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::wait_for_all_cpus_ready();

    ax_app_entry();

    #[cfg(feature = "multitask")]
    task::exit_current(0);
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

    #[cfg(feature = "ipi")]
    ax_ipi::mark_current_cpu_ready();
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
    unsafe { ax_ipi::run_on_cpu_sync_raw(ax_ipi::CpuId(cpu), f, arg) }
}

#[cfg(feature = "irq")]
fn periodic_interval_nanos() -> u64 {
    (ax_hal::time::NANOS_PER_SEC / ticks_per_sec()).max(1)
}

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static NEXT_PERIODIC_DEADLINE_NANOS: u64 = 0;

#[cfg(any(feature = "irq", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimerArmState {
    Disarmed,
    Armed { deadline_ns: u64 },
}

#[cfg(any(feature = "irq", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeTimerMuxState {
    arm: TimerArmState,
}

#[cfg(any(feature = "irq", test))]
impl RuntimeTimerMuxState {
    const fn new() -> Self {
        Self {
            arm: TimerArmState::Disarmed,
        }
    }

    fn claim_interrupt(&mut self) -> Option<u64> {
        match core::mem::replace(&mut self.arm, TimerArmState::Disarmed) {
            TimerArmState::Disarmed => None,
            TimerArmState::Armed { deadline_ns } => Some(deadline_ns),
        }
    }

    const fn next_programming(self, desired_deadline_ns: u64) -> Option<u64> {
        if desired_deadline_ns == 0 {
            return None;
        }
        match self.arm {
            TimerArmState::Disarmed => Some(desired_deadline_ns),
            TimerArmState::Armed { deadline_ns } if desired_deadline_ns < deadline_ns => {
                Some(desired_deadline_ns)
            }
            TimerArmState::Armed { .. } => None,
        }
    }

    fn commit_programming(&mut self, deadline_ns: u64) {
        assert_ne!(
            deadline_ns, 0,
            "a one-shot deadline must not use the disarmed sentinel"
        );
        self.arm = TimerArmState::Armed { deadline_ns };
    }
}

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static RUNTIME_TIMER_MUX: RuntimeTimerMuxState = RuntimeTimerMuxState::new();

#[cfg(feature = "irq")]
fn init_timer() {
    ax_hal::time::enable_timer_irq();
    let now_ns = ax_hal::time::monotonic_time_nanos();
    unsafe {
        NEXT_PERIODIC_DEADLINE_NANOS
            .write_current_raw(now_ns.saturating_add(periodic_interval_nanos()));
    }
    program_next_timer();
}

#[cfg(feature = "irq")]
fn advance_periodic_timer(now_ns: u64) -> bool {
    let mut deadline = unsafe { NEXT_PERIODIC_DEADLINE_NANOS.read_current_raw() };
    if deadline == 0 {
        unsafe {
            NEXT_PERIODIC_DEADLINE_NANOS
                .write_current_raw(now_ns.saturating_add(periodic_interval_nanos()));
        }
        return false;
    }
    if now_ns < deadline {
        return false;
    }

    deadline = next_periodic_deadline(deadline, now_ns, periodic_interval_nanos());
    unsafe { NEXT_PERIODIC_DEADLINE_NANOS.write_current_raw(deadline) };
    true
}

#[cfg(any(feature = "irq", test))]
const fn next_periodic_deadline(deadline_ns: u64, now_ns: u64, interval_ns: u64) -> u64 {
    if now_ns == u64::MAX {
        return u64::MAX;
    }
    if deadline_ns > now_ns {
        return deadline_ns;
    }

    let interval_ns = if interval_ns == 0 { 1 } else { interval_ns };
    let elapsed_ns = (now_ns - deadline_ns) as u128;
    let interval_ns = interval_ns as u128;
    let periods = elapsed_ns / interval_ns + 1;
    let next = deadline_ns as u128 + periods * interval_ns;
    if next > u64::MAX as u128 {
        u64::MAX
    } else {
        next as u64
    }
}

#[cfg(any(feature = "irq", test))]
const fn select_next_timer_deadline(periodic_ns: u64, task_ns: Option<u64>) -> u64 {
    match (periodic_ns, task_ns) {
        (0, Some(task_ns)) => task_ns,
        (0, None) => 0,
        (periodic_ns, Some(task_ns)) => {
            if task_ns < periodic_ns {
                task_ns
            } else {
                periodic_ns
            }
        }
        (periodic_ns, None) => periodic_ns,
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
fn program_next_timer() {
    let mut periodic_deadline = unsafe { NEXT_PERIODIC_DEADLINE_NANOS.read_current_raw() };
    if periodic_deadline == 0 {
        let now_ns = ax_hal::time::monotonic_time_nanos();
        periodic_deadline = now_ns.saturating_add(periodic_interval_nanos());
        unsafe { NEXT_PERIODIC_DEADLINE_NANOS.write_current_raw(periodic_deadline) };
    }
    #[cfg(feature = "multitask")]
    let task_deadline = task::next_timer_deadline_nanos();
    #[cfg(not(feature = "multitask"))]
    let task_deadline = None;
    let deadline = select_next_timer_deadline(periodic_deadline, task_deadline);

    // SAFETY: every caller holds the current CPU's raw IRQ exclusion, so the
    // software arm state and the hardware clockevent form one transaction.
    let timer_mux = unsafe {
        // SAFETY: raw local-IRQ exclusion pins this execution context and is
        // the sole mutation authority for the current CPU's timer mux.
        RUNTIME_TIMER_MUX.current_ref_mut_raw()
    };
    let Some(deadline) = timer_mux.next_programming(deadline) else {
        return;
    };

    ax_hal::time::set_oneshot_timer(deadline);
    timer_mux.commit_programming(deadline);
}

#[cfg(feature = "irq")]
fn claim_timer_interrupt() {
    // SAFETY: the timer handler runs with local IRQs masked. Consuming Armed
    // before accounting lets exactly one delivery re-evaluate the desired
    // periodic/task deadline set; a spurious delivery is a harmless no-op.
    let timer_mux = unsafe {
        // SAFETY: hard-IRQ entry pins this CPU and excludes every other local
        // mux transition until this handler returns.
        RUNTIME_TIMER_MUX.current_ref_mut_raw()
    };
    let _claimed_deadline = timer_mux.claim_interrupt();
}

#[cfg(feature = "irq")]
fn timer_irq_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    let _ = ctx;
    claim_timer_interrupt();
    #[cfg(feature = "multitask")]
    let scheduler_tick = advance_periodic_timer(ax_hal::time::monotonic_time_nanos());
    #[cfg(not(feature = "multitask"))]
    let _ = advance_periodic_timer(ax_hal::time::monotonic_time_nanos());
    #[cfg(feature = "multitask")]
    task::on_timer_irq(scheduler_tick);
    program_next_timer();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "irq", feature = "ipi"))]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    ax_ipi::ipi_handler();
    #[cfg(feature = "multitask")]
    task::on_scheduler_ipi();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "irq", feature = "wake-ipi", not(feature = "ipi")))]
fn ipi_irq_handler(_ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    #[cfg(feature = "multitask")]
    task::on_scheduler_ipi();
    ax_hal::irq::IrqReturn::Handled
}

#[cfg(all(feature = "tls", not(feature = "multitask")))]
fn init_tls() {
    let main_tls = ax_hal::tls::TlsArea::alloc();
    unsafe {
        ax_hal::asm::write_thread_pointer(ax_hal::context::KernelTlsBase::new(
            main_tls.tls_ptr() as usize
        ))
    };
    core::mem::forget(main_tls);
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeTimerMuxState, TimerArmState, configured_runtime_cpu_count, next_periodic_deadline,
        select_next_timer_deadline, timer_resolution_from_frequency,
    };

    #[test]
    fn non_smp_runtime_exposes_only_the_boot_cpu_to_worker_services() {
        assert_eq!(configured_runtime_cpu_count(8, 8, false), 1);
        assert_eq!(configured_runtime_cpu_count(1, 8, false), 1);
        assert_eq!(configured_runtime_cpu_count(8, 4, true), 4);
    }

    #[test]
    fn fs_init_accepts_bootargs_without_fs_feature() {
        crate::fs::init(Some("root=/dev/vda"));
    }

    #[test]
    fn later_task_timer_does_not_postpone_periodic_tick() {
        assert_eq!(select_next_timer_deadline(10, Some(20)), 10);
    }

    #[test]
    fn earlier_task_timer_advances_hardware_deadline() {
        assert_eq!(select_next_timer_deadline(20, Some(10)), 10);
    }

    #[test]
    fn zero_periodic_sentinel_uses_task_or_reports_unarmed() {
        assert_eq!(select_next_timer_deadline(0, Some(10)), 10);
        assert_eq!(select_next_timer_deadline(0, None), 0);
    }

    #[test]
    fn timer_irq_rearms_a_future_task_deadline_without_a_scheduler_entry() {
        let mut timer_mux = RuntimeTimerMuxState::new();
        timer_mux.commit_programming(10);

        assert_eq!(timer_mux.claim_interrupt(), Some(10));

        let next = select_next_timer_deadline(100, Some(40));
        assert_eq!(timer_mux.next_programming(next), Some(40));
        timer_mux.commit_programming(next);
        assert_eq!(timer_mux.arm, TimerArmState::Armed { deadline_ns: 40 });
    }

    #[test]
    fn cancelled_earlier_arm_becomes_one_stale_irq_before_later_rearm() {
        let mut timer_mux = RuntimeTimerMuxState::new();
        timer_mux.commit_programming(40);

        assert_eq!(timer_mux.next_programming(40), None);
        assert_eq!(timer_mux.next_programming(50), None);
        assert_eq!(timer_mux.claim_interrupt(), Some(40));
        assert_eq!(timer_mux.next_programming(50), Some(50));
        timer_mux.commit_programming(50);
        assert_eq!(timer_mux.arm, TimerArmState::Armed { deadline_ns: 50 });
    }

    #[test]
    fn timer_mux_rewrites_an_armed_clockevent_for_an_earlier_deadline() {
        let mut timer_mux = RuntimeTimerMuxState::new();
        timer_mux.commit_programming(50);

        assert_eq!(timer_mux.next_programming(30), Some(30));
    }

    #[test]
    fn spurious_timer_irq_leaves_the_mux_ready_for_the_next_real_deadline() {
        let mut timer_mux = RuntimeTimerMuxState::new();

        assert_eq!(timer_mux.claim_interrupt(), None);
        assert_eq!(timer_mux.next_programming(25), Some(25));
        timer_mux.commit_programming(25);
        assert_eq!(timer_mux.arm, TimerArmState::Armed { deadline_ns: 25 });
    }

    #[test]
    fn periodic_deadline_advances_past_now_without_iteration() {
        assert_eq!(next_periodic_deadline(10, 10, 5), 15);
        assert_eq!(next_periodic_deadline(10, 12, 5), 15);
        assert_eq!(next_periodic_deadline(10, 1_000_000, 3), 1_000_003);
    }

    #[test]
    fn zero_periodic_interval_is_normalized_to_one_nanosecond() {
        assert_eq!(next_periodic_deadline(10, 12, 0), 13);
    }

    #[test]
    fn periodic_deadline_saturates_at_timestamp_limit() {
        assert_eq!(
            next_periodic_deadline(u64::MAX - 1, u64::MAX - 1, 10),
            u64::MAX
        );
        assert_eq!(next_periodic_deadline(1, u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn timer_resolution_rounds_one_hardware_tick_up_to_nanoseconds() {
        assert_eq!(timer_resolution_from_frequency(1_000_000_000), 1);
        assert_eq!(timer_resolution_from_frequency(10_000_000), 100);
        assert_eq!(timer_resolution_from_frequency(24_000_000), 42);
        assert_eq!(timer_resolution_from_frequency(2_500_000_000), 1);
        assert_eq!(timer_resolution_from_frequency(0), 1_000_000_000);
    }
}
