---
sidebar_position: 1
sidebar_label: "概述"
---

# Axvisor

Axvisor 是 TGOSKits 的 Hypervisor 子系统。与 [ArceOS](../arceos/overview)（app 模块化）和 [StarryOS](../starry/overview)（单内核）不同，Axvisor 编译的是**虚拟机监控器**本身，同时需要管理一个或多个 Guest VM 的配置（`--vmconfigs`）、Guest 镜像和 rootfs。Axvisor 也是**唯一支持 `test uboot` 测试模式**和**唯一需要 LVZ 扩展版 QEMU（loongarch64）**的子系统。

本目录详细描述 Axvisor 的全部命令。深入的主题有独立文档：

- [Axvisor 构建](./build)：Hypervisor 编译、`defplat→myplat` 归一化、板卡配置自动复制
- [Axvisor 运行](./runtime)：QEMU / U-Boot / 板卡、loongarch64 LVZ QEMU、Guest UEFI firmware
- [Axvisor 测试](./test)：QEMU / U-Boot / Board 三种测试模式（含 Axvisor 独有的 `test uboot`）

通用的参数解析、Snapshot、Build Info、axconfig 机制详见 [参数与配置](../configuration)。

## 子命令

```text
cargo xtask axvisor <subcommand> [options]
```

| 子命令 | 说明 | 详细文档 |
|--------|------|----------|
| `build` | 编译 Axvisor | [构建](./build) |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs/guest 镜像准备） | [运行](./runtime) |
| `uboot` | 编译并通过 U-Boot 运行 | [运行](./runtime) |
| `board` | 编译并在远程板卡运行 | [运行](./runtime) |
| `test qemu` | QEMU 测试 | [测试](./test) |
| `test uboot` | U-Boot 测试（Axvisor 独有） | [测试](./test) |
| `test board` | 板级测试 | [测试](./test) |
| `defconfig <board>` | 生成默认板卡配置 | 见下文 |
| `config ls` | 列出可用板卡名称 | 见下文 |

## 参数

**通用参数**（`build` / `qemu` / `uboot` / `board`）：`--arch`、`--target`、`--config`、`--plat-dyn`/`--plat_dyn`、`--smp`、`--debug`、`--vmconfigs`。默认架构 `aarch64`。

**QEMU 额外参数**：`--qemu-config <PATH>`、`--rootfs <IMAGE>`
**Board 额外参数**：`--board-config <PATH>`、`-b/--board-type <TYPE>`、`--server <HOST>`、`--port <PORT>`
**U-Boot 测试参数**（`test uboot`）：`--board <TYPE>`（必需）、`--guest <IMAGE>`、`--uboot-config <CFG>`

## 特有行为

### `--vmconfigs`：多 Guest VM 配置

Axvisor 的核心特有参数 `--vmconfigs <PATH>...` 指定一个或多个 VM 配置文件列表。每个 VM 配置描述一个 Guest（如 Linux、StarryOS guest）的内存、CPU、设备和启动来源。Axvisor 在 QEMU 运行前会准备所有引用的 rootfs 和 guest 镜像。详见 [Axvisor 构建](./build) 和 [Axvisor 运行](./runtime)。

### `defplat → myplat` feature 归一化

Axvisor 的 board 配置声明 `ax-std/defplat`，但 Cargo 编译需要 `ax-std/myplat`。`axbuild` 自动归一化，详见 [Axvisor 构建](./build)。

### LoongArch LVZ QEMU

loongarch64 需要定制 QEMU（LVZ 扩展），axbuild 自动定位，详见 [Axvisor 运行](./runtime)。

### 独有的 `test uboot` 模式

Axvisor 是唯一支持 U-Boot 测试模式的子系统，验证"U-Boot → Axvisor → Guest"引导链路。详见 [Axvisor 测试](./test)。

## defconfig：生成默认板卡配置

```bash
cargo xtask axvisor defconfig <board>
```

把对应板卡的默认配置复制到默认构建配置位置（`tmp/axbuild/config/<pkg>/build-<target>.toml`），并更新 Axvisor 命令快照。之后的 `build`/`qemu`/`uboot`/`board` 会沿用该配置。

## config ls：列出可用板卡名称

```bash
cargo xtask axvisor config ls
```

输出 `os/axvisor/configs/board/` 目录下所有可用的板卡配置名称，供 `defconfig <board>` 使用。

## 用法示例

```bash
# 构建 + QEMU 运行（默认 aarch64）
cargo axvisor build
cargo axvisor qemu --vmconfigs os/axvisor/configs/vm/aarch64-linux.toml

# 多个 Guest
cargo axvisor qemu \
    --vmconfigs configs/vm/aarch64-linux.toml \
    --vmconfigs configs/vm/aarch64-starry.toml

# 板卡流程
cargo axvisor config ls
cargo axvisor defconfig <board>
cargo axvisor board

# loongarch64（自动定位 LVZ QEMU）
cargo axvisor qemu --arch loongarch64 --vmconfigs configs/vm/loongarch64-linux.toml

# U-Boot 测试（Axvisor 独有）
cargo axvisor test uboot --board OrangePi-5-Plus --guest <image>
```
