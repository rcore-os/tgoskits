# TGOSKits 第一天上手

这篇文档的目标不是把所有细节讲完，而是让你在第一次进入仓库时，先把 ArceOS、StarryOS 和 Axvisor 的入口跑通，再知道后续应该读哪篇文档。

## 1. 先记住命令入口

| 位置 | 命令 | 用途 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 统一入口，负责 ArceOS、StarryOS 和测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 本地 xtask 的别名 |
| `os/axvisor/` | `cargo xtask ...` | Axvisor 自己的构建与运行入口 |

如果你只记一条规则，请记这一条：

- ArceOS 和 StarryOS 主要从仓库根目录启动。
- Axvisor 的 build/qemu 要么在根目录执行 `cargo axvisor ...`，要么进入 `os/axvisor/` 执行 `cargo xtask ...`。

## 2. 准备环境

### 基础工具

下面是 Ubuntu / Debian 的最小安装示例：

```bash
sudo apt update
sudo apt install -y \
    build-essential cmake clang curl file git libssl-dev libudev-dev \
    pkg-config python3 qemu-system-arm qemu-system-riscv64 qemu-system-x86 \
    xz-utils
```

建议预留至少 10GB 磁盘空间。首次下载 rootfs 或 Guest 镜像时会额外占用一些空间。

### Rust 工具链

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat

cargo install cargo-binutils
cargo install ostool
```

### 可选：Musl 交叉工具链

如果你要为 StarryOS rootfs 或某些用户态程序编译静态二进制，需要额外准备 Musl 交叉工具链。首次只是跑通 ArceOS 示例时并不需要。

### WSL2 提示

如果你在 WSL2 下开发：

- 可以正常使用 QEMU 进行纯软件仿真。
- 通常不要指望 KVM 或宿主机硬件虚拟化加速可用。
- 遇到性能问题时，优先减少并行任务和避免依赖硬件加速选项。

## 3. 克隆仓库

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

## 4. 第一条成功路径：ArceOS

先跑最小示例，确认工具链和 QEMU 是通的：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

你还可以继续试两个更能体现功能差异的例子：

```bash
# 网络示例
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net

# 文件系统示例
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

首次上手建议固定使用 `riscv64`。等你熟悉后，再切换到 `x86_64`、`aarch64` 或 `loongarch64`。

## 5. 第二条成功路径：StarryOS

StarryOS 第一次运行前必须先准备 rootfs：

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

这一步会把 rootfs 镜像准备到 StarryOS 的目标产物目录中，通常会生成对应目标下的 `disk.img`。如果你改走 `os/StarryOS/Makefile` 路径，才会使用 `os/StarryOS/make/disk.img`。

如果你已经熟悉了基本流程，也可以尝试：

```bash
cargo xtask starry run --arch loongarch64 --package starryos
```

## 6. 第三条成功路径：Axvisor

Axvisor 和前两个系统最大的区别是：它不是单独跑一个内核，而是要先准备 Guest 镜像，并让板级配置引用对应的 VM 配置。

推荐先使用 QEMU AArch64 路径，因为当前仓库里现成的板级配置和 CI 入口都围绕它。

### 6.1 推荐方式：使用官方脚本一次性准备 Guest、VM 配置和 rootfs

最稳妥的方式不是手工拼 `defconfig/build/qemu`，而是直接使用 Axvisor 仓库自带的 `setup_qemu.sh`：

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
```

这个脚本会自动完成三件事：

- 下载并解压 Guest 镜像到 `/tmp/.axvisor-images/`
- 生成 `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
- 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

### 6.2 运行 QEMU

```bash
cd os/axvisor
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果启动成功，ArceOS Guest 会输出 `Hello, world!`。

### 6.3 为什么我前面的 `cargo axvisor defconfig/build/qemu` 会失败

因为 Axvisor 默认使用的 QEMU 配置模板里会引用：

```text
os/axvisor/tmp/rootfs.img
```

这个文件不会通过 `cargo axvisor defconfig` 或 `cargo axvisor build` 自动生成。只有你手工准备，或者运行 `./scripts/setup_qemu.sh arceos` 之后，它才会存在。

### 6.4 Axvisor 的统一测试命令

这条命令和上面的手工 QEMU 运行不是一回事：

```bash
cd /home/chyyuu/thecodes/tgoskits
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

根工作区测试入口会走自己的测试逻辑，并自动确保测试所需镜像被下载。它主要对应 CI，不要求你手工准备 `os/axvisor/tmp/rootfs.img`。

## 7. 第一天的开发闭环

第一次修改代码时，不要一上来跑全量测试。先选离你改动最近的消费者：

| 改动位置 | 先做什么 | 再做什么 |
| --- | --- | --- |
| `components/axerrno`、`components/kspin`、`components/percpu` 这类基础 crate | `cargo test -p <crate>` | 再跑一个最小 ArceOS 或 StarryOS 路径 |
| `os/arceos/modules/*` 或 `os/arceos/api/*` | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` | 再补 `cargo xtask test arceos --target riscv64gc-unknown-none-elf` |
| `components/starry-*` 或 `os/StarryOS/kernel/*` | `cargo xtask starry rootfs --arch riscv64` | 再跑 `cargo xtask starry run --arch riscv64 --package starryos` |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`os/axvisor/src/*` | `cd os/axvisor && cargo xtask build` | 需要 Guest 时先运行 `./scripts/setup_qemu.sh arceos`，再执行 `cargo xtask qemu --build-config ... --qemu-config ... --vmconfigs ...` |

当你准备提交前，再考虑统一测试：

```bash
cargo xtask test std
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 8. 接下来该读什么

| 你已经跑通了什么 | 下一篇建议文档 |
| --- | --- |
| 只想继续做 ArceOS 示例、模块或平台 | [arceos-guide.md](arceos-guide.md) |
| 想系统理解 ArceOS 的分层、feature 装配和启动路径 | [arceos-internals.md](arceos-internals.md) |
| 想改 StarryOS 内核、rootfs 或 syscall | [starryos-guide.md](starryos-guide.md) |
| 想系统理解 StarryOS 的 syscall、进程和 rootfs 装载链路 | [starryos-internals.md](starryos-internals.md) |
| 想搞清楚 Axvisor 的板级配置、VM 配置和虚拟化组件 | [axvisor-guide.md](axvisor-guide.md) |
| 想系统理解 Axvisor 的 VMM、vCPU 与配置生效路径 | [axvisor-internals.md](axvisor-internals.md) |
| 想从“组件”视角理解三个系统的关系 | [components.md](components.md) |
| 想理解工作区、xtask、Makefile 和测试矩阵 | [build-system.md](build-system.md) |

## 9. 常见问题

### `rust-lld` 或目标工具链缺失

先确认 Rust 目标已经安装：

```bash
rustup target list --installed
```

如果缺少对应目标，重新执行：

```bash
rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

### StarryOS 提示找不到 rootfs

这是最常见的问题。先执行：

```bash
cargo xtask starry rootfs --arch riscv64
```

并确认对应目标产物目录下的 `disk.img` 已生成。只有在本地 Makefile 路径下，才检查 `os/StarryOS/make/disk.img`。

### Axvisor 启动不了 Guest

优先检查两件事：

- `os/axvisor/tmp/rootfs.img` 是否已经由 `./scripts/setup_qemu.sh arceos` 准备好。
- `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 是否已经生成，且其中 `kernel_path` 指向真实存在的镜像文件。

### 在 WSL2 下速度很慢

这通常不是仓库配置问题，而是纯软件仿真导致的。先确保你没有依赖硬件加速，再尽量从最小示例开始，不要第一次就跑最重的系统路径。
