---
sidebar_position: 1
sidebar_label: "概述"
---

# StarryOS

StarryOS 在三大子系统中**命令面最广**：它编译整个内核（无需 `--package`），并增加了 rootfs 管理、app 运行、性能剖析（qperf）、内核模块（kmod）编译等特有命令。test-suit 用例直接从 `test-suit/starryos/` 根目录发现，压力/K230/visual 等重型用例迁移到 `apps/starry/` 后通过 `app` 命令显式运行。

StarryOS 复用 axbuild 的全部公共能力——构建流水线、QEMU/U-Boot/板卡运行、测试框架——这些公共部分见：

- 构建过程(../build_process)：八阶段流水线、Feature 解析、axconfig 生成
- 运行时环境(../runtime)：QEMU/U-Boot/板卡三种运行目标
- 测试框架(../test_framework)：用例发现、分组构建、资产准备、结果判定
- [StarryOS 测试](./test)：平铺 test-suit、QEMU 聚合 case、Board 测试

本文只描述 StarryOS 特有的命令结构、参数和行为。

## 子命令

```text
cargo xtask starry <subcommand> [options]
```

| 子命令 | 说明 |
|--------|------|
| `build` | 编译整个 StarryOS 内核 |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | QEMU 测试 |
| `test board` | 板级测试 |
| `app list` | 列出 `apps/starry/` 下发现的可运行应用 |
| `app qemu` | 构建并在 QEMU 中运行 `apps/starry/` 下的应用 |
| `app board` | 在远程板卡上运行应用 |
| `perf` | qperf 性能剖析（火焰图/callchain） |
| `kmod build` | 编译内核模块（`.ko`） |
| `rootfs` | 按架构准备默认 managed rootfs，并打印 image storage 中的最终路径 |
| `defconfig <board>` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |
| `quick-start ...` | 旧版常见平台便捷入口，后续会废弃 |

## 参数

**通用参数**（`build` / `qemu` / `uboot` / `board`）：`--arch`、`--target`、`--config`、`--smp`、`--debug`。默认架构为 `riscv64`。

**QEMU 额外参数**：`--qemu-config`、`--rootfs`
**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`
**测试参数**（`test qemu`）：`--arch`（与 `--target`/`--list` 三选一）、`--target`、`--test-case`、`--list`
**测试参数**（`test board`）：`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`
**App 参数**（`app list`）：`--kind qemu|board`
**App 参数**（`app qemu`）：`--all`、`-t/--test-case`、`--cap`（可重复）、`--arch`、`--qemu-config`、`--debug`
**App 参数**（`app board`）：`-t/--test-case`（必需）、`--board-config`、`-b/--board-type`、`--server`、`--port`、`--debug`

## 推荐配置流程

```bash
cargo starry config ls          # 查看支持的板卡名称
cargo starry defconfig <board>  # 复制板卡配置到默认构建位置，并更新快照
cargo starry build              # 之后所有命令沿用该配置
cargo starry qemu
cargo starry uboot
cargo starry board
```

`cargo starry defconfig <board>` 把对应板卡配置复制到默认构建配置位置，并更新 StarryOS 命令快照。之后的 `build`/`qemu`/`uboot`/`board` 会沿用该配置。

`quick-start` 是旧版便捷入口，保留用于兼容已有脚本，后续会废弃；新文档和新流程不再推荐使用它。

## 特有行为

### 不需要 `--package`

StarryOS 编译完整的 Linux 兼容内核镜像（单一 ELF），不存在 ArceOS 那样的"多 app 选择"问题。`--package` 对 StarryOS 不适用。

### 默认架构 `riscv64`

StarryOS 的默认架构是 `riscv64`（与 ArceOS 和 Axvisor 的 `aarch64` 不同），反映其最常用的开发和测试目标。详见 [参数与配置 §默认值](../configuration#默认值)。

### rootfs 是一等公民

StarryOS 运行强依赖 rootfs（Alpine/Debian 用户空间）。`cargo starry rootfs` 按架构准备默认 managed rootfs 并打印最终路径；`cargo starry qemu` 在运行前自动准备 rootfs。rootfs 镜像的下载/缓存/注入逻辑详见 [运行时环境 §Rootfs 基础设施](../runtime#rootfs-基础设施) 和 [镜像管理](../image)。

### `app` 子命令：重型/板端应用入口

压力测试、K230、visual 等重型用例已从 `test-suit/starryos/` 迁移到 `apps/starry/`，通过 `app qemu` / `app board` 显式运行：

- `app list --kind qemu|board` 发现 `apps/starry/` 下的应用
- `app qemu --all` 运行所有匹配应用；`-t <case>` 选择单个
- 带**能力要求**的应用可通过 `--cap <CAP>` 声明（如 `--cap board:OrangePi-5-Plus`）
- `app board -t <case>` 在远程板卡上运行 `apps/starry/<case>/` 下的板端应用，每个应用目录包含 `init.sh` 启动脚本以及自动发现的 `board-*.toml`、`build-*.toml`

### `perf`：qperf 性能剖析

`cargo starry perf` 构建 StarryOS 并通过 qperf 进行性能剖析，输出火焰图（SVG/HTML/Folded）、Pprof 或 callchain 数据。关键参数：

- `--format Folded|Svg|Pprof|All`（默认 `All`）
- `--mode Tb|Insn`（trace buffer 或指令级采样）
- `--callchain Leaf|Fp|Logical`、`--debuginfo`、`--force-frame-pointers`（控制解栈质量）
- `--shell-init-cmd`、`--start-marker`/`--stop-marker`（控制采样窗口）
- `--host-perf`、`--host-time`（采集 QEMU 进程的 host 指标）
- `--output-dir`（最终报告位于 `<DIR>/perf/<arch>/latest`）

完整参数列表见 `cargo starry perf --help`，或 [命令索引](../commands)。

### `kmod build`：内核模块

`cargo starry kmod build` 编译 StarryOS 可加载内核模块（`.ko`）：

```text
cargo xtask starry kmod build [--arch <ARCH>] [--target <TARGET>] [--config <PATH>] [--smp <N>] [--debug] \
                              [-m/--module <PATH>... | --all] [--rootfs <IMAGE>]
```

模块从 `os/StarryOS/lkm/` 目录或 `--module` 显式指定的路径发现。Rust 模块复用 StarryOS 内核构建配置，使用独立链接脚本 `os/StarryOS/scripts/kmod-linker.ld` 把 rlib 部分链接为 ET_REL `.ko`；Linux Kbuild C 模块仅在所选架构与 host 架构相同时调用模块目录自带的 Makefile。`--rootfs` 指定时产物通过 `debugfs` 注入镜像的 `/modules/`。`--all` 与 `--module` 互斥；两者都未提供时默认扫描 `os/StarryOS/lkm/`。

### 测试：平铺 test-suit + build wrapper

StarryOS 测试直接从 `test-suit/starryos/` 根目录发现，通过 **build wrapper**（含 `build-<target>.toml` 的目录，如 `qemu-smp1`、`qemu-smp4`、`board-*`）划分构建组。同一 wrapper 下的 case 共享一次内核构建。详见 [StarryOS 测试](./test)。

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
```
