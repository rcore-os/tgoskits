# axplat-aarch64-bsta1000b

Implementation of [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) hardware abstraction layer for Black Sesame Technology A1000b SoC.

## Install

```bash
cargo +nightly add axplat axplat-aarch64-bsta1000b
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
extern crate axplat_aarch64_bsta1000b;
```

#### 3. Use a linker script like the following

```text
ENTRY(_start)
SECTIONS
{
    . = 0xffff000081000000;

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

    _ekernel = .;                   /* Symbol `_ekernel` is required */

    /DISCARD/ : {
        *(.comment)
    }
}
```

Some symbols and sections are required to be defined in the linker script, listed as below:
- `_ekernel`: End of kernel image.
- `.text.boot`: Kernel boot code.
- `.bss.stack`: Stack for kernel booting.

[hello-kernel](https://github.com/arceos-org/axplat_crates/tree/main/examples/hello-kernel) is a complete example of a minimal kernel implemented using [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) and related platform packages.
