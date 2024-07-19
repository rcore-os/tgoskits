#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(naked_functions)]
#![allow(warnings)]
#[macro_use]
#[cfg(feature = "axstd")]
extern crate axstd as std;

extern crate alloc;

#[macro_use]
extern crate log;

// mod device_emu;
mod gpm;
mod hal;
// mod vmexit; temporarily removed

#[cfg(target_arch = "aarch64")]
mod dtb_aarch64;

use alloc::vec::Vec;

use axerrno::{AxError, AxResult};
use axvm::config::{AxArchVCpuConfig, AxVCpuConfig, AxVMConfig};
use axvm::{AxVM, AxVMPerCpu, GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use page_table_entry::MappingFlags;

use self::gpm::{setup_gpm, GuestMemoryRegion, GuestPhysMemorySet, GUEST_ENTRY};
use self::hal::AxVMHalImpl;
use alloc::vec;

#[cfg(target_arch = "aarch64")]
use dtb_aarch64::MachineMeta;

#[percpu::def_percpu]
pub static mut AXVM_PER_CPU: AxVMPerCpu<AxVMHalImpl> = AxVMPerCpu::new_uninit();

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    println!("Starting virtualization...");
    info!("Hardware support: {:?}", axvm::has_hardware_support());

    let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
    percpu.init(0).expect("Failed to initialize percpu state");
    percpu
        .hardware_enable()
        .expect("Failed to enable virtualization");

    let gpm = setup_gpm().expect("Failed to set guest physical memory set");
    debug!("{:#x?}", gpm);

    let config = AxVMConfig {
        cpu_count: 1,
        cpu_config: AxVCpuConfig {
            arch_config: AxArchVCpuConfig {
                setup_config: (),
                create_config: (),
            },
            ap_entry: GUEST_ENTRY,
            bsp_entry: GUEST_ENTRY,
        },
        // gpm: gpm.nest_page_table_root(),
        // gpm : 0.into(),
    };

    let vm = AxVM::<AxVMHalImpl>::new(config, 0, gpm.nest_page_table_root())
        .expect("Failed to create VM");
    info!("Boot VM...");
    vm.boot().unwrap();
    panic!("VM boot failed")
}