---
sidebar_position: 2
sidebar_label: "StarryOS"
title: "StarryOS 快速上手"
---

# StarryOS 快速上手

StarryOS 的快速上手建议先选择板卡配置，再执行常规构建或运行命令。`cargo starry config ls` 用于查看当前支持的板卡名称，`cargo starry defconfig <board>` 会把选中的板卡配置写入默认构建配置并记录到 StarryOS 命令快照，后续 `cargo starry build`、`cargo starry qemu`、`cargo starry uboot` 或 `cargo starry board` 会沿用这份配置。

```mermaid
flowchart LR
  A[cargo starry config ls] --> B[cargo starry defconfig board]
  B --> C[cargo starry qemu / build / board]
  C --> D{单次启动通过?}
  D -- 是 --> E[测试套件]
  D -- 否 --> F[检查环境 / rootfs / 板卡连接]
  F --> A
```

## 1. 选择板卡配置

先查看仓库当前支持的 StarryOS 板卡配置：

```bash
cargo starry config ls
```

输出中的名称可以直接传给 `defconfig`：

```bash
cargo starry defconfig <board>
```

完成 `defconfig` 后，后续命令通常不需要再重复传 `--config`、`--target` 或 `--arch`。`quick-start` 是旧的便捷入口，后续会废弃；新的快速上手路径请使用 `config ls`、`defconfig` 和常规 `cargo starry` 子命令。

## 2. QEMU 快速启动

StarryOS 的 QEMU 启动通常包含 rootfs。当前 `qemu` 路径会在缺少 rootfs 时自动补齐，也可以显式先执行 `rootfs`。

### 2.1 RISC-V 64

`riscv64` 仍然是最适合作为首条验证路径的架构。它在文档和测试套件中都较常用，适合先确认 rootfs 和 QEMU 路径是否已经接通。

推荐第一次从 `riscv64` 开始：

```bash
cargo starry defconfig qemu-riscv64
cargo starry qemu
```

或显式分步执行：

```bash
cargo starry defconfig qemu-riscv64
cargo starry rootfs --arch riscv64
cargo starry build
cargo starry qemu
```

### 2.2 AArch64

如果后续会继续关注板级路径或与 Axvisor 的 AArch64 环境对齐，可以尽快补跑这一条。它也是 StarryOS 当前非常重要的一条验证路径。

```bash
cargo starry defconfig qemu-aarch64
cargo starry qemu
```

分步执行：

```bash
cargo starry defconfig qemu-aarch64
cargo starry rootfs --arch aarch64
cargo starry build
cargo starry qemu
```

### 2.3 x86_64

`x86_64` 适合作为 PC 类平台的补充验证路径。命令和其它架构基本一致，差异主要体现在目标 triple 和对应的 QEMU 配置上。

```bash
cargo starry defconfig qemu-x86_64
cargo starry qemu
```

分步执行：

```bash
cargo starry defconfig qemu-x86_64
cargo starry rootfs --arch x86_64
cargo starry build
cargo starry qemu
```

### 2.4 LoongArch64

LoongArch64 路径更适合在主流架构已经跑通之后再验证。这样出现问题时，也更容易区分是环境问题还是实验性架构路径带来的差异。

```bash
cargo starry defconfig qemu-loongarch64
cargo starry qemu
```

分步执行：

```bash
cargo starry defconfig qemu-loongarch64
cargo starry rootfs --arch loongarch64
cargo starry build
cargo starry qemu
```

> `starry rootfs` 当前使用 `--arch`，不是 `--target`。  
> `starry qemu` 的 `--target` 可接受完整 target triple，也可接受简写架构名。

## 3. 开发板快速启动

### 3.1 LicheeRV-Nano-SG2002

LicheeRV-Nano-SG2002 当前走 U-Boot 串口启动路径，适合在已经烧录并能正常进入 Linux 的开发板上验证 StarryOS。StarryOS 直接使用板上的 Linux 原生 ext4 根文件系统，默认根分区为 `root=/dev/mmcblk0p2`，不需要再单独制作 Starry rootfs 分区。

#### 3.1.1 相关 crates 与驱动

| 类型 | crates | feature 或实现位置 | 作用 |
| --- | --- | --- | --- |
| 早期启动 | `someboot` | `platforms/someboot/src/arch/riscv64/` | 接收 U-Boot 传入的 FDT，建立页表并进入内核 |
| CPU 与动态平台 | `ax-cpu`、`axplat-dyn`、`ax-hal` | `axplat-dyn` feature `thead-mae` | 提供玄铁 C906/RISC-V 上下文、陷阱和动态平台接口 |
| 板级支持 | `starry-kernel`、`sg200x-bsp` | `starry-kernel` feature `sg2002` | 提供 SG2002 板级设备和用户态支持 |
| 驱动发现 | `rdrive`、`ax-driver` | `drivers/ax-driver/` | 根据 FDT 探测并注册板载设备 |
| 串口 | `ax-driver`、`some-serial`、`rdif-serial` | `ax-driver` feature `serial` | 注册运行期硬件控制台和 TTY |
| SD 卡 | `ax-driver`、`cv181x-sdhci`、`sdmmc-protocol`、`rdif-block` | `ax-driver` feature `cvsd` | 初始化 SD 卡并向文件系统提供 block device |
| 根文件系统 | `ax-fs-ng`、`rsext4` | — | 挂载 `/dev/mmcblk0p2` 上的 ext4 rootfs |

板卡构建配置位于 `os/StarryOS/configs/board/licheerv-nano-sg2002.toml`。其中 `cvsd` feature 会启用 CV181x SDHCI、SD/MMC 协议和块设备接口，`sg2002` feature 提供 StarryOS 所需的 SG2002 板级支持。

#### 3.1.2 启动准备与构建

实板启动前需要准备：

- 能正常进入 U-Boot 的 LicheeRV-Nano-SG2002；
- 已烧录并能启动 Linux 的 SD 卡；
- SD 卡第二分区中可由 StarryOS 挂载的 ext4 根文件系统；
- 用于 U-Boot 和 StarryOS 交互的串口连接。

选择 SG2002 构建配置并单独构建内核：

```bash
cargo starry defconfig licheerv-nano-sg2002
cargo starry build
```

也可以不修改默认配置，直接显式指定配置文件：

```bash
cargo starry build \
  --config os/StarryOS/configs/board/licheerv-nano-sg2002.toml
```

该配置使用 `riscv64gc-unknown-none-elf` 目标，并启用 SG2002 板级支持、T-Head MAE、SD 卡和串口驱动。后面的 `cargo starry uboot` 或 `cargo starry board` 都会自动构建，因此只想快速启动时可以跳过这里的 `cargo starry build`。

#### 3.1.3 通过 U-Boot 启动

本地串口启动使用 `uboot` 子命令。默认配置来自 `os/StarryOS/configs/board/licheerv-nano-sg2002-uboot.toml`，串口是 `/dev/ttyUSB0`，波特率为 `115200`：

```bash
cargo starry uboot \
  --uboot-config os/StarryOS/configs/board/licheerv-nano-sg2002-uboot.toml
```

这条路径会构建 `riscv64gc-unknown-none-elf` 目标，并根据 SG2002 的 ITS 模板生成 FIT image，随后通过 U-Boot 的 `loady` 串口传输到 `fit_load_addr = 0x82200000`，再执行 `bootm 0x82200000`。内核入口地址为 `kernel_load_addr = 0x80200000`。

也可以通过 ostool-server 自动完成板卡申请、U-Boot 启动和串口连接：

```bash
cargo starry board \
  --board-config os/StarryOS/configs/board/licheerv-nano-sg2002-board.toml \
  --server <ip> \
  --port <port>
```

`licheerv-nano-sg2002-board.toml` 中维护的是 StarryOS 侧的运行和判定配置：板卡类型为 `LicheeRV-Nano-SG2002`，shell 提示符为 `root@starry:`，超时时间为 600 秒。进入 shell 后会执行：

```bash
echo STARRY_SG2002_BOOT_OK
```

看到下面的输出表示内核启动、SD 卡 rootfs 挂载和用户态 shell 均已成功：

```text
STARRY_SG2002_BOOT_OK
```

如果要使用 test-suit 运行板级启动验证：

```bash
cargo starry test board \
  --board licheerv-nano-sg2002 \
  --server <ip> \
  --port <port>
```

常规远端启动使用 `os/StarryOS/configs/board` 下的配置；板测使用 `test-suit/starryos/board-licheerv-nano-sg2002` 下的配置。若启动停在根设备探测阶段，请确认 SD 卡第二分区存在可挂载的 ext4 根文件系统。

### 3.2 StarFive VisionFive 2（星光 2）

VisionFive 2 与 LicheeRV-Nano-SG2002 一样通过 U-Boot 启动。VisionFive 2 使用动态平台配置，并通过 JH7110 MMC 驱动挂载开发板上已有的 Linux ext4 根文件系统；与 QEMU 路径不同，这里不需要执行 `cargo starry rootfs`。仓库当前已验证的自动化流程由 ostool-server 申请板卡，通过 U-Boot 加载内核并连接串口。

#### 3.2.1 相关 crates 与驱动

| 类型 | crates | feature 或实现位置 | 作用 |
| --- | --- | --- | --- |
| 早期启动 | `someboot` | `platforms/someboot/src/arch/riscv64/` | 接收 U-Boot 传入的 FDT，建立页表并进入内核 |
| CPU 与动态平台 | `ax-cpu`、`axplat-dyn`、`ax-hal` | `components/axcpu/src/riscv/`、`platforms/axplat-dyn/` | 提供 RISC-V 上下文、陷阱和动态平台接口 |
| 中断控制器 | `somehal`、`ax-riscv-plic`、`rdif-intc`、`irq-framework` | `platforms/somehal/src/arch/riscv64/plic.rs` | 从 FDT 探测并驱动 JH7110 PLIC |
| 驱动发现 | `rdrive`、`ax-driver` | `drivers/ax-driver/` | 根据 FDT 探测并注册板载设备 |
| 串口 | `ax-driver`、`some-serial`、`rdif-serial` | `ax-driver` feature `serial` | 注册运行期硬件控制台和 TTY |
| RTC | `ax-driver` | `ax-driver` feature `rtc`；`drivers/ax-driver/src/time/starfive.rs` | 探测 `starfive,jh7110-rtc` |
| 时钟与复位 | `ax-driver`、`rdif-clk`、`rdif-reset` | `ax-driver` feature `starfive-soc`；`drivers/ax-driver/src/soc/starfive/` | 准备 JH7110 MMC 所需的 SYSCRG 时钟和复位 |
| SD/MMC | `starfive-jh7110-dwmmc`、`dwmmc-host`、`sdmmc-protocol`、`rdif-block` | `ax-driver` feature `starfive-jh7110-dwmmc` | 初始化 SD 卡并向文件系统提供 block device |
| 根文件系统 | `ax-fs-ng`、`rsext4` | — | 扫描 SD 卡分区并挂载 ext4 rootfs |

板卡构建配置位于 `os/StarryOS/configs/board/visionfive2.toml`。其中 `starfive-jh7110-dwmmc` feature 会同时启用通用 DWMMC/SD 协议、块设备接口以及 JH7110 SoC 时钟和复位支持。

#### 3.2.2 启动准备与构建

实板启动前需要准备：

- 能正常进入 U-Boot 的 VisionFive 2；
- 开发板上可由 StarryOS 识别的 SD 卡；
- SD 卡中可挂载的 Linux ext4 根文件系统；
- 可访问 VisionFive 2 的 ostool-server 和串口连接。

板卡路径不会像 QEMU 一样自动下载或制作 rootfs 镜像。`ax-fs-ng` 会扫描 JH7110 MMC 块设备及其分区表，并根据 U-Boot 传入的 `root=` 参数选择根分区；没有明确指定时，再从探测到的文件系统中选择。

选择 VisionFive 2 配置并单独构建内核：

```bash
cargo starry defconfig visionfive2
cargo starry build
```

也可以不修改默认配置，直接显式指定配置文件：

```bash
cargo starry build \
  --config os/StarryOS/configs/board/visionfive2.toml
```

该配置使用 `riscv64gc-unknown-none-elf` 目标，并启用串口、RTC 和 `starfive-jh7110-dwmmc` 驱动。后面的 `cargo starry board` 也会自动构建，因此只想快速启动时可以跳过这里的 `cargo starry build`。

#### 3.2.3 通过 U-Boot 启动

当前维护入口使用 ostool-server 驱动 VisionFive 2 的 U-Boot 启动流程：

```bash
cargo starry board \
  --board-config os/StarryOS/configs/board/visionfive2-board.toml \
  --server <ip> \
  --port <port>
```

这条命令会使用 `visionfive2.toml` 构建 StarryOS，并将构建产物转换为板卡运行时需要的内核镜像；随后根据 `visionfive2-board.toml` 向 ostool-server 申请 `VisionFive2`，由服务器控制开发板进入 U-Boot、传输并加载内核。U-Boot 启动内核并传入当前开发板的 FDT 后，StarryOS 会从 FDT 发现 PLIC、串口、RTC 和 JH7110 MMC，从 SD 卡选择并挂载 ext4 rootfs，进入 `root@starry:` shell，最后执行预设的 shell 探针并在成功后释放板卡。

VisionFive 2 的镜像传输方式、U-Boot 加载地址和具体启动命令由 ostool-server 的 `VisionFive2` 板卡配置管理，不在 `visionfive2-board.toml` 中写死。因此不能直接复用 SG2002 的 `loady` 地址或 `bootm` 命令；调试这些参数时应检查所连接板卡服务器的 VisionFive 2 配置和 U-Boot 串口日志。

`visionfive2-board.toml` 中维护的是 StarryOS 侧的运行和判定配置：板卡类型为 `VisionFive2`，shell 提示符为 `root@starry:`，超时时间为 600 秒。进入 shell 后会执行：

```bash
echo STARRY_VISIONFIVE2_SHELL_OK
```

看到下面的输出表示内核启动、MMC rootfs 挂载和用户态 shell 均已成功：

```text
STARRY_VISIONFIVE2_SHELL_OK
```

如果要使用 test-suit 运行同一条板级启动验证：

```bash
cargo starry test board \
  --board visionfive2 \
  --server <ip> \
  --port <port>
```

常规启动使用 `os/StarryOS/configs/board` 下的配置；板测使用 `test-suit/starryos/board-visionfive2` 下的配置。若启动停在根设备探测阶段，请先确认 SD 卡能被 U-Boot/Linux 正常识别，并且其中存在可挂载的 ext4 根文件系统。

## 4. 测试入口

StarryOS 除了单次启动外，更常见的验证方式是直接进入测试套件。这里的命令会读取 `test-suit/starryos` 下的用例配置并运行；迁出的压力测试通过 Starry app 命令显式选择。

```bash
# 全部 test-suit QEMU 测试
cargo starry test qemu --target riscv64gc-unknown-none-elf

# 压力测试
cargo starry app qemu -t stress/git --arch riscv64

# 仅运行指定用例
cargo starry test qemu --target aarch64-unknown-none-softfloat -c qemu-smp1/system

# 其他架构
cargo starry test qemu --target x86_64-unknown-none
cargo starry test qemu --target loongarch64-unknown-none-softfloat
```

如果需要板测：

```bash
cargo starry test board --board orangepi-5-plus --server <ip> --port <port>
cargo starry test board --board licheerv-nano-sg2002 --server <ip> --port <port>
cargo starry test board --board visionfive2 --server <ip> --port <port>
```

详细说明见：[StarryOS 测试套件设计](/docs/build/starry/test)

若需要继续了解 case 结构、rootfs 组织方式和测试实现细节，可以继续阅读：

- [StarryOS 开发指南](/docs/development/starryos)
- [StarryOS 测试套件设计](/docs/build/starry/test)
- [QEMU 运行](/docs/build/overview)
