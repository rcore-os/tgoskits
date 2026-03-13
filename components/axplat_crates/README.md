# axplat_crates

Reusable crates used for [ArceOS](https://github.com/arceos-org/arceos) Hardware Abstraction Layer (HAL).

## Library crates

* [axplat](./axplat)
* [axplat-macros](./axplat-macros)
* [axplat-aarch64-peripherals](./platforms/axplat-aarch64-peripherals)

## Platform-specific crates

* [x] [axplat-x86-pc](./platforms/axplat-x86-pc)
* [x] [axplat-riscv-qemu-virt](./platforms/axplat-riscv-qemu-virt)
* [x] [axplat-aarch64-qemu-virt](./platforms/axplat-aarch64-qemu-virt)
* [x] [axplat-aarch64-raspi](./platforms/axplat-aarch64-raspi)
* [x] [axplat-aarch64-phytium-pi](./platforms/axplat-aarch64-phytium-pi)
* [x] [axplat-aarch64-bsta1000b](./platforms/axplat-aarch64-bsta1000b)
* [x] [axplat-loongarch64-qemu-virt](./platforms/axplat-loongarch64-qemu-virt)

## Utility crate

* [cargo-axplat](./cargo-axplat): A cargo subcommand to manage hardware platform packages using [axplat](./axplat).
