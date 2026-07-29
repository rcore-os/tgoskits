//! Primary CPU boot orchestration.

use core::sync::atomic::Ordering;

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

#[cfg(feature = "paging")]
fn runtime_page_fault_handler(
    addr: ax_memory_addr::VirtAddr,
    flags: ax_hal::trap::PageFaultFlags,
) -> bool {
    #[cfg(feature = "stack-guard-page")]
    if crate::task::diagnose_current_stack_guard_page_fault(addr) {
        return false;
    }

    ax_mm::kernel_aspace().lock().handle_page_fault(addr, flags)
}

/// The main entry point of the ArceOS runtime.
///
/// It is called from the bootstrapping code in the specific platform crate
/// (see [`ax_plat::main`]).
///
/// `cpu_id` is the logic ID of the current CPU, and `arg` is passed from the
/// bootloader (typically the device tree blob address).
///
/// In multi-core environment, this function is called on the primary core, and
/// secondary cores call [`crate::rust_main_secondary`].
#[cfg_attr(not(test), ax_plat::main)]
pub fn rust_main(cpu_id: usize, arg: usize) -> ! {
    ax_hal::percpu::init_primary(cpu_id);
    crate::guard::assert_boot_guards_released();
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
        crate::build_info::ARCH,
        crate::hal::platform_name(),
        crate::build_info::TARGET,
        crate::build_info::MODE,
        log_level,
        axbacktrace::is_enabled(),
        ax_hal::cpu_num()
    );

    ax_log::init();
    ax_log::set_max_level(log_level); // no effect if set `log-level-*` features
    info!("Logging is enabled.");
    info!("Primary CPU {cpu_id} started, arg = {arg:#x}.");

    info!("Found physcial memory regions:");
    for region in ax_hal::mem::memory_regions() {
        info!(
            "  [{:x?}, {:x?}) {} ({:?})",
            region.paddr,
            region.paddr + region.size,
            region.name,
            region.flags
        );
    }

    crate::boot_memory::init_allocator();

    #[cfg(all(feature = "tls", feature = "multitask"))]
    crate::task::initialize_early_bootstrap_tls()
        .expect("failed to initialize primary bootstrap TLS");
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
        crate::registers::append_linker_registers();
        #[cfg(feature = "irq")]
        ax_hal::irq::init_boot_irqs(cpu_id)
            .unwrap_or_else(|error| panic!("failed to initialize boot IRQs: {error:?}"));
        #[cfg(not(feature = "irq"))]
        rdrive::probe_pre_kernel()
            .unwrap_or_else(|error| panic!("failed to run pre-kernel driver probes: {error:?}"));
    } else {
        warn!("rdrive is not initialized; skip pre-kernel driver probe");
    }

    #[cfg(feature = "multitask")]
    crate::task::initialize_primary(cpu_id).expect("failed to initialize primary task scheduler");

    #[cfg(feature = "ipi")]
    {
        ax_ipi::init();
        #[cfg(feature = "irq")]
        ax_hal::irq::set_run_on_cpu_sync(crate::ipi_delivery::run_on_cpu_sync);
    }

    #[cfg(feature = "irq")]
    {
        info!("Initialize interrupt handlers...");
        crate::interrupt_bootstrap::init_current_cpu();
    }

    #[cfg(feature = "multitask")]
    let online_cpu =
        crate::task::publish_current_cpu_online().expect("failed to publish primary scheduler CPU");

    #[cfg(all(feature = "irq", feature = "multitask"))]
    crate::clock_event_runtime::enable_irqs_after_scheduler_online(online_cpu);
    #[cfg(all(feature = "irq", not(feature = "multitask")))]
    ax_hal::asm::enable_irqs();
    #[cfg(all(feature = "multitask", not(feature = "irq")))]
    let _ = online_cpu;

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::mark_current_cpu_ready();

    #[cfg(feature = "multitask")]
    crate::task::start_deferred_task_work_service()
        .expect("failed to start deferred scheduler task-work service");

    // Install the ArceOS runtime glue into the OS-independent Wi-Fi driver
    // cores (aic8800 / sdhci-cv1800) *before* probing, since the FDT probe
    // brings the chip up and that needs timing/task capabilities. The cores
    // declare no ArceOS dependency themselves; this is the adapter layer (see
    // `wifi_glue`).
    #[cfg(feature = "aic8800-wifi")]
    crate::wifi_glue::install_runtime();

    crate::devices::probe_all_devices();

    #[cfg(feature = "serial")]
    crate::serial::init(cpu_id);

    #[cfg(feature = "rtc")]
    ax_println!(
        "Boot at {}\n",
        chrono::DateTime::from_timestamp_nanos(ax_hal::time::wall_time_nanos() as _),
    );

    crate::fs::init(ax_hal::boot::bootargs());

    #[cfg(feature = "display")]
    crate::devices::init_display();

    #[cfg(feature = "input")]
    crate::devices::init_input();

    #[cfg(feature = "net")]
    crate::devices::init_net();

    #[cfg(feature = "vsock")]
    crate::devices::init_vsock();

    #[cfg(feature = "smp")]
    crate::mp::start_secondary_cpus(cpu_id);

    ax_ctor_bare::call_ctors();

    info!("Primary CPU {cpu_id} init OK.");
    crate::INITED_CPUS.fetch_add(1, Ordering::Release);

    while !crate::is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::wait_for_all_cpus_ready();

    #[cfg(all(feature = "smp", feature = "ipi"))]
    crate::fs::online_smp();

    crate::ax_app_entry();

    #[cfg(feature = "multitask")]
    crate::task::exit_current(0);
    #[cfg(not(feature = "multitask"))]
    {
        debug!("main task exited: exit_code={}", 0);
        #[cfg(feature = "irq")]
        crate::clock_event_runtime::take_current_clock_event_offline();
        ax_hal::power::system_off();
    }
}

#[cfg(all(feature = "tls", not(feature = "multitask")))]
pub(crate) fn init_tls() {
    let main_tls = ax_hal::tls::TlsArea::alloc();
    let kernel_tls = ax_hal::context::KernelTlsBase::new(main_tls.tls_ptr() as usize);
    // SAFETY: the boot CPU owns this newly allocated TLS area and no task can
    // observe its thread pointer before initialization completes.
    unsafe { ax_hal::asm::write_thread_pointer(kernel_tls) };
    core::mem::forget(main_tls);
}
