# axplat-x86-pc

[![Crates.io](https://img.shields.io/crates/v/axplat-x86-pc)](https://crates.io/crates/axplat-x86-pc)
[![CI](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml)

Implementation of [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) hardware abstraction layer for x86 Standard PC machine.

## Install

```bash
cargo +nightly add axcpu axplat axplat-x86-pc
```

## Usage

#### 1. Write your kernel code

```rust
#[axplat::main]
fn kernel_main(cpu_id: usize, arg: usize) -> ! {
    // x86_64 requires the `percpu` crate to be initialized first.
    axcpu::init::init_percpu(cpu_id);
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
extern crate axplat_x86_pc;
```

#### 3. Use a linker script like the following

```text
ENTRY(_start)
SECTIONS
{
    . = 0xffff000040200000;
    _skernel = .;                   /* Symbol `_skernel` is required */

    .text : ALIGN(4K) {
        *(.text.boot)               /* This section is required */
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data.boot_page_table)    /* This section is required */
        . = ALIGN(4K);
        *(.data .data.*)
    }

    /* .percpu section and related symbols are required */
    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu 0x0 : AT(_percpu_start) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        . = _percpu_load_start + ALIGN(64) * 1;
    }
    . = _percpu_end;
    _edata = .;                     /* Symbol `_edata` is required */

    .bss : ALIGN(4K) {
        *(.bss.stack)               /* This section is required */
        . = ALIGN(4K);
        *(.bss .bss.*)
        *(COMMON)
        _ebss = .;                  /* Symbol `_ebss` is required */
    }

    /DISCARD/ : {
        *(.comment)
    }
}
```

Some symbols and sections are required to be defined in the linker script, listed as below:
- `_skernel`: Start of kernel image.
- `_edata`: End of data section.
- `_ebss`: End of BSS section.
- `.text.boot`: Kernel boot code.
- `.data.boot_page_table`: Page table for kernel booting.
- `.bss.stack`: Stack for kernel booting.
- `.percpu` section and related symbols: CPU-local data managed by the [percpu](https://crates.io/crates/percpu) crate.

[hello-kernel](https://github.com/arceos-org/axplat_crates/tree/main/examples/hello-kernel) is a complete example of a minimal kernel implemented using [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) and related platform packages.
