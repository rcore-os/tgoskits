---
sidebar_position: 1
sidebar_label: "概述"
---

# StarryOS

StarryOS 在三大子系统中**命令面最广**：它编译整个内核（无需 `--package`），并增加了 rootfs 管理、app 运行、性能剖析（qperf）、内核模块（kmod）编译等特有命令。这与 [ArceOS](../arceos/overview)（app 模块化）和 [Axvisor](../axvisor/overview)（hypervisor + 多 guest）形成对比。

本目录详细描述 StarryOS 的全部命令。深入的主题有独立文档：

- [StarryOS 构建](./build)：内核构建、注入的环境变量、动态平台
- [StarryOS 运行](./runtime)：QEMU / U-Boot / 板卡三种运行目标、rootfs 与 DNS/APK 配置
- [StarryOS 测试](./test)：平铺 test-suit、QEMU 聚合 case、Board 测试
- [应用运行](./app)：`apps/starry/` 下的 app list / qemu / board
- [性能剖析](./perf)：qperf 火焰图与 callchain
- [内核模块](./kmod)：可加载内核模块（`.ko`）编译
- [rootfs 准备](./rootfs)：独立预拉取 rootfs 镜像

通用的参数解析、Snapshot、Build Info 和动态平台构建约定详见 [参数与配置](../configuration)。

## 子命令

```text
cargo xtask starry <subcommand> [options]
```

| 子命令 | 说明 | 详细文档 |
|--------|------|----------|
| `build` | 编译整个 StarryOS 内核 | [构建](./build) |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs 准备） | [运行](./runtime) |
| `uboot` | 编译并通过 U-Boot 运行 | [运行](./runtime) |
| `board` | 编译并在远程板卡运行 | [运行](./runtime) |
| `test qemu` | QEMU 测试 | [测试](./test) |
| `test board` | 板级测试 | [测试](./test) |
| `app list` / `app qemu` / `app board` | `apps/starry/` 应用运行 | [应用运行](./app) |
| `perf` | qperf 性能剖析（火焰图/callchain） | [性能剖析](./perf) |
| `kmod build` | 编译内核模块（`.ko`） | [内核模块](./kmod) |
| `rootfs` | 按架构准备默认 managed rootfs | [rootfs 准备](./rootfs) |
| `defconfig <board>` | 生成默认板卡配置 | 见下文 |
| `config ls` | 列出可用板卡名称 | 见下文 |
| `quick-start ...` | 旧版常见平台便捷入口，后续会废弃 | 见下文 |

## 参数

**通用参数**（`build` / `qemu` / `uboot` / `board`）：`--arch`、`--target`、`--config`、`--smp`、`--debug`。默认架构为 `riscv64`。

**QEMU 额外参数**：`--qemu-config`、`--rootfs`
**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

## 特有行为

### 不需要 `--package`

StarryOS 编译完整的 Linux 兼容内核镜像（单一 ELF），不存在 ArceOS 那样的"多 app 选择"问题。`--package` 对 StarryOS 不适用。

### 默认架构 `riscv64`

StarryOS 的默认架构是 `riscv64`（与 ArceOS 和 Axvisor 的 `aarch64` 不同），反映其最常用的开发和测试目标。详见 [参数与配置 §默认值](../configuration#默认值)。

### rootfs 是一等公民

StarryOS 运行强依赖 rootfs（Alpine/Debian 用户空间）。`cargo starry qemu` 在运行前自动准备 rootfs；`cargo starry rootfs` 可独立预拉取。详见 [rootfs 准备](./rootfs) 和 [镜像管理](../image)。

## defconfig：生成默认板卡配置

```bash
cargo starry defconfig <board>
```

把对应板卡配置复制到默认构建配置位置，并更新 StarryOS 命令快照。之后的 `build`/`qemu`/`uboot`/`board` 会沿用该配置。`<board>` 是板卡名称，可用 `config ls` 查看。

## config ls：列出可用板卡名称

```bash
cargo starry config ls
```

输出 `os/StarryOS/configs/board/` 目录下所有可用的板卡配置名称，供 `defconfig <board>` 使用。

## quick-start：旧版便捷入口

```bash
cargo starry quick-start <platform> {build|run}
```

`quick-start` 是旧版常见平台便捷入口，保留用于兼容已有脚本，**后续会废弃**。新文档和新流程不再推荐使用它，请改用 `defconfig` + `build`/`qemu` 流程。

支持的平台（`cargo starry quick-start list` 查看）：

| 平台 | 说明 |
|------|------|
| `qemu-aarch64` / `qemu-riscv64` / `qemu-loongarch64` / `qemu-x86_64` | QEMU 平台的构建/运行 |
| `orangepi-5-plus` | Orange Pi 5 Plus 板卡，run 支持 `--serial`/`--baud`/`--dtb` 覆盖 |
| `licheerv-nano-sg2002` | LicheeRV Nano SG2002 板卡，run 支持 `--serial`/`--baud` 覆盖 |

## 推荐配置流程

```bash
cargo starry config ls          # 查看支持的板卡名称
cargo starry defconfig <board>  # 复制板卡配置到默认构建位置，并更新快照
cargo starry build              # 之后所有命令沿用该配置
cargo starry qemu
cargo starry uboot
cargo starry board
```

## 用法示例

```bash
# 默认 riscv64 构建并在 QEMU 运行
cargo starry build
cargo starry qemu

# 切换架构（基于 snapshot 复用）
cargo starry build --arch aarch64
cargo starry qemu

# 板卡流程
cargo starry config ls
cargo starry defconfig orangepi-5-plus
cargo starry board

# 性能剖析
cargo starry perf --format Svg --arch riscv64

# 运行 apps/starry/ 下的应用
cargo starry app qemu --all
cargo starry app board -t my-board-app

# 内核模块
cargo starry kmod build --all
```
