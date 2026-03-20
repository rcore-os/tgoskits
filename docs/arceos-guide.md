# ArceOS 开发指南

ArceOS 是一个用 Rust 编写的模块化操作系统框架（或 Unikernel），灵感来源于 Unikraft。

## 📋 目录

- [简介](#简介)
- [架构设计](#架构设计)
- [快速开始](#快速开始)
- [模块系统](#模块系统)
- [开发应用](#开发应用)
- [平台支持](#平台支持)
- [调试技巧](#调试技巧)
- [进阶主题](#进阶主题)

## 简介

### 特性

- ✅ **多架构支持**: x86_64, RISC-V 64, AArch64, LoongArch64
- ✅ **模块化设计**: 组件可插拔，按需组合
- ✅ **多线程**: 支持多线程和多核调度
- ✅ **调度器**: FIFO/RR/CFS 调度算法
- ✅ **设备驱动**: VirtIO net/blk/gpu 驱动
- ✅ **网络栈**: 基于 smoltcp 的 TCP/UDP 协议栈
- ✅ **文件系统**: 多种文件系统支持
- ✅ **SMP**: 对称多处理支持

### 适用场景

- **Unikernel 应用**: 高性能、低延迟的专用应用
- **嵌入式系统**: 资源受限的嵌入式环境
- **教学研究**: 操作系统教学和研究
- **原型开发**: 快速验证操作系统概念

## 架构设计

### 分层架构

```
┌─────────────────────────────────────┐
│         Application Layer           │  用户应用
├─────────────────────────────────────┤
│        API Layer (axstd)            │  标准库接口
├─────────────────────────────────────┤
│      Module Layer (axhal, etc)      │  功能模块
├─────────────────────────────────────┤
│    Platform Layer (axplat crates)   │  平台抽象
└─────────────────────────────────────┘
```

### 核心模块

| 模块 | 路径 | 说明 |
|------|------|------|
| **axconfig** | `os/arceos/modules/axconfig` | 配置管理 |
| **axhal** | `os/arceos/modules/axhal` | 硬件抽象层 |
| **axalloc** | `os/arceos/modules/axalloc` | 内存分配 |
| **axtask** | `os/arceos/modules/axtask` | 任务管理 |
| **axsched** | `os/arceos/modules/axsched` | 调度器 |
| **axdriver** | `os/arceos/modules/axdriver` | 设备驱动 |
| **axnet** | `os/arceos/modules/axnet` | 网络协议栈 |
| **axfs** | `os/arceos/modules/axfs` | 文件系统 |
| **axlog** | `os/arceos/modules/axlog` | 日志系统 |
| **axruntime** | `os/arceos/modules/axruntime` | 运行时 |

## 快速开始

### 在 TGOSKits 中构建

```bash
# 1. 进入 TGOSKits 根目录
cd /path/to/tgoskits

# 2. 构建并运行示例
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# 3. 只构建不运行
cargo xtask arceos build --package arceos-helloworld --arch riscv64

# 4. 指定平台
cargo xtask arceos run --package arceos-helloworld --arch aarch64 --platform axplat-aarch64-qemu-virt
```

### 使用 Makefile（在 ArceOS 目录）

```bash
# 1. 进入 ArceOS 目录
cd os/arceos

# 2. 构建并运行
make A=examples/helloworld ARCH=riscv64 run

# 3. 指定日志级别
make A=examples/helloworld ARCH=riscv64 LOG=debug run

# 4. 启用网络
make A=examples/httpserver ARCH=riscv64 NET=y run

# 5. 启用块设备
make A=examples/shell ARCH=riscv64 BLK=y run
```

### 构建选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `ARCH` | 目标架构 | `riscv64`, `x86_64`, `aarch64`, `loongarch64` |
| `LOG` | 日志级别 | `error`, `warn`, `info`, `debug`, `trace` |
| `MODE` | 构建模式 | `release` (默认), `debug` |
| `SMP` | CPU 数量 | `1`, `2`, `4` |
| `NET` | 启用网络 | `y`, `n` |
| `BLK` | 启用块设备 | `y`, `n` |
| `GRAPHIC` | 启用图形 | `y`, `n` |
| `MEM` | 内存大小 | `128M`, `256M`, `1G` |

## 模块系统

### 模块依赖关系

```
axruntime (运行时入口)
    ├── axlog (日志)
    ├── axalloc (内存分配)
    ├── axtask (任务管理)
    │   └── axsched (调度)
    ├── axdriver (驱动)
    │   ├── axdriver-net
    │   ├── axdriver-block
    │   └── axdriver-display
    ├── axnet (网络)
    │   └── axdriver-net
    ├── axfs (文件系统)
    │   └── axdriver-block
    └── axhal (硬件抽象)
        └── axconfig (配置)
```

### 启用/禁用模块

通过 features 控制模块的启用：

```bash
# 启用网络
cargo xtask arceos run --package arceos-httpserver --arch riscv64 \
    --features axfeat/net

# 启用文件系统
cargo xtask arceos run --package arceos-shell --arch riscv64 \
    --features axfeat/fs

# 启用 SMP
cargo xtask arceos run --package arceos-helloworld --arch riscv64 \
    --features axfeat/smp --smp 4
```

### 常用 features 组合

```toml
[features]
default = ["axfeat/defplat"]

# 网络应用
net-app = ["axfeat/net", "axfeat/bus-pci"]

# 文件系统应用
fs-app = ["axfeat/fs", "axfeat/blk"]

# 图形应用
graphic-app = ["axfeat/display", "axfeat/graphic"]

# 多核应用
smp-app = ["axfeat/smp"]
```

## 开发应用

### 最小应用示例

```rust
// src/main.rs
#![no_std]
#![no_main]

use axstd::println;

#[no_mangle]
fn main() {
    println!("Hello, ArceOS!");
}
```

### 应用 Cargo.toml

```toml
[package]
name = "myapp"
version = "0.1.0"
edition = "2021"

[dependencies]
axstd.workspace = true

# 可选依赖
axfeat = { workspace = true, optional = true }

[features]
default = []

# 指定构建目标
[package.metadata.build-target]
default = "riscv64gc-unknown-none-elf"
```

### 使用多线程

```rust
#![no_std]
#![no_main]

use axstd::{println, thread, time::Duration};

#[no_mangle]
fn main() {
    println!("Multi-threading example");
    
    let handle = thread::spawn(|| {
        for i in 0..5 {
            println!("Child thread: {}", i);
            thread::sleep(Duration::from_millis(100));
        }
    });
    
    for i in 0..5 {
        println!("Main thread: {}", i);
        thread::sleep(Duration::from_millis(100));
    }
    
    handle.join().unwrap();
}
```

### 使用网络

```rust
#![no_std]
#![no_main]

use axstd::{println, net::TcpListener};

#[no_mangle]
fn main() {
    println!("TCP Server example");
    
    let listener = TcpListener::bind("0.0.0.0:5555").unwrap();
    println!("Listening on 0.0.0.0:5555");
    
    loop {
        let (stream, addr) = listener.accept().unwrap();
        println!("Accepted connection from {:?}", addr);
        
        // 处理连接...
    }
}
```

### 使用文件系统

```rust
#![no_std]
#![no_main]

use axstd::{println, fs::File, io::{Read, Write}};

#[no_mangle]
fn main() {
    println!("File system example");
    
    // 写文件
    let mut file = File::create("/tmp/test.txt").unwrap();
    file.write_all(b"Hello, File!").unwrap();
    
    // 读文件
    let mut file = File::open("/tmp/test.txt").unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    println!("Content: {}", content);
}
```

## 平台支持

### 支持的平台

| 平台包 | 架构 | 目标硬件 | 说明 |
|--------|------|----------|------|
| `axplat-riscv64-qemu-virt` | RISC-V 64 | QEMU virt | 默认 RISC-V 平台 |
| `axplat-aarch64-qemu-virt` | AArch64 | QEMU virt | 默认 ARM64 平台 |
| `axplat-x86-pc` | x86_64 | QEMU pc-q35 | 默认 x86_64 平台 |
| `axplat-loongarch64-qemu-virt` | LoongArch64 | QEMU virt | LoongArch64 平台 |
| `axplat-aarch64-raspi` | AArch64 | Raspberry Pi 4 | 树莓派 4 |
| `axplat-aarch64-phytium-pi` | AArch64 | 飞腾派 | 飞腾开发板 |

### 自定义平台

1. **创建平台 crate**

```rust
// my-platform/src/lib.rs
use axplat::Platform;

pub struct MyPlatform;

impl Platform for MyPlatform {
    fn name() -> &'static str {
        "my-platform"
    }
    
    // 实现必要的 trait 方法...
}

axplat::register_platform!(MyPlatform);
```

2. **在应用中使用**

```bash
cargo xtask arceos run --package myapp --arch riscv64 \
    --platform my-platform
```

## 调试技巧

### 使用 GDB 调试

```bash
# 1. 启动 QEMU 并等待 GDB 连接
cargo xtask arceos run --package arceos-helloworld --arch riscv64 -- -s -S

# 2. 在另一个终端连接 GDB
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/release/arceos-helloworld

# GDB 命令
(gdb) target remote :1234
(gdb) break rust_main
(gdb) continue
(gdb) backtrace
(gdb) info registers
```

### 启用详细日志

```bash
# 方法1：通过命令行
cargo xtask arceos run --package arceos-helloworld --arch riscv64 --features axfeat/log-debug

# 方法2：通过 Makefile
make A=examples/helloworld ARCH=riscv64 LOG=trace run
```

### QEMU 监控命令

```bash
# 在 QEMU 运行时按 Ctrl+A, C 进入监控模式
(qemu) info registers    # 查看寄存器
(qemu) info mtree        # 查看内存布局
(qemu) x/10i $pc         # 反汇编当前位置
(qemu) quit              # 退出 QEMU
```

## 进阶主题

### 添加新的系统调用

1. **定义系统调用号** (在 `axhal/src/syscall.rs`)

```rust
pub const SYS_MY_SYSCALL: usize = 100;
```

2. **实现系统调用处理**

```rust
fn sys_mysyscall(arg1: usize, arg2: usize) -> isize {
    // 实现逻辑
    0
}
```

3. **注册到系统调用表**

```rust
pub fn init() {
    register_syscall(SYS_MY_SYSCALL, sys_mysyscall);
}
```

### 添加新的驱动

1. **实现驱动 trait**

```rust
use axdriver_base::DeviceDriver;

pub struct MyDriver {
    // 驱动状态
}

impl DeviceDriver for MyDriver {
    fn name(&self) -> &str {
        "my-driver"
    }
    
    fn init(&mut self) -> Result<()> {
        // 初始化硬件
        Ok(())
    }
}
```

2. **注册驱动**

```rust
use axdriver::register_driver;

register_driver!(MyDriver::new());
```

### 性能优化

1. **使用 release 模式**

```bash
cargo xtask arceos build --package myapp --arch riscv64 --release
```

2. **启用 LTO**

```toml
# Cargo.toml
[profile.release]
lto = true
codegen-units = 1
opt-level = "z"  # 优化大小
```

3. **禁用日志**

```bash
make A=examples/helloworld ARCH=riscv64 LOG=off run
```

## 示例应用

### 内置示例

- **helloworld**: 基础示例
- **httpserver**: HTTP 服务器
- **httpclient**: HTTP 客户端
- **shell**: 交互式 Shell
- **helloworld-myplat**: 自定义平台示例

### 运行示例

```bash
# HTTP 服务器（需要网络支持）
cargo xtask arceos run --package arceos-httpserver --arch riscv64

# HTTP 客户端
cargo xtask arceos run --package arceos-httpclient --arch riscv64

# Shell（需要块设备支持）
cargo xtask arceos run --package arceos-shell --arch riscv64
```

## 参考资源

- [ArceOS 官方仓库](https://github.com/arceos-org/arceos)
- [ArceOS 文档](https://arceos-org.github.io/arceos/)
- [Unikraft 论文](https://dl.acm.org/doi/10.1145/3358800)
- [Rust OSDev 社区](https://rust-osdev.com/)

---

**下一步**: 学习 [组件开发指南](components.md) 了解如何开发可复用组件
