---
sidebar_position: 2
sidebar_label: "命令参考"
---

# 命令参考

所有命令由 `scripts/axbuild` 实现，通过 `cargo xtask` 统一入口调用。`.cargo/config.toml` 中预配置了 Cargo 别名，使命令可以简写。

## 调用方式与别名

默认调用方式为 `cargo xtask <cmd>`，经 `tg-xtask` 包转发到 `axbuild::run()`：

```text
cargo xtask <cmd>  →  cargo run -p tg-xtask -- <cmd>  →  axbuild::run()
```

`.cargo/config.toml` 中预配置了以下别名，使命令更简洁：

| 完整命令 | 别名 | 说明 |
|----------|------|------|
| `cargo xtask arceos ...` | `cargo arceos ...` | ArceOS 命令快捷入口 |
| `cargo xtask starry ...` | `cargo starry ...` | StarryOS 命令快捷入口 |
| `cargo xtask axloader ...` | `cargo axloader ...` | Axloader 命令快捷入口 |
| `cargo xtask axvisor ...` | `cargo axvisor ...` | Axvisor 命令快捷入口 |
| `cargo xtask board ...` | `cargo board ...` | 板卡管理快捷入口 |
| `cargo xtask ...` | `cargo xtask ...` | 其他命令无额外别名 |

以下文档统一使用 `cargo xtask` 前缀，实际使用时可替换为对应的别名。例如：

```bash
# 以下两条命令等价
cargo xtask arceos qemu --package arceos-httpserver
cargo arceos qemu --package arceos-httpserver
```

## 命令总览

axbuild 使用 clap 进行命令行参数解析。顶层命令按 `<os> <action>` 模式组织，其中 `<os>` 为 `arceos`、`starry`、`axloader`、`axvisor` 之一。此外还有一些不绑定特定 OS 的横切命令。

命令按能力分为四类：**构建**（`build`）、**运行**（`qemu`/`uboot`/`board`）、**测试**（`test`）、**辅助**（`config`/`board` 管理等）。

| 命令 | 能力 | 说明 |
|------|------|------|
| `cargo xtask <os> build` | 构建 | 编译 OS/loader 产物 |
| `cargo xtask <os> qemu` | 运行 | 编译并在 QEMU 中运行（ArceOS/StarryOS/Axvisor） |
| `cargo xtask <os> uboot` | 运行 | 编译并通过 U-Boot 运行（ArceOS/StarryOS/Axvisor） |
| `cargo xtask <os> board` | 运行 | 编译并在远程板卡运行（StarryOS/Axvisor） |
| `cargo xtask {arceos,starry,axloader} test qemu` | 测试 | QEMU 测试套件 |
| `cargo xtask {starry,axvisor} test board` | 测试 | 板级测试套件 |
| `cargo xtask axloader build` | 构建 | 编译 Axloader 产物（复用 Axvisor 包构建配置） |
| `cargo xtask axloader test qemu` | 测试 | Axloader QEMU loader 测试套件 |
| `cargo xtask test` | 测试 | host/std 白名单测试 |
| `cargo xtask clippy` | 测试 | workspace 静态检查 |
| `cargo xtask sync-lint` | 测试 | Relaxed 原子序检查 |
| `cargo xtask config ...` | 辅助 | 配置生成与检查 |
| `cargo xtask board ...` | 辅助 | 板卡管理（ls/connect/config） |

`cargo xtask <os> qemu` 等运行类命令会先触发构建再执行运行，因此用户通常不需要单独先 `build` 再运行。

---

## ArceOS

ArceOS 以模块化 app 的方式组织，需要显式指定 `--package`（如 `arceos-httpserver`），每个包对应一个独立的可运行应用。

```text
cargo xtask arceos <subcommand> [options]
```

### 子命令

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 |
| `qemu` | 编译并在 QEMU 中运行 |
| `uboot` | 编译并通过 U-Boot 运行 |
| `test qemu` | QEMU 测试（Rust + C） |

### 参数

**通用参数**：`--package`（必需）、`--arch`、`--target`、`--config`、`--plat-dyn`、`--smp`、`--debug`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**测试参数**：`--test-group`、`--test-case`、`--list`

`--plat-dyn` 控制是否使用动态平台加载（仅 aarch64 支持），`--smp` 设置对称多处理器核数。ArceOS 测试支持 Rust 和 C 两类用例，通过 `--test-group` 选择测试组。

---

## StarryOS

StarryOS 编译整个内核（不需要 `--package`），增加了 rootfs 管理和 app 运行命令。test-suit 用例直接从 `test-suit/starryos/` 根目录发现，压力、K230 和 visual 等重型用例迁到 `apps/starry/` 后通过 app 命令显式运行。

```text
cargo xtask starry <subcommand> [options]
```

### 子命令

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | QEMU 测试 |
| `test board` | 板级测试 |
| `app list` | 列出 `apps/starry/` 下发现的可运行应用 |
| `app run` | 构建并运行 `apps/starry/` 下发现的应用 |
| `app board` | 在远程板卡上运行应用 |
| `quick-start` | 常见平台便捷入口 |
| `rootfs` | 下载 rootfs 到 target 目录 |
| `defconfig` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

`quick-start` 提供常见 QEMU 平台和 Orange Pi 5 Plus 的简化工作流，每个平台包含 `build` 和 `run` 两阶段：

| 子命令 | 说明 |
|--------|------|
| `quick-start list` | 列出所有支持的 quick-start 平台 |
| `quick-start qemu-aarch64 {build,run}` | aarch64 QEMU 平台的构建/运行 |
| `quick-start qemu-riscv64 {build,run}` | riscv64 QEMU 平台的构建/运行 |
| `quick-start qemu-loongarch64 {build,run}` | loongarch64 QEMU 平台的构建/运行 |
| `quick-start qemu-x86_64 {build,run}` | x86_64 QEMU 平台的构建/运行 |
| `quick-start orangepi-5-plus {build,run}` | Orange Pi 5 Plus 板卡的构建/运行，run 支持 `--serial`/`--baud`/`--dtb` 参数覆盖 |

`app list` 从 `apps/starry/` 目录发现可运行应用，可通过 `--kind qemu|board` 过滤。`app run` 支持用 `--all` 运行所有匹配应用，或用 `-t/--test-case <case>` 选择单个应用；QEMU 应用可通过 `--arch`、`--qemu-config` 覆盖运行配置，带能力要求的应用可通过 `--cap <CAP>` 声明可用能力。`app board` 从 `apps/starry/<case>/` 目录中按名称发现板端应用，每个应用目录包含 `init.sh` 启动脚本（定义板卡上执行的命令）以及自动发现的 `board-*.toml` 和 `build-*.toml` 配置文件，无需手动指定所有配置路径。

### 参数

**通用参数**：`--arch`、`--target`、`--config`、`--smp`、`--debug`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

**测试参数**（`test qemu`）：`--test-case`、`--list`

**测试参数**（`test board`）：`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`

**App 参数**（`app list`）：`--kind`

**App 参数**（`app run`）：`--all`、`--test-case`、`--kind`、`--cap`、`--arch`、`--qemu-config`、`--debug`

**App 参数**（`app board`）：`--test-case`（必需）、`--board-config`、`--board-type`、`--server`、`--port`、`--debug`

板卡运行通过 `ostool-server` 与远程板卡交互，需要指定 `--server` 和 `--port` 参数或通过 `board config` 预先配置。`app board` 用于在远程板卡上快速运行 `apps/starry/` 下的预定义板端应用，每个应用是一个包含 `init.sh` 启动脚本和构建配置的目录。

---

## Axvisor

Axvisor 作为 Hypervisor，增加了 `--vmconfigs` 参数指定虚拟机配置列表，`image` 子命令管理 Guest 镜像，并保留 build/run/board/U-Boot 相关入口。QEMU loader 测试已迁移到独立的 `axloader test qemu`。

```text
cargo xtask axvisor <subcommand> [options]
```

### 子命令

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | 已迁移；请使用 `cargo xtask axloader test qemu` |
| `test uboot` | U-Boot 测试 |
| `test board` | 板级测试 |
| `image ls` | 列出可用的 Guest 镜像 |
| `image pull` | 拉取并解压 Guest 镜像 |
| `defconfig` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

### 参数

**通用参数**：`--arch`、`--target`、`--config`、`--plat-dyn`、`--smp`、`--debug`、`--vmconfigs`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

**测试参数**（`test board`）：`--test-group`、`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`

**U-Boot 测试参数**（`test uboot`）：`--board`（必需）、`--guest`、`--uboot-config`

在 loongarch64 架构上运行时，axbuild 会自动搜索 LVZ 扩展版 QEMU。

### 镜像管理

Axvisor 的 `image` 子命令管理 Guest 虚拟机镜像。镜像名称格式为 `<name>:<tag>`（如 `linux:riscv64`），从 `arceos-hypervisor/axvisor-guest` 仓库拉取。

| 子命令 | 说明 |
|--------|------|
| `image ls [-v] [PATTERN]` | 列出可用的 Guest 镜像，`-v` 显示详细信息，支持 glob 模式过滤 |
| `image pull <IMAGE> [-o DIR] [--no-extract]` | 拉取 Guest 镜像到本地存储，默认自动解压 |

全局选项：`-S/--local-storage`（本地存储路径）、`-R/--registry`（镜像仓库地址）、`-N/--no-auto-sync`（禁用自动同步）、`--auto-sync-threshold`（自动同步阈值）

---

## Axloader

Axloader 是当前 Axvisor loader QEMU 测试的独立入口。`dev` 分支还没有单独的 axloader workspace package，因此构建内部复用 Axvisor 包和 `AXVISOR_VM_CONFIGS`，但命令状态写入独立的 `.axloader.toml`。

```text
cargo xtask axloader <subcommand> [options]
```

### 子命令

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 Axloader 产物 |
| `test qemu` | QEMU loader 测试 |

### 参数

**Build 通用参数**：`--config`、`--arch`、`--target`、`--plat-dyn`、`--smp`、`--debug`、`--vmconfigs`

**QEMU 测试参数**：`--arch`、`--target`、`--test-group`、`--test-case`、`--list`

---

## Host 端检查

### `cargo xtask test`

对 `scripts/test/std_crates.csv` 白名单中的每个 crate 执行 `cargo test -p <package>`。白名单机制确保只有已知能在当前环境中通过的 crate 被纳入测试。

### `cargo xtask clippy`

对 workspace 包进行多维 clippy 检查：

- 默认：检查全部 workspace 包
- `--all`：检查全部 workspace 包（显式全量模式）
- `--package <name>`：检查指定包（可重复，与 `--all`、`--since` 互斥）
- `--since <ref>`：仅检查自指定 git ref 以来变更并受影响的 workspace 包
- 对每个包检查所有 feature 组合和 `docs.rs` 目标平台

### `cargo xtask sync-lint`

扫描 workspace 中 Rust 源文件，检测可疑的 `Relaxed` 原子序使用。支持 `--since <ref>` 参数进行增量检查。

---

## 辅助命令

### `cargo xtask config`

配置生成与检查辅助命令：

| 子命令 | 说明 |
|--------|------|
| `platform-path --package <pkg>` | 定位平台包的 axconfig.toml 路径 |
| `read <SPECS...> --read <ITEM>` | 从合并后的配置规格中读取单个配置值 |
| `generate <SPECS...> --output <PATH>` | 生成合并配置文件，支持 `--oldconfig` 和 `--write KEY=VAL` 覆盖 |
| `inspect --package <pkg>` | 检查平台配置字段，支持 `--manifest-dir`、`--config`、`--makefile` 参数 |

### `cargo xtask board`

板卡管理命令（通过 `ostool-server` 交互）：

| 子命令 | 说明 |
|--------|------|
| `ls` | 列出可用远程板卡类型 |
| `connect -b <type>` | 分配板卡并连接串口 |
| `config` | 编辑板卡服务器配置 |
