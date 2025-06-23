# axplat-aarch64-raspi

[![Crates.io](https://img.shields.io/crates/v/axplat-aarch64-raspi)](https://crates.io/crates/axplat-aarch64-raspi)
[![CI](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml)

Implementation of [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) hardware abstraction layer for Raspberry Pi 4B board.

## Install

```bash
cargo +nightly add axplat axplat-aarch64-raspi
```

## Usage

#### 1. Write your kernel code

```rust
#[axplat::main]
fn kernel_main(cpu_id: usize, arg: usize) -> ! {
    // Initialize trap, console, time.
    axplat::init::init_early(cpu_id, arg);
    // Initialize platform peripherals (not used in this example).
    axplat::init::init_later(cpu_id, arg);

    // Write your kernel code here.
    axplat::console_println!("Hello, ArceOS!");

    // Power off the system.
    axplat::power::system_off();
}
```

#### 2. Link your kernel with this package

```rust
// Can be located at any dependency crate.
extern crate axplat_aarch64_raspi;
```

#### 3. Use a linker script like the following

```text
ENTRY(_start)
SECTIONS
{
    . = 0xffff000000080000;

    .text : ALIGN(4K) {
        *(.text.boot)               /* This section is required */
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
    }

    .bss : ALIGN(4K) {
        *(.bss.stack)               /* This section is required */
        . = ALIGN(4K);
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.comment)
    }
}
```

Some sections are required to be defined in the linker script, listed as below:
- `.text.boot`: Kernel boot code.
- `.bss.stack`: Stack for kernel booting.

[hello-kernel](https://github.com/arceos-org/axplat_crates/tree/main/examples/hello-kernel) is a complete example of a minimal kernel implemented using [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) and related platform packages.
