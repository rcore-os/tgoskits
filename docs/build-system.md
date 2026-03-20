# 构建系统说明

本指南详细介绍 TGOSKits 的构建系统，包括 xtask 工具、Makefile 系统和构建配置。

## 📋 目录

- [构建系统概述](#构建系统概述)
- [xtask 工具](#xtask-工具)
- [Makefile 系统](#makefile-系统)
- [构建配置](#构建配置)
- [环境变量](#环境变量)
- [常见构建任务](#常见构建任务)

## 构建系统概述

TGOSKits 使用多层次的构建系统：

```
┌─────────────────────────────────────┐
│      用户命令行 (cargo xtask)       │
├─────────────────────────────────────┤
│         xtask (Rust 工具)           │  统一构建接口
├─────────────────────────────────────┤
│    Makefile (各子项目)              │  传统构建入口
├─────────────────────────────────────┤
│         Cargo (Rust 构建系统)       │  底层构建
├─────────────────────────────────────┤
│      axbuild (构建辅助库)           │  构建逻辑
└─────────────────────────────────────┘
```

### 构建方式对比

| 方式 | 优点 | 适用场景 |
|------|------|----------|
| **cargo xtask** | 统一接口，自动依赖管理 | 推荐，日常开发 |
| **make** | 灵活，传统方式 | 子项目独立开发 |
| **cargo build** | 直接，底层控制 | 调试构建问题 |

## xtask 工具

### 基本用法

```bash
# ArceOS 构建
cargo xtask arceos build --package arceos-helloworld --arch riscv64
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# StarryOS 构建
cargo xtask starry build --arch riscv64 --package starryos
cargo xtask starry run --arch riscv64 --package starryos

# 测试
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
```

### xtask 子命令

#### ArceOS 命令

```bash
# 构建
cargo xtask arceos build --package <package> [options]

# 运行
cargo xtask arceos run --package <package> [options]
```

**选项：**
- `--arch <arch>`: 目标架构 (riscv64, x86_64, aarch64, loongarch64)
- `--package <package>`: 包名
- `--platform <platform>`: 平台名
- `--release`: Release 模式
- `--features <features>`: 特性列表
- `--smp <num>`: CPU 数量
- `--plat-dyn`: 动态平台

#### StarryOS 命令

```bash
# 构建
cargo xtask starry build --arch <arch> --package <package>

# 运行
cargo xtask starry run --arch <arch> --package <package>

# 准备 rootfs
cargo xtask starry rootfs --arch <arch>
```

#### 测试命令

```bash
# 标准 Rust crate 测试
cargo xtask test std

# ArceOS 测试
cargo xtask test arceos --target <target>

# StarryOS 测试
cargo xtask test starry --target <target>

# Axvisor 测试
cargo xtask test axvisor --target <target>
```

### xtask 实现原理

`xtask` 是一个 Rust 程序，位于 `xtask/` 目录：

```rust
// xtask/src/main.rs
#[derive(Subcommand)]
enum Commands {
    Test {
        #[command(subcommand)]
        command: TestCommand,
    },
    Arceos {
        #[command(subcommand)]
        command: arceos::ArceosCommand,
    },
    Starry {
        #[command(subcommand)]
        command: starry::StarryCommand,
    },
}
```

## Makefile 系统

### ArceOS Makefile

位于 `os/arceos/Makefile`：

```makefile
# 基本选项
ARCH ?= x86_64
LOG ?= warn
MODE ?= release

# QEMU 选项
BLK ?= n
NET ?= n
MEM ?= 128M

# 构建目标
build:
    @$(MAKE) -C make $@

run: build
    @$(MAKE) -C make $@
```

**常用命令：**

```bash
# 构建
make A=examples/helloworld ARCH=riscv64 build

# 运行
make A=examples/helloworld ARCH=riscv64 run

# 清理
make A=examples/helloworld ARCH=riscv64 clean

# 启用网络
make A=examples/httpserver ARCH=riscv64 NET=y run

# 启用块设备
make A=examples/shell ARCH=riscv64 BLK=y run
```

### StarryOS Makefile

位于 `os/StarryOS/Makefile`：

```makefile
# 基本选项
ARCH := riscv64
LOG := warn
MEM := 1G

# rootfs 下载
rootfs:
    @curl -f -L $(ROOTFS_URL)/$(ROOTFS_IMG).xz -O
    @xz -d $(ROOTFS_IMG).xz
    @cp $(ROOTFS_IMG) make/disk.img

# 构建
build:
    @$(MAKE) -C make $@

# 运行
run: build
    @$(MAKE) -C make $@
```

**常用命令：**

```bash
# 准备 rootfs
make rootfs ARCH=riscv64

# 构建并运行
make ARCH=riscv64 run

# 快捷命令
make rv  # RISC-V
make la  # LoongArch64
```

### Axvisor Makefile

位于 `os/axvisor/`，使用 xtask：

```bash
# 配置
cargo xtask defconfig qemu-aarch64

# 构建
cargo xtask build

# 运行
cargo xtask run
```

## 构建配置

### 配置文件位置

```
os/arceos/.axconfig.toml     # ArceOS 配置
os/axvisor/.build.toml       # Axvisor 配置
os/StarryOS/make/            # StarryOS 配置
```

### ArceOS 配置

`.axconfig.toml` 示例：

```toml
[plat]
name = "axplat-riscv64-qemu-virt"

[arch]
name = "riscv64"

[features]
default = ["virtio", "net"]

[log]
level = "info"

[build]
mode = "release"
smp = 1
```

### Axvisor 配置

`configs/board/qemu-aarch64.toml` 示例：

```toml
[build]
arch = "aarch64"
smp = 4
log = "info"

[plat]
name = "axplat-aarch64-qemu-virt"

[features]
default = ["virtio", "gicv3"]

[vm_configs]
vms = ["configs/vms/arceos-aarch64.toml"]
```

### 生成配置

```bash
# ArceOS - 通过构建参数
cargo xtask arceos build --package arceos-helloworld --arch riscv64

# Axvisor - 通过 defconfig
cargo xtask defconfig qemu-aarch64

# 修改配置
cargo xtask menuconfig
```

## 环境变量

### 通用环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `ARCH` | 目标架构 | `x86_64` |
| `LOG` | 日志级别 | `warn` |
| `MODE` | 构建模式 | `release` |
| `SMP` | CPU 数量 | `1` |
| `TARGET_DIR` | 输出目录 | `target` |

### ArceOS 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `A` 或 `APP` | 应用路径 | - |
| `FEATURES` | ArceOS 特性 | - |
| `APP_FEATURES` | 应用特性 | - |
| `BLK` | 块设备支持 | `n` |
| `NET` | 网络支持 | `n` |
| `GRAPHIC` | 图形支持 | `n` |
| `BUS` | 设备总线类型 | `mmio` |
| `MEM` | 内存大小 | `128M` |
| `ACCEL` | 硬件加速 | `n` |

### QEMU 环境变量

| 变量 | 说明 |
|------|------|
| `QEMU_LOG` | QEMU 日志 |
| `NET_DUMP` | 网络包捕获 |
| `NET_DEV` | 网络设备类型 |
| `VFIO_PCI` | PCI 直通地址 |
| `VHOST` | vhost-net 支持 |

### 网络环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `IP` | IPv4 地址 | `10.0.2.15` |
| `GW` | 网关地址 | `10.0.2.2` |

## 常见构建任务

### 构建 ArceOS 应用

```bash
# 最简单的方式
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# 自定义选项
cargo xtask arceos build \
    --package arceos-httpserver \
    --arch aarch64 \
    --platform axplat-aarch64-qemu-virt \
    --release \
    --features axfeat/net \
    --smp 2
```

### 构建 StarryOS

```bash
# 准备环境
cargo xtask starry rootfs --arch riscv64

# 构建
cargo xtask starry build --arch riscv64 --package starryos

# 运行
cargo xtask starry run --arch riscv64 --package starryos
```

### 构建 Axvisor

```bash
# 进入目录
cd os/axvisor

# 配置
cargo xtask defconfig qemu-aarch64

# 自定义配置（可选）
cargo xtask menuconfig

# 构建
cargo xtask build

# 运行
cargo xtask run
```

### 运行测试

```bash
# 单元测试
cargo test -p axerrno

# ArceOS 系统测试
cargo xtask test arceos --target riscv64gc-unknown-none-elf

# StarryOS 系统测试
cargo xtask test starry --target riscv64gc-unknown-none-elf

# 标准库测试
cargo xtask test std
```

### 清理构建

```bash
# 清理特定项目
cd os/arceos
make clean

# 清理所有
cargo clean

# 清理特定架构
rm -rf target/riscv64gc-unknown-none-elf
```

### 交叉编译

```bash
# 添加目标
rustup target add riscv64gc-unknown-none-elf

# 构建特定目标
cargo build --target riscv64gc-unknown-none-elf

# 使用 xtask
cargo xtask arceos build --package arceos-helloworld --arch riscv64
```

## 构建输出

### 输出目录结构

```
target/
├── riscv64gc-unknown-none-elf/
│   ├── debug/
│   │   └── arceos-helloworld
│   └── release/
│       └── arceos-helloworld
├── aarch64-unknown-none-softfloat/
│   └── release/
│       └── arceos-helloworld
└── x86_64-unknown-none/
    └── release/
        └── arceos-helloworld
```

### 镜像文件

```bash
# ELF 格式
target/riscv64gc-unknown-none-elf/release/arceos-helloworld

# 二进制格式
target/riscv64gc-unknown-none-elf/release/arceos-helloworld.bin

# 反汇编
rust-objdump -d target/riscv64gc-unknown-none-elf/release/arceos-helloworld
```

## 调试构建问题

### 查看详细输出

```bash
# Cargo 详细输出
cargo build -v

# Make 详细输出
make V=1

# 查看构建脚本输出
cargo build -vv
```

### 检查依赖

```bash
# 查看依赖树
cargo tree

# 检查过时的依赖
cargo outdated

# 查看特性依赖
cargo tree -f "{p} {f}"
```

### 常见问题

#### 1. 链接器错误

```bash
# 安装必要的工具
rustup component add rust-src
cargo install cargo-binutils

# 检查链接器
rust-lld --version
```

#### 2. 目标未安装

```bash
# 添加目标
rustup target add riscv64gc-unknown-none-elf

# 检查已安装目标
rustup target list --installed
```

#### 3. 特性冲突

```bash
# 查看启用的特性
cargo tree -f "{p} {f}"

# 明确指定特性
cargo build --no-default-features --features "feature1,feature2"
```

## CI/CD 集成

### GitHub Actions

```yaml
name: Build and Test

on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      
      - name: Install Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          targets: riscv64gc-unknown-none-elf
      
      - name: Build
        run: cargo xtask arceos build --package arceos-helloworld --arch riscv64
      
      - name: Test
        run: cargo xtask test arceos --target riscv64gc-unknown-none-elf
```

### 本地 CI 测试

```bash
# 运行所有测试
cargo xtask test std
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
```

## 性能优化

### 构建缓存

```bash
# 使用 sccache
cargo install sccache
export RUSTC_WRAPPER=sccache

# 构建时会自动缓存
cargo build
```

### 并行构建

```bash
# 设置并行度
export CARGO_BUILD_JOBS=8

# 或使用
cargo build -j 8
```

### 增量构建

```toml
# .cargo/config.toml
[build]
incremental = true
```

## 参考资源

- [Cargo 书籍](https://doc.rust-lang.org/cargo/)
- [Make 手册](https://www.gnu.org/software/make/manual/)
- [xtask 模式](https://github.com/matklad/cargo-xtask)

---

**相关文档**:
- [快速开始指南](quick-start.md)
- [组件开发指南](components.md)
- [仓库管理指南](repo.md)
