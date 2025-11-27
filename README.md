<!-- <div align="center">

<img src="https://arceos-hypervisor.github.io/doc/assets/logo.svg" alt="axvisor-logo" width="64">

</div> -->

<h2 align="center">AxVisor</h1>

<p align="center">A unified modular hypervisor based on ArceOS.</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/network)
[![license](https://img.shields.io/github/license/arceos-hypervisor/axvisor)](https://github.com/arceos-hypervisor/axvisor/blob/master/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

AxVisor is a Hypervisor implemented based on the ArceOS unikernel framework. Its goal is to leverage the basic operating system functionalities provided by ArceOS as a foundation to build a unified and componentized Hypervisor.

**Unified** means using the same codebase to support three architectures—x86_64, Arm (aarch64), and RISC-V—maximizing the reuse of architecture-agnostic code and simplifying development and maintenance efforts.

**Componentized** means that the Hypervisor's functionalities are decomposed into multiple independently usable components. Each component implements a specific function, and components communicate through standardized interfaces to achieve decoupling and reusability.

## Architecture

The software architecture of AxVisor is divided into five layers as shown in the diagram below. Each box represents an independent module, and the modules communicate with each other through standard interfaces.

![Architecture](https://arceos-hypervisor.github.io/doc/assets/arceos-hypervisor-architecture.png)

The complete architecture description can be found in the [documentation](https://arceos-hypervisor.github.io/doc/arch_cn.html).

## Hardwares

Currently, AxVisor has been verified on the following platforms:

- [x] QEMU ARM64 virt (qemu-max)
- [x] Rockchip RK3568 / RK3588
- [x] PhytiumPi

## Guest VMs

Currently, AxVisor has been verified in scenarios with the following systems as guests:

- [ArceOS](https://github.com/arceos-org/arceos)
- [Starry-OS](https://github.com/Starry-OS)
- [NimbOS](https://github.com/equation314/nimbos)
- Linux

## Shell Management

AxVisor provides an interactive shell interface for managing virtual machines and file operations.

For detailed information about shell features, commands, and usage, see: [Shell模块介绍.md](doc/Shell模块介绍.md)

# Build and Run

After AxVisor starts, it loads and starts the guest based on the information in the guest configuration file. Currently, AxVisor supports loading guest images from a FAT32 file system and also supports binding guest images to the hypervisor image through static compilation (using include_bytes).

## Build Environment

AxVisor is written in the Rust programming language, so you need to install the Rust development environment following the instructions on the official Rust website. Additionally, you need to install cargo-binutils to use tools like rust-objcopy and rust-objdump.

```console
cargo install cargo-binutils
```

If necessary, you may also need to install [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) to build guest applications.

## Configuration Files

Since guest configuration is a complex process, AxVisor chooses to use toml files to manage guest configurations, which include the virtual machine ID, virtual machine name, virtual machine type, number of CPU cores, memory size, virtual devices, and passthrough devices.

- In the source code's `./config/vms` directory, there are some example templates for guest configurations. The configuration files are named in the format `<os>-<arch>-board_or_cpu-smpx`, where:
  - `<os>` is the guest operating system name
  - `<arch>` is the architecture
  - `board_or_cpu` is the name of the hardware development board or CPU (different strings are concatenated with `_`)
  - `smpx` refers to the number of CPUs allocated to the guest, where `x` is the specific value
  - The different components are concatenated with `-` to form the whole name

- Additionally, you can also use the [axvmconfig](https://github.com/arceos-hypervisor/axvmconfig) tool to generate a custom configuration file. For detailed information, please refer to [axvmconfig](https://arceos-hypervisor.github.io/axvmconfig/axvmconfig/index.html).

## Load and run from file system

Loading from the filesystem refers to the method where the AxVisor image, Linux guest image, and its device tree are independently deployed in the filesystem on the storage. After AxVisor starts, it loads the guest image and its device tree from the filesystem to boot the guest.

### NimbOS as guest 

1. Execute script to download and prepare NimbOS image.

   ```shell
   ./scripts/nimbos.sh --arch aarch64
   ```

2. Execute `./axvisor.sh run --plat aarch64-generic --features fs,ept-level-4 --arceos-args BUS=mmio,BLK=y,DISK_IMG=tmp/nimbos-aarch64.img,LOG=info --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml` to build AxVisor and start it in QEMU.

3. After that, you can directly run `./axvisor.sh run` to start it, and modify `.hvconfig.toml` to change the startup parameters.

### More guest

   TODO

## Load and run from memory

Loading from memory refers to a method where the AxVisor image, guest image, and its device tree are already packaged together during the build phase. Only AxVisor itself needs to be deployed in the file system on the storage device. After AxVisor starts, it loads the guest image and its device tree from memory to boot the guest.

### linux as guest 

1. Prepare working directory
   ```console
   mkdir -p tmp
   cp configs/vms/linux-aarch64-qemu-smp1.toml tmp/
   cp configs/vms/linux-aarch64-qemu-smp1.dts tmp/
   ```

2. [See Linux build help](https://github.com/arceos-hypervisor/guest-test-linux) to get the guest Image and rootfs.img, then copy them to the `tmp` directory.

3. Execute `dtc -O dtb -I dts -o tmp/linux-aarch64-qemu-smp1.dtb tmp/linux-aarch64-qemu-smp1.dts` to build the guest device tree file

4. Execute `./axvisor.sh defconfig`, then edit the `.hvconfig.toml` file, set the `vmconfigs` item to your guest machine configuration file path, with the following content:

   ```toml
   arceos_args = [
      "BUS=mmio",
      "BLK=y",
      "MEM=8g",
      "LOG=debug",
      "QEMU_ARGS=\"-machine gic-version=3  -cpu cortex-a72  \"",
      "DISK_IMG=\"tmp/rootfs.img\"",
   ]
   vmconfigs = [ "tmp/linux-aarch64-qemu-smp1.toml"]
   ```

4. Execute `./axvisor.sh run` to build AxVisor and start it in QEMU.

### More guest

   TODO

# Contributing

Feel free to fork this repository and submit a pull request.

You can refer to these [discussions]((https://github.com/arceos-hypervisor/axvisor/discussions)) to gain deeper insights into the project's ideas and future development direction.

## Development

To contribute to AxVisor, you can follow these steps:

1. Fork the repository on GitHub.
2. Clone your forked repository to your local machine.
3. Create a new branch for your feature or bug fix.
4. Make your changes and commit them with clear messages.
5. Push your changes to your forked repository.
6. Open a pull request against the main branch of the original repository.

To develop crates used by AxVisor, you can use the following command to build and run the project:

```bash
cargo install cargo-lpatch
cargo lpatch -n deps_crate_name
```

Then you can modify the code in the `crates/deps_crate_name` directory, and it will be automatically used by AxVisor.

## Contributors

This project exists thanks to all the people who contribute.

<a href="https://github.com/arceos-hypervisor/axvisor/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=arceos-hypervisor/axvisor" />
</a>

# License

AxVisor uses the following open-source license:

- Apache-2.0
- MulanPubL-2.0
- MulanPSL2
- GPL-3.0-or-later
