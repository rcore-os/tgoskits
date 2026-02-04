<!-- <div align="center">

<img src="https://arceos-hypervisor.github.io/doc/assets/logo.svg" alt="axvisor-logo" width="64">

</div> -->

<h1 align="center">AxVisor</h1>

<p align="center">一个统一组件化的 Type I 类型的虚拟机管理程序</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/network)
[![license](https://img.shields.io/github/license/arceos-hypervisor/axvisor)](https://github.com/arceos-hypervisor/axvisor/blob/master/LICENSE)

</div>

[English](README.md) | 中文

# 简介

AxVisor 是一个基于 ArceOS 内核实现的 Hypervisor。其目标是利用 ArceOS 提供的基础操作系统功能作为基础，实现一个轻量级统一组件化 Hypervisor。

- **统一**是指使用同一套代码同时支持 x86_64、Arm(aarch64) 和 RISC-V 三种架构，以最大化复用架构无关代码，简化代码开发和维护成本。

- **组件化**是指 Hypervisor 的功能被分解为多个可独立使用的组件，每个组件实现一个特定的功能，组件之间通过标准接口进行通信，以实现功能的解耦和复用。

## 架构

AxVisor 的软件架构分为如下图所示的五层，其中，每一个框都是一个独立的组件，组件之间通过标准接口进行通信。完整的架构描述可以在[文档](https://arceos-hypervisor.github.io/axvisorbook/docs/overview)中找到。

![Architecture](https://arceos-hypervisor.github.io/doc/assets/arceos-hypervisor-architecture.png)

## 硬件平台

AxVisor 已在多种硬件平台上完成验证，涵盖从虚拟化环境到实际物理设备的广泛支持。为方便用户快速部署，我们在 [axvisor-guest](https://github.com/arceos-hypervisor/axvisor-guest) 仓库中提供了各平台的一键构建脚本，可自动生成对应的镜像文件。

| 平台名称 | 架构支持 | 主要特点 |
|---------|---------|---------|
| QEMU | ARM64, x86_64 | 虚拟化平台，支持多架构，用于开发和测试 |
| Orange Pi 5 Plus | ARM64 | 基于 Rockchip RK3588 的开发板，高性能 ARM 平台 |
| 飞腾派 | ARM64 | 基于飞腾 E2000Q 处理器的开发板，国产 ARM 平台 |
| ROC-RK3568-PC | ARM64 | 基于 Rockchip RK3568 的开发板，适合工业应用 |
| EVM3588 | ARM64 | 基于 Rockchip RK3588 的评估板，企业级应用 |

## 客户机系统

AxVisor 支持多种操作系统作为客户机运行，从轻量级微内核到成熟的宏内核系统均有良好兼容性。为简化用户部署流程，我们在 [axvisor-guest](https://github.com/arceos-hypervisor/axvisor-guest) 仓库中提供了针对不同客户机系统的一键构建脚本，可快速生成适配的客户机镜像。

| 客户机系统 | 系统类型 | 架构支持 | 特点描述 |
|-----------|---------|---------|---------|
| [ArceOS](https://github.com/arceos-org/arceos) | Unikernel | ARM64, x86_64, RISC-V | 基于Rust的组件化操作系统，轻量级、高性能 |
| [Starry-OS](https://github.com/Starry-OS) | 宏内核操作系统 | ARM64, x86_64 | 面向嵌入式场景的实时操作系统 |
| [NimbOS](https://github.com/equation314/nimbos) | RTOS 系统 | ARM64, x86_64, RISC-V | 简洁的类Unix系统，支持POSIX接口 |
| Linux | 宏内核操作系统 | ARM64, x86_64, RISC-V | 成熟稳定的通用操作系统，丰富的软件生态 |

# 构建

AxVisor 基于 Rust 生态系统构建，通过扩展的 xtask 工具链提供了完整的项目构建、配置管理和调试支持，为开发者提供统一且高效的开发体验。

## 构建环境

首先，在 Linux 环境下，需要安装 `libssl-dev gcc libudev-dev pkg-config` 等基本开发工具包。

其次，AxVisor 是使用 Rust 编程语言编写的，因此，需要根据 Rust 官方网站的说明安装 Rust 开发环境，并使用 `cargo install cargo-binutils` 命令安装 [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) 以便使用 `rust-objcopy` 和 `rust-objdump` 等工具

> 根据需要，可能还要安装 [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) 来构建客户机应用程序

## 配置文件

AxVisor 使用分层配置系统，包含硬件平台配置和客户机配置两部分，均采用 TOML 格式。

### 硬件平台配置

硬件平台配置文件位于 `configs/board/` 目录下，每个配置文件都对应了一个经过我们验证的开发板（或 QEMU 平台架构）。其中指定了目标架构、功能特性、驱动支持、日志级别以及构建选项。

> 客户机配置项 `vm_configs` 默认并没有指定，在实际使用时需要进行指定!

### 客户机配置

客户机配置文件位于 `configs/vms/` 目录下，定义了客户机的运行参数，包括基本信息、内核配置、内存区域以及设备配置等详细信息。

配置文件命名格式为 `<os>-<arch>-board_or_cpu-smpx`，其中 `<os>` 是客户机系统名字（如 `arceos`、`linux`、`nimbos`），`<arch>` 是架构（如 `aarch64`、`x86_64`、`riscv64`），`board_or_cpu` 是硬件开发板或 CPU 名称，`smpx` 是分配给客户机的 CPU 数量。

## 编译

AxVisor 使用 xtask 工具进行构建管理，支持多种硬件平台和配置选项。快速构建及运行 AxVisor，请参见配置套文档中的[快速上手](https://arceos-hypervisor.github.io/axvisorbook/docs/category/quickstart)章节。

1. **生成配置**：使用 `cargo xtask defconfig <board_name>` 选择 `configs/board/` 目录下目标硬件平台配置。此命令会将对应板级配置复制为 `.build.toml` 作为构建配置。

2. **修改配置**：使用 `cargo xtask menuconfig` 启动交互式配置界面，可调整目标架构、功能特性、日志级别等参数。

3. **执行构建**：使用 `cargo xtask build` 根据 `.build.toml` 配置文件编译项目，生成目标平台的二进制文件。

# 贡献

欢迎 fork 本仓库并提交 pull request。这个项目的存在与发展得益于所有贡献者的支持。

<a href="https://github.com/arceos-hypervisor/axvisor/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=arceos-hypervisor/axvisor" />
</a>

您也可以扫描下方二维码加入讨论群（请务必发送备注信息：AxVisor），进行问题咨询、经验交流与反馈建议。

![group](https://arceos-hypervisor.github.io/axvisorbook/assets/images/group-c0e9fb6c8a7720a1f7eb55d3f4f40b4c.png)

# 许可协议

Axvisor 采用 Apache License 2.0 开源协议。详见 [LICENSE](./LICENSE) 文件。