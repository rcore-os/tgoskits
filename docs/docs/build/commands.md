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
| `cargo xtask axvisor ...` | `cargo axvisor ...` | Axvisor 命令快捷入口 |
| `cargo xtask board ...` | `cargo board ...` | 板卡管理快捷入口 |
| `cargo xtask ...` | `cargo xtask ...` | 其他命令无额外别名 |

本文大多数通用命令仍展示 `cargo xtask` 前缀；StarryOS 快速上手和板卡配置流程优先展示 `cargo starry` 别名。实际使用时两种写法可以互换。例如：

```bash
# 以下两条命令等价
cargo xtask arceos qemu --package arceos-httpserver
cargo arceos qemu --package arceos-httpserver
```

## 命令总览

axbuild 使用 clap 进行命令行参数解析。顶层命令按 `<os> <action>` 模式组织，其中 `<os>` 为 `arceos`、`starry`、`axvisor` 之一。此外还有一些不绑定特定 OS 的横切命令。

命令按能力分为四类：**构建**（`build`）、**运行**（`qemu`/`uboot`/`board`）、**测试**（`test`）、**辅助**（`config`/`board` 管理等）。

| 命令 | 能力 | 说明 |
|------|------|------|
| `cargo xtask <os> build` | 构建 | 编译 OS 产物 |
| `cargo xtask <os> qemu` | 运行 | 编译并在 QEMU 中运行 |
| `cargo xtask <os> uboot` | 运行 | 编译并通过 U-Boot 运行 |
| `cargo xtask <os> board` | 运行 | 编译并在远程板卡运行 |
| `cargo xtask <os> test qemu` | 测试 | QEMU 测试套件 |
| `cargo xtask <os> test board` | 测试 | 板级测试套件 |
| `cargo xtask <os> test uboot` | 测试 | U-Boot 测试套件（Axvisor 独有） |
| `cargo xtask test` | 测试 | host/std 白名单测试 |
| `cargo xtask clippy` | 测试 | workspace 静态检查 |
| `cargo xtask sync-lint` | 测试 | Relaxed 原子序检查 |
| `cargo xtask spin-lint` | 测试 | 校验无外部 `spin` crate，仅使用 vendored 版本 |
| `cargo xtask image ...` | 辅助 | Guest 镜像管理（ls/pull/resize/check） |
| `cargo xtask axloader ...` | 构建/测试 | UEFI bootloader 构建与 HTTP smoke test |
| `cargo xtask backtrace ...` | 辅助 | host 端 backtrace 符号化 |
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

**测试参数**：`--test-group`/`-g`、`--test-case`/`-c`、`--list`/`-l`、`--no-symbolize`、`--keep-qemu-log`；`--arch` 与 `--target`/`--list` 三选一

`--plat-dyn` 控制是否使用动态平台加载（支持 aarch64、x86_64、riscv64 和 loongarch64 QEMU 路径），`--smp` 设置对称多处理器核数。ArceOS 测试支持 Rust 和 C 两类用例，通过 `--test-group` 选择测试组（`rust`、`c` 或自定义）。每个 Rust QEMU 用例运行结束后默认调用 `cargo xtask backtrace symbolize` 符号化捕获的 backtrace 块；`--no-symbolize` 跳过该步骤，`--keep-qemu-log` 保留 QEMU 日志（默认成功符号化后删除）。

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
| `app qemu` | 构建并在 QEMU 中运行 `apps/starry/` 下发现的应用 |
| `app board` | 在远程板卡上运行应用 |
| `perf` | qperf 性能剖析（火焰图/callchain） |
| `kmod build` | 编译内核模块（.ko） |
| `quick-start` | 旧版常见平台便捷入口，后续会废弃 |
| `rootfs` | 按架构准备默认 managed rootfs，并打印 image storage 中的最终路径 |
| `defconfig` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

推荐的 StarryOS 配置流程是先查看支持的板卡名称，再选择默认配置，后续直接执行常规子命令：

```bash
cargo starry config ls
cargo starry defconfig <board>
cargo starry build
```

`cargo starry defconfig <board>` 会把对应板卡配置复制到默认构建配置位置，并更新 StarryOS 命令快照。之后 `cargo starry build`、`cargo starry qemu`、`cargo starry uboot`、`cargo starry board` 等命令会沿用该配置。`quick-start` 是旧版便捷入口，保留用于兼容已有脚本，后续会废弃；新文档和新流程不再推荐使用它。

旧版 `quick-start` 每个平台包含 `build` 和 `run` 两阶段：

| 子命令 | 说明 |
|--------|------|
| `quick-start list` | 列出所有支持的 quick-start 平台 |
| `quick-start qemu-aarch64 {build,run}` | aarch64 QEMU 平台的构建/运行 |
| `quick-start qemu-riscv64 {build,run}` | riscv64 QEMU 平台的构建/运行 |
| `quick-start qemu-loongarch64 {build,run}` | loongarch64 QEMU 平台的构建/运行 |
| `quick-start qemu-x86_64 {build,run}` | x86_64 QEMU 平台的构建/运行 |
| `quick-start orangepi-5-plus {build,run}` | Orange Pi 5 Plus 板卡的构建/运行，run 支持 `--serial`/`--baud`/`--dtb` 参数覆盖 |
| `quick-start licheerv-nano-sg2002 {build,run}` | LicheeRV Nano SG2002 板卡的构建/本地串口运行，run 支持 `--serial`/`--baud` 参数覆盖 |

`app list` 从 `apps/starry/` 目录发现可运行应用，可通过 `--kind qemu|board` 过滤。`app qemu` 支持用 `--all` 运行所有匹配应用，或用 `-t/--test-case <case>` 选择单个应用；QEMU 应用可通过 `--arch`、`--qemu-config` 覆盖运行配置，带能力要求的应用可通过 `--cap <CAP>` 声明可用能力（如 `--cap board:OrangePi-5-Plus`）。`app board` 从 `apps/starry/<case>/` 目录中按名称发现板端应用，每个应用目录包含 `init.sh` 启动脚本（定义板卡上执行的命令）以及自动发现的 `board-*.toml` 和 `build-*.toml` 配置文件，无需手动指定所有配置路径。

### 参数

**通用参数**：`--arch`、`--target`、`--config`、`--smp`、`--debug`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

**测试参数**（`test qemu`）：`--arch`（与 `--target`/`--list` 三选一）、`--target`、`--test-case`、`--list`

**测试参数**（`test board`）：`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`

**App 参数**（`app list`）：`--kind`

**App 参数**（`app qemu`）：`--all`、`--test-case`/`-t`、`--cap`（可重复）、`--arch`、`--qemu-config`、`--debug`

**App 参数**（`app board`）：`--test-case`/`-t`（必需）、`--board-config`、`--board-type`/`-b`、`--server`、`--port`、`--debug`

板卡运行通过 `ostool-server` 与远程板卡交互，需要指定 `--server` 和 `--port` 参数或通过 `board config` 预先配置。`app board` 用于在远程板卡上快速运行 `apps/starry/` 下的预定义板端应用，每个应用是一个包含 `init.sh` 启动脚本和构建配置的目录。

### perf

`cargo starry perf` 构建 StarryOS 并通过 qperf 进行性能剖析，输出火焰图或 callchain 数据：

```text
cargo xtask starry perf [options]
```

| 参数 | 说明 |
|------|------|
| `-c/--case` | 性能测试用例名（默认 `boot`） |
| `--arch` | 目标架构 |
| `--freq` | 采样频率（Hz，默认 99） |
| `--format` | 输出格式：`Folded`/`Svg`/`Pprof`/`All`（默认 `All`） |
| `--mode` | 采样模式：`Tb`（trace buffer，默认）/ `Insn`（指令级） |
| `--max-depth` | 最大调用栈深度（默认 128） |
| `--timeout` | 采集超时（秒，默认 20） |
| `--output-dir`/`--out` | 输出根目录，最终报告位于 `<DIR>/perf/<arch>/latest` |
| `--host-time`/`--no-host-time` | 收集/禁用 QEMU 进程的 host wall/user/system CPU 时间 |
| `--host-perf` | 在 host 侧用 `perf stat` 采集 QEMU 进程指标 |
| `--host-perf-events` | host perf stat 事件（逗号分隔，默认 `task-clock,cycles,...`） |
| `--shell-init-cmd`/`--workload` | Guest shell 出现 boot 提示后发送的命令 |
| `--shell-prefix` | 发送 `--shell-init-cmd` 前匹配的提示子串 |
| `--start-marker`/`--stop-marker` | Guest stdout 标记，控制采样窗口起止 |
| `--workload-timeout` | 采样窗口超时（秒），超时则停止 QEMU |
| `--qperf-metrics` | 启用 feature-gated 的 in-guest qperf 指标计数 |
| `--flamegraph` | 即使 `--format` 非 SVG 也生成火焰图 |
| `--flamegraph-kind` | 火焰图格式：`Svg`（默认）/`Html`/`Folded` |
| `--full-stack` | 保留本构建可采集的最深栈 |
| `--callchain`/`--perf-callchain` | qperf callchain 模式：`Leaf`（最快）/`Fp`（需帧指针）/`Logical` |
| `--debuginfo`/`--perf-debuginfo` | 添加 DWARF 调试信息并保留符号 |
| `--force-frame-pointers`/`--perf-force-frame-pointers` | 强制帧指针以支持 FP 解栈 |
| `--demangle` | 在 qperf-analyzer 中强制 Rust demangle |
| `--no-truncate` | 火焰图中保留极小帧（min width 设为 0） |
| `--include-kernel-symbols` | 包含内核符号（StarryOS 默认开启） |
| `--include-user-symbols` | 包含用户符号（当前 qperf 仅解析内核 ELF） |
| `--symbol-style` | 折叠栈符号风格：`Full`（默认）/`Short`/`Module` |
| `--focus` | 为匹配正则的帧生成额外的聚焦折叠栈/火焰图 |
| `--kernel-filter` | 仅保留内核态帧 |
| `--smp` | CPU 核数 |
| `--debug` | debug 构建 |

### kmod build

`cargo starry kmod build` 编译 StarryOS 可加载内核模块（`.ko`）：

```text
cargo xtask starry kmod build [--arch <ARCH>] [--target <TARGET>] [--config <PATH>] [--smp <N>] [--debug] \
                              [-m/--module <PATH>... | --all] [--rootfs <IMAGE>]
```

模块从 `os/StarryOS/lkm/` 目录或 `--module` 显式指定的路径发现（支持目录深度 ≤ 10 的自动查找）。Rust 模块复用 StarryOS 内核构建配置的 Cargo 环境，使用独立链接脚本 `os/StarryOS/scripts/kmod-linker.ld` 把 rlib 部分链接为 ET_REL `.ko`；Linux Kbuild C 模块仅在所选架构与 host 架构相同时调用模块目录自带的 Makefile。`--rootfs` 指定时，所有产物会通过 `debugfs` 注入到镜像的 `/modules/` 目录下。

`--all` 与 `--module` 互斥；两者都未提供时默认扫描 `os/StarryOS/lkm/`。

---

## Axvisor

Axvisor 作为 Hypervisor，增加了 `--vmconfigs` 参数指定虚拟机配置列表，`image` 子命令管理 Guest 镜像，并独有 `test uboot` 测试模式。

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
| `test qemu` | QEMU 测试 |
| `test uboot` | U-Boot 测试 |
| `test board` | 板级测试 |
| `defconfig` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

### 参数

**通用参数**：`--arch`、`--target`、`--config`、`--plat-dyn`、`--smp`、`--debug`、`--vmconfigs`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

**测试参数**（`test qemu`）：`--test-group`、`--test-case`、`--list`

**测试参数**（`test board`）：`--test-group`、`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`

**U-Boot 测试参数**（`test uboot`）：`--board`（必需）、`--guest`、`--uboot-config`

在 loongarch64 架构上运行时，axbuild 会自动搜索 LVZ 扩展版 QEMU。若 `--vmconfigs` 中的 Linux guest 使用 `/guest/linux/linux-qemu`，`axvisor/rootfs.rs` 还会把 VM config 复制到 `tmp/axbuild/axvisor/loongarch64/` 并填入可找到的 LoongArch UEFI firmware 路径（优先 `/tmp/ostool/ovmf/loongarch64/code.fd`、`tmp/ostool/ovmf/loongarch64/code.fd`、`tmp/loongarch-uefi-stage1/assets/qemu-binary/QEMU_EFI.fd`）。

---

## 镜像管理

`cargo xtask image` 是独立于各 OS 子系统的顶层命令，管理 TGOS rootfs/Guest 镜像。详细原理（注册表引导、includes 合并、存储结构、SHA-256 校验、子系统集成）见 [镜像管理](./image)。

| 子命令 | 说明 |
|--------|------|
| `image ls [-v] [PATTERN]` | 列出注册表中的镜像，`-v` 显示详细信息（按名称聚合版本），支持正则过滤 |
| `image pull [<IMAGE>] [--arch <ARCH>] [-o DIR] [--no-extract]` | 拉取镜像；省略 `IMAGE` 时配合 `--arch` 拉取该架构默认 rootfs |
| `image resize <IMAGE> --size-mib <MIB> [-o OUTPUT]` | 扩容 ext rootfs 镜像，`-o` 存在时先复制再扩容（不支持缩容） |
| `image check <IMAGE> [--sha256 <HASH>]` | 输出本地镜像 SHA-256，并可选校验期望值 |

全局选项（所有子命令可用）：`-S/--local-storage <PATH>`、`-R/--registry <URL>`、`-N/--no-auto-sync`、`--auto-sync-threshold <SECS>`

---

## Axloader

`cargo xtask axloader` 管理 UEFI bootloader 的构建和测试。

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 axloader（默认 target `x86_64-unknown-uefi`） |
| `test qemu` | 运行 axloader host 单元测试 + QEMU HTTP smoke test |

```text
cargo xtask axloader build [--target <TARGET>] [--release | --debug]
cargo xtask axloader test qemu [--target <TARGET>]
```

`build` 默认 `--target x86_64-unknown-uefi`，`--release` 与 `--debug` 互斥（默认 release）。

HTTP smoke test（`test qemu`）流程：
1. `cargo test -p axloader --all-targets` 运行 host 单元测试
2. `cargo check -p axloader --target <TARGET> --bin axloader` 验证 UEFI target 可编译
3. release 模式构建 UEFI loader，复制到临时 `esp/EFI/BOOT/` 目录
4. 在内存中构造一个最小内核 ELF，由 host 侧的 `SmokeHttpServer`（绑定随机/指定端口）提供
5. 启动 QEMU + OVMF，axloader 通过 slirp 网关（`10.0.2.2:<port>`）HTTP 拉取内核 ELF
6. 校验 axloader 输出的 `AXLOADER BOOT {...}` 启动行中的 `kernel_url`、`kernel_size`、`image_format`、`arch` 等字段

OVMF 固件查找顺序（`X86_64_UEFI_FIRMWARE_CANDIDATES`）：先读 `AXLOADER_X86_64_UEFI_FIRMWARE`，再兼容旧变量 `AXVISOR_X86_64_UEFI_FIRMWARE`，最后依次检查 `/usr/share/OVMF/OVMF_CODE_4M.fd`、`/usr/share/OVMF/OVMF_CODE.fd`、`/usr/share/ovmf/OVMF.fd`、`/usr/share/qemu/OVMF.fd`。

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

### `cargo xtask spin-lint`

校验 workspace 中不存在外部 `spin` crate，仅使用 vendored `components/spin`（v0.12.0）。检查 root manifest `[patch]` 段、workspace member manifest、rust 源码中的 `spin::RwLock` 使用路径、以及 lockfile 中是否意外解析到外部 spin。

### `cargo xtask backtrace symbolize`

host 端 backtrace 符号化工具，从 QEMU/板卡日志中提取 `BACKTRACE_BEGIN/BACKTRACE_END` 区块并使用 `addr2line` 符号化地址：

```text
cargo xtask backtrace symbolize --elf <ELF> [--log <LOG>] [--kind <KIND>] [--adjust-ip] [--ip-bias <BIAS>]
```

| 参数 | 说明 |
|------|------|
| `--elf` | 内核/应用 ELF 文件路径（必需） |
| `--log` | 捕获日志文件路径，省略则从 stdin 读取 |
| `--kind` | 仅符号化匹配种类名的区块 |
| `--adjust-ip` | 是否对 IP 地址做 ARM Thumb 模式调整 |
| `--ip-bias` | IP 地址偏移（如 KASLR 偏移） |

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
