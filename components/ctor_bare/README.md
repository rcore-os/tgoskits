# ctor_bare

[![Crates.io](https://img.shields.io/crates/v/ctor_bare)](https://crates.io/crates/ctor_bare)
[![Docs.rs](https://docs.rs/ctor_bare/badge.svg)](https://docs.rs/ctor_bare)
[![CI](https://github.com/arceos-org/ctor_bare/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/ctor_bare/actions/workflows/ci.yml)


Module initialization functions for Rust (like __attribute__((constructor)) in C/C++) under no_std.


After registering a constructor function, a function pointer pointing to it will be stored in the `.init_array` section.


It can support Linux, MacOS and other systems, and can be also used in `no_std` environments when developing your own kernel.


In Linux, Windows, MacOS and other systems, the `.init_array` section is a default section to store initialization functions. When the program starts, the system will call all functions in the `.init_array` section in order.


When you are running your own operating system, you can call `ctor_bare::call_ctors` to invoke all registered constructor functions.

## Usage

```rust
use ctor_bare::register_ctor;
#[register_ctor]
fn hello_world() {
    println!("Hello, world!");
}

static MAX_NUM: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[register_ctor]
fn set_max_num() {
    MAX_NUM.store(20, std::sync::atomic::Ordering::Relaxed);
}

fn main() {
    assert_eq!(MAX_NUM.load(std::sync::atomic::Ordering::Relaxed), 20);
}
```

Because the `.init_array` section is a default section to store initialization functions in Linux and some other systems, it will be included in the linker script of compilers like GCC and Clang.


**However**, if you are using a custom linker script, you need to **add the `.init_array` section and map them in the page table manually**, so that these functions can be executed correctly. You can add the following line to your linker script as a reference:

```test, ignore
.init_array : ALIGN(4K) {
    PROVIDE_HIDDEN (__init_array_start = .);
    *(.init_array .init_array.*)
    PROVIDE_HIDDEN (__init_array_end = .);
    . = ALIGN(4K);
}
```

## Notes 
To avoid section-related symbols being optimized by the compiler, you need to add "-z nostart-stop-gc" to the compile flags (see <https://lld.llvm.org/ELF/start-stop-gc>).


For example, in `.cargo/config.toml`:
```toml
[build]
rustflags = ["-C", "link-arg=-z", "link-arg=nostart-stop-gc"]
rustdocflags = ["-C", "link-arg=-z", "-C", "link-arg=nostart-stop-gc"]
```