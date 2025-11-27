<!-- <div align="center">

<img src="https://arceos-hypervisor.github.io/doc/assets/logo.svg" alt="axvisor-logo" width="64">

</div> -->

<h2 align="center">AxVisor</h1>

<p align="center">一个基于 ArceOS 的统一组件化虚拟机管理程序</p>

<div align="center">

[![GitHub stars](https://img.shields.io/github/stars/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/stargazers)
[![GitHub forks](https://img.shields.io/github/forks/arceos-hypervisor/axvisor?logo=github)](https://github.com/arceos-hypervisor/axvisor/network)
[![license](https://img.shields.io/github/license/arceos-hypervisor/axvisor)](https://github.com/arceos-hypervisor/axvisor/blob/master/LICENSE)

</div>

[English](README.md) | 中文

# 简介

AxVisor 是基于 ArceOS unikernel 框架实现的 Hypervisor。其目标是利用 ArceOS 提供的基础操作系统功能作为基础，实现一个统一的组件化 Hypervisor。

**统一**是指使用同一套代码同时支持 x86_64、Arm(aarch64) 和 RISC-V 三种架构，以最大化复用架构无关代码，简化代码开发和维护成本。

**组件化**是指 Hypervisor 的功能被分解为多个可独立使用的组件，每个组件实现一个特定的功能，组件之间通过标准接口进行通信，以实现功能的解耦和复用。

## 架构

AxVisor 的软件架构分为如下图所示的五层，其中，每一个框都是一个独立的组件，组件之间通过标准接口进行通信。

![Architecture](https://arceos-hypervisor.github.io/doc/assets/arceos-hypervisor-architecture.png)

完整的架构描述可以在[文档](https://arceos-hypervisor.github.io/doc/arch_cn.html)中找到。

## 硬件平台

目前，AxVisor 已经在如下平台进行了验证：

- [x] QEMU ARM64 virt (qemu-max)
- [x] Rockchip RK3568 / RK3588
- [x] 飞腾派

## 客户机

目前，AxVisor 已经在对如下系统作为客户机的情况进行了验证：

- [ArceOS](https://github.com/arceos-org/arceos)
- [Starry-OS](https://github.com/Starry-OS)
- [NimbOS](https://github.com/equation314/nimbos)
- Linux

# 构建及运行

AxVisor 启动之后会根据客户机配置文件中的信息加载并启动客户机。目前，AxVisor 即支持从 FAT32 文件系统加载客户机镜像，也支持通过静态编译方式（include_bytes）将客户机镜像绑定到虚拟机管理程序镜像中。

## 构建环境

AxVisor 是使用 Rust 编程语言编写的，因此，需要根据 Rust 官方网站的说明安装 Rust 开发环境。此外，还需要安装 [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) 以便使用 `rust-objcopy` 和 `rust-objdump` 等工具

```console
cargo install cargo-binutils
```

根据需要，可能还要安装 [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) 来构建客户机应用程序

## 配置文件

由于客户机配置是一个复杂的过程，AxVisor 选择使用 toml 文件来管理客户机的配置，其中包括虚拟机 ID、虚拟机名称、虚拟机类型、CPU 核心数量、内存大小、虚拟设备和直通设备等。

- 在源码的 `./config/vms` 目录下是一些客户机配置的示例模板，配置文件命名格式为 `<os>-<arch>-board_or_cpu-smpx`，其中
  - `<os>` 是客户机系统名字
  - `<arch>` 是架构
  - `board_or_cpu` 硬件开发板或者 CPU 的名字（以 `_` 拼接不同字符串）
  - `smpx` 是指分配客户机的 CPU 数量 `x` 为具体数值
  - 各项之间以 `-` 拼接为整体

- 此外，也可以使用 [axvmconfig](https://github.com/arceos-hypervisor/axvmconfig) 工具来生成一个自定义配置文件。详细介绍参见 [axvmconfig](https://arceos-hypervisor.github.io/axvmconfig/axvmconfig/index.html)。

## 从文件系统加载运行

从文件系统加载是指将 AxVisor 镜像和 Linux 客户机镜像及其设备树独立部署在存储器上的文件系统中，AxVisor 启动后从文件系统中加载客户机镜像及其设备树进而启动客户机的方式。

### NimbOS 作为客户机

1. 执行仓库内的脚本下载并准备 NimbOS 镜像：

   ```bash
   ./scripts/nimbos.sh --arch aarch64
   ```

2. 执行 `./axvisor.sh run --plat aarch64-generic --features fs,ept-level-4 --arceos-args BUS=mmio,BLK=y,DISK_IMG=tmp/nimbos-aarch64.img,LOG=info --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml` 构建 AxVisor 并在 QEMU 中启动 NimbOS 客户机。

3. 后续可直接执行 `./axvisor.sh run` 来启动，并可通过修改 `.hvconfig.toml` 来更改启动参数

### 更多客户机

   TODO

## 从内存加载运行

从内存加载是指在构建时已经将 AxVisor 镜像与客户机镜像及其设备树打包在了一起，而只需要将 AxVisor 本身部署在存储器上文件系统中，AxVisor 启动后从内存中加载客户机镜像及其设备树进而启动客户机的方式。

### Linux 作为客户机

1. 准备工作目录及相关文件
   ```console
   mkdir -p tmp
   cp configs/vms/linux-aarch64-qemu-smp1.toml tmp/
   cp configs/vms/linux-aarch64-qemu-smp1.dts tmp/
   ```

2. [参见 linux 构建帮助](https://github.com/arceos-hypervisor/guest-test-linux)获取客户机 Image 和 rootfs.img，然后复制到 `tmp` 目录中。

3. 构建客户机设备树 `dtc -O dtb -I dts -o tmp/linux-aarch64-qemu-smp1.dtb tmp/linux-aarch64-qemu-smp1.dts`

4. 执行 `./axvisor.sh defconfig`，然后编辑 `.hvconfig.toml` 文件，设置 `vmconfigs` 项为您的客户机配置文件路径，例如：

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

4. 执行 `./axvisor.sh run` 构建 AxVisor 并在 QEMU 中启动。

### 更多客户机

   TODO

## 启动示例

```bash
       d8888                            .d88888b.   .d8888b.
      d88888                           d88P" "Y88b d88P  Y88b
     d88P888                           888     888 Y88b.
    d88P 888 888d888  .d8888b  .d88b.  888     888  "Y888b.
   d88P  888 888P"   d88P"    d8P  Y8b 888     888     "Y88b.
  d88P   888 888     888      88888888 888     888       "888
 d8888888888 888     Y88b.    Y8b.     Y88b. .d88P Y88b  d88P
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"

log_level = info
smp = 1

[  0.020822 0 axruntime:130] Logging is enabled.
[  0.026419 0 axruntime:131] Primary CPU 0 started, dtb = 0x44000000.
[  0.028520 0 axruntime:133] Found physcial memory regions:
[  0.030673 0 axruntime:135]   [PA:0x40080000, PA:0x400d6000) .text (READ | EXECUTE | RESERVED)
[  0.033564 0 axruntime:135]   [PA:0x400d6000, PA:0x400ef000) .rodata (READ | RESERVED)
[  0.035313 0 axruntime:135]   [PA:0x400ef000, PA:0x400f5000) .data .tdata .tbss .percpu (READ | WRITE | RESERVED)
[  0.037083 0 axruntime:135]   [PA:0x400f5000, PA:0x40135000) boot stack (READ | WRITE | RESERVED)
[  0.038622 0 axruntime:135]   [PA:0x40135000, PA:0x4013b000) .bss (READ | WRITE | RESERVED)
[  0.040643 0 axruntime:135]   [PA:0x4013b000, PA:0x48000000) free memory (READ | WRITE | FREE)
[  0.042907 0 axruntime:135]   [PA:0x9000000, PA:0x9001000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.045011 0 axruntime:135]   [PA:0x9040000, PA:0x9041000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.047070 0 axruntime:135]   [PA:0x9100000, PA:0x9101000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.049093 0 axruntime:135]   [PA:0x8000000, PA:0x8020000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.051081 0 axruntime:135]   [PA:0xa000000, PA:0xa004000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.053120 0 axruntime:135]   [PA:0x10000000, PA:0x3eff0000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.055229 0 axruntime:135]   [PA:0x4010000000, PA:0x4020000000) mmio (READ | WRITE | DEVICE | RESERVED)
[  0.057642 0 axruntime:208] Initialize global memory allocator...
[  0.059377 0 axruntime:209]   use TLSF allocator.
[  0.072071 0 axmm:60] Initialize virtual memory management...
[  0.136312 0 axruntime:150] Initialize platform devices...
[  0.137733 0 axhal::platform::aarch64_common::gic:67] Initialize GICv2...
[  0.143653 0 axtask::api:73] Initialize scheduling...
[  0.151435 0 axtask::api:79]   use FIFO scheduler.
[  0.152744 0 axruntime:176] Initialize interrupt handlers...
[  0.157472 0 axruntime:186] Primary CPU 0 init OK.
[  0.159027 0:2 axvisor:17] Starting virtualization...
[  0.160968 0:2 axvisor:19] Hardware support: true
[  0.168619 0:4 axvisor::vmm::timer:103] Initing HV Timer...
[  0.170399 0:4 axvisor::hal:117] Hardware virtualization support enabled on core 0
[  0.295531 0:2 axvisor::vmm::config:33] Creating VM [1] "arceos"
[  0.301423 0:2 axvm::vm:113] Setting up memory region: [0x40000000~0x41000000] READ | WRITE | EXECUTE
[  0.334424 0:2 axvm::vm:156] Setting up passthrough device memory region: [0x8000000~0x8050000] -> [0x8000000~0x8050000]
[  0.339431 0:2 axvm::vm:156] Setting up passthrough device memory region: [0x9000000~0x9001000] -> [0x9000000~0x9001000]
[  0.341925 0:2 axvm::vm:156] Setting up passthrough device memory region: [0x9010000~0x9011000] -> [0x9010000~0x9011000]
[  0.343758 0:2 axvm::vm:156] Setting up passthrough device memory region: [0x9030000~0x9031000] -> [0x9030000~0x9031000]
[  0.345559 0:2 axvm::vm:156] Setting up passthrough device memory region: [0xa000000~0xa004000] -> [0xa000000~0xa004000]
[  0.348819 0:2 axvm::vm:191] VM created: id=1
[  0.350749 0:2 axvm::vm:206] VM setup: id=1
[  0.352526 0:2 axvisor::vmm::config:40] VM[1] created success, loading images...
[  0.355270 0:2 axvisor::vmm::images:24] Loading VM[1] images from memory
[  0.363583 0:2 axvisor::vmm:29] Setting up vcpus...
[  0.368014 0:2 axvisor::vmm::vcpus:176] Initializing VM[1]'s 1 vcpus
[  0.370802 0:2 axvisor::vmm::vcpus:207] Spawning task for VM[1] Vcpu[0]
[  0.374805 0:2 axvisor::vmm::vcpus:219] Vcpu task Task(5, "VM[1]-VCpu[0]") created cpumask: [0, ]
[  0.378878 0:2 axvisor::vmm:36] VMM starting, booting VMs...
[  0.380775 0:2 axvm::vm:273] Booting VM[1]
[  0.382631 0:2 axvisor::vmm:42] VM[1] boot success
[  0.387436 0:5 axvisor::vmm::vcpus:240] VM[1] Vcpu[0] waiting for running
[  0.390048 0:5 axvisor::vmm::vcpus:243] VM[1] Vcpu[0] running...

       d8888                            .d88888b.   .d8888b.
      d88888                           d88P" "Y88b d88P  Y88b
     d88P888                           888     888 Y88b.
    d88P 888 888d888  .d8888b  .d88b.  888     888  "Y888b.
   d88P  888 888P"   d88P"    d8P  Y8b 888     888     "Y88b.
  d88P   888 888     888      88888888 888     888       "888
 d8888888888 888     Y88b.    Y8b.     Y88b. .d88P Y88b  d88P
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"

arch = aarch64
platform = aarch64-qemu-virt
target = aarch64-unknown-none-softfloat
build_mode = release
log_level = warn
smp = 1

Hello, world!
[  0.416823 0:5 axvisor::vmm::vcpus:288] VM[1] run VCpu[0] SystemDown
[  0.419035 0:5 axhal::platform::aarch64_common::psci:98] Shutting down...
```

# 如何贡献

欢迎 fork 本仓库并提交 pull request。

您可以参考这些[讨论](https://github.com/arceos-hypervisor/axvisor/discussions)，以深入了解该项目的思路和未来发展方向。

## 开发

要为 AxVisor 做贡献，您可以按照以下步骤操作：

1. 在 GitHub 上 fork 本仓库。
2. 将您 fork 的仓库克隆到本地。
3. 为您的功能或错误修复创建新分支。
4. 进行更改并提交清晰的消息。
5. 将更改推送到您 fork 的仓库。
6. 对原始仓库的主分支提交 pull request。

要开发 AxVisor 使用的 crate，您可以使用以下命令构建和运行项目：

```bash
cargo install cargo-lpatch
cargo lpatch -n deps_crate_name
```

然后您可以修改 `crates/deps_crate_name` 目录中的代码，AxVisor 将自动使用这些代码。

## 贡献者

这个项目的存在得益于所有贡献者的支持。

<a href="https://github.com/arceos-hypervisor/axvisor/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=arceos-hypervisor/axvisor" />
</a>

# 许可协议

AxVisor 使用如下开源协议

- Apache-2.0
- MulanPubL-2.0
- MulanPSL2
- GPL-3.0-or-later
