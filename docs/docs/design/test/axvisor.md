---
sidebar_position: 6
sidebar_label: "Axvisor Test-Suit"
---

# Axvisor test-suit 设计

Axvisor 目前没有在 `test-suit/` 目录下放置用例配置文件。其测试基础设施通过**硬编码的板级测试组**定义，配置分布在 `os/axvisor/configs/` 中。

## 1. 测试类型

| 类型 | 说明 | 运行命令 |
|------|------|----------|
| QEMU 测试 | 在 QEMU 中启动 hypervisor 并运行 Guest | `cargo xtask axvisor test qemu --target <arch>` |
| U-Boot 测试 | 通过 U-Boot 引导 hypervisor | `cargo xtask axvisor test uboot --board <board> --guest <guest>` |
| 板级测试 | 在物理开发板上运行 | `cargo xtask axvisor test board [--test-group <group>]` |

## 2. QEMU 测试

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

## 3. U-Boot 测试

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

## 4. 板级测试

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

## 5. 新增测试用例

目前 Axvisor 的测试配置是硬编码在 `scripts/axbuild/src/axvisor/` 中的。新增测试用例需要：

1. 在 `os/axvisor/configs/board/` 下准备构建配置
2. 在 `os/axvisor/configs/vms/` 下准备 VM 配置
3. 在 `os/axvisor/configs/board-test/` 下准备板级测试配置
4. 在 `scripts/axbuild/src/axvisor/` 中注册新的测试组
