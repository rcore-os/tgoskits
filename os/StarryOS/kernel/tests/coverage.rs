#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![feature(core_io)]
#![feature(core_io_borrowed_buf)]

extern crate alloc;

use ax_cpumask as _;
use ax_driver as _;
use ax_errno as _;
use ax_io as _;
use ax_kernel_guard as _;
use ax_lazyinit as _;
use ax_memory_addr as _;
use ax_memory_set as _;
use ax_net as _;
use ax_page_table_entry as _;
use ax_std as _;
use axfs_ng_vfs as _;
use axpoll as _;
use dma_api as _;
use irq_framework as _;
use kernutil as _;
use mmio_api as _;
use rdif_base as _;
use rdif_block as _;
use rdif_def as _;
use rdif_display as _;
use rdif_eth as _;
use rdif_input as _;
use rdif_intc as _;
use rdif_msi as _;
use rdif_pcie as _;
use rdif_pinctrl as _;
use rdif_power as _;
use rdif_reset as _;
use rdif_serial as _;
use rdif_vsock as _;
use rdrive as _;
use rsext4 as _;
use scope_local as _;

#[path = "cases/axtest_fs.rs"]
mod axtest_fs;
#[path = "cases/axtest_memory.rs"]
mod axtest_memory;
#[path = "cases/axtest_runtime.rs"]
mod axtest_runtime;
#[path = "cases/axtest_starry_vm.rs"]
mod axtest_starry_vm;
#[path = "cases/axtest_syscall.rs"]
mod axtest_syscall;

#[axtest::tests]
mod tests {}
