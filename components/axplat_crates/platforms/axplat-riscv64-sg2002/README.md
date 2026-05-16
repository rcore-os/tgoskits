# axplat-riscv64-sg2002

Implementation of [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat) hardware abstraction layer for SG2002 board.

## Install

```bash
cargo +nightly add axplat axplat-riscv64-sg2002
```

## Usage
### How to build
```
git clone https://github.com/elliott10/axplat-riscv64-sg2002 -b sg2002-apache
cd axplat-riscv64-sg2002
cargo build --target riscv64gc-unknown-none-elf
```

### Startup on SG2002
```
make ARCH=riscv64 APP_FEATURES=sg2002 MYPLAT=axplat-riscv64-sg2002 LOG=debug BUS=mmio UIMAGE=y build

```

----
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
extern crate axplat_riscv64_sg2002;
```

#### 3. Use a linker script like the following

Some sections are required to be defined in the linker script, listed as below:
- `.text.boot`: Kernel boot code.
- `.bss.stack`: Stack for kernel booting.
