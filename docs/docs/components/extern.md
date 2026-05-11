# Lock 外部依赖

> 统计概览见 [组件依赖分析](dependency)。


按 crate **名称**关键词粗分类；**内部组件**为本文扫描到的 137 个仓库 crate。
关系统计来自根目录 **Cargo.lock** 各 `[[package]]` 的 `dependencies` 列表，仅统计**直接**依赖。
简介来自 `cargo metadata` 的 `description`（≤100 字）；无数据或 metadata 失败时为 —。

| 类别 | 外部包条目数（去重 name+version） |
|------|-------------------------------------|
| 工具库/其他 | 528 |
| 宏/代码生成 | 53 |
| 系统/平台 | 50 |
| 网络/协议 | 29 |
| 异步/并发 | 27 |
| 加密/安全 | 26 |
| 序列化/数据格式 | 24 |
| 日志/错误 | 14 |
| 命令行/配置 | 11 |
| 嵌入式/裸机 | 11 |
| 数据结构/算法 | 10 |
| 设备树/固件 | 8 |

#### 加密/安全

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `digest` `0.10.7` | Traits for cryptographic hash functions and message authentication codes | — | — |
| `digest` `0.11.2` | Traits for cryptographic hash functions and message authentication codes | — | — |
| `fastrand` `2.3.0` | A simple and fast random number generator | `ax-sync` | — |
| `getrandom` `0.2.17` | A small cross-platform library for retrieving random data from system source | — | — |
| `getrandom` `0.3.4` | A small cross-platform library for retrieving random data from system source | — | — |
| `getrandom` `0.4.2` | A small cross-platform library for retrieving random data from system source | — | — |
| `iri-string` `0.7.12` | IRI as string types | — | — |
| `oorandom` `11.1.5` | A tiny, robust PRNG implementation. | — | — |
| `phf_shared` `0.11.3` | Support code shared by PHF libraries | — | — |
| `rand` `0.10.0` | Random number generators and other randomness functionality. | `starry-kernel` | — |
| `rand` `0.8.5` | Random number generators and other randomness functionality. | `arceos-memtest` `arceos-parallel` `ax-allocator` `smoltcp` | — |
| `rand` `0.9.2` | Random number generators and other randomness functionality. | — | — |
| `rand_chacha` `0.3.1` | ChaCha random number generator | `smoltcp` | — |
| `rand_chacha` `0.9.0` | ChaCha random number generator | — | — |
| `rand_core` `0.10.0` | Core random number generation traits and tools for implementation. | — | — |
| `rand_core` `0.6.4` | Core random number generator traits and tools for implementation. | — | — |
| `rand_core` `0.9.5` | Core random number generator traits and tools for implementation. | — | — |
| `ring` `0.17.14` | An experiment. | — | — |
| `ringbuf` `0.4.8` | Lock-free SPSC FIFO ring buffer with direct access to inner data | `ax-net-ng` `starry-kernel` | — |
| `sha1` `0.10.6` | SHA-1 hash function | — | — |
| `sha1` `0.11.0` | SHA-1 hash function | — | — |
| `sha2` `0.10.9` | Pure Rust implementation of the SHA-2 hash function family including SHA-224, SHA-256, SHA-384, and… | `axbuild` | — |
| `sha2` `0.11.0` | Pure Rust implementation of the SHA-2 hash function family including SHA-224, SHA-256, SHA-384, and… | — | — |
| `sharded-slab` `0.1.7` | A lock-free concurrent slab. | — | — |
| `wasm-bindgen-shared` `0.2.117` | Shared support between wasm-bindgen and wasm-bindgen cli, an internal dependency. | — | — |
| `windows-strings` `0.5.1` | Windows string types | — | — |


#### 命令行/配置

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `bitflags` `1.3.2` | A macro to generate structures which behave like bitflags. | `smoltcp` | — |
| `bitflags` `2.11.0` | A macro to generate structures which behave like bitflags. | `ax-cap-access` `ax-fs-ng` `ax-fs-vfs` `ax-net-ng` `ax-page-table-entry` `ax-plat` `ax-plat-x86-pc` `axaddrspace` `axfs-ng-vfs` `axplat-x86-qemu-q35` `axpoll` `axvisor` `riscv-h` `riscv_vcpu` `rsext4` `starry-kernel` `starry-signal` `x86_vcpu` | — |
| `cargo_metadata` `0.23.1` | structured access to the output of `cargo metadata` | `axbuild` | — |
| `clap` `4.6.0` | A simple to use, efficient, and full-featured Command Line Argument Parser | `ax-config-gen` `axbuild` `axvisor` `axvmconfig` `starryos` | — |
| `clap_builder` `4.6.0` | A simple to use, efficient, and full-featured Command Line Argument Parser | — | — |
| `clap_derive` `4.6.0` | Parse command line argument by defining a struct, derive crate. | — | — |
| `clap_lex` `1.1.0` | Minimal, flexible command line parser | — | — |
| `lenient_semver` `0.4.2` | Lenient Semantic Version numbers. | — | — |
| `lenient_semver_parser` `0.4.2` | Lenient parser for Semantic Version numbers. | — | — |
| `lenient_semver_version_builder` `0.4.2` | VersionBuilder trait for lenient parser for Semantic Version numbers. | — | — |
| `semver` `1.0.27` | Parser and evaluator for Cargo's flavor of Semantic Versioning | — | — |


#### 宏/代码生成

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `borsh-derive` `1.6.1` | Binary Object Representation Serializer for Hashing | — | — |
| `bytecheck` `0.6.12` | Derive macro for bytecheck | — | — |
| `bytecheck_derive` `0.6.12` | Derive macro for bytecheck | — | — |
| `bytemuck_derive` `1.10.2` | derive proc-macros for `bytemuck` | — | — |
| `ctor-proc-macro` `0.0.6` | proc-macro support for the ctor crate | — | — |
| `ctor-proc-macro` `0.0.7` | proc-macro support for the ctor crate | — | — |
| `darling` `0.13.4` | A proc-macro library for reading attributes into structs when implementing custom derives. | — | — |
| `darling` `0.20.11` | A proc-macro library for reading attributes into structs when implementing custom derives. | — | — |
| `darling` `0.21.3` | A proc-macro library for reading attributes into structs when implementing custom derives. | — | — |
| `darling` `0.23.0` | A proc-macro library for reading attributes into structs when implementing custom derives. | — | — |
| `darling_core` `0.13.4` | Helper crate for proc-macro library for reading attributes into structs when implementing custom de… | — | — |
| `darling_core` `0.20.11` | Helper crate for proc-macro library for reading attributes into structs when implementing custom de… | — | — |
| `darling_core` `0.21.3` | Helper crate for proc-macro library for reading attributes into structs when implementing custom de… | — | — |
| `darling_core` `0.23.0` | Helper crate for proc-macro library for reading attributes into structs when implementing custom de… | — | — |
| `darling_macro` `0.13.4` | Internal support for a proc-macro library for reading attributes into structs when implementing cus… | — | — |
| `darling_macro` `0.20.11` | Internal support for a proc-macro library for reading attributes into structs when implementing cus… | — | — |
| `darling_macro` `0.21.3` | Internal support for a proc-macro library for reading attributes into structs when implementing cus… | — | — |
| `darling_macro` `0.23.0` | Internal support for a proc-macro library for reading attributes into structs when implementing cus… | — | — |
| `derive_more` `2.1.1` | Adds #[derive(x)] macros for more traits | `starry-signal` | — |
| `derive_more-impl` `2.1.1` | Internal implementation of `derive_more` crate | — | — |
| `dtor-proc-macro` `0.0.5` | proc-macro support for the dtor crate | — | — |
| `dtor-proc-macro` `0.0.6` | proc-macro support for the dtor crate | — | — |
| `enum-map-derive` `0.17.0` | Macros 1.1 implementation of #[derive(Enum)] | — | — |
| `enumerable_derive` `1.2.0` | A proc-macro helping you to enumerate all possible values of a enum or struct | — | — |
| `enumset_derive` `0.14.0` | An internal helper crate for enumset. Not public API. | — | — |
| `heck` `0.4.1` | heck is a case conversion library. | — | — |
| `heck` `0.5.0` | heck is a case conversion library. | — | — |
| `num-derive` `0.4.2` | Numeric syntax extensions | — | — |
| `num_enum_derive` `0.7.6` | Internal implementation details for ::num_enum (Procedural macros to make inter-operation between p… | — | — |
| `paste` `1.0.15` | Macros for all your token pasting needs | `axbacktrace` `x86_vcpu` `x86_vlapic` | — |
| `pest_derive` `2.8.6` | pest's derive macro | — | — |
| `proc-macro-crate` `3.5.0` | Replacement for crate (macro_rules keyword) in proc-macros | `axvisor_api_proc` | — |
| `proc-macro-error-attr2` `2.0.0` | Attribute macro for the proc-macro-error2 crate | — | — |
| `proc-macro-error2` `2.0.1` | Almost drop-in replacement to panics in proc-macros | — | — |
| `proc-macro2` `1.0.106` | A substitute implementation of the compiler's `proc_macro` API to decouple token-based libraries fr… | `ax-config-macros` `ax-crate-interface` `ax-ctor-bare-macros` `ax-percpu-macros` `ax-plat-macros` `axvisor_api_proc` | — |
| `proc-macro2-diagnostics` `0.10.1` | Diagnostics for proc-macro2. | — | — |
| `ptr_meta_derive` `0.1.4` | Macros for ptr_meta | — | — |
| `ptr_meta_derive` `0.3.1` | Proc macros for ptr_meta | — | — |
| `quote` `1.0.45` | Quasi-quoting macro quote!(...) | `ax-config-macros` `ax-crate-interface` `ax-ctor-bare-macros` `ax-percpu-macros` `ax-plat-macros` `axvisor` `axvisor_api_proc` | — |
| `regex-syntax` `0.8.10` | A regular expression parser. | — | — |
| `rkyv_derive` `0.7.46` | Derive macro for rkyv | — | — |
| `schemars_derive` `1.2.1` | Macros for #[derive(JsonSchema)], for use with schemars | — | — |
| `syn` `1.0.109` | Parser for Rust source code | — | — |
| `syn` `2.0.117` | Parser for Rust source code | `ax-config-macros` `ax-crate-interface` `ax-ctor-bare-macros` `ax-percpu-macros` `ax-plat-macros` `axvisor` `axvisor_api_proc` | — |
| `sync_wrapper` `1.0.2` | A tool for enlisting the compiler's help in proving the absence of concurrency | — | — |
| `synstructure` `0.13.2` | Helper methods and macros for custom derives | — | — |
| `version_check` `0.9.5` | Tiny crate to check the version of the installed/running rustc. | — | — |
| `wezterm-dynamic-derive` `0.1.1` | config serialization for wezterm via dynamic json-like data values | — | — |
| `yoke-derive` `0.7.5` | Custom derive for the yoke crate | — | — |
| `zerocopy-derive` `0.7.35` | Custom derive for traits from the zerocopy crate | — | — |
| `zerocopy-derive` `0.8.48` | Custom derive for traits from the zerocopy crate | — | — |
| `zerofrom-derive` `0.1.7` | Custom derive for the zerofrom crate | — | — |
| `zerovec-derive` `0.10.3` | Custom derive for the zerovec crate | — | — |


#### 嵌入式/裸机

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `critical-section` `1.2.0` | Cross-platform critical section | — | — |
| `defmt` `0.3.100` | A highly efficient logging framework that targets resource-constrained devices, like microcontrolle… | `smoltcp` | — |
| `defmt` `1.0.1` | A highly efficient logging framework that targets resource-constrained devices, like microcontrolle… | — | — |
| `defmt-macros` `1.0.1` | defmt macros | — | — |
| `defmt-parser` `1.0.0` | Parsing library for defmt format strings | — | — |
| `embedded-graphics` `0.8.2` | Embedded graphics library for small hardware displays | `arceos-display` | — |
| `embedded-graphics-core` `0.4.1` | Core traits and functionality for embedded-graphics | — | — |
| `embedded-hal` `1.0.0` | A Hardware Abstraction Layer (HAL) for embedded systems | — | — |
| `tock-registers` `0.10.1` | Memory-Mapped I/O and register interface developed for Tock. | `arm_vgic` `ax-cpu` `ax-riscv-plic` `riscv_vcpu` `x86_vlapic` | — |
| `tock-registers` `0.8.1` | Memory-Mapped I/O and register interface developed for Tock. | `ax-arm-pl011` | — |
| `tock-registers` `0.9.0` | Memory-Mapped I/O and register interface developed for Tock. | — | — |


#### 工具库/其他

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `aarch32-cpu` `0.2.0` | — | — | — |
| `aarch64-cpu` `10.0.0` | Low level access to processors using the AArch64 execution state | `fxmac_rs` | — |
| `aarch64-cpu` `11.2.0` | Low level access to processors using the AArch64 execution state | `arm_vcpu` `arm_vgic` `ax-cpu` `ax-page-table-entry` `ax-plat-aarch64-peripherals` `ax-plat-aarch64-raspi` | — |
| `aarch64-cpu-ext` `0.1.4` | Extended AArch64 CPU utilities and cache management operations | `axvisor` | — |
| `acpi` `6.1.1` | A pure-Rust library for interacting with ACPI | — | — |
| `addr2line` `0.26.1` | — | `axbacktrace` | — |
| `adler2` `2.0.1` | A simple clean-room implementation of the Adler-32 checksum | — | — |
| `ahash` `0.7.8` | A non-cryptographic hash function using AES-NI for high performance | — | — |
| `ahash` `0.8.12` | A non-cryptographic hash function using AES-NI for high performance | — | — |
| `aho-corasick` `1.1.4` | Fast multiple substring searching. | — | — |
| `aliasable` `0.1.3` | Basic aliasable (non unique pointer) types | — | — |
| `allocator-api2` `0.2.21` | Mirror of Rust's allocator API | — | — |
| `aml` `0.16.4` | Library for parsing AML | — | — |
| `android_system_properties` `0.1.5` | Minimal Android system properties wrapper | — | — |
| `anes` `0.1.6` | ANSI Escape Sequences provider & parser | — | — |
| `ansi_rgb` `0.2.0` | Colorful console text using ANSI escape sequences | — | — |
| `anstream` `1.0.0` | IO stream adapters for writing colored text that will gracefully degrade according to your terminal… | — | — |
| `anstyle` `1.0.14` | ANSI text styling | — | — |
| `anstyle-parse` `1.0.0` | Parse ANSI Style Escapes | — | — |
| `anstyle-query` `1.1.5` | Look up colored console capabilities | — | — |
| `anstyle-wincon` `3.0.11` | Styling legacy Windows terminals | — | — |
| `arbitrary-int` `1.3.0` | — | — | — |
| `arbitrary-int` `2.1.1` | — | — | — |
| `arm-gic-driver` `0.16.5` | A driver for the Arm Generic Interrupt Controller. | `ax-plat-aarch64-peripherals` | — |
| `arm-gic-driver` `0.17.0` | A driver for the Arm Generic Interrupt Controller. | `axvisor` | — |
| `arm-targets` `0.4.1` | — | — | — |
| `arm_pl011` `0.1.0` | — | — | — |
| `as-any` `0.3.2` | provide the AsAny trait | — | — |
| `assert_matches` `1.5.0` | Asserts that a value matches a pattern | `axaddrspace` | — |
| `atomic` `0.6.1` | `Generic Atomic<T> wrapper type` | — | — |
| `atomic-waker` `1.1.2` | A synchronization primitive for task wakeup | — | — |
| `autocfg` `1.5.0` | Automatic cfg for Rust compiler features | `ax-io` | — |
| `aws-lc-rs` `1.16.2` | aws-lc-rs is a cryptographic library using AWS-LC for its cryptographic operations. This library st… | — | — |
| `aws-lc-sys` `0.39.1` | AWS-LC is a general-purpose cryptographic library maintained by the AWS Cryptography team for AWS a… | — | — |
| `ax_slab_allocator` `0.4.0` | Slab allocator for `no_std` systems. Uses multiple slabs with blocks of different sizes and a linke… | `ax-allocator` | — |
| `axallocator` `0.2.0` | — | — | — |
| `axconfig-gen` `0.2.1` | — | — | — |
| `axconfig-macros` `0.2.1` | — | — | — |
| `axcpu` `0.3.1` | — | — | — |
| `axfatfs` `0.1.0-pre.0` | FAT filesystem library. | `ax-fs` | — |
| `axin` `0.1.0` | A Rust procedural macro library for function instrumentation | `axaddrspace` | — |
| `axplat` `0.3.1-pre.6` | — | — | — |
| `axplat-macros` `0.1.0` | — | — | — |
| `axplat-riscv64-visionfive2` `0.1.0-pre.2` | — | `starryos` `starryos-test` | — |
| `az` `1.2.1` | Casts and checked casts | — | — |
| `bare-metal` `1.0.0` | Abstractions common to bare metal systems | `riscv-h` | — |
| `bare-test-macros` `0.2.0` | macros for bare-test | — | — |
| `bcm2835-sdhci` `0.1.1` | — | `ax-driver-block` | — |
| `bindgen` `0.72.1` | Automatically generates Rust FFI bindings to C and C++ libraries. | `ax-libc` `ax-posix-api` | — |
| `bit` `0.1.1` | A library which provides helpers to manipulate bits and bit ranges. | `x86_vlapic` | — |
| `bit-set` `0.5.3` | A set of bits | — | — |
| `bit-vec` `0.6.3` | A vector of bits | — | — |
| `bit_field` `0.10.3` | Simple bit field trait providing get_bit, get_bits, set_bit, and set_bits methods for Rust's integr… | `axaddrspace` `bitmap-allocator` `riscv-h` `riscv_vcpu` `x86_vcpu` | — |
| `bitbybit` `1.4.0` | — | — | — |
| `bitfield-struct` `0.11.0` | — | — | — |
| `bitmaps` `3.2.1` | Fixed size boolean arrays | `arm_vgic` `ax-cpumask` `ax-page-table-multiarch` `riscv_vplic` `starry-kernel` | — |
| `block-buffer` `0.10.4` | Buffer type for block processing of data | — | — |
| `block-buffer` `0.12.0` | Buffer types for block processing of data | — | — |
| `borsh` `1.6.1` | Binary Object Representation Serializer for Hashing | — | — |
| `buddy-slab-allocator` `0.2.0` | — | `ax-alloc` `ax-dma` | — |
| `buddy_system_allocator` `0.10.0` | A bare metal allocator that uses buddy system. | `ax-allocator` | — |
| `buddy_system_allocator` `0.12.0` | A bare metal allocator that uses buddy system. | — | — |
| `bumpalo` `3.20.2` | A fast bump allocation arena for Rust. | — | — |
| `byte-unit` `5.2.0` | A library for interacting with units of bytes. | `axvisor` | — |
| `bytemuck` `1.25.0` | A crate for mucking around with piles of bytes. | `starry-kernel` `starry-vm` | — |
| `camino` `1.2.2` | UTF-8 paths | — | — |
| `cargo-platform` `0.3.2` | Cargo's representation of a target platform. | — | — |
| `cast` `0.3.0` | Ergonomic, checked cast functions for primitive types | — | — |
| `castaway` `0.2.4` | Safe, zero-cost downcasting for limited compile-time specialization. | — | — |
| `cesu8` `1.1.0` | Convert to and from CESU-8 encoding (similar to UTF-8) | — | — |
| `cexpr` `0.6.0` | A C expression parser and evaluator | — | — |
| `cfg-if` `1.0.4` | A macro to ergonomically define an item depending on a large number of #[cfg] parameters. Structure… | `ax-alloc` `ax-allocator` `ax-cpu` `ax-driver` `ax-fs-ng` `ax-hal` `ax-helloworld-myplat` `ax-kernel-guard` `ax-kspin` `ax-log` `ax-net` `ax-net-ng` `ax-percpu` `ax-percpu-macros` `ax-runtime` `ax-task` `axaddrspace` `axbacktrace` `axdevice` `axfs-ng-vfs` `axvisor` `axvm` `riscv_vcpu` `smoltcp` `starry-kernel` `starry-signal` `x86_vcpu` | — |
| `cfg_aliases` `0.2.1` | A tiny utility to help save you a lot of effort with long winded `#[cfg()]` checks. | — | — |
| `chrono` `0.4.44` | Date and time library for Rust | `ax-arm-pl031` `ax-fs-ng` `ax-log` `ax-plat-loongarch64-qemu-virt` `ax-runtime` `axbuild` `starry-kernel` | — |
| `ciborium` `0.2.2` | serde implementation of CBOR using ciborium-basic | — | — |
| `ciborium-io` `0.2.2` | Simplified Read/Write traits for no_std usage | — | — |
| `ciborium-ll` `0.2.2` | Low-level CBOR codec primitives | — | — |
| `clang-sys` `1.8.1` | Rust bindings for libclang. | — | — |
| `colorchoice` `1.0.5` | Global override of color control | — | — |
| `colored` `3.1.1` | The most simple way to add colors in your terminal | `axbuild` | — |
| `combine` `4.6.7` | Fast parser combinators on arbitrary streams with zero-copy support. | — | — |
| `compact_str` `0.8.1` | A memory efficient string type that transparently stores strings on the stack, when possible | — | — |
| `compact_str` `0.9.0` | A memory efficient string type that transparently stores strings on the stack, when possible | — | — |
| `concurrent-queue` `2.5.0` | Concurrent multi-producer multi-consumer queue | — | — |
| `console` `0.16.3` | A terminal and console abstraction for Rust | — | — |
| `const-default` `1.0.0` | A const Default trait | — | — |
| `const-oid` `0.10.2` | Const-friendly implementation of the ISO/IEC Object Identifier (OID) standard as defined in ITU X.6… | — | — |
| `const-str` `1.1.0` | compile-time string operations | `ax-config` `ax-plat` | — |
| `const_fn` `0.4.12` | A lightweight attribute for easy generation of const functions with conditional compilations. | — | — |
| `convert_case` `0.10.0` | Convert strings into any case | — | — |
| `convert_case` `0.8.0` | Convert strings into any case | — | — |
| `core-foundation` `0.10.1` | Bindings to Core Foundation for macOS | — | — |
| `core-foundation` `0.9.4` | Bindings to Core Foundation for macOS | — | — |
| `core-foundation-sys` `0.8.7` | Bindings to Core Foundation for macOS | — | — |
| `core_detect` `1.0.0` | — | — | — |
| `cpp_demangle` `0.5.1` | — | — | — |
| `cpufeatures` `0.2.17` | Lightweight runtime CPU feature detection for aarch64, loongarch64, and x86/x86_64 targets, with no… | — | — |
| `cpufeatures` `0.3.0` | Lightweight runtime CPU feature detection for aarch64, loongarch64, and x86/x86_64 targets, with no… | — | — |
| `crate_interface` `0.1.4` | — | — | — |
| `crate_interface` `0.3.0` | — | — | — |
| `crc` `3.4.0` | Rust implementation of CRC with support of various standards | — | — |
| `crc32fast` `1.5.0` | Fast, SIMD-accelerated CRC32 (IEEE) checksum computation | — | — |
| `criterion` `0.5.1` | Statistics-driven micro-benchmarking library | `ax-allocator` | — |
| `criterion-plot` `0.5.0` | Criterion's plotting library | — | — |
| `crossterm` `0.28.1` | A crossplatform terminal library for manipulating terminals. | — | — |
| `crossterm` `0.29.0` | A crossplatform terminal library for manipulating terminals. | — | — |
| `crossterm_winapi` `0.9.1` | WinAPI wrapper that provides some basic simple abstractions around common WinAPI calls | — | — |
| `crunchy` `0.2.4` | Crunchy unroller: deterministically unroll constant loops | — | — |
| `crypto-common` `0.1.7` | Common cryptographic traits | — | — |
| `crypto-common` `0.2.1` | Common traits used by cryptographic algorithms | — | — |
| `csscolorparser` `0.6.2` | CSS color parser library | — | — |
| `ctor` `0.4.3` | __attribute__((constructor)) for Rust | `starry-process` | — |
| `ctor` `0.6.3` | __attribute__((constructor)) for Rust | `scope-local` | — |
| `cursive` `0.21.1` | A TUI (Text User Interface) library focused on ease-of-use. | — | — |
| `cursive-macros` `0.1.0` | Proc-macros for the cursive TUI library. | — | — |
| `cursive_core` `0.4.6` | Core components for the Cursive TUI | — | — |
| `data-encoding` `2.10.0` | Efficient and customizable data-encoding functions like base64, base32, and hex | — | — |
| `deltae` `0.3.2` | Calculate Delta E between two colors in CIE Lab space. | — | — |
| `deranged` `0.5.8` | Ranged integers | — | — |
| `device_tree` `1.1.0` | Reads and parses Linux device tree images | — | — |
| `displaydoc` `0.2.5` | A derive macro for implementing the display Trait via a doc comment and string interpolation | — | — |
| `dma-api` `0.2.2` | — | — | — |
| `dma-api` `0.3.1` | — | — | — |
| `dma-api` `0.5.2` | Trait for DMA alloc and some collections | — | — |
| `dma-api` `0.7.1` | Trait for DMA alloc and some collections | `axplat-dyn` | — |
| `document-features` `0.2.12` | Extract documentation for the feature flags from comments in Cargo.toml | — | — |
| `downcast-rs` `2.0.2` | Trait object downcasting support using only safe Rust. It supports type parameters, associated type… | `starry-kernel` | — |
| `dtor` `0.0.6` | __attribute__((destructor)) for Rust | — | — |
| `dtor` `0.1.1` | __attribute__((destructor)) for Rust | — | — |
| `dunce` `1.0.5` | Normalize Windows paths to the most compatible format, avoiding UNC where possible | — | — |
| `dw_apb_uart` `0.1.0` | — | `ax-plat-aarch64-bsta1000b` | — |
| `dyn-clone` `1.0.20` | Clone trait that is dyn-compatible | — | — |
| `either` `1.15.0` | The enum `Either` with variants `Left` and `Right` is a general purpose sum type with two cases. | — | — |
| `encode_unicode` `1.0.0` | UTF-8 and UTF-16 character types, iterators and related methods for char, u8 and u16. | — | — |
| `encoding_rs` `0.8.35` | A Gecko-oriented implementation of the Encoding Standard | — | — |
| `enum-map` `2.7.3` | A map with C-like enum keys represented internally as an array | — | — |
| `enum_dispatch` `0.3.13` | Near drop-in replacement for dynamic-dispatched method calls with up to 10x the speed | `ax-net-ng` `starry-kernel` | — |
| `enumerable` `1.2.0` | A library helping you to enumerate all possible values of a type | `axvmconfig` | — |
| `enumn` `0.1.14` | Convert number to enum | — | — |
| `enumset` `1.1.10` | A library for creating compact sets of enums. | — | — |
| `env_filter` `1.0.1` | Filter log events using environment variables | — | — |
| `equivalent` `1.0.2` | Traits for key comparison in maps. | — | — |
| `errno` `0.3.14` | Cross-platform interface to the `errno` variable. | — | — |
| `euclid` `0.22.14` | Geometry primitives | — | — |
| `event-listener` `5.4.1` | Notify async tasks or threads | `ax-net-ng` `starry-kernel` `starry-signal` | — |
| `event-listener-strategy` `0.5.4` | Block or poll on event_listener easily | — | — |
| `extern-trait` `0.4.1` | Opaque types for traits using static dispatch | `ax-task` `axvisor` `starry-kernel` `starry-signal` `starry-vm` | — |
| `extern-trait-impl` `0.4.1` | Proc-macro implementation for extern-trait | — | — |
| `fancy-regex` `0.11.0` | An implementation of regexes, supporting a relatively rich set of features, including backreference… | — | — |
| `filedescriptor` `0.8.3` | More ergonomic wrappers around RawFd and RawHandle | — | — |
| `filetime` `0.2.27` | Platform-agnostic accessors of timestamps in File metadata | — | — |
| `find-msvc-tools` `0.1.9` | Find windows-specific tools, read MSVC versions from the registry and from COM interfaces | — | — |
| `finl_unicode` `1.4.0` | Library for handling Unicode functionality for finl (categories and grapheme segmentation) | — | — |
| `fixedbitset` `0.4.2` | FixedBitSet is a simple bitset collection | — | — |
| `flate2` `1.1.9` | DEFLATE compression and decompression exposed as Read/BufRead/Write streams. Supports miniz_oxide a… | `axbuild` | — |
| `flatten_objects` `0.2.4` | A container that stores numbered objects. Each object can be assigned with a unique ID. | `ax-posix-api` `starry-kernel` | — |
| `float-cmp` `0.9.0` | Floating point approximate comparison traits | — | — |
| `fnv` `1.0.7` | Fowler–Noll–Vo hash function | — | — |
| `foldhash` `0.1.5` | A fast, non-cryptographic, minimally DoS-resistant hashing algorithm. | — | — |
| `foldhash` `0.2.0` | A fast, non-cryptographic, minimally DoS-resistant hashing algorithm. | — | — |
| `form_urlencoded` `1.2.2` | Parser and serializer for the application/x-www-form-urlencoded syntax, as used by HTML forms. | — | — |
| `fs_extra` `1.3.0` | Expanding std::fs and std::io. Recursively copy folders with information about process and much mor… | — | — |
| `funty` `2.0.0` | Trait generalization over the primitive types | — | — |
| `generic-array` `0.14.7` | Generic types implementing functionality of arrays | — | — |
| `getopts` `0.2.24` | getopts-like option parsing | `smoltcp` | — |
| `gimli` `0.33.1` | — | `axbacktrace` `starry-kernel` | — |
| `glob` `0.3.3` | Support for matching file paths against Unix shell style patterns. | — | — |
| `h2` `0.4.13` | An HTTP/2 client and server | — | — |
| `half` `2.7.1` | Half-precision floating point f16 and bf16 types for Rust implementing the IEEE 754-2008 standard b… | — | — |
| `handler_table` `0.1.2` | — | — | — |
| `hash32` `0.3.1` | 32-bit hashing algorithms | — | — |
| `heapless` `0.8.0` | `static` friendly data structures that don't require dynamic memory allocation | `smoltcp` | — |
| `heapless` `0.9.2` | `static` friendly data structures that don't require dynamic memory allocation | `ax-hal` `ax-io` `ax-plat-x86-pc` `axplat-dyn` `axplat-x86-qemu-q35` | — |
| `hermit-abi` `0.5.2` | Hermit system calls definitions. | — | — |
| `humantime` `2.3.0` | `A parser and formatter for std::time::{Duration, SystemTime}` | — | — |
| `hybrid-array` `0.4.10` | Hybrid typenum-based and const generic array types designed to provide the flexibility of typenum-b… | — | — |
| `iana-time-zone` `0.1.65` | get the IANA time zone for the current system | — | — |
| `iana-time-zone-haiku` `0.1.2` | iana-time-zone support crate for Haiku OS | — | — |
| `icu_collections` `1.5.0` | Collection of API for use in ICU libraries. | — | — |
| `icu_locid` `1.5.0` | API for managing Unicode Language and Locale Identifiers | — | — |
| `icu_locid_transform` `1.5.0` | API for Unicode Language and Locale Identifiers canonicalization | — | — |
| `icu_locid_transform_data` `1.5.1` | Data for the icu_locid_transform crate | — | — |
| `icu_normalizer` `1.5.0` | API for normalizing text into Unicode Normalization Forms | — | — |
| `icu_normalizer_data` `1.5.1` | Data for the icu_normalizer crate | — | — |
| `icu_properties` `1.5.1` | Definitions for Unicode properties | — | — |
| `icu_properties_data` `1.5.1` | Data for the icu_properties crate | — | — |
| `icu_provider` `1.5.0` | Trait and struct definitions for the ICU data provider | — | — |
| `icu_provider_macros` `1.5.0` | Proc macros for ICU data providers | — | — |
| `id-arena` `2.3.0` | A simple, id-based arena. | — | — |
| `ident_case` `1.0.1` | Utility for applying case rules to Rust identifiers. | — | — |
| `idna` `0.5.0` | IDNA (Internationalizing Domain Names in Applications) and Punycode. | — | — |
| `idna` `1.0.1` | IDNA (Internationalizing Domain Names in Applications) and Punycode. | `smoltcp` | — |
| `indicatif` `0.18.4` | A progress bar and cli reporting library for Rust | `axbuild` | — |
| `indoc` `2.0.7` | Indented document literals | `ax-runtime` `starry-kernel` | — |
| `inherit-methods-macro` `0.1.0` | Inherit methods from a field automatically (via procedural macros) | `axfs-ng-vfs` `starry-kernel` | — |
| `insta` `1.47.2` | A snapshot testing library for Rust | `smoltcp` | — |
| `instability` `0.3.12` | Rust API stability attributes for the rest of us. A fork of the `stability` crate. | — | — |
| `intrusive-collections` `0.9.7` | Intrusive collections for Rust (linked list and red-black tree) | `ax-fs-ng` | — |
| `io-kit-sys` `0.4.1` | Bindings to IOKit for macOS | — | — |
| `ipnet` `2.12.0` | Provides types and useful methods for working with IPv4 and IPv6 network addresses, commonly called… | — | — |
| `is-terminal` `0.4.17` | Test whether a given stream is a terminal | — | — |
| `is_terminal_polyfill` `1.70.2` | Polyfill for `is_terminal` stdlib feature for use with older MSRVs | — | — |
| `itertools` `0.10.5` | Extra iterator adaptors, iterator methods, free functions, and macros. | — | — |
| `itertools` `0.13.0` | Extra iterator adaptors, iterator methods, free functions, and macros. | — | — |
| `itertools` `0.14.0` | Extra iterator adaptors, iterator methods, free functions, and macros. | — | — |
| `itoa` `1.0.18` | Fast integer primitive to string conversion | — | — |
| `ixgbe-driver` `0.1.1` | — | `ax-driver-net` | `smoltcp` |
| `jiff` `0.2.23` | A date-time library that encourages you to jump into the pit of success. This library is heavily in… | — | — |
| `jiff-static` `0.2.23` | Create static TimeZone values for Jiff (useful in core-only environments). | — | — |
| `jkconfig` `0.1.8` | A Cursive-based TUI component library for JSON Schema configuration | `axbuild` | — |
| `jkconfig` `0.2.2` | A Ratatui-based TUI component library for JSON Schema configuration | — | — |
| `jni` `0.21.1` | Rust bindings to the JNI | — | — |
| `jni-sys` `0.3.1` | Rust definitions corresponding to jni.h | — | — |
| `jni-sys` `0.4.1` | Rust definitions corresponding to jni.h | — | — |
| `jni-sys-macros` `0.4.1` | Macros for jni-sys crate | — | — |
| `jobserver` `0.1.34` | An implementation of the GNU Make jobserver for Rust. | — | — |
| `js-sys` `0.3.94` | Bindings for all JS global objects and functions in all JS environments like Node.js and browsers, … | — | — |
| `kasm-aarch64` `0.2.0` | Boot kernel code with mmu. | — | — |
| `kasuari` `0.4.12` | A rust layout solver for GUIs, based on the Cassowary algorithm. A fork of the unmaintained cassowa… | — | — |
| `kernel_guard` `0.1.3` | — | — | — |
| `kernutil` `0.2.0` | A kernel. | — | — |
| `kspin` `0.1.1` | — | — | — |
| `lab` `0.11.0` | Tools for converting RGB colors to the CIE-L*a*b* color space, and comparing differences in color. | — | — |
| `lazy_static` `1.5.0` | A macro for declaring lazily evaluated statics in Rust. | `ax-net-ng` `ax-posix-api` `axaddrspace` `axvisor` `rsext4` `starry-kernel` | — |
| `lazyinit` `0.2.2` | — | — | — |
| `leb128fmt` `0.1.0` | A library to encode and decode LEB128 compressed integers. | — | — |
| `libloading` `0.8.9` | Bindings around the platform's dynamic library loading primitives with greatly improved memory safe… | — | — |
| `libredox` `0.1.15` | Redox stable ABI | — | — |
| `libudev` `0.3.0` | Rust wrapper for libudev | — | — |
| `libudev-sys` `0.1.4` | FFI bindings to libudev | — | — |
| `libz-sys` `1.1.25` | Low-level bindings to the system libz library (also known as zlib). | — | — |
| `line-clipping` `0.3.7` | A simple crate implementing line clipping algorithms. | — | — |
| `linkme` `0.3.35` | Safe cross-platform linker shenanigans | `arceos-exception` `ax-cpu` `ax-hal` `starry-kernel` | — |
| `linkme-impl` `0.3.35` | Implementation detail of the linkme crate | — | — |
| `litemap` `0.7.5` | A key-value Map implementation based on a flat, sorted Vec. | — | — |
| `litrs` `1.0.0` | Parse and inspect Rust literals (i.e. tokens in the Rust programming language representing fixed va… | — | — |
| `lock_api` `0.4.14` | Wrappers to create fully-featured Mutex and RwLock types. Compatible with no_std. | `ax-std` `ax-sync` `starry-kernel` | — |
| `loongArch64` `0.2.5` | loongArch64 support for Rust | `ax-cpu` `ax-plat-loongarch64-qemu-virt` | — |
| `lwext4_rust` `0.2.0` | lwext4 in Rust | `ax-fs-ng` | — |
| `lzma-rs` `0.3.0` | A codec for LZMA, LZMA2 and XZ written in pure Rust | — | — |
| `lzma-sys` `0.1.20` | Raw bindings to liblzma which contains an implementation of LZMA and xz stream encoding/decoding. H… | — | — |
| `mac_address` `1.1.8` | Cross-platform retrieval of a network interface MAC address. | — | — |
| `mach2` `0.4.3` | A Rust interface to the user-space API of the Mach 3.0 kernel that underlies OSX. | — | — |
| `managed` `0.8.0` | An interface for logically owning objects, whether or not heap allocation is available. | `smoltcp` | — |
| `matchit` `0.8.4` | A high performance, zero-copy URL router. | — | — |
| `mbarrier` `0.1.3` | Cross-platform memory barrier implementations for Rust, inspired by Linux kernel | — | — |
| `md5` `0.8.0` | The package provides the MD5 hash function. | — | — |
| `memmem` `0.1.1` | Substring searching | — | — |
| `memoffset` `0.9.1` | offset_of functionality for Rust structs. | `riscv_vcpu` | — |
| `memory_addr` `0.4.1` | — | — | — |
| `micromath` `2.1.0` | Embedded-friendly math library featuring fast floating point approximations (with small code size) … | — | — |
| `mime` `0.3.17` | Strongly Typed Mimes | — | — |
| `mime_guess` `2.0.5` | A simple crate for detection of a file's MIME type by its extension. | — | — |
| `minimal-lexical` `0.2.1` | Fast float parsing conversion routines. | — | — |
| `miniz_oxide` `0.8.9` | DEFLATE compression and decompression library rewritten in Rust based on miniz | — | — |
| `nb` `1.1.0` | — | — | — |
| `network-interface` `2.0.5` | Retrieve system's Network Interfaces on Linux, FreeBSD, macOS and Windows on a standarized manner | — | — |
| `nom` `7.1.3` | A byte-oriented, zero-copy, parser combinators library | — | — |
| `nu-ansi-term` `0.50.3` | Library for ANSI terminal colors and styles (bold, underline) | — | — |
| `num` `0.4.3` | A collection of numeric types and traits for Rust, including bigint, complex, rational, range itera… | — | — |
| `num-align` `0.1.0` | Some hal for os | — | — |
| `num-complex` `0.4.6` | Complex numbers implementation for Rust | — | — |
| `num-conv` `0.2.1` | `num_conv` is a crate to convert between integer types without using `as` casts. This provides bett… | — | — |
| `num-integer` `0.1.46` | Integer traits and functions | — | — |
| `num-iter` `0.1.45` | External iterators for generic mathematics | — | — |
| `num-rational` `0.4.2` | Rational numbers implementation for Rust | — | — |
| `num-traits` `0.2.19` | Numeric traits for generic mathematics | — | — |
| `num_enum` `0.7.6` | Procedural macros to make inter-operation between primitives and enums easier. | `starry-kernel` | — |
| `num_threads` `0.1.7` | A minimal library that determines the number of running threads for the current process. | — | — |
| `numeric-enum-macro` `0.2.0` | A declarative macro for type-safe enum-to-numbers conversion | `arm_vcpu` `axaddrspace` `x86_vcpu` | — |
| `object` `0.38.1` | A unified interface for reading and writing object file formats. | `axbuild` | — |
| `object` `0.39.0` | A unified interface for reading and writing object file formats. | — | — |
| `once_cell` `1.21.4` | Single assignment cells and lazy values. | — | — |
| `once_cell_polyfill` `1.70.2` | Polyfill for `OnceCell` stdlib feature for use with older MSRVs | — | — |
| `openssl-probe` `0.2.1` | A library for helping to find system-wide trust anchor ("root") certificate locations based on path… | — | — |
| `ordered-float` `4.6.0` | Wrappers for total ordering on floats | — | — |
| `ostool` `0.12.4` | A tool for operating system development | `axbuild` | — |
| `ouroboros` `0.18.5` | Easy, safe self-referential struct generation. | `starry-kernel` | — |
| `ouroboros_macro` `0.18.5` | Proc macro for ouroboros crate. | — | — |
| `page-table-generic` `0.7.1` | Generic page table walk and map. | — | — |
| `page_table_entry` `0.6.1` | — | — | — |
| `page_table_multiarch` `0.6.1` | — | — | — |
| `pci_types` `0.10.1` | Library with types for handling PCI devices | — | — |
| `pcie` `0.5.0` | A simple PCIE driver for enumerating devices. | `axvisor` | — |
| `pcie` `0.6.0` | A simple PCIE driver for enumerating devices. | — | — |
| `percent-encoding` `2.3.2` | Percent encoding and decoding | — | — |
| `percpu` `0.2.3-preview.1` | — | — | — |
| `percpu` `0.4.0` | — | — | — |
| `percpu_macros` `0.2.3-preview.1` | — | — | — |
| `percpu_macros` `0.4.0` | — | — | — |
| `pest` `2.8.6` | The Elegant Parser | — | — |
| `pest_generator` `2.8.6` | pest code generator | — | — |
| `pest_meta` `2.8.6` | pest meta language parser and validator | — | — |
| `phf` `0.11.3` | Runtime support for perfect hash function data structures | — | — |
| `phf_codegen` `0.11.3` | Codegen library for PHF types | — | — |
| `phf_generator` `0.11.3` | PHF generation logic | — | — |
| `phf_macros` `0.11.3` | Macros to generate types in the phf crate | — | — |
| `phytium-mci` `0.1.1` | — | `axvisor` | — |
| `pin-project-lite` `0.2.17` | A lightweight version of pin-project written with declarative macros. | — | — |
| `pin-utils` `0.1.0` | Utilities for pinning | — | — |
| `pkg-config` `0.3.32` | A library to run the pkg-config system tool at build time in order to be used in Cargo build script… | — | — |
| `plain` `0.2.3` | A small Rust library that allows users to reinterpret data of certain types safely. | — | — |
| `plotters` `0.3.7` | A Rust drawing library focus on data plotting for both WASM and native applications | — | — |
| `plotters-backend` `0.3.7` | Plotters Backend API | — | — |
| `plotters-svg` `0.3.7` | Plotters SVG backend | — | — |
| `portable-atomic` `1.13.1` | Portable atomic types including support for 128-bit atomics, atomic float, etc. | — | — |
| `portable-atomic-util` `0.2.6` | Synchronization primitives built with portable-atomic. | — | — |
| `powerfmt` `0.2.0` | `powerfmt` is a library that provides utilities for formatting values. This crate makes it signific… | — | — |
| `ppv-lite86` `0.2.21` | Cross-platform cryptography-oriented low-level SIMD library. | — | — |
| `prettyplease` `0.2.37` | A minimal `syn` syntax tree pretty-printer | `axvisor` | — |
| `ptr_meta` `0.1.4` | A radioactive stabilization of the ptr_meta rfc | — | — |
| `ptr_meta` `0.3.1` | A radioactive stabilization of the ptr_meta rfc | — | — |
| `quinn` `0.11.9` | Versatile QUIC transport protocol implementation | — | — |
| `quinn-proto` `0.11.14` | State machine for the QUIC transport protocol | — | — |
| `quinn-udp` `0.5.14` | UDP sockets with ECN information for the QUIC transport protocol | — | — |
| `r-efi` `5.3.0` | UEFI Reference Specification Protocol Constants and Definitions | — | — |
| `r-efi` `6.0.0` | UEFI Reference Specification Protocol Constants and Definitions | — | — |
| `radium` `0.7.0` | Portable interfaces for maybe-atomic types | — | — |
| `ranges-ext` `0.6.2` | A kernel. | — | — |
| `ratatui` `0.30.0` | A library that's all about cooking up terminal user interfaces | — | — |
| `ratatui-core` `0.1.0` | Core types and traits for the Ratatui Terminal UI library. Widget libraries should use this crate. … | — | — |
| `ratatui-crossterm` `0.1.0` | Crossterm backend for the Ratatui Terminal UI library. | — | — |
| `ratatui-macros` `0.7.0` | Macros for Ratatui | — | — |
| `ratatui-termwiz` `0.1.0` | Termwiz backend for the Ratatui Terminal UI library. | — | — |
| `ratatui-widgets` `0.3.0` | A collection of Ratatui widgets for building terminal user interfaces using Ratatui. | — | — |
| `raw-cpuid` `10.7.0` | A library to parse the x86 CPUID instruction, written in rust with no external dependencies. The im… | — | — |
| `raw-cpuid` `11.6.0` | A library to parse the x86 CPUID instruction, written in rust with no external dependencies. The im… | `ax-plat-x86-pc` `axplat-x86-qemu-q35` `x86_vcpu` | — |
| `rd-block` `0.1.1` | Driver Interface block definition. | `axplat-dyn` `axvisor` | — |
| `rdif-base` `0.7.0` | Driver Interface base definition. | — | — |
| `rdif-base` `0.8.0` | Driver Interface base definition. | — | — |
| `rdif-block` `0.7.0` | Driver Interface block definition. | `axvisor` | — |
| `rdif-clk` `0.5.0` | Driver Interface clk definition. | `axvisor` | — |
| `rdif-def` `0.2.2` | Driver Interface base definition. | — | — |
| `rdif-intc` `0.14.0` | Driver Interface of interrupt controller. | `axvisor` | — |
| `rdif-pcie` `0.2.0` | Driver Interface of interrupt controller. | — | — |
| `rdif-serial` `0.6.0` | Driver Interface base definition. | — | — |
| `rdrive` `0.20.0` | A dyn driver manager. | `axplat-dyn` `axvisor` | — |
| `rdrive-macros` `0.4.1` | macros for rdrive | — | — |
| `redox_syscall` `0.5.18` | A Rust library to access raw Redox system calls | — | — |
| `redox_syscall` `0.7.3` | A Rust library to access raw Redox system calls | — | — |
| `ref-cast` `1.0.25` | Safely cast &T to &U where the struct U contains a single field of type T. | — | — |
| `ref-cast-impl` `1.0.25` | Derive implementation for ref_cast::RefCast. | — | — |
| `regex` `1.12.3` | An implementation of regular expressions for Rust. This implementation uses finite automata and gua… | `axbuild` | — |
| `regex-automata` `0.4.14` | Automata construction and matching using regular expressions. | — | — |
| `rend` `0.4.2` | Endian-aware primitives for Rust | — | — |
| `reqwest` `0.13.2` | higher level HTTP client library | `axbuild` | — |
| `rgb` `0.8.53` | `struct RGB/RGBA/etc.` for sharing pixels between crates + convenience methods for color manipulati… | — | — |
| `riscv` `0.14.0` | Low level access to RISC-V processors | `ax-plat-riscv64-qemu-virt` `riscv-h` `riscv_vcpu` | — |
| `riscv` `0.16.0` | Low level access to RISC-V processors | `ax-cpu` `ax-page-table-multiarch` `ax-plat-riscv64-qemu-virt` `starry-kernel` | — |
| `riscv-decode` `0.2.3` | A simple library for decoding RISC-V instructions | `riscv_vcpu` | — |
| `riscv-macros` `0.2.0` | Procedural macros re-exported in `riscv` | — | — |
| `riscv-macros` `0.4.0` | Procedural macros re-exported in `riscv` | — | — |
| `riscv-pac` `0.2.0` | Low level access to RISC-V processors | — | — |
| `riscv-types` `0.1.0` | Low level access to RISC-V processors | — | — |
| `riscv_goldfish` `0.1.1` | System Real Time Clock (RTC) Drivers for riscv based on goldfish. | `ax-plat-riscv64-qemu-virt` | — |
| `riscv_plic` `0.2.0` | — | — | — |
| `rk3568_clk` `0.1.0` | — | `axvisor` | — |
| `rockchip-soc` `0.1.1` | — | `axvisor` | — |
| `rkyv` `0.7.46` | Zero-copy deserialization framework for Rust | — | — |
| `rlsf` `0.2.2` | Real-time dynamic memory allocator based on the TLSF algorithm | `ax-allocator` | — |
| `rockchip-pm` `0.4.1` | — | `axvisor` | — |
| `rstest` `0.17.0` | Rust fixture based test framework. It use procedural macro to implement fixtures and table based te… | `smoltcp` | — |
| `rstest_macros` `0.17.0` | Rust fixture based test framework. It use procedural macro to implement fixtures and table based te… | — | — |
| `rust_decimal` `1.41.0` | Decimal number implementation written in pure Rust suitable for financial and fixed-precision calcu… | — | — |
| `rustc-demangle` `0.1.27` | — | — | — |
| `rustc-hash` `2.1.2` | A speedy, non-cryptographic hashing algorithm used by rustc | — | — |
| `rustc_version` `0.4.1` | A library for querying the version of a installed rustc compiler | — | — |
| `rustsbi` `0.4.0` | Minimal RISC-V's SBI implementation library in Rust | `riscv_vcpu` | — |
| `rustsbi-macros` `0.0.2` | Proc-macros for RustSBI, a RISC-V SBI implementation library in Rust | — | — |
| `rustversion` `1.0.22` | Conditional compilation according to rustc compiler version | — | — |
| `ruzstd` `0.8.2` | A decoder for the zstd compression format | — | — |
| `ryu` `1.0.23` | Fast floating point to string conversion | — | — |
| `same-file` `1.0.6` | A simple crate for determining whether two file paths point to the same file. | — | — |
| `sbi-rt` `0.0.3` | Runtime library for supervisors to call RISC-V Supervisor Binary Interface (RISC-V SBI) | `ax-plat-riscv64-qemu-virt` `riscv_vcpu` | — |
| `sbi-spec` `0.0.7` | Definitions and constants in RISC-V Supervisor Binary Interface (RISC-V SBI) | `riscv_vcpu` | — |
| `schannel` `0.1.29` | Schannel bindings for rust, allowing SSL/TLS (e.g. https) without openssl | — | — |
| `schemars` `1.2.1` | Generate JSON Schemas from Rust code | `axbuild` `axvmconfig` | — |
| `scopeguard` `1.2.0` | A RAII scope guard that will run a given closure when it goes out of scope, even if the code betwee… | — | — |
| `sdmmc` `0.1.0` | — | `axvisor` | — |
| `seahash` `4.1.0` | A blazingly fast, portable hash function with proven statistical guarantees. | — | — |
| `security-framework` `3.7.0` | Security.framework bindings for macOS and iOS | — | — |
| `security-framework-sys` `2.17.0` | Apple `Security.framework` low-level FFI bindings | — | — |
| `serialport` `4.9.0` | A cross-platform low-level serial port library. | — | — |
| `shlex` `1.3.0` | Split a string into shell words, like Python's shlex. | — | — |
| `signal-hook` `0.3.18` | Unix signal handling | — | — |
| `signal-hook-registry` `1.4.8` | Backend crate for signal-hook | — | — |
| `simd-adler32` `0.3.9` | A SIMD-accelerated Adler-32 hash algorithm implementation. | — | — |
| `simdutf8` `0.1.5` | SIMD-accelerated UTF-8 validation. | — | — |
| `similar` `2.7.0` | A diff library for Rust | — | — |
| `simple-ahci` `0.1.1-preview.1` | — | `ax-driver-block` | — |
| `simple-sdmmc` `0.1.0` | — | `ax-driver-block` | — |
| `siphasher` `1.0.2` | SipHash-2-4, SipHash-1-3 and 128-bit variants in pure Rust | — | — |
| `slab` `0.4.12` | Pre-allocated storage for a uniform data type | `ax-fs-ng` `starry-kernel` | — |
| `some-serial` `0.3.1` | Unified serial driver collection for embedded and bare-metal environments | — | — |
| `someboot` `0.1.12` | Sparreal OS kernel | — | — |
| `somehal` `0.6.6` | A kernel. | `axplat-dyn` | — |
| `somehal-macros` `0.1.2` | A kernel. | — | — |
| `spin` `0.10.0` | Spin-based synchronization primitives | `arm_vcpu` `arm_vgic` `ax-fs` `ax-fs-ng` `ax-hal` `ax-net` `ax-net-ng` `ax-percpu` `ax-plat-aarch64-peripherals` `ax-posix-api` `ax-std` `ax-task` `axaddrspace` `axbacktrace` `axdevice` `axfs-ng-vfs` `axplat-dyn` `axpoll` `axvisor` `axvm` `riscv_vplic` `scope-local` `starry-kernel` `x86_vcpu` | — |
| `spin` `0.9.8` | Spin-based synchronization primitives | `ax-driver-net` `ax-fs-devfs` `ax-fs-ramfs` | — |
| `spin_on` `0.1.1` | A simple, inefficient Future executor | — | — |
| `spinning_top` `0.2.5` | A simple spinlock crate based on the abstractions provided by `lock_api`. | — | — |
| `spinning_top` `0.3.0` | A simple spinlock crate based on the abstractions provided by `lock_api`. | — | — |
| `stable_deref_trait` `1.2.1` | An unsafe marker trait for types like Box and Rc that dereference to a stable address even when mov… | — | — |
| `starry-fatfs` `0.4.1-preview.2` | — | `ax-fs-ng` | — |
| `static_assertions` `1.1.0` | Compile-time assertions to ensure that invariants are met. | — | — |
| `strsim` `0.10.0` | Implementations of string similarity metrics. Includes Hamming, Levenshtein, OSA, Damerau-Levenshte… | — | — |
| `strsim` `0.11.1` | Implementations of string similarity metrics. Includes Hamming, Levenshtein, OSA, Damerau-Levenshte… | — | — |
| `strum` `0.27.2` | Helpful macros for working with enums and strings | `ax-alloc` `ax-driver-input` `ax-errno` `starry-signal` | — |
| `strum` `0.28.0` | Helpful macros for working with enums and strings | `starry-kernel` | — |
| `strum_macros` `0.27.2` | Helpful macros for working with enums and strings | — | — |
| `strum_macros` `0.28.0` | Helpful macros for working with enums and strings | — | — |
| `subtle` `2.6.1` | Pure-Rust traits and utilities for constant-time cryptographic implementations. | — | — |
| `svgbobdoc` `0.3.0` | Renders ASCII diagrams in doc comments as SVG images. | — | — |
| `syscalls` `0.8.1` | A list of Linux system calls. | `starry-kernel` | — |
| `system-configuration` `0.7.0` | Bindings to SystemConfiguration framework for macOS | — | — |
| `system-configuration-sys` `0.6.0` | Low level bindings to SystemConfiguration framework for macOS | — | — |
| `tap` `1.0.1` | Generic extensions for tapping values in Rust | — | — |
| `tar` `0.4.45` | A Rust implementation of a TAR file reader and writer. This library does not currently handle compr… | `axbuild` | — |
| `tempfile` `3.27.0` | A library for managing temporary files and directories. | `axbuild` | — |
| `termcolor` `1.4.1` | A simple cross platform library for writing colored text to a terminal. | — | — |
| `terminfo` `0.9.0` | Terminal information. | — | — |
| `termwiz` `0.23.3` | Terminal Wizardry for Unix and Windows | — | — |
| `tftpd` `0.5.3` | Multithreaded TFTP server daemon | — | — |
| `thread_local` `1.1.9` | Per-object thread-local storage | — | — |
| `time` `0.3.47` | Date and time library. Fully interoperable with the standard library. Mostly compatible with #![no_… | — | — |
| `time-core` `0.1.8` | This crate is an implementation detail and should not be relied upon directly. | — | — |
| `time-macros` `0.2.27` | Procedural macros for the time crate. This crate is an implementation detail and should not be reli… | — | — |
| `tinystr` `0.7.6` | A small ASCII-only bounded length string representation. | — | — |
| `tinytemplate` `1.2.1` | Simple, lightweight template engine | — | — |
| `tinyvec` `1.11.0` | `tinyvec` provides 100% safe vec-like data structures. | — | — |
| `tinyvec_macros` `0.1.1` | Some macros for tiny containers | — | — |
| `trait-ffi` `0.2.11` | A Rust procedural macro library for creating and implementing extern fn with Trait. | `axklib` | — |
| `try-lock` `0.2.5` | A lightweight atomic lock. | — | — |
| `tungstenite` `0.28.0` | Lightweight stream-based WebSocket implementation | — | — |
| `twox-hash` `2.1.2` | A Rust implementation of the XXHash and XXH3 algorithms | — | — |
| `typeid` `1.0.3` | Const TypeId and non-'static TypeId | — | — |
| `typenum` `1.19.0` | Typenum is a Rust library for type-level numbers evaluated at compile time. It currently supports b… | — | — |
| `uart_16550` `0.4.0` | Minimal support for uart_16550 serial output. | `ax-plat-riscv64-qemu-virt` `axplat-x86-qemu-q35` | — |
| `uart_16550` `0.5.0` | Simple yet highly configurable low-level driver for 16550 UART devices, typically known and used as… | `ax-plat-loongarch64-qemu-virt` `ax-plat-riscv64-qemu-virt` `ax-plat-x86-pc` | — |
| `uboot-shell` `0.2.3` | A crate for communicating with u-boot | — | — |
| `ucd-trie` `0.1.7` | A trie for storing Unicode codepoint sets and maps. | — | — |
| `ucs2` `0.3.3` | UCS-2 decoding and encoding functions | — | — |
| `uefi` `0.36.1` | This crate makes it easy to develop Rust software that leverages safe, convenient, and performant a… | — | — |
| `uefi-macros` `0.19.0` | Procedural macros for the `uefi` crate. | — | — |
| `uefi-raw` `0.13.0` | Raw UEFI types and bindings for protocols, boot, and runtime services. This can serve as base for a… | — | — |
| `uguid` `2.2.1` | GUID (Globally Unique Identifier) no_std library | — | — |
| `uluru` `3.1.0` | A simple, fast, LRU cache implementation | `starry-kernel` | — |
| `unescaper` `0.1.8` | Unescape strings with escape sequences written out as literal characters. | — | — |
| `unicase` `2.9.0` | A case-insensitive wrapper around strings. | — | — |
| `unicode-bidi` `0.3.18` | Implementation of the Unicode Bidirectional Algorithm | — | — |
| `unicode-ident` `1.0.24` | Determine whether characters have the XID_Start or XID_Continue properties according to Unicode Sta… | — | — |
| `unicode-normalization` `0.1.25` | This crate provides functions for normalization of Unicode strings, including Canonical and Compati… | — | — |
| `unicode-segmentation` `1.13.2` | This crate provides Grapheme Cluster, Word and Sentence boundaries according to Unicode Standard An… | — | — |
| `unicode-truncate` `2.0.1` | Unicode-aware algorithm to pad or truncate `str` in terms of displayed width. | — | — |
| `unicode-width` `0.1.14` | Determine displayed width of `char` and `str` types according to Unicode Standard Annex #11 rules. | — | — |
| `unicode-width` `0.2.2` | Determine displayed width of `char` and `str` types according to Unicode Standard Annex #11 rules. | — | — |
| `unicode-xid` `0.2.6` | Determine whether characters have the XID_Start or XID_Continue properties according to Unicode Sta… | — | — |
| `unit-prefix` `0.5.2` | Format numbers with metric and binary unit prefixes | — | — |
| `untrusted` `0.9.0` | Safe, fast, zero-panic, zero-crashing, zero-allocation parsing of untrusted inputs in Rust. | — | — |
| `ureq` `3.3.0` | Simple, safe HTTP client | — | — |
| `ureq-proto` `0.6.0` | ureq support crate | — | — |
| `url` `2.5.2` | URL library for Rust, based on the WHATWG URL Standard | `smoltcp` | — |
| `utf-8` `0.7.6` | Incremental, zero-copy UTF-8 decoding with error handling | — | — |
| `utf16_iter` `1.0.5` | Iterator by char over potentially-invalid UTF-16 in &[u16] | — | — |
| `utf8-width` `0.1.8` | To determine the width of a UTF-8 character by providing its first byte. | — | — |
| `utf8-zero` `0.8.1` | Zero-copy, incremental UTF-8 decoding with error handling | — | — |
| `utf8_iter` `1.0.4` | Iterator by char over potentially-invalid UTF-8 in &[u8] | — | — |
| `utf8parse` `0.2.2` | Table-driven UTF-8 parser | — | — |
| `uuid` `1.23.0` | A library to generate and parse UUIDs. | — | — |
| `valuable` `0.1.1` | Object-safe value inspection, used to pass un-typed structured data across trait-object boundaries. | — | — |
| `vcpkg` `0.2.15` | A library to find native dependencies in a vcpkg tree at build time in order to be used in Cargo bu… | — | — |
| `virtio-drivers` `0.7.5` | VirtIO guest drivers. | `ax-driver-pci` `ax-driver-virtio` | — |
| `volatile` `0.3.0` | — | — | — |
| `volatile` `0.4.6` | A simple volatile wrapper type | — | — |
| `volatile` `0.6.1` | — | — | — |
| `volatile-macro` `0.6.0` | — | — | — |
| `vtparse` `0.6.2` | Low level escape sequence parser | — | — |
| `walkdir` `2.5.0` | Recursively walk a directory. | — | — |
| `want` `0.3.1` | Detect when another Future wants a result. | — | — |
| `wasi` `0.11.1+wasi-snapshot-preview1` | Experimental WASI API bindings for Rust | — | — |
| `wasip2` `1.0.2+wasi-0.2.9` | WASIp2 API bindings for Rust | — | — |
| `wasip3` `0.4.0+wasi-0.3.0-rc-2026-01-06` | WASIp3 API bindings for Rust | — | — |
| `wasm-bindgen` `0.2.117` | Easy support for interacting between JS and Rust. | — | — |
| `wasm-bindgen-macro` `0.2.117` | Definition of the `#[wasm_bindgen]` attribute, an internal dependency | — | — |
| `wasm-bindgen-macro-support` `0.2.117` | Implementation APIs for the `#[wasm_bindgen]` attribute | — | — |
| `wasm-encoder` `0.244.0` | A low-level WebAssembly encoder. | — | — |
| `wasm-metadata` `0.244.0` | Read and manipulate WebAssembly metadata | — | — |
| `wasm-streams` `0.5.0` | Bridging between web streams and Rust streams using WebAssembly | — | — |
| `wasmparser` `0.244.0` | A simple event-driven library for parsing WebAssembly binary files. | — | — |
| `weak-map` `0.1.2` | BTreeMap with weak references | `starry-kernel` `starry-process` | — |
| `web-sys` `0.3.94` | Bindings for all Web APIs, a procedurally generated crate from WebIDL | — | — |
| `web-time` `1.1.0` | Drop-in replacement for std::time for Wasm in browsers | — | — |
| `webpki-root-certs` `1.0.6` | Mozilla trusted certificate authorities in self-signed X.509 format for use with crates other than … | — | — |
| `webpki-roots` `1.0.6` | Mozilla's CA root certificates for use with webpki | — | — |
| `wezterm-bidi` `0.2.3` | The Unicode Bidi Algorithm (UBA) | — | — |
| `wezterm-blob-leases` `0.1.1` | Manage image blob caching/leasing for wezterm | — | — |
| `wezterm-color-types` `0.3.0` | Types for working with colors | — | — |
| `wezterm-dynamic` `0.2.1` | config serialization for wezterm via dynamic json-like data values | — | — |
| `wezterm-input-types` `0.1.0` | config serialization for wezterm via dynamic json-like data values | — | — |
| `winapi` `0.3.9` | Raw FFI bindings for all of Windows API. | — | — |
| `winapi-util` `0.1.11` | A dumping ground for high level safe wrappers over windows-sys. | — | — |
| `winnow` `0.7.15` | A byte-oriented, zero-copy, parser combinators library | — | — |
| `winnow` `1.0.1` | A byte-oriented, zero-copy, parser combinators library | — | — |
| `wit-bindgen` `0.51.0` | Rust bindings generator and runtime support for WIT and the component model. Used when compiling Ru… | — | — |
| `wit-bindgen-core` `0.51.0` | Low-level support for bindings generation based on WIT files for use with `wit-bindgen-cli` and oth… | — | — |
| `wit-bindgen-rust` `0.51.0` | Rust bindings generator for WIT and the component model, typically used through the `wit-bindgen` c… | — | — |
| `wit-bindgen-rust-macro` `0.51.0` | Procedural macro paired with the `wit-bindgen` crate. | — | — |
| `wit-component` `0.244.0` | Tooling for working with `*.wit` and component files together. | — | — |
| `wit-parser` `0.244.0` | Tooling for parsing `*.wit` files and working with their contents. | — | — |
| `write16` `1.0.0` | A UTF-16 analog of the Write trait | — | — |
| `writeable` `0.5.5` | A more efficient alternative to fmt::Display | — | — |
| `wyz` `0.5.1` | myrrlyn’s utility collection | — | — |
| `x2apic` `0.5.0` | A Rust interface to the x2apic interrupt architecture. | `ax-plat-x86-pc` `axplat-x86-qemu-q35` | — |
| `x86` `0.52.0` | Library to program x86 (amd64) hardware. Contains x86 specific data structure descriptions, data-ta… | `ax-cpu` `ax-page-table-multiarch` `ax-percpu` `ax-plat-x86-pc` `axaddrspace` `axplat-x86-qemu-q35` `starry-kernel` `x86_vcpu` | — |
| `x86_64` `0.15.4` | Support for x86_64 specific instructions, registers, and structures. | `ax-cpu` `ax-page-table-entry` `ax-plat-x86-pc` `axplat-x86-qemu-q35` `x86_vcpu` | — |
| `x86_rtc` `0.1.1` | System Real Time Clock (RTC) Drivers for x86_64 based on CMOS. | `ax-plat-x86-pc` `axplat-x86-qemu-q35` | — |
| `xattr` `1.6.1` | unix extended filesystem attributes | — | — |
| `xi-unicode` `0.3.0` | Unicode utilities useful for text editing, including a line breaking iterator. | — | — |
| `xz2` `0.1.7` | Rust bindings to liblzma providing Read/Write streams as well as low-level in-memory encoding/decod… | `axbuild` | — |
| `yansi` `1.0.1` | A dead simple ANSI terminal color painting library. | — | — |
| `yoke` `0.7.5` | Abstraction allowing borrowed data to be carried along with the backing data it borrows from | — | — |
| `zero` `0.1.3` | A Rust library for zero-allocation parsing of binary data. | — | — |
| `zerocopy` `0.7.35` | Utilities for zero-copy parsing and serialization | — | — |
| `zerocopy` `0.8.48` | Zerocopy makes zero-cost memory manipulation effortless. We write "unsafe" so you don't have to. | `starry-kernel` | — |
| `zerofrom` `0.1.7` | ZeroFrom trait for constructing | — | — |
| `zeroize` `1.8.2` | Securely clear secrets from memory with a simple trait built on stable Rust primitives which guaran… | — | — |
| `zerovec` `0.10.4` | Zero-copy vector backed by a byte array | — | — |
| `zmij` `1.0.21` | A double-to-string conversion algorithm based on Schubfach and yy | — | — |


#### 序列化/数据格式

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `base64` `0.13.1` | encodes and decodes base64 as bytes or utf8 | — | — |
| `base64` `0.22.1` | encodes and decodes base64 as bytes or utf8 | — | — |
| `byteorder` `1.5.0` | Library for reading/writing numbers in big-endian and little-endian. | `smoltcp` | — |
| `bytes` `1.11.1` | Types and traits for working with bytes | — | — |
| `hex` `0.4.3` | Encoding and decoding data into/from hexadecimal representation. | — | — |
| `serde` `1.0.228` | A generic serialization/deserialization framework | `axbuild` `axdevice_base` `axvmconfig` | — |
| `serde_core` `1.0.228` | Serde traits only, with no support for derive -- use the `serde` crate instead | — | — |
| `serde_derive` `1.0.228` | Macros 1.1 implementation of #[derive(Serialize, Deserialize)] | — | — |
| `serde_derive_internals` `0.29.1` | AST representation used by Serde derive macros. Unstable. | — | — |
| `serde_json` `1.0.149` | A JSON serialization file format | `axbuild` | — |
| `serde_path_to_error` `0.1.20` | Path to the element that failed to deserialize | — | — |
| `serde_repr` `0.1.20` | Derive Serialize and Deserialize that delegates to the underlying repr of a C-like enum. | `axvmconfig` | — |
| `serde_spanned` `1.1.1` | Serde-compatible spanned Value | — | — |
| `serde_urlencoded` `0.7.1` | `x-www-form-urlencoded` meets Serde | — | — |
| `toml` `0.9.12+spec-1.1.0` | A native Rust encoder and decoder of TOML-formatted files and streams. Provides implementations of … | `axvisor` `axvmconfig` | — |
| `toml` `1.1.2+spec-1.1.0` | A native Rust encoder and decoder of TOML-formatted files and streams. Provides implementations of … | `axbuild` | — |
| `toml_datetime` `0.6.11` | A TOML-compatible datetime type | — | — |
| `toml_datetime` `0.7.5+spec-1.1.0` | A TOML-compatible datetime type | — | — |
| `toml_datetime` `1.1.1+spec-1.1.0` | A TOML-compatible datetime type | — | — |
| `toml_edit` `0.22.27` | Yet another format-preserving TOML parser. | `ax-config-gen` | — |
| `toml_edit` `0.25.10+spec-1.1.0` | Yet another format-preserving TOML parser. | — | — |
| `toml_parser` `1.1.2+spec-1.1.0` | Yet another format-preserving TOML parser. | — | — |
| `toml_write` `0.1.2` | A low-level interface for writing out TOML | — | — |
| `toml_writer` `1.1.1+spec-1.1.0` | A low-level interface for writing out TOML | — | — |


#### 异步/并发

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `async-channel` `2.5.0` | Async multi-producer multi-consumer channel | `ax-net-ng` | — |
| `async-trait` `0.1.89` | Type erasure for async trait methods | `ax-net-ng` | — |
| `crossbeam-channel` `0.5.15` | Multi-producer multi-consumer channels for message passing | — | — |
| `crossbeam-deque` `0.8.6` | Concurrent work-stealing deque | — | — |
| `crossbeam-epoch` `0.9.18` | Epoch-based garbage collection | — | — |
| `crossbeam-utils` `0.8.21` | Utilities for concurrent programming | — | — |
| `futures` `0.3.32` | An implementation of futures and streams featuring zero allocations, composability, and iterator-li… | `axpoll` | — |
| `futures-channel` `0.3.32` | Channels for asynchronous communication using futures-rs. | — | — |
| `futures-core` `0.3.32` | The core traits and types in for the `futures` library. | — | — |
| `futures-executor` `0.3.32` | Executors for asynchronous tasks based on the futures-rs library. | — | — |
| `futures-io` `0.3.32` | The `AsyncRead`, `AsyncWrite`, `AsyncSeek`, and `AsyncBufRead` traits for the futures-rs library. | — | — |
| `futures-macro` `0.3.32` | The futures-rs procedural macro implementations. | — | — |
| `futures-sink` `0.3.32` | The asynchronous `Sink` trait for the futures-rs library. | — | — |
| `futures-task` `0.3.32` | Tools for working with tasks. | — | — |
| `futures-timer` `3.0.3` | Timeouts for futures. | — | — |
| `futures-util` `0.3.32` | Common utilities and extension traits for the futures-rs library. | `ax-task` `axbuild` | — |
| `parking_lot` `0.12.5` | More compact and efficient implementations of the standard synchronization primitives. | — | — |
| `parking_lot_core` `0.9.12` | An advanced API for creating custom synchronization primitives. | — | — |
| `rayon` `1.11.0` | Simple work-stealing parallelism for Rust | — | — |
| `rayon-core` `1.13.0` | Core APIs for Rayon | — | — |
| `tokio` `1.51.0` | An event-driven, non-blocking I/O platform for writing asynchronous I/O backed applications. | `axbuild` `axpoll` `axvisor` `starryos` `tg-xtask` | — |
| `tokio-macros` `2.7.0` | Tokio's proc macros. | — | — |
| `tokio-rustls` `0.26.4` | Asynchronous TLS/SSL streams for Tokio using Rustls. | — | — |
| `tokio-serial` `5.4.5` | A serial port implementation for tokio | — | — |
| `tokio-tungstenite` `0.28.0` | Tokio binding for Tungstenite, the Lightweight stream-based WebSocket implementation | — | — |
| `tokio-util` `0.7.18` | Additional utilities for working with Tokio. | — | — |
| `wasm-bindgen-futures` `0.4.67` | Bridging the gap between Rust Futures and JavaScript Promises | — | — |


#### 数据结构/算法

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `arrayvec` `0.7.6` | A vector with fixed capacity, backed by an array (it can be stored on the stack too). Implements fi… | `ax-page-table-multiarch` | — |
| `bitvec` `1.0.1` | Addresses memory by bits, for packed collections and bitfields | — | — |
| `hashbrown` `0.12.3` | A Rust port of Google's SwissTable hash map | — | — |
| `hashbrown` `0.14.5` | A Rust port of Google's SwissTable hash map | `axvisor` | — |
| `hashbrown` `0.15.5` | A Rust port of Google's SwissTable hash map | — | — |
| `hashbrown` `0.16.1` | A Rust port of Google's SwissTable hash map | `ax-net-ng` `axfs-ng-vfs` `starry-kernel` | — |
| `indexmap` `2.13.1` | A hash table with consistent order and fast iteration. | — | — |
| `lru` `0.16.3` | A LRU cache implementation | `ax-fs-ng` | — |
| `lru-slab` `0.1.2` | Pre-allocated storage with constant-time LRU tracking | — | — |
| `smallvec` `1.15.1` | 'Small vector' optimization: store up to a small number of items on the stack | `ax-driver` `axfs-ng-vfs` | — |


#### 日志/错误

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `anyhow` `1.0.102` | Flexible concrete Error type built on std::error::Error | `axbuild` `axplat-dyn` `axvisor` `starryos` `tg-xtask` | — |
| `crc-catalog` `2.4.0` | Catalog of CRC algorithms (generated from http://reveng.sourceforge.net/crc-catalogue) expressed as… | — | — |
| `env_logger` `0.10.2` | A logging implementation for `log` which is configured via an environment variable. | `smoltcp` | — |
| `env_logger` `0.11.10` | A logging implementation for `log` which is configured via an environment variable. | `axbuild` `axvmconfig` | — |
| `log` `0.4.29` | A lightweight logging facade for Rust | `arm_vcpu` `arm_vgic` `ax-alloc` `ax-cpu` `ax-display` `ax-dma` `ax-driver` `ax-driver-block` `ax-driver-net` `ax-driver-virtio` `ax-driver-vsock` `ax-errno` `ax-fs` `ax-fs-devfs` `ax-fs-ng` `ax-fs-ramfs` `ax-fs-vfs` `ax-hal` `ax-input` `ax-ipi` `ax-log` `ax-mm` `ax-net` `ax-net-ng` `ax-page-table-multiarch` `ax-plat-aarch64-bsta1000b` `ax-plat-aarch64-peripherals` `ax-plat-aarch64-phytium-pi` `ax-plat-aarch64-qemu-virt` `ax-plat-aarch64-raspi` `ax-plat-loongarch64-qemu-virt` `ax-plat-riscv64-qemu-virt` `ax-plat-x86-pc` `ax-task` `axaddrspace` `axbacktrace` `axbuild` `axdevice` `axfs-ng-vfs` `axplat-dyn` `axplat-x86-qemu-q35` `axvisor` `axvm` `axvmconfig` `fxmac_rs` `riscv-h` `riscv_vcpu` `riscv_vplic` `rsext4` `smoltcp` `starry-signal` `x86_vcpu` `x86_vlapic` | — |
| `thiserror` `1.0.69` | derive(Error) | — | — |
| `thiserror` `2.0.18` | derive(Error) | — | — |
| `thiserror-impl` `1.0.69` | Implementation detail of the `thiserror` crate | — | — |
| `thiserror-impl` `2.0.18` | Implementation detail of the `thiserror` crate | — | — |
| `tracing` `0.1.44` | Application-level tracing for Rust. | `axbuild` | — |
| `tracing-attributes` `0.1.31` | Procedural macro attributes for automatically instrumenting functions. | — | — |
| `tracing-core` `0.1.36` | Core primitives for application-level tracing. | — | — |
| `tracing-log` `0.2.0` | Provides compatibility between `tracing` and the `log` crate. | `axbuild` | — |
| `tracing-subscriber` `0.3.23` | Utilities for implementing and composing `tracing` subscribers. | `axbuild` | — |


#### 系统/平台

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `cc` `1.2.58` | A build-time dependency for Cargo build scripts to assist in invoking the native C compiler to comp… | — | — |
| `cmake` `0.1.58` | A build dependency for running `cmake` to build a native library | — | — |
| `libc` `0.2.184` | Raw FFI bindings to platform libraries like libc. | `smoltcp` | — |
| `linux-raw-sys` `0.12.1` | Generated bindings for Linux's userspace API | `axpoll` `starry-kernel` `starry-signal` | — |
| `linux-raw-sys` `0.4.15` | Generated bindings for Linux's userspace API | — | — |
| `memchr` `2.8.0` | Provides extremely fast (uses SIMD on x86_64, aarch64 and wasm32) routines for 1, 2 or 3 byte searc… | `ax-io` | — |
| `nix` `0.26.4` | Rust friendly bindings to *nix APIs | — | — |
| `nix` `0.29.0` | Rust friendly bindings to *nix APIs | — | — |
| `rustix` `0.38.44` | Safe Rust bindings to POSIX/Unix/Linux/Winsock-like syscalls | — | — |
| `rustix` `1.1.4` | Safe Rust bindings to POSIX/Unix/Linux/Winsock-like syscalls | — | — |
| `smccc` `0.2.2` | Functions and constants for the Arm SMC Calling Convention (SMCCC) 1.4 and Arm Power State Coordina… | — | — |
| `winapi-i686-pc-windows-gnu` `0.4.0` | Import libraries for the i686-pc-windows-gnu target. Please don't use this crate directly, depend o… | — | — |
| `winapi-x86_64-pc-windows-gnu` `0.4.0` | Import libraries for the x86_64-pc-windows-gnu target. Please don't use this crate directly, depend… | — | — |
| `windows-core` `0.62.2` | Core type support for COM and Windows | — | — |
| `windows-implement` `0.60.2` | The implement macro for the Windows crates | — | — |
| `windows-interface` `0.59.3` | The interface macro for the Windows crates | — | — |
| `windows-link` `0.2.1` | Linking for Windows | — | — |
| `windows-registry` `0.6.1` | Windows registry | — | — |
| `windows-result` `0.4.1` | Windows error handling | — | — |
| `windows-sys` `0.45.0` | Rust for Windows | — | — |
| `windows-sys` `0.52.0` | Rust for Windows | — | — |
| `windows-sys` `0.59.0` | Rust for Windows | — | — |
| `windows-sys` `0.60.2` | Rust for Windows | — | — |
| `windows-sys` `0.61.2` | Rust for Windows | — | — |
| `windows-targets` `0.42.2` | Import libs for Windows | — | — |
| `windows-targets` `0.52.6` | Import libs for Windows | — | — |
| `windows-targets` `0.53.5` | Import libs for Windows | — | — |
| `windows_aarch64_gnullvm` `0.42.2` | Import lib for Windows | — | — |
| `windows_aarch64_gnullvm` `0.52.6` | Import lib for Windows | — | — |
| `windows_aarch64_gnullvm` `0.53.1` | Import lib for Windows | — | — |
| `windows_aarch64_msvc` `0.42.2` | Import lib for Windows | — | — |
| `windows_aarch64_msvc` `0.52.6` | Import lib for Windows | — | — |
| `windows_aarch64_msvc` `0.53.1` | Import lib for Windows | — | — |
| `windows_i686_gnu` `0.42.2` | Import lib for Windows | — | — |
| `windows_i686_gnu` `0.52.6` | Import lib for Windows | — | — |
| `windows_i686_gnu` `0.53.1` | Import lib for Windows | — | — |
| `windows_i686_gnullvm` `0.52.6` | Import lib for Windows | — | — |
| `windows_i686_gnullvm` `0.53.1` | Import lib for Windows | — | — |
| `windows_i686_msvc` `0.42.2` | Import lib for Windows | — | — |
| `windows_i686_msvc` `0.52.6` | Import lib for Windows | — | — |
| `windows_i686_msvc` `0.53.1` | Import lib for Windows | — | — |
| `windows_x86_64_gnu` `0.42.2` | Import lib for Windows | — | — |
| `windows_x86_64_gnu` `0.52.6` | Import lib for Windows | — | — |
| `windows_x86_64_gnu` `0.53.1` | Import lib for Windows | — | — |
| `windows_x86_64_gnullvm` `0.42.2` | Import lib for Windows | — | — |
| `windows_x86_64_gnullvm` `0.52.6` | Import lib for Windows | — | — |
| `windows_x86_64_gnullvm` `0.53.1` | Import lib for Windows | — | — |
| `windows_x86_64_msvc` `0.42.2` | Import lib for Windows | — | — |
| `windows_x86_64_msvc` `0.52.6` | Import lib for Windows | — | — |
| `windows_x86_64_msvc` `0.53.1` | Import lib for Windows | — | — |


#### 网络/协议

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `axum` `0.8.8` | Web framework that focuses on ergonomics and modularity | — | — |
| `axum-core` `0.5.6` | Core types and traits for axum | — | — |
| `http` `1.4.0` | A set of types for representing HTTP requests and responses. | — | — |
| `http-body` `1.0.1` | Trait representing an asynchronous, streaming, HTTP request or response body. | — | — |
| `http-body-util` `0.1.3` | Combinators and adapters for HTTP request or response bodies. | — | — |
| `http-range-header` `0.4.2` | No-dep range header parser | — | — |
| `httparse` `1.10.1` | A tiny, safe, speedy, zero-copy HTTP/1.x parser. | — | — |
| `httpdate` `1.0.3` | HTTP date parsing and formatting | — | — |
| `hyper` `1.9.0` | A protective and efficient HTTP library for all. | — | — |
| `hyper-rustls` `0.27.7` | Rustls+hyper integration for pure rust HTTPS | — | — |
| `hyper-util` `0.1.20` | hyper utilities | — | — |
| `mio` `1.2.0` | Lightweight non-blocking I/O. | — | — |
| `mio-serial` `5.0.6` | A serial port implementation for mio | — | — |
| `mmio-api` `0.2.1` | Memory-mapped I/O abstraction API for OS kernel development. | — | — |
| `rustls` `0.23.37` | Rustls is a modern TLS library written in Rust. | — | — |
| `rustls-native-certs` `0.8.3` | rustls-native-certs allows rustls to use the platform native certificate store | — | — |
| `rustls-pki-types` `1.14.0` | Shared types for the rustls PKI ecosystem | — | — |
| `rustls-platform-verifier` `0.6.2` | rustls-platform-verifier supports verifying TLS certificates in rustls with the operating system ve… | — | — |
| `rustls-platform-verifier-android` `0.1.1` | The internal JVM support component of the rustls-platform-verifier crate. You shouldn't depend on t… | — | — |
| `rustls-webpki` `0.103.10` | Web PKI X.509 Certificate Verification. | — | — |
| `signal-hook-mio` `0.2.5` | MIO support for signal-hook | — | — |
| `smoltcp` `0.12.0` | — | — | — |
| `socket2` `0.6.3` | Utilities for handling networking sockets with a maximal amount of configuration possible intended. | — | — |
| `starry-smoltcp` `0.12.1-preview.1` | A TCP/IP stack designed for bare-metal, real-time systems without a heap. | `ax-net` `ax-net-ng` | — |
| `termios` `0.3.3` | Safe bindings for the termios library. | — | — |
| `tower` `0.5.3` | Tower is a library of modular and reusable components for building robust clients and servers. | — | — |
| `tower-http` `0.6.8` | Tower middleware and utilities for HTTP clients and servers | — | — |
| `tower-layer` `0.3.3` | Decorates a `Service` to allow easy composition between `Service`s. | — | — |
| `tower-service` `0.3.3` | Trait representing an asynchronous, request / response based, client or server. | — | — |


#### 设备树/固件

| 外部组件（name version） | 简介（≤100字） | 直接依赖该外部的内部组件 | 该外部直接依赖的内部组件 |
|--------------------------|----------------|---------------------------|---------------------------|
| `fdt-edit` `0.2.3` | A high-level library for creating, editing, and encoding Flattened Device Tree (FDT) structures | `axplat-dyn` | — |
| `fdt-parser` `0.4.19` | A crate for parsing FDT | `ax-hal` `axvisor` | — |
| `fdt-raw` `0.3.0` | A low-level, no-std compatible library for parsing Flattened Device Tree (FDT) binary files | — | — |
| `fitimage` `0.1.3` | A Rust library for creating U-Boot compatible FIT images | — | — |
| `kernel-elf-parser` `0.3.4` | An lightweight ELF parser that parses ELF files and converts them into information needed for kerne… | `starry-kernel` | — |
| `multiboot` `0.8.0` | Library to access multiboot structures. | `ax-plat-x86-pc` `axplat-x86-qemu-q35` | — |
| `vm-fdt` `0.3.0` | Crate for writing Flattened Devicetree blobs | — | — |
| `xmas-elf` `0.9.1` | Library for parsing and navigating ELF data; zero-allocation, type-safe. | `starry-kernel` | — |
