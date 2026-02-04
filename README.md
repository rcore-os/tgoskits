<!-- <div align="center">

<img src="https://arceos-hypervisor.github.io/doc/assets/logo.svg" alt="axvisor-logo" width="64">

</div> -->

<h1 align="center">AxVisor</h1>

<p align="center">A unified modular Type I hypervisor</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/network)
[![license](https://img.shields.io/github/license/arceos-hypervisor/axvisor)](https://github.com/arceos-hypervisor/axvisor/blob/master/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

AxVisor is a Hypervisor implemented based on the ArceOS kernel. Its goal is to leverage the basic operating system functionalities provided by ArceOS as a foundation to implement a lightweight unified modular Hypervisor.

- **Unified** means using the same codebase to support three architectures—x86_64, Arm (aarch64), and RISC-V—maximizing the reuse of architecture-agnostic code and simplifying development and maintenance costs.

- **Modular** means that the Hypervisor's functionalities are decomposed into multiple independently usable components. Each component implements a specific function, and components communicate through standardized interfaces to achieve decoupling and reusability.

## Architecture

The software architecture of AxVisor is divided into five layers as shown in the diagram below. Each box represents an independent component, and components communicate with each other through standard interfaces. The complete architecture description can be found in the [documentation](https://arceos-hypervisor.github.io/axvisorbook/docs/overview).

![Architecture](https://arceos-hypervisor.github.io/doc/assets/arceos-hypervisor-architecture.png)

## Hardware Platforms

AxVisor has been verified on multiple hardware platforms, covering extensive support from virtualization environments to actual physical devices. To facilitate rapid user deployment, we provide one-click build scripts for each platform in the [axvisor-guest](https://github.com/arceos-hypervisor/axvisor-guest) repository, which can automatically generate corresponding image files.

| Platform Name | Architecture Support | Key Features |
|---------------|---------------------|--------------|
| QEMU | ARM64, x86_64 | Virtualization platform, supports multiple architectures, for development and testing |
| Orange Pi 5 Plus | ARM64 | Development board based on Rockchip RK3588, high-performance ARM platform |
| Phytium Pi | ARM64 | Development board based on Phytium E2000Q processor, domestic ARM platform |
| ROC-RK3568-PC | ARM64 | Development board based on Rockchip RK3568, suitable for industrial applications |
| EVM3588 | ARM64 | Evaluation board based on Rockchip RK3588, enterprise-level applications |

## Guest Systems

AxVisor supports multiple operating systems as guests, with good compatibility from lightweight microkernels to mature macrokernel systems. To simplify the user deployment process, we provide one-click build scripts for different guest systems in the [axvisor-guest](https://github.com/arceos-hypervisor/axvisor-guest) repository, which can quickly generate adapted guest images.

| Guest System | System Type | Architecture Support | Feature Description |
|--------------|-------------|---------------------|-------------------|
| [ArceOS](https://github.com/arceos-org/arceos) | Unikernel | ARM64, x86_64, RISC-V | Rust-based componentized operating system, lightweight and high-performance |
| [Starry-OS](https://github.com/Starry-OS) | Macrokernel OS | ARM64, x86_64 | Real-time operating system for embedded scenarios |
| [NimbOS](https://github.com/equation314/nimbos) | RTOS System | ARM64, x86_64, RISC-V | Concise Unix-like system, supports POSIX interface |
| Linux | Macrokernel OS | ARM64, x86_64, RISC-V | Mature and stable general-purpose operating system, rich software ecosystem |

# Build

AxVisor is built based on the Rust ecosystem, providing complete project build, configuration management, and debugging support through the extended xtask toolchain, offering developers a unified and efficient development experience.

## Build Environment

First, in a Linux environment, you need to install basic development tool packages such as `libssl-dev gcc libudev-dev pkg-config`.

Second, AxVisor is written in the Rust programming language, so you need to install the Rust development environment according to the official Rust website instructions, and use the `cargo install cargo-binutils` command to install [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) to use tools like `rust-objcopy` and `rust-objdump`.

> If necessary, you may also need to install [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) to build guest applications.

## Configuration Files

AxVisor uses a layered configuration system, including hardware platform configuration and guest configuration, both in TOML format.

### Hardware Platform Configuration

Hardware platform configuration files are located in the `configs/board/` directory, with each configuration file corresponding to a development board (or QEMU platform architecture) that we have verified. They specify the target architecture, feature sets, driver support, log levels, and build options.

> The guest configuration item `vm_configs` is not specified by default and needs to be specified in actual use!

### Guest Configuration

Guest configuration files are located in the `configs/vms/` directory, defining the runtime parameters of guests, including basic information, kernel configuration, memory regions, and device configuration details.

The configuration file naming format is `<os>-<arch>-board_or_cpu-smpx`, where `<os>` is the guest system name (such as `arceos`, `linux`, `nimbos`), `<arch>` is the architecture (such as `aarch64`, `x86_64`, `riscv64`), `board_or_cpu` is the hardware development board or CPU name, and `smpx` is the number of CPUs allocated to the guest.

## Compilation

AxVisor uses the xtask tool for build management, supporting multiple hardware platforms and configuration options. For a quick build and run of AxVisor, please refer to the [Quick Start](https://arceos-hypervisor.github.io/axvisorbook/docs/category/quickstart) chapter in the configuration documentation.

1. **Generate Configuration**: Use `cargo xtask defconfig <board_name>` to select the target hardware platform configuration from the `configs/board/` directory. This command copies the corresponding board-level configuration to `.build.toml` as the build configuration.

2. **Modify Configuration**: Use `cargo xtask menuconfig` to launch the interactive configuration interface, where you can adjust the target architecture, feature sets, log levels, and other parameters.

3. **Execute Build**: Use `cargo xtask build` to compile the project according to the `.build.toml` configuration file, generating the target platform binary file.

# Contributing

Welcome to fork this repository and submit pull requests. The existence and development of this project is thanks to the support of all contributors.

<a href="https://github.com/arceos-hypervisor/axvisor/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=arceos-hypervisor/axvisor" />
</a>

You are also welcome to scan the QR code below to join the discussion group (please send a note with: AxVisor). We look forward to consulting on issues, exchanging experiences, and receiving feedback suggestions.

![group](https://arceos-hypervisor.github.io/axvisorbook/assets/images/group-c0e9fb6c8a7720a1f7eb55d3f4f40b4c.png)

# License

Axvisor is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.