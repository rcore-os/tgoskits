#![cfg_attr(not(test), no_std)]
#![no_main]
#![allow(dead_code)]

#[macro_use]
extern crate ax_log;
extern crate alloc;

use ax_hal::mem::{MemRegionFlags, PhysAddr, memory_regions, phys_to_virt};

mod config;
mod dtb;
#[cfg(feature = "paging")]
mod klib;
mod log;
mod medium;
mod panic;
mod runtime;
mod shell;
mod sync;

#[cfg_attr(not(test), ax_plat::main)]
pub fn rust_main(cpu_id: usize, dtb: usize) -> ! {
    ax_hal::percpu::init_primary(cpu_id);
    #[cfg(feature = "alloc")]
    ax_alloc::init_percpu_slab(cpu_id);
    ax_hal::init_early(cpu_id, dtb);

    ax_log::init();
    ax_log::set_max_level(option_env!("AX_LOG").unwrap_or("")); // no effect if set `log-level-*` features
    info!("Logging is enabled.");

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

    #[cfg(feature = "alloc")]
    init_allocator();

    #[cfg(feature = "paging")]
    ax_mm::init_memory_management();

    info!("Initialize platform devices...");
    ax_hal::init_later(cpu_id, dtb);
    init_devices();

    ax_ctor_bare::call_ctors();

    unsafe {
        dtb::GLOBAL_NOW_DTB_ADDRESS = phys_to_virt(PhysAddr::from_usize(dtb)).as_usize();
    }

    #[cfg(feature = "ramdisk_cpio")]
    crate::medium::ramdisk_cpio::check_ramdisk();

    crate::shell::shell_main();

    info!("will shut down.");

    ax_hal::power::system_off();
}

/// Probes platform devices and initializes the enabled device subsystems.
///
/// Driver discovery goes through the `rdrive` linker-section registry and
/// `ax-driver` take APIs (the same flow used by `axruntime`), instead of the
/// legacy `axdriver::init_drivers()` container.
fn init_devices() {
    #[cfg(any(feature = "net", feature = "display"))]
    {
        if !rdrive::is_initialized() {
            warn!("rdrive is not initialized; skip platform device probe");
            #[cfg(feature = "display")]
            ax_display::init_display(core::iter::empty::<ax_display::ErasedDisplayDevice>());
            return;
        }
        rdrive::probe_all(false)
            .unwrap_or_else(|err| panic!("failed to probe platform devices: {err:?}"));

        #[cfg(feature = "net")]
        init_net();

        #[cfg(feature = "display")]
        init_display();
    }
}

#[cfg(feature = "net")]
fn init_net() {
    // The ax-net queue runtime needs IRQ-driven executors, which a bootloader
    // without interrupt support cannot provide. Only report the probed
    // devices here; bringing the network stack up for netboot is future work.
    let devices = rdrive::get_list::<ax_driver::net::PlatformNetDevice>();
    if devices.is_empty() {
        info!("no net devices were probed");
        return;
    }
    for dev in devices {
        info!(
            "net device {} probed; the network stack is not wired up in ArceBoot yet",
            dev.descriptor().name
        );
    }
}

#[cfg(feature = "display")]
fn init_display() {
    let devices = ax_driver::display::take_display_devices()
        .unwrap_or_else(|err| panic!("failed to open display devices: {err:?}"))
        .into_iter()
        .map(|taken| {
            let name = alloc::string::String::from(taken.device.name());
            if taken.irq.is_some() {
                warn!(
                    "display device {name} has an IRQ binding but ArceBoot runs without IRQ \
                     support"
                );
            }
            let display = ax_display::rdif::RdifDisplayDevice::new_with_irq(taken.device, None)
                .unwrap_or_else(|err| panic!("failed to adapt display device {name}: {err:?}"));
            ax_display::ErasedDisplayDevice::new(display)
        });
    ax_display::init_display(devices);
}

fn init_allocator() {
    info!("Initialize global memory allocator...");
    info!("  use {} allocator.", ax_alloc::global_allocator().name());

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
                .expect("init heap memory region failed");
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
