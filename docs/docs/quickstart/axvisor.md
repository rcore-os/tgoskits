---
sidebar_position: 3
sidebar_label: "Axvisor"
title: "Axvisor 快速上手"
---

# Axvisor 快速上手

Axvisor 通过板卡配置确定目标架构和平台特性。Build TOML 的 `vm_configs` 只把客户机资源打包到 `/guest/builtin` 并建立启动索引；宿主启动参数 `axvisor.vm_start=<id>[,<id>...]` 决定开机创建哪些客户机，缺少该参数时保持零个 VM。`cargo axvisor config ls` 列出配置名称，`cargo axvisor defconfig BOARD_NAME` 写入默认构建配置，后续 `cargo axvisor qemu` 沿用该配置。

```mermaid
flowchart LR
  A[cargo axvisor config ls] --> B[cargo axvisor defconfig board]
  B --> C[cargo axvisor qemu]
  C --> D{单次启动通过?}
  D -- 是 --> E[测试套件]
  D -- 否 --> F[检查环境 / Guest 资产]
  F --> A
```

## 1. QEMU

四种架构使用相同的三步流程：查看配置、选择 QEMU 板卡配置、构建并启动。配置文件定义 target、Axvisor feature 和 builtin VM 资源列表；需要开机客户机时，应在 QEMU 宿主 `-append` 或等效 `cmdline` 中加入 selector。客户机 TOML 的 `[kernel].cmdline` 只传给客户机内核。

### 1.1 AArch64

`qemu-aarch64` 使用 `aarch64-unknown-none-softfloat` target，并启用文件系统和 VirtIO 块设备支持。

```bash
cargo axvisor config ls
cargo axvisor defconfig qemu-aarch64
cargo axvisor qemu
```

### 1.2 RISC-V 64

`qemu-riscv64` 使用 `riscv64gc-unknown-none-elf` target，并启用 `sstc`、文件系统和 VirtIO 块设备支持。

```bash
cargo axvisor config ls
cargo axvisor defconfig qemu-riscv64
cargo axvisor qemu
```

### 1.3 x86_64

`qemu-x86_64` 使用 `x86_64-unknown-none` target，并启用文件系统和 VirtIO 块设备支持。

```bash
cargo axvisor config ls
cargo axvisor defconfig qemu-x86_64
cargo axvisor qemu
```

### 1.4 LoongArch64

LoongArch64 路径依赖 LVZ 虚拟化扩展，必须使用专用的 [QEMU-LVZ](https://github.com/Hengyu-Yu/QEMU-LVZ)，不能使用标准 `qemu-system-loongarch64`。

```bash
cargo axvisor config ls
cargo axvisor defconfig qemu-loongarch64
cargo axvisor qemu
```

完成 `defconfig` 后，后续命令通常不需要重复传入 `--target` 或 `--arch`。切换架构时重新执行以上三步即可。

## 2. U-Boot 测试

当需要贴近板级启动链路时，可以进入 `test uboot`。这一入口通过 `test-suit/axvisor/normal/board-<platform>/` 目录发现 `(board, guest)` 组合，而非硬编码白名单。

当前 `test uboot` 已维护的板型组合示例：

```bash
cargo xtask axvisor test uboot --board orangepi-5-plus --guest linux
cargo xtask axvisor test uboot --board phytiumpi --guest linux
cargo xtask axvisor test uboot --board roc-rk3568-pc --guest linux
```

## 3. Board 测试

`test board` 依赖已有板级环境或 self-hosted runner。这里的命令按
test-suit 中的板卡名选择平台；指定 `--board` 后，会依次运行测试命名空间中匹配的
`board-*.toml` 测例。`test board` 不再扫描 `apps/axvisor/`，因此下面的 OrangePi
命令只运行测试套件中的匹配用例，不包含 PCIe 网卡直通应用。

当前 `test board` 使用板卡名：

```bash
cargo xtask axvisor test board --board orangepi-5-plus-linux
cargo xtask axvisor test board --board rock-4d-linux
```

应用使用独立的 `app` 入口。先列出应用，再运行 PCIe 网卡直通；`-t` 相对
`apps/axvisor/normal/`，实体板卡使用运行配置中的 `board_type`。运行前按
[PCIe 网卡应用说明](https://github.com/rcore-os/tgoskits/blob/dev/apps/axvisor/normal/board-ballpick-robot/pci-network/README.md)
准备内核、匹配模块和独立根盘，并按环境添加 `--server`、`--port`。

```bash
cargo xtask axvisor app list
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/pci-network/ping \
  --board orangepi-5-plus-linux
```

应用目录前缀选择、QEMU 入口和客户机构建准备规则见[测试与应用入口](../build/axvisor/test#8-应用入口)。

> Board 测试通常需要 self-hosted runner、串口服务器或物理板环境，本地普通开发机通常无法直接复现。
>
> ROCK 4D 的 Linux kernel 和 guest DTB 从板卡文件系统加载。首次运行、更新 BSP kernel 或修改 guest DTS 后，必须先按 [ROCK 4D Linux Guest](../build/axvisor/rock-4d) 构建并部署这两项资产。
