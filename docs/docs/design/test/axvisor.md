---
sidebar_position: 6
sidebar_label: "Axvisor 测试套件"
---

# Axvisor 测试套件设计

Axvisor 目前没有在 `test-suit/` 目录下放置用例配置文件。其测试基础设施通过**硬编码的板级测试组**定义，配置分布在 `os/axvisor/configs/` 中。

## 1. 入口

当前 Axvisor 测试的权威实现入口主要在：

- `scripts/axbuild/src/axvisor/mod.rs`
- `scripts/axbuild/src/axvisor/qemu_test.rs`
- `scripts/axbuild/src/test_qemu.rs`

其中：

- `mod.rs` 负责 `test qemu`、`test uboot`、`test board` 三类命令的调度
- `qemu_test.rs` 负责准备默认 guest 资产与生成 VM config
- `test_qemu.rs` 保存 Axvisor QEMU shell auto-init、U-Boot 测试板型映射、board test group 等硬编码测试定义

## 2. 类型

| 类型 | 说明 | 运行命令 |
|------|------|----------|
| QEMU 测试 | 在 QEMU 中启动 hypervisor 并运行 Guest | `cargo xtask axvisor test qemu --target <arch>` |
| U-Boot 测试 | 通过 U-Boot 引导 hypervisor | `cargo xtask axvisor test uboot --board <board> --guest <guest>` |
| 板级测试 | 在物理开发板上运行 | `cargo xtask axvisor test board [--test-group <group>]` |

## 3. QEMU

QEMU 测试的 Shell 交互配置是硬编码的，不从 TOML 文件读取：

| 架构 | Shell 前缀 | 初始化命令 | 成功判定 |
|------|-----------|-----------|----------|
| `aarch64` | `~ #` | `pwd && echo 'guest test pass!'` | `(?m)^guest test pass!\s*$` |
| `x86_64` | `>>` | `hello_world` | `Hello world from user mode program!` |

**失败判定正则**（所有架构通用）：

- `(?i)\bpanic(?:ked)?\b`
- `(?i)kernel panic`
- `(?i)login incorrect`
- `(?i)permission denied`

**命令行参数：**

```text
cargo xtask axvisor test qemu --target <arch>
```

| 参数 | 说明 |
|------|------|
| `--target` | 目标架构（如 `aarch64`、`x86_64`） |

### 3.1 执行链路

`cargo xtask axvisor test qemu --target <arch>` 的实现主流程为：

1. 在 `mod.rs` 中解析目标架构或 target triple
2. 在 `test_qemu.rs` 中归一化目标，并加载当前架构对应的 shell auto-init 规则
3. 在 `qemu_test.rs` 中准备 guest 资产：
   - `aarch64`：准备 Linux guest 资产并生成 VM config
   - `x86_64`：准备 NimbOS x86_64 guest VM config
4. 组装 Axvisor build request，读取默认 QEMU 模板配置
5. 将 shell auto-init 配置注入 QEMU 运行配置后，交给 `AppContext::qemu(...)`

Axvisor 的 QEMU 测试除命令行参数外，还依赖代码中硬编码的 guest 资产准备和 shell 初始化逻辑。

## 4. U-Boot

U-Boot 测试通过硬编码的板型/客户机映射表定义：

| 板型 | 客户机 | 构建配置 | VM 配置 |
|------|--------|----------|---------|
| `orangepi-5-plus` | `linux` | `os/axvisor/configs/board/orangepi-5-plus.toml` | `os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml` |
| `phytiumpi` | `linux` | `os/axvisor/configs/board/phytiumpi.toml` | `os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml` |
| `roc-rk3568-pc` | `linux` | `os/axvisor/configs/board/roc-rk3568-pc.toml` | `os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml` |

**命令行参数：**

```text
cargo xtask axvisor test uboot --board <board> --guest <guest>
```

| 参数 | 说明 |
|------|------|
| `--board` / `-b` | 板型名称 |
| `--guest` | 客户机类型 |
| `--uboot-config` | 自定义 U-Boot 配置文件路径 |

### 4.1 执行链路

`cargo xtask axvisor test uboot --board <board> --guest <guest>` 的实现主流程为：

1. 在 `test_qemu.rs` 中按 `(board, guest)` 查找硬编码映射
2. 得到对应的 build config 与 VM config
3. 若用户传入 `--uboot-config`，优先使用显式配置；否则走默认 U-Boot 配置搜索
4. 组装 build request，并交给 `AppContext::uboot(...)`

当前 Axvisor 的 U-Boot 测试不采用目录扫描发现，而是**只支持硬编码白名单中的板型/guest 组合**。

## 5. Board

板级测试通过硬编码的测试组定义，每组包含构建配置、VM 配置和板级测试配置：

| 测试组 | 构建配置 | VM 配置 | 板级测试配置 |
|--------|----------|---------|-------------|
| `phytiumpi-linux` | `os/axvisor/configs/board/phytiumpi.toml` | `os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml` | `os/axvisor/configs/board-test/phytiumpi-linux.toml` |
| `orangepi-5-plus-linux` | `os/axvisor/configs/board/orangepi-5-plus.toml` | `os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml` | `os/axvisor/configs/board-test/orangepi-5-plus-linux.toml` |
| `roc-rk3568-pc-linux` | `os/axvisor/configs/board/roc-rk3568-pc.toml` | `os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml` | `os/axvisor/configs/board-test/roc-rk3568-pc-linux.toml` |
| `rdk-s100-linux` | `os/axvisor/configs/board/rdk-s100.toml` | `os/axvisor/configs/vms/linux-aarch64-s100-smp1.toml` | `os/axvisor/configs/board-test/rdk-s100-linux.toml` |

**命令行参数：**

```text
cargo xtask axvisor test board [--test-group <group>] [--board-type <type>] [--server <addr>] [--port <port>]
```

| 参数 | 说明 |
|------|------|
| `--test-group` / `-t` | 指定测试组名（如 `orangepi-5-plus-linux`） |
| `--board-type` / `-b` | 指定板型 |
| `--board-test-config` | 自定义板级测试配置路径 |
| `--server` | 串口服务器地址 |
| `--port` | 串口服务器端口 |

### 5.1 执行链路

`cargo xtask axvisor test board ...` 的实现主流程为：

1. 在 `test_qemu.rs` 中展开一个或多个 board test group
2. 对每个 group：
   - 准备对应 VM config
   - 组装 build request
   - 读取 board run config
   - 调用 `AppContext::board(...)`
3. 汇总失败组并统一报错

Axvisor board test 由**构建配置、VM 配置和板测配置三者共同驱动**，并非单独的板级串口运行步骤。

## 6. 限制

- Axvisor 当前并未在 `test-suit/axvisor/` 下维护可扫描的用例目录；主流测试入口全部由代码中的硬编码映射驱动。
- `test qemu` 当前只支持 `aarch64` 和 `x86_64` 两类目标，相关限制定义在 `scripts/axbuild/src/test_qemu.rs`。
- `test uboot` 仅支持硬编码白名单中的 `(board, guest)` 组合。
- `test board` 仅支持硬编码测试组；即使存在额外配置文件，也不会自动纳入测试矩阵。

## 7. 新增用例

目前 Axvisor 的测试配置是硬编码在 `scripts/axbuild/src/axvisor/` 中的。新增测试用例需要：

1. 在 `os/axvisor/configs/board/` 下准备构建配置
2. 在 `os/axvisor/configs/vms/` 下准备 VM 配置
3. 在 `os/axvisor/configs/board-test/` 下准备板级测试配置
4. 在 `scripts/axbuild/src/axvisor/` 中注册新的测试组
