---
sidebar_position: 1
sidebar_label: "概述"
---

# ArceOS

ArceOS 在三大子系统中**最模块化**：它以 app 为单位组织，每次构建/运行/测试都需要显式指定 `--package`（如 `arceos-httpserver`），每个 package 对应一个独立可运行的应用。这与 [StarryOS](../starry/overview)（编译整个内核，无需 `--package`）和 [Axvisor](../axvisor/overview)（编译 hypervisor + 多个 guest VM 配置）形成对比。

本目录详细描述 ArceOS 的全部命令。深入的主题有独立文档：

- [ArceOS 构建](./build)：八阶段构建流水线、Feature 解析、C 应用构建管线
- [ArceOS 运行](./runtime)：QEMU 运行、运行时资产准备（FAT32 disk image）
- [ArceOS 测试](./test)：Rust feature 测试与 C 用例（`test_cmd`）流程

通用的参数解析、Snapshot、Build Info 和动态平台构建约定详见 [参数与配置](../configuration)。

## 子命令

```text
cargo xtask arceos <subcommand> [options]
```

| 子命令 | 说明 | 详细文档 |
|--------|------|----------|
| `build` | 编译指定 ArceOS app | [构建](./build) |
| `qemu` | 编译并在 QEMU 中运行 | [运行](./runtime) |
| `uboot` | 编译并通过 U-Boot 运行 | [运行](./runtime) |
| `board` | 编译并在远程板卡运行 | [运行](./runtime) |
| `test qemu` | 运行 ArceOS QEMU 测试套件（Rust + C） | [测试](./test) |
| `test board` | 运行 ArceOS 板级测试套件 | [测试](./test) |
| `defconfig <board>` | 生成默认动态板卡配置 | 见下文 |
| `config ls` | 列出可用的板卡名称 | 见下文 |

## 参数

**通用参数**（`build` / `qemu` / `uboot` / `board`）：

| 参数 | 说明 |
|------|------|
| `--package <PKG>`（必需） | ArceOS app 包名，如 `arceos-httpserver` |
| `--arch <ARCH>` | 目标架构，默认 `aarch64` |
| `--target <TRIPLE>` | target triple（与 `--arch` 互为校验） |
| `--config <PATH>` | 显式 Build Info 路径 |
| `--smp <N>` | CPU 核数 |
| `--debug` | debug 构建 |

**QEMU 额外参数**：`--qemu-config <PATH>`、`--rootfs <IMAGE>`
**U-Boot 额外参数**：`--uboot-config <PATH>`
**Board 额外参数**：`--board-config <PATH>`、`--board-type`/`-b`、`--server`、`--port`

**测试参数**（`test qemu`）：`--test-group`/`-g`、`--test-case`/`-c`、`--list`/`-l`、`--no-symbolize`、`--keep-qemu-log`。`--arch`、`--target`、`--list` 三选一。

## 特有行为

### `--package` 必需

ArceOS 把每个可运行的应用建模为 workspace 内的独立 crate（如 `apps/arceos/arceos-httpserver`）。与 StarryOS 的"一次编译整个内核"不同，ArceOS 的构建目标必须由 `--package` 显式锁定，否则报错。`--package` 会被写入 Snapshot，后续短命令（如 `cargo arceos qemu`）会自动复用。

### 动态平台固定启用

ArceOS 当前构建固定走 `axplat-dyn` 路径。Build Info 中不再提供平台选择开关，旧 `plat_dyn` 字段会被拒绝；旧平台选择 feature 会在最终 Cargo 配置中被过滤。

### Rust 测试：feature 即用例

ArceOS 的 Rust 测试收敛到单一 crate `arceos-test-suit`，所有测例按 feature 切分（如 `task-yield`、`fs-basic`）。`--test-case` 直接使用 feature 名，未指定时跑 `all`。所有选中的 feature 在**一次 QEMU 启动**中由 runner 顺序执行，而非每个用例重启 QEMU。详见 [ArceOS 测试](./test)。

### C 测试：`test_cmd` 驱动

C 用例通过目录内的 `test_cmd` 文件定义多轮 `test_one` 指令（指定 `MAKE_VARS` 和 `expect_*.out`），由传统 Makefile 驱动 `defconfig → build → justrun → 比对`。C 用例与 Rust 用例的构建系统完全独立。详见 [ArceOS 测试 §C 用例](./test#c-用例)。

## defconfig：生成默认板卡配置

```bash
cargo xtask arceos defconfig <board>
```

把对应板卡的默认动态平台配置复制到默认构建配置位置（`tmp/axbuild/config/<package>/build-<target>.toml`），并更新 ArceOS 命令快照。之后的 `build`/`qemu` 会沿用该配置。`<board>` 是板卡名称，可用 `config ls` 查看。

## config ls：列出可用板卡名称

```bash
cargo xtask arceos config ls
```

输出 `os/arceos/configs/board/` 目录下所有可用的板卡配置名称，供 `defconfig <board>` 使用。每行一个板卡名。

ArceOS 的 checked-in 配置与 StarryOS、Axvisor 保持相同布局：`board/` 存放
`defconfig` 构建模板，`qemu/` 存放默认使用、也可通过 `--qemu-config` 显式选择的
`qemu-<arch>.toml` 运行模板。应用目录内的同名文件可用于显式指定应用专属的
成功正则、主机服务和运行时磁盘设置。

## 用法示例

```bash
# 构建/运行单个 app
cargo arceos build --package arceos-helloworld --arch aarch64
cargo arceos qemu  --package arceos-httpserver

# 板卡配置流程
cargo arceos config ls
cargo arceos defconfig <board>
cargo arceos build

# 运行全部 Rust 测试组
cargo arceos test qemu --arch riscv64

# 运行单个 Rust feature 用例
cargo arceos test qemu --arch riscv64 -g rust -c task-yield

# 列出某架构下可用的测试用例
cargo arceos test qemu --arch aarch64 --list

# U-Boot / 板卡运行
cargo arceos uboot --package arceos-helloworld --uboot-config <path>
cargo arceos board --package arceos-helloworld --board-config <path>
```
