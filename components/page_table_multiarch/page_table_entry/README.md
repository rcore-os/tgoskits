# page_table_entry

[![Crates.io](https://img.shields.io/crates/v/page_table_entry)](https://crates.io/crates/page_table_entry)
[![Docs.rs](https://docs.rs/page_table_entry/badge.svg)](https://docs.rs/page_table_entry)
[![CI](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/page_table_multiarch/actions/workflows/ci.yml)

This crate provides the definition of page table entry for various hardware
architectures.

Currently supported architectures and page table entry types:

- x86: [`x86_64::X64PTE`][1]
- ARM: [`aarch64::A64PTE`][2]
- ARM (32-bit): [`arm::A32PTE`][3]
- RISC-V: [`riscv::Rv64PTE`][4]
- LoongArch: [`loongarch64::LA64PTE`][5]

All these types implement the [`GenericPTE`][6] trait, which provides unified
methods for manipulating various page table entries.

[1]: https://docs.rs/page_table_entry/latest/page_table_entry/x86_64/struct.X64PTE.html
[2]: https://docs.rs/page_table_entry/latest/page_table_entry/aarch64/struct.A64PTE.html
[3]: https://docs.rs/page_table_entry/latest/page_table_entry/arm/struct.A32PTE.html
[4]: https://docs.rs/page_table_entry/latest/page_table_entry/riscv/struct.Rv64PTE.html
[5]: https://docs.rs/page_table_entry/latest/page_table_entry/loongarch64/struct.LA64PTE.html
[6]: https://docs.rs/page_table_entry/latest/page_table_entry/trait.GenericPTE.html

## Examples (x86_64)

```rust
use memory_addr::PhysAddr;
use x86_64::structures::paging::page_table::PageTableFlags;
use page_table_entry::{GenericPTE, MappingFlags, x86_64::X64PTE};

let paddr = PhysAddr::from(0x233000);
let pte = X64PTE::new_page(
    paddr,
    /* flags: */ MappingFlags::READ | MappingFlags::WRITE,
    /* is_huge: */ false,
);
assert!(!pte.is_unused());
assert!(pte.is_present());
assert_eq!(pte.paddr(), paddr);
assert_eq!(
    pte.bits(),
    0x800_0000000233_003, // PRESENT | WRITE | NO_EXECUTE | paddr(0x233000)
);
```
