# page_table_multiarch

Generic, unified, architecture-independent, and OS-free page table structures for various hardware architectures.

Currently supported architectures:

- x86_64 (4 levels)
- AArch64 (4 levels)
- ARM (32-bit) (2 levels)
- RISC-V (3 level Sv39, 4 levels Sv48)
- LoongArch64 (4 levels)

See the documentation of the following crates for more details:

1. [page_table_entry](https://crates.io/crates/page_table_entry): Page table entry definition for various hardware architectures. [![Crates.io](https://img.shields.io/crates/v/page_table_entry)](https://crates.io/crates/page_table_entry)
2. [page_table_multiarch](https://crates.io/crates/page_table_multiarch): Generic page table structures for various hardware architectures. [![Crates.io](https://img.shields.io/crates/v/page_table_multiarch)](https://crates.io/crates/page_table_multiarch)
