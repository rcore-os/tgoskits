<!-- <div align="center">

<img src="https://arceos-hypervisor.github.io/doc/assets/logo.svg" alt="axvisor-logo" width="64">

</div> -->

<h2 align="center">AxVisor</h1>

<p align="center">一个基于 ArceOS 的统一模块化虚拟机管理程序</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/network)
[![license](https://img.shields.io/github/license/arceos-hypervisor/axvisor)](https://github.com/arceos-hypervisor/axvisor/blob/master/LICENSE)

</div>

[English](README.md) | 中文版

# 简介

AxVisor 是基于 ArceOS unikernel 框架实现的 Hypervisor。其目标是利用 ArceOS 提供的基础操作系统功能作为基础，实现一个统一的模块化 Hypervisor。

“统一”指使用同一套代码同时支持 x86_64、Arm(aarch64) 和 RISC-V 三种架构，以最大化复用架构无关代码，简化代码开发和维护成本。

“模块化”指 Hypervisor 的功能被分解为多个模块，每个模块实现一个特定的功能，模块之间通过标准接口进行通信，以实现功能的解耦和复用。

## 架构

AxVisor 的软件架构分为如下图所示的五层，其中，每一个框都是一个独立的模块，模块之间通过标准接口进行通信。

![Architecture](https://arceos-hypervisor.github.io/doc/assets/arceos-hypervisor-architecture.png)

完整的架构描述可以在[文档](https://arceos-hypervisor.github.io/doc/arch_cn.html)中找到。

## 硬件平台

目前，AxVisor 已经在如下平台进行了验证：

- [x] QEMU ARM64 virt (qemu-max)
- [x] Rockchip RK3568 / RK3588
- [x] 黑芝麻华山 A1000

## 客户机

目前，AxVisor 已经在对如下系统作为客户机的情况进行了验证：

* [ArceOS](https://github.com/arceos-org/arceos)
* [Starry-OS](https://github.com/Starry-OS)
* [NimbOS](https://github.com/equation314/nimbos)
* Linux
  * currently only Linux with passthrough device on aarch64 is tested.
  * single core: [config.toml](configs/vms/linux-qemu-aarch64.toml) | [dts](configs/vms/linux-qemu.dts)
  * smp: [config.toml](configs/vms/linux-qemu-aarch64-smp2.toml) | [dts](configs/vms/linux-qemu-smp2.dts)

# 构建及运行

使用 `git clone https://github.com/arceos-hypervisor/axvisor.git` 命令拉取 AxVisor 源代码

## 构建环境

AxVisor 是使用 Rust 编程语言编写的，因此，需要根据 Rust 官方网站的说明安装 Rust 开发环境。此外，还需要安装 [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) 以便使用 `rust-objcopy` 和 `rust-objdump` 等工具

```console
$ cargo install cargo-binutils
```

根据需要，可能还要安装 [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) 来构建客户机应用程序

## 准备客户机

AxVisor 启动之后会根据客户机配置文件中的信息加载并启动客户机。目前，AxVisor 即支持从 FAT32 文件系统加载客户机镜像，也支持通过静态编译方式（include_bytes）将客户机镜像绑定到虚拟机管理程序镜像中。

### 配置文件

由于客户机配置是一个复杂的过程，AxVisor 选择使用 toml 文件来管理客户机的配置，其中包括虚拟机 ID、虚拟机名称、虚拟机类型、CPU 核心数量、内存大小、虚拟设备和直通设备等。在源码的 `./config/vms` 目录下是一些客户机配置的示例模板。

此外，也可以使用 [axvmconfig](https://github.com/arceos-hypervisor/axvmconfig) 工具来生成一个自定义配置文件。详细介绍参见 [axvmconfig](https://arceos-hypervisor.github.io/axvmconfig/axvmconfig/index.html)。

### 从文件系统加载

1. 构建自己的客户机镜像文件

2. 修改对应的 `./configs/vms/<ARCH_CONFIG>.toml` 中的配置项
     - `image_location="fs"` 表示从文件系统加载
     - `kernel_path` 指出内核镜像在文件系统中的路径
     - 其他

3. 制作一个磁盘镜像文件，并将客户机镜像放到文件系统中

  1. 使用 `make disk_img` 命令生成一个空的 FAT32 磁盘镜像文件 `disk.img`
  2. 手动挂载 `disk.img`，然后将自己的客户机镜像放到该文件系统中

      ```console
      $ mkdir -p tmp
      $ sudo mount disk.img tmp
      $ sudo cp /PATH/TO/YOUR/GUEST/VM/IMAGE tmp/
      $ sudo umount tmp
      ```

4. 在构建 AxVisor 时，需要 `APP_FEATURES=fs` 选项

### 从内存加载

1. 构建自己的客户机镜像文件

2. 修改对应的 `./configs/vms/<ARCH_CONFIG>.toml` 中的配置项
     - `image_location="memory"` 配置项
     - `kernel_path` 指定内核镜像在工作空间中的相对/绝对路径
     - 其他

3. 当前通过静态编译绑定客户机镜像的方法最多仅支持加载一个客户机镜像

## 构建及启动

根据选择的加载客户机镜像文件的方式的不同，以下命令需要有对应的修改！

### x86_64 for nimbos

1. `make ARCH=x86_64 defconfig`
2. `make ACCEL=y ARCH=x86_64 LOG=info VM_CONFIGS=configs/vms/nimbos-x86_64.toml APP_FEATURES=fs run`

### aarch64 for nimbos

1. `make ARCH=aarch64 defconfig`
2. `make ACCEL=n ARCH=aarch64 LOG=info VM_CONFIGS=configs/vms/nimbos-aarch64.toml APP_FEATURES=fs run`

### riscv64 for nimbos

1. `make ARCH=aarch64 defconfig`
2. `make ACCEL=n ARCH=riscv64 LOG=info VM_CONFIGS=configs/vms/nimbos-riscv64.toml APP_FEATURES=fs run`

### aarch64 for Linux

需要修改 `configs/platforms/aarch64-qemu-virt-hv.toml` 中的 `phys-memory-size` 为 `0x2_0000_0000` (8G)

1. `make ARCH=aarch64 defconfig`
2. `make ARCH=aarch64 VM_CONFIGS=configs/vms/linux-qemu-aarch64.toml LOG=debug BUS=mmio NET=y FEATURES=page-alloc-64g MEM=8g run`

### aarch64 for Linux SMP=2

1. `make ARCH=aarch64 defconfig`
2. `make ARCH=aarch64 VM_CONFIGS=configs/vms/linux-qemu-aarch64-smp2.toml LOG=debug BUS=mmio NET=y  BLK=y SMP=2 FEATURES=page-alloc-64g MEM=8g run`

## 启动示例

```console
Booting from ROM..
Initialize IDT & GDT...

       d8888                            .d88888b.   .d8888b.
      d88888                           d88P" "Y88b d88P  Y88b
     d88P888                           888     888 Y88b.
    d88P 888 888d888  .d8888b  .d88b.  888     888  "Y888b.
   d88P  888 888P"   d88P"    d8P  Y8b 888     888     "Y88b.
  d88P   888 888     888      88888888 888     888       "888
 d8888888888 888     Y88b.    Y8b.     Y88b. .d88P Y88b  d88P
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"

arch = x86_64
platform = x86_64-qemu-q35
target = x86_64-unknown-none
smp = 1
build_mode = release
log_level = warn

Starting virtualization...
Running guest...

NN   NN  iii               bb        OOOOO    SSSSS
NNN  NN       mm mm mmmm   bb       OO   OO  SS
NN N NN  iii  mmm  mm  mm  bbbbbb   OO   OO   SSSSS
NN  NNN  iii  mmm  mm  mm  bb   bb  OO   OO       SS
NN   NN  iii  mmm  mm  mm  bbbbbb    OOOO0    SSSSS
              ___    ____    ___    ___
             |__ \  / __ \  |__ \  |__ \
             __/ / / / / /  __/ /  __/ /
            / __/ / /_/ /  / __/  / __/
           /____/ \____/  /____/ /____/

arch = x86_64
platform = rvm-guest-x86_64
build_mode = release
log_level = warn

Initializing kernel heap at: [0xffffff800028ed00, 0xffffff800068ed00)
Initializing IDT...
Loading GDT for CPU 0...
Initializing frame allocator at: [PA:0x68f000, PA:0x1000000)
Mapping .text: [0xffffff8000200000, 0xffffff800021b000)
Mapping .rodata: [0xffffff800021b000, 0xffffff8000220000)
Mapping .data: [0xffffff8000220000, 0xffffff800028a000)
Mapping .bss: [0xffffff800028e000, 0xffffff800068f000)
Mapping boot stack: [0xffffff800028a000, 0xffffff800028e000)
Mapping physical memory: [0xffffff800068f000, 0xffffff8001000000)
Mapping MMIO: [0xffffff80fec00000, 0xffffff80fec01000)
Mapping MMIO: [0xffffff80fed00000, 0xffffff80fed01000)
Mapping MMIO: [0xffffff80fee00000, 0xffffff80fee01000)
Initializing drivers...
Initializing Local APIC...
Initializing HPET...
HPET: 100.000000 MHz, 64-bit, 3 timers
Calibrated TSC frequency: 2993.778 MHz
Calibrated LAPIC frequency: 1000.522 MHz
Initializing task manager...
/**** APPS ****
cyclictest
exit
fantastic_text
forktest
forktest2
forktest_simple
forktest_simple_c
forktree
hello_c
hello_world
matrix
sleep
sleep_simple
stack_overflow
thread_simple
user_shell
usertests
yield
**************/
Running tasks...
test kernel task: pid = TaskId(2), arg = 0xdead
test kernel task: pid = TaskId(3), arg = 0xbeef
Rust user shell
>> hello_world
Hello world from user mode program!
Shell: Process 5 exited with code 0
>>
......
```

# 如何贡献

欢迎 FORK 本仓库并提交 PR。

您可以参考这些[讨论](https://github.com/arceos-hypervisor/axvisor/discussions)，以深入了解该项目的思路和未来发展方向。

## 开发

AxVisor 作为组件化的虚拟机管理程序，很多组件是作为 Crate 来使用的，可以使用 `tool/dev_env.py` 命令将相关 Crate 本地化，方便开发调试。

## 贡献者

这个项目的存在得益于所有贡献者的支持。

<a href="https://github.com/arceos-hypervisor/axvisor/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=arceos-hypervisor/axvisor" />
</a>

# 许可协议

AxVisor 使用如下开源协议

 * Apache-2.0
 * MulanPubL-2.0
 * MulanPSL2
 * GPL-3.0-or-later
