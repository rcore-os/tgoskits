# TGOSKits 快速上手指南

本指南将帮助您在 5 分钟内快速上手 TGOSKits，开始操作系统开发之旅。

## 📋 目录

- [环境配置](#环境配置)
- [快速体验](#快速体验)
- [开发流程](#开发流程)
- [常见问题](#常见问题)

## 🔧 环境配置

### 系统要求

- **操作系统**: Ubuntu 22.04+ / Debian 11+ / Fedora 36+ 或其他 Linux 发行版
- **内存**: 建议 8GB 以上
- **磁盘**: 至少 10GB 可用空间

### 1. 安装基础工具

```bash
# Debian/Ubuntu
sudo apt update
sudo apt install -y build-essential cmake clang curl git \
    qemu-system-x86 qemu-system-arm qemu-system-riscv64 \
    libssl-dev pkg-config

# Fedora
sudo dnf install -y @development-tools cmake clang curl git \
    qemu-system-x86 qemu-system-arm qemu-system-riscv64 \
    openssl-devel pkgconf-pkg-config
```

### 2. 安装 Rust 工具链

```bash
# 安装 rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 配置当前 shell
source $HOME/.cargo/env

# 安装必要的工具
rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat

# 安装 cargo 工具
cargo install cargo-binutils
cargo install ostool --version ^0.8
```

### 3. 安装 Musl 工具链（可选，用于构建用户态应用）

```bash
# 下载预编译的 Musl 工具链
# RISC-V 64位
wget https://github.com/arceos-org/setup-musl/releases/download/prebuilt/riscv64-linux-musl-cross.tgz
tar xzf riscv64-linux-musl-cross.tgz

# LoongArch64
wget https://github.com/arceos-org/setup-musl/releases/download/prebuilt/loongarch64-linux-musl-cross.tgz
tar xzf loongarch64-linux-musl-cross.tgz

# 添加到 PATH
export PATH=$PWD/riscv64-linux-musl-cross/bin:$PATH
export PATH=$PWD/loongarch64-linux-musl-cross/bin:$PATH
```

### 4. 克隆仓库

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

## 🚀 快速体验

### ArceOS - Hello World

最简单的入门方式是运行 ArceOS 的 Hello World 示例：

```bash
# RISC-V 64位架构
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# x86_64架构
cargo xtask arceos run --package arceos-helloworld --arch x86_64

# ARM64架构
cargo xtask arceos run --package arceos-helloworld --arch aarch64

# LoongArch64架构
cargo xtask arceos run --package arceos-helloworld --arch loongarch64
```

### ArceOS - 其他示例

```bash
# HTTP 服务器
cargo xtask arceos run --package arceos-httpserver --arch riscv64

# HTTP 客户端
cargo xtask arceos run --package arceos-httpclient --arch riscv64

# Shell
cargo xtask arceos run --package arceos-shell --arch riscv64
```

### StarryOS

```bash
# 1. 准备 rootfs（首次运行需要）
cargo xtask starry rootfs --arch riscv64

# 2. 运行 StarryOS
cargo xtask starry run --arch riscv64 --package starryos

# 其他架构
cargo xtask starry run --arch loongarch64 --package starryos
```

### Axvisor

Axvisor 需要先准备 Guest 系统镜像，详细步骤请参考 [Axvisor 开发指南](axvisor-guide.md)。

```bash
# 进入 Axvisor 目录
cd os/axvisor

# 选择配置
cargo xtask defconfig qemu-aarch64

# 构建
cargo xtask build

# 运行
cargo xtask run
```

## 👨‍💻 开发流程

### IDE 配置

推荐使用 VSCode，并安装以下插件：

1. **rust-analyzer** - Rust 语言服务器
2. **Rust Targets** - 多目标架构支持
3. **CodeLLDB** - 调试支持
4. **Better TOML** - TOML 文件支持

### 基本开发流程

```bash
# 1. 创建功能分支
git checkout -b my-feature

# 2. 修改代码（例如修改组件）
vim components/axerrno/src/lib.rs

# 3. 本地测试
cargo xtask test arceos --target riscv64gc-unknown-none-elf

# 4. 提交更改
git add .
git commit -m "feat(axerrno): improve error handling"

# 5. 推送分支
git push origin my-feature

# 6. 创建 Pull Request
```

### 添加新的 ArceOS 应用

```bash
# 1. 创建应用目录
mkdir -p os/arceos/examples/myapp

# 2. 创建 Cargo.toml
cat > os/arceos/examples/myapp/Cargo.toml << 'EOF'
[package]
name = "myapp"
version = "0.1.0"
edition = "2021"

[dependencies]
axstd.workspace = true

[package.metadata.build-target]
default = "riscv64gc-unknown-none-elf"
EOF

# 3. 创建源代码
cat > os/arceos/examples/myapp/src/main.rs << 'EOF'
#![no_std]
#![no_main]

#[macro_use]
extern crate axstd;

#[no_mangle]
fn main() {
    println!("Hello from my app!");
}
EOF

# 4. 运行应用
cargo xtask arceos run --package myapp --arch riscv64
```

## 🧪 测试

### 运行测试

```bash
# 测试 ArceOS
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test arceos --target aarch64-unknown-none-softfloat

# 测试 StarryOS
cargo xtask test starry --target riscv64gc-unknown-none-elf

# 测试 Axvisor
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

### 运行单元测试

```bash
# 测试特定组件
cd components/axerrno
cargo test

# 测试所有组件
cargo test --workspace
```

## 📖 进阶学习

- [ArceOS 开发指南](arceos-guide.md) - 深入学习 ArceOS
- [StarryOS 开发指南](starryos-guide.md) - 学习教学操作系统
- [Axvisor 开发指南](axvisor-guide.md) - 虚拟化技术
- [组件开发指南](components.md) - 开发可复用组件
- [构建系统说明](build-system.md) - 理解构建系统

## ❓ 常见问题

### Q: 编译时出现 "linker 'rust-lld' not found"

**A:** 安装 Rust 的 linker 工具：
```bash
rustup component add rust-src
cargo install cargo-binutils
```

### Q: QEMU 运行时提示 "Could not open '.../target/.../os-image'"

**A:** 确保已经构建项目：
```bash
cargo xtask arceos build --package arceos-helloworld --arch riscv64
```

### Q: StarryOS 运行时找不到 rootfs

**A:** 需要先准备 rootfs：
```bash
cargo xtask starry rootfs --arch riscv64
```

### Q: 如何查看详细的构建日志？

**A:** 使用 `-v` 参数或设置环境变量：
```bash
# 方法1：使用 verbose 参数
cargo xtask arceos build --package arceos-helloworld --arch riscv64 -v

# 方法2：设置环境变量
export RUST_LOG=debug
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

### Q: 如何调试内核？

**A:** 使用 QEMU 的调试功能：
```bash
# 启动 QEMU 并等待 GDB 连接
cargo xtask arceos run --package arceos-helloworld --arch riscv64 -- -s -S

# 在另一个终端连接 GDB
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/release/arceos-helloworld
(gdb) target remote :1234
(gdb) break rust_main
(gdb) continue
```

### Q: 组件修改后如何同步到独立仓库？

**A:** CI 会自动同步。如需手动同步：
```bash
python3 scripts/repo/repo.py push <component-name>
```

## 🔗 有用的链接

- [Rust Embedded Book](https://doc.rust-lang.org/stable/embedded-book/)
- [OSDev Wiki](https://wiki.osdev.org/)
- [Rust OSDev 社区](https://rust-osdev.com/)
- [ArceOS 官方文档](https://arceos-org.github.io/arceos/)

---

**遇到问题？** 欢迎在 [GitHub Issues](https://github.com/rcore-os/tgoskits/issues) 提问！
