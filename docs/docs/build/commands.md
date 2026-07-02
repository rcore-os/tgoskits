---
sidebar_position: 2
sidebar_label: "命令索引"
---

# 命令索引

所有命令由 `scripts/axbuild` 实现，通过 `cargo xtask` 统一入口调用。本文是**完整的命令参考**：列出每个 `cargo xtask` 顶层命令及其全部子命令、参数和用法示例，并提供到详细原理文档的链接。

## 调用方式与别名

默认调用方式为 `cargo xtask <cmd>`，经 `tg-xtask` 包转发到 `axbuild::run()`：

```text
cargo xtask <cmd>  →  cargo run -p tg-xtask -- <cmd>  →  axbuild::run()
```

`.cargo/config.toml` 中预配置了以下别名，使命令更简洁：

| 完整命令 | 别名 |
|----------|------|
| `cargo xtask arceos ...` | `cargo arceos ...` |
| `cargo xtask starry ...` | `cargo starry ...` |
| `cargo xtask axvisor ...` | `cargo axvisor ...` |
| `cargo xtask board ...` | `cargo board ...` |

两种写法等价：

```bash
cargo xtask arceos qemu --package arceos-httpserver
cargo arceos qemu --package arceos-httpserver   # 同上
```

## 顶层命令一览

`cargo xtask` 的顶层命令（与 `tg-xtask --help` 输出一致），按职能分组：

| 命令 | 说明 | 详细文档 |
|------|------|----------|
| **代码质量检查** | | |
| `cargo xtask test` | workspace std 白名单测试 | [Std 白名单测试](./test) |
| `cargo xtask clippy` | workspace clippy（feature × target 矩阵） | [Clippy 检查](./clippy) |
| `cargo xtask sync-lint` | 可疑 `Relaxed` 原子序检查 | [Sync Lint](./sync_lint) |
| `cargo xtask spin-lint` | vendored `spin` 迁移守护 | [Spin Lint](./spin_lint) |
| **辅助工具** | | |
| `cargo xtask board` | 远程板卡管理（ls/connect/config） | [板卡管理](./board) |
| `cargo xtask config` | axconfig 平台配置工具 | [Config 辅助命令](./config_cmd) |
| `cargo xtask backtrace` | host 端 backtrace 符号化 | [Backtrace 符号化](./backtrace) |
| `cargo xtask image` | TGOS rootfs/guest 镜像管理 | [镜像管理](./image) |
| `cargo xtask axloader` | UEFI bootloader 构建与 HTTP smoke 测试 | [Axloader](./axloader) |
| **OS 子系统** | | |
| `cargo xtask arceos` | ArceOS 构建/运行/测试 | [ArceOS](./arceos/overview) |
| `cargo xtask starry` | StarryOS 构建/运行/测试/app/perf/kmod | [StarryOS](./starry/overview) |
| `cargo xtask axvisor` | Axvisor 构建/运行/测试（含 `test uboot`） | [Axvisor](./axvisor/overview) |

通用的参数解析、Snapshot、Build Info、axconfig 机制见 [参数与配置](./configuration)；CI 自动化见 [自动 CI 测试](./ci)。

---

## 代码质量检查

### `test`

对 `scripts/test/std_crates.csv` 白名单中的每个 crate 执行 `cargo test -p <package>`。无参数。

```bash
cargo xtask test
```

详见 [Std 白名单测试](./test)。

### `clippy`

对 workspace 包按 feature × target 矩阵执行 clippy。三种模式互斥：`--all` 或无参数 = 全量；`--package` = 显式；`--since` = 增量。

| 参数 | 说明 |
|------|------|
| `--all` | 审计全部 workspace 包 |
| `--package <PACKAGE>`（可重复） | 仅检查指定的 workspace 包 |
| `--since <REF>` | 仅检查自 git ref 以来变更及受影响的包 |

```bash
cargo xtask clippy                         # 全量（CI 默认）
cargo xtask clippy --package axcpu
cargo xtask clippy --since origin/main
```

详见 [Clippy 检查](./clippy)。

### `sync-lint`

用 `syn` 识别可疑的 `Relaxed` 原子序同步模式。

| 参数 | 说明 |
|------|------|
| `--since <REF>` | 仅检查自 git ref 以来变更的 Rust 文件（省略则全量） |

```bash
cargo xtask sync-lint                     # 全量（CI 默认）
cargo xtask sync-lint --since origin/main # 增量
```

详见 [Sync Lint](./sync_lint)。

### `spin-lint`

守护 vendored `spin` 迁移结果，禁止外部 `spin` 与 `spin::RwLock`。无参数。

```bash
cargo xtask spin-lint
```

详见 [Spin Lint](./spin_lint)。

---

## 辅助工具

### `board`

远程板卡管理，通过 ostool-server 交互。

| 子命令 | 用法 | 说明 |
|--------|------|------|
| `ls` | `board ls [--server <H>] [--port <P>]` | 列出可用板卡类型 |
| `connect` | `board connect -b <TYPE> [--server <H>] [--port <P>]` | 分配板卡并连接串口 |
| `config` | `board config` | 编辑板卡服务器配置 |

详见 [板卡管理](./board)。

### `config`

axconfig 平台配置工具（手动调试和 Makefile 兼容）。

| 子命令 | 用法 | 说明 |
|--------|------|------|
| `platform-path` | `config platform-path --package <PKG>` | 定位平台包的 `axconfig.toml` |
| `read` | `config read -r <ITEM> <SPEC>...` | 合并配置规范，读取单个配置项 |
| `generate` | `config generate -o <OUT> <SPEC>... [-c <OLD>] [-w <K=V>...]` | 合并配置规范生成 TOML |
| `inspect` | `config inspect --package <PKG> [--makefile]` | 输出平台包字段供 Makefile 解析 |

`generate` 参数：`-o/--output <PATH>`（必需）、`-c/--oldconfig <PATH>`（保留旧值）、`-w/--write <table.key=value>`（覆盖项，可重复）。

`inspect` 参数：`--manifest-dir <DIR>`（依赖查找目录）、`--config <PATH>`（显式配置路径）、`--makefile`（单行 key=value 输出）。

详见 [Config 辅助命令](./config_cmd)。

### `backtrace symbolize`

从日志中提取并符号化 `BACKTRACE_BEGIN/BT/BACKTRACE_END` 块。

```bash
cargo xtask backtrace symbolize --elf <PATH> [--log <PATH>] [--kind <KIND>] [--adjust-ip <BOOL>] [--ip-bias <I64>]
```

| 参数 | 默认 | 说明 |
|------|------|------|
| `--elf <PATH>` | 必填 | 用于符号化的 ELF（必须保留 debug info） |
| `--log <PATH>` | stdin | 输入日志路径，省略则读 stdin |
| `--kind <KIND>` | 自动 | 仅符号化匹配的块 kind |
| `--adjust-ip <BOOL>` | `true` | 符号化前 `ip -= 1`（call-site 调整） |
| `--ip-bias <I64>` | `0` | 符号化前对 `ip` 施加有符号偏移（地址 slide） |

```bash
cargo xtask backtrace symbolize --elf target/x86_64/debug/arceos-httpserver --log qemu.log
```

详见 [Backtrace 符号化](./backtrace)。

### `image`

TGOS rootfs/guest 镜像管理。**全局选项**（所有子命令可用）：`-S/--local-storage <PATH>`、`-R/--registry <URL>`、`-N/--no-auto-sync`、`--auto-sync-threshold <SECS>`

| 子命令 | 用法 | 说明 |
|--------|------|------|
| `ls` | `image ls [-v] [PATTERN]` | 列出注册表镜像（`-v` 详情，`PATTERN` 正则过滤） |
| `pull` | `image pull [<IMAGE>] [--arch <ARCH>] [-o <DIR>] [--no-extract]` | 拉取镜像并校验 SHA-256 |
| `resize` | `image resize <IMAGE> --size-mib <MIB> [-o <OUT>]` | 扩容 ext rootfs（不支持缩容） |
| `check` | `image check <IMAGE> [--sha256 <HASH>]` | 输出并可选校验本地镜像 SHA-256 |

`pull` 的 `<IMAGE>` 可选带 `:version`（如 `rootfs-riscv64-alpine.img:v0.0.6`）；省略时配合 `--arch` 拉取该架构默认 rootfs。

详见 [镜像管理](./image)。

### `axloader`

UEFI bootloader（axloader）构建与 HTTP smoke 测试。

| 子命令 | 用法 | 说明 |
|--------|------|------|
| `build` | `axloader build [--target <T>] [--release\|--debug]` | 编译（默认 `x86_64-unknown-uefi`，默认 release） |
| `test qemu` | `axloader test qemu [--target <T>]` | host 单测 + QEMU HTTP smoke test |

详见 [Axloader](./axloader)。

---

## ArceOS

`cargo xtask arceos` 的全部子命令。详细原理见 [ArceOS 概述](./arceos/overview)、[构建](./arceos/build)、[运行](./arceos/runtime)、[测试](./arceos/test)。

**通用参数**（`build` / `qemu` / `uboot` / `board`）：

| 参数 | 说明 |
|------|------|
| `-c/--config <CONFIG>` | 显式 Build Info 路径 |
| `-p/--package <PACKAGE>` | ArceOS app 包名（必需） |
| `--arch <ARCH>` | 目标架构，默认 `aarch64` |
| `-t/--target <TARGET>` | target triple |
| `--plat_dyn <true\|false>` | 是否使用动态平台（默认 true） |
| `--smp <CPUS>` | CPU 核数 |
| `--debug` | debug 构建 |

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 ArceOS app |
| `qemu` | 编译并在 QEMU 中运行 |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | QEMU 测试套件（Rust + C） |
| `test board` | 板级测试套件 |
| `defconfig <BOARD>` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

**各运行目标的额外参数**：

| 子命令 | 额外参数 |
|--------|----------|
| `qemu` | `--qemu-config <PATH>`、`--rootfs <IMAGE>` |
| `uboot` | `--uboot-config <PATH>` |
| `board` | `--board-config <PATH>`、`-b/--board-type <TYPE>`、`--server <HOST>`、`--port <PORT>` |

**测试子命令**：

| 子命令 | 用法 |
|--------|------|
| `test qemu` | `[--arch \| -t/--target \| --list] [-g/--test-group <G>] [-c/--test-case <C>] [--no-symbolize] [--keep-qemu-log]`（三选一） |
| `test board` | `[-c/--test-case <C>] [--board <B>] [-b/--board-type <T>] [--server <H>] [--port <P>] [--list]` |

```bash
cargo arceos build --package arceos-helloworld --arch aarch64
cargo arceos qemu  --package arceos-httpserver
cargo arceos test qemu --arch riscv64 -g rust -c task-yield
```

---

## StarryOS

`cargo xtask starry` 的全部子命令，命令面最广。详细原理见 [StarryOS 概述](./starry/overview)、[构建](./starry/build)、[运行](./starry/runtime)、[测试](./starry/test)、[应用运行](./starry/app)、[性能剖析](./starry/perf)、[内核模块](./starry/kmod)、[rootfs 准备](./starry/rootfs)。

**通用参数**（`build` / `qemu` / `uboot` / `board`）：

| 参数 | 说明 |
|------|------|
| `-c/--config <CONFIG>` | 显式 Build Info 路径 |
| `--arch <ARCH>` | 目标架构，默认 `riscv64` |
| `-t/--target <TARGET>` | target triple |
| `--smp <CPUS>` | CPU 核数 |
| `--debug` | debug 构建 |

| 子命令 | 说明 |
|--------|------|
| `build` | 编译整个 StarryOS 内核 |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` / `test board` | QEMU / 板级测试 |
| `app list` / `app qemu` / `app board` | `apps/starry/` 应用运行 |
| `perf` | qperf 性能剖析 |
| `kmod build` | 编译内核模块 |
| `rootfs` | 准备默认 managed rootfs |
| `defconfig <BOARD>` / `config ls` | 板卡配置 |
| `quick-start ...` | 旧版便捷入口（后续废弃） |

**各运行目标的额外参数**：

| 子命令 | 额外参数 |
|--------|----------|
| `qemu` | `--qemu-config <PATH>`、`--rootfs <IMAGE>` |
| `uboot` | `--uboot-config <PATH>` |
| `board` | `--board-config <PATH>`、`-b/--board-type <TYPE>`、`--server <HOST>`、`--port <PORT>` |

**测试子命令**：

| 子命令 | 用法 |
|--------|------|
| `test qemu` | `[--arch \| -t/--target \| --list] [-c/--test-case <C>]` |
| `test board` | `[-c/--test-case <C>] [--board <B>] [-b/--board-type <T>] [--server <H>] [--port <P>] [--list]` |

**应用运行**（`app`）：

| 子命令 | 用法 | 关键参数 |
|--------|------|----------|
| `app list` | `app list [--kind qemu\|board]` | — |
| `app qemu` | `app qemu [--all] [-t <CASE>] [--cap <CAP>...] [--arch <A>] [--qemu-config <P>] [--debug]` | `--all` 跑全部；`--cap` 声明能力 |
| `app board` | `app board -t <CASE> [--board-config <P>] [-b <T>] [--server <H>] [--port <P>] [--debug]` | `-t` 必需 |

**性能剖析**（`perf`）：

```bash
cargo xtask starry perf [options]
```

| 参数 | 默认 | 说明 |
|------|------|------|
| `-c/--case <NAME>` | `boot` | 性能测试用例名 |
| `--arch <ARCH>` | `riscv64` | 目标架构 |
| `--freq <HZ>` | `99` | 采样频率 |
| `--format` | `all` | `folded`/`svg`/`pprof`/`all` |
| `--mode` | `tb` | `tb`（trace buffer）/ `insn`（指令级） |
| `--max-depth <N>` | `128` | 最大调用栈深度 |
| `--timeout <SEC>` | `20` | 采集超时 |
| `--output-dir <DIR>` | — | 输出根目录，报告位于 `<DIR>/perf/<arch>/latest` |
| `--shell-init-cmd <CMD>` | — | Guest shell 就绪后发送的命令（别名 `--workload`） |
| `--shell-prefix <STR>` | — | 发送 `--shell-init-cmd` 前匹配的提示子串 |
| `--start-marker` / `--stop-marker` | — | 采样窗口起止标记 |
| `--workload-timeout <SEC>` | — | 采样窗口超时 |
| `--host-time` / `--no-host-time` | 开 | 收集/禁用 QEMU 进程 host CPU 时间 |
| `--host-perf` | — | 用 host `perf stat` 采集 QEMU 进程指标 |
| `--host-perf-events <LIST>` | 默认集 | host perf stat 事件（逗号分隔） |
| `--qperf-metrics` | — | 启用 in-guest qperf 指标计数 |
| `--flamegraph` | — | 即使 `--format` 非 SVG 也生成火焰图 |
| `--flamegraph-kind` | `svg` | `svg`/`html`/`folded` |
| `--full-stack` | — | 保留最深栈 |
| `--callchain`（别名 `--perf-callchain`） | — | `leaf`/`fp`/`logical` |
| `--debuginfo`（别名 `--perf-debuginfo`） | — | 添加 DWARF 调试信息 |
| `--force-frame-pointers`（别名 `--perf-force-frame-pointers`） | — | 强制帧指针 |
| `--demangle` | — | 强制 Rust demangle |
| `--no-truncate` | — | 火焰图保留极小帧 |
| `--include-kernel-symbols` | 开 | 包含内核符号 |
| `--include-user-symbols` | — | 包含用户符号 |
| `--symbol-style` | `full` | `full`/`short`/`module` |
| `--focus <REGEX>` | — | 为匹配帧生成聚焦火焰图 |
| `--kernel-filter` | — | 仅保留内核态帧 |
| `--qemu-arg <ARG>`（可重复） | — | 追加原始 QEMU 参数 |
| `--smp <CPUS>` | — | CPU 核数 |
| `--debug` | — | debug 构建 |

详见 [StarryOS 性能剖析](./starry/perf)。

**内核模块**（`kmod build`）：

```bash
cargo xtask starry kmod build [--arch <A>] [--target <T>] [--config <P>] [--smp <N>] [--debug] [-m/--module <PATH>... | --all] [--rootfs <IMAGE>]
```

| 参数 | 说明 |
|------|------|
| `-m/--module <PATH>`（可重复） | 模块 crate 路径（自动查找深度 ≤ 10） |
| `--all` | 构建 `os/StarryOS/lkm/` 下所有模块（与 `--module` 互斥） |
| `--rootfs <IMAGE>` | 把产物注入到此 rootfs 的 `/modules/` |

**其他命令**：

| 子命令 | 用法 |
|--------|------|
| `rootfs` | `rootfs [--arch <ARCH>]`（准备默认 managed rootfs） |
| `defconfig` | `defconfig <BOARD>` |
| `config ls` | `config ls` |
| `quick-start` | `quick-start <platform> {build\|run}`（支持 `qemu-{aarch64,riscv64,loongarch64,x86_64}`/`orangepi-5-plus`/`licheerv-nano-sg2002`，后续废弃） |

```bash
cargo starry build
cargo starry qemu
cargo starry test qemu --arch riscv64
cargo starry app qemu --all
cargo starry perf --format Svg
cargo starry kmod build --all
```

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

`cargo xtask axvisor` 的全部子命令。详细原理见 [Axvisor 概述](./axvisor/overview)、[构建](./axvisor/build)、[运行](./axvisor/runtime)、[测试](./axvisor/test)。

**通用参数**（`build` / `qemu` / `uboot` / `board`）：

| 参数 | 说明 |
|------|------|
| `-c/--config <CONFIG>` | 显式 Build Info 路径 |
| `--arch <ARCH>` | 目标架构，默认 `aarch64` |
| `-t/--target <TARGET>` | target triple |
| `--plat_dyn <true\|false>` | 是否使用动态平台 |
| `--smp <CPUS>` | CPU 核数 |
| `--debug` | debug 构建 |
| `--vmconfigs <VMCONFIGS>`（可重复） | Guest VM 配置文件列表 |

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 Axvisor |
| `qemu` | 编译并在 QEMU 中运行 |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | QEMU 测试 |
| `test uboot` | U-Boot 测试（Axvisor 独有） |
| `test board` | 板级测试 |
| `defconfig <BOARD>` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

**各运行目标的额外参数**：

| 子命令 | 额外参数 |
|--------|----------|
| `qemu` | `--qemu-config <PATH>`、`--rootfs <IMAGE>` |
| `uboot` | `--uboot-config <PATH>` |
| `board` | `--board-config <PATH>`、`-b/--board-type <TYPE>`、`--server <HOST>`、`--port <PORT>` |

**测试子命令**：

| 子命令 | 用法 | 关键参数 |
|--------|------|----------|
| `test qemu` | `[--arch \| -t/--target \| --list] [-g/--test-group <G>] [-c/--test-case <C>]` | 三选一 |
| `test uboot` | `-b/--board <BOARD> [--guest <GUEST>] [--uboot-config <P>]` | `--guest` 默认 `linux` |
| `test board` | `[-g/--test-group <G>] [-c/--test-case <C>] [--board <B>] [-b/--board-type <T>] [--server <H>] [--port <P>] [--list]` | — |

```bash
cargo axvisor build
cargo axvisor qemu --vmconfigs os/axvisor/configs/vm/aarch64-linux.toml
cargo axvisor test uboot --board OrangePi-5-Plus
```
