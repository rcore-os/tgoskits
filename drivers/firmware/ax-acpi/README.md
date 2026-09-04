# ax-acpi

`ax-acpi` is TGOSKits' in-tree ACPI and AML implementation. It is derived from
[`rust-osdev/acpi` v6.1.1](https://github.com/rust-osdev/acpi/releases/tag/v6.1.1) and retains its
MIT OR Apache-2.0 licensing.

The in-tree version adds AML compatibility required by real machines, including string and
`RefOf` stores, referenced `SizeOf`, `Match`, per-execution method stacks, and bounded failure when
firmware owns the ACPI Global Lock but SCI-backed wakeups are unavailable.

`ax-acpi` is a Rust library for interacting with the Advanced Configuration and Power Interface, a
complex framework for power management and device discovery and configuration. ACPI is used on
modern x64, as well as some ARM and RISC-V platforms. An operating system needs to interact with
ACPI to correctly set up a platform's interrupt controllers, perform power management, and fully
support many other platform capabilities.

This crate provides a limited API that can be used without an allocator, for example for use
from a bootloader. This API will allow you to search for the RSDP, enumerate over the available
tables, and interact with the tables using their raw structures. All other functionality is
behind an `alloc` feature (enabled by default) and requires an allocator.

With an allocator, this crate provides a richer higher-level interfaces to the static tables, as
well as a dynamic interpreter for AML - the bytecode format encoded in the DSDT and SSDT tables.

See the library documentation for example usage. You will almost certainly need to read portions
of the [ACPI Specification](https://uefi.org/specifications) too (however, be aware that firmware often
ships with ACPI tables that are not spec-compliant).

## Licence
This project is dual-licenced under:
- Apache Licence, Version 2.0 ([LICENCE-APACHE](LICENCE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENCE-MIT](LICENCE-MIT) or http://opensource.org/licenses/MIT)

Unless you explicitly state otherwise, any contribution submitted for inclusion in this work by you,
as defined in the Apache-2.0 licence, shall be dual licenced as above, without additional terms or
conditions.
