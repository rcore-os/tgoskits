# page_table_multiarch

[![Crates.io](https://img.shields.io/crates/v/page_table_multiarch)](https://crates.io/crates/page_table_multiarch)
[![Docs.rs](https://docs.rs/page_table_multiarch/badge.svg)](https://docs.rs/page_table_multiarch)
[![CI](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml)

This crate provides generic, unified, architecture-independent, and OS-free page table structures for various hardware architectures.

The core struct is [`PageTable64<M, PTE, H>`][1]. OS-functions and architecture-dependent types are provided by generic parameters:

- `M`: The architecture-dependent metadata, requires to implement the [`PagingMetaData`][2] trait.
- `PTE`: The architecture-dependent page table entry, requires to implement the [`GenericPTE`][3] trait.
- `H`: OS-functions such as physical memory allocation, requires to implement the [`PagingHandler`][4] trait.

Currently supported architectures and page table structures:

- x86: [`x86_64::X64PageTable`][5]
- ARM: [`aarch64::A64PageTable`][6]
- RISC-V: [`riscv::Sv39PageTable`][7], [`riscv::Sv48PageTable`][8]
- LoongArch64: [`loongarch64:LA64PageTable`][9]

[1]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/struct.PageTable64.html
[2]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/trait.PagingMetaData.html
[3]: https://docs.rs/page_table_entry/latest/page_table_entry/trait.GenericPTE.html
[4]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/trait.PagingHandler.html
[5]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/x86_64/type.X64PageTable.html
[6]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/aarch64/type.A64PageTable.html
[7]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/riscv/type.Sv39PageTable.html
[8]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/riscv/type.Sv48PageTable.html
[9]: https://docs.rs/page_table_multiarch/latest/page_table_multiarch/loongarch64/type.LA64PageTable.html

## Examples (x86_64)

```rust
use memory_addr::{MemoryAddr, PhysAddr, VirtAddr};
use page_table_multiarch::x86_64::{X64PageTable};
use page_table_multiarch::{MappingFlags, PagingHandler, PageSize};

use core::alloc::Layout;

extern crate alloc;

struct PagingHandlerImpl;

impl PagingHandler for PagingHandlerImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        let layout = Layout::from_size_align(0x1000, 0x1000).unwrap();
        let ptr = unsafe { alloc::alloc::alloc(layout) };
        Some(PhysAddr::from(ptr as usize))
    }

    fn dealloc_frame(paddr: PhysAddr) {
        let layout = Layout::from_size_align(0x1000, 0x1000).unwrap();
        let ptr = paddr.as_usize() as *mut u8;
        unsafe { alloc::alloc::dealloc(ptr, layout) };
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        VirtAddr::from(paddr.as_usize())
    }
}

let vaddr = VirtAddr::from(0xdead_beef_000);
let paddr = PhysAddr::from(0x2000);
let flags = MappingFlags::READ | MappingFlags::WRITE;
let mut pt = X64PageTable::<PagingHandlerImpl>::try_new().unwrap();

assert!(pt.root_paddr().is_aligned_4k());
assert!(pt.map(vaddr, paddr, PageSize::Size4K, flags).is_ok());
assert_eq!(pt.query(vaddr), Ok((paddr, flags, PageSize::Size4K)));
```
