//! Execute modified instructions through a distinct, owned virtual alias.

use core::{alloc::Layout, ptr::NonNull};
use std::os::arceos::{
    api::{
        mem::{ax_alloc, ax_dealloc},
        modules::ax_runtime::kernel_mapping::{map_kernel_pages, unmap_kernel_range},
    },
    sync::IrqSaveGuard,
};

use ax_cpu::{VirtAddr, cache::CacheRange, paging::MappingFlags};

struct ExecutablePage {
    backing: NonNull<u8>,
    alias: Option<VirtAddr>,
}
impl Drop for ExecutablePage {
    fn drop(&mut self) {
        if let Some(alias) = self.alias {
            // Retain backing on failure rather than freeing a still-live alias.
            unmap_kernel_range(alias, 4096).unwrap();
        }
        // SAFETY: this owner retired the executable alias and retains exactly
        // the pointer/layout returned by the allocator.
        unsafe {
            ax_dealloc(self.backing, Layout::from_size_align(4096, 4096).unwrap());
        }
    }
}

pub(super) fn run() {
    // SAFETY: the owner below retains and ultimately frees this complete page.
    let backing = unsafe { ax_alloc(Layout::from_size_align(4096, 4096).unwrap()) }.unwrap();
    let mut page = ExecutablePage {
        backing,
        alias: None,
    };
    let writable = VirtAddr::from_usize(backing.as_ptr() as usize);
    let physical = ax_hal::mem::virt_to_phys(writable);
    let base = ax_hal::mem::virtual_address_space().unwrap().kernel().start;
    let alias = map_kernel_pages(
        base,
        &[physical],
        MappingFlags::READ | MappingFlags::EXECUTE,
    )
    .unwrap();
    page.alias = Some(alias);
    {
        let _irq = IrqSaveGuard::new();
        // No other CPU can obtain or execute this private alias. Each body
        // returns an immediate without accessing memory or changing the stack.
        for value in [17u32, 93, 17] {
            let bytes = code(value);
            // SAFETY: all bytes lie in the uniquely owned writable page. The
            // executable alias maps the same retained frame; instruction sync
            // completes before the call, and the generated body obeys the ABI.
            let result = unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), backing.as_ptr(), bytes.len());
                ax_cpu::cache::clean_dcache_range_to_pou(
                    CacheRange::new(writable, bytes.len()).unwrap(),
                );
                ax_cpu::cache::flush_icache_all();
                let entry = core::mem::transmute::<usize, unsafe extern "C" fn() -> usize>(
                    alias.as_usize(),
                );
                entry()
            };
            assert_eq!(
                result, value as usize,
                "executed stale instructions through the alias"
            );
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn code(value: u32) -> [u8; 8] {
    let immediate = value.to_le_bytes();
    [
        0xb8,
        immediate[0],
        immediate[1],
        immediate[2],
        immediate[3],
        0xc3,
        0x90,
        0x90,
    ]
}

#[cfg(not(target_arch = "x86_64"))]
fn code(value: u32) -> [u8; 8] {
    core::cfg_select! {
        target_arch = "aarch64" => {
            let instructions = [0xd2800000u32 | (value << 5), 0xd65f03c0];
        }
        target_arch = "riscv64" => {
            let instructions = [0x00000513u32 | (value << 20), 0x00008067];
        }
        target_arch = "loongarch64" => {
            let instructions = [0x02c00004u32 | (value << 10), 0x4c000020];
        }
    }
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&instructions[0].to_le_bytes());
    bytes[4..].copy_from_slice(&instructions[1].to_le_bytes());
    bytes
}
