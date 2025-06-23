# axplat-aarch64-peripherals

[![Crates.io](https://img.shields.io/crates/v/axplat-aarch64-peripherals)](https://crates.io/crates/axplat-aarch64-peripherals)
[![Docs.rs](https://docs.rs/axplat-aarch64-peripherals/badge.svg)](https://docs.rs/axplat-aarch64-peripherals)
[![CI](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axplat_crates/actions/workflows/ci.yml)

Common peripheral drivers for ARM64 platforms.

It is [axplat](https://github.com/arceos-org/axplat_crates/tree/main/axplat)-compatible and can be used to implement the hardware
abstraction layer (HAL) for diverse ARM64 platforms.

It includes:

- PL011 UART driver.
- PL031 Real Time Clock (RTC) driver.
- GICv2 (Generic Interrupt Controller) driver.
- Generic Timer related functions.
- PSCI (Power State Coordination Interface) calls.
