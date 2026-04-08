# ax-page-table-multiarch

Generic, unified, architecture-independent, and OS-free page table structures for various hardware architectures.

Currently supported architectures:

- x86_64 (4 levels)
- AArch64 (4 levels)
- ARM (32-bit) (2 levels)
- RISC-V (3 level Sv39, 4 levels Sv48)
- LoongArch64 (4 levels)

See the documentation of the following crates for more details:

1. [ax-page-table-entry](https://crates.io/crates/ax-page-table-entry): Page table entry definition for various hardware architectures. [![Crates.io](https://img.shields.io/crates/v/ax-page-table-entry)](https://crates.io/crates/ax-page-table-entry)
2. [ax-page-table-multiarch](https://crates.io/crates/ax-page-table-multiarch): Generic page table structures for various hardware architectures. [![Crates.io](https://img.shields.io/crates/v/ax-page-table-multiarch)](https://crates.io/crates/ax-page-table-multiarch)
