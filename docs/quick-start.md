# TGOSKits 快速上手指南

本文档帮助你第一次进入 TGOSKits 工作区时，快速跑通 ArceOS、StarryOS 和 Axvisor 的主路径，并避免被已经过时的命令说明带偏。

## 1. 命令入口概览

TGOSKits 当前统一使用仓库根目录入口：

| 位置 | 命令 | 用途 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 统一入口，负责 ArceOS、StarryOS、Axvisor 和测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | `cargo xtask axvisor ...` 的别名 |
| `os/arceos/` | `make ...` | ArceOS 本地入口，适合调 Makefile/feature/QEMU 细节 |
| `os/StarryOS/` | `make ...` | StarryOS 本地入口，适合调 rootfs 和本地启动流程 |
| `os/axvisor/scripts/*.sh` | Shell 辅助脚本 | 准备 Axvisor Guest 镜像、rootfs 和 VM 配置 |

记住一条规则就够了：三组命令都优先从仓库根目录启动。`os/axvisor/xtask` 在这个 workspace 里当前只是占位实现，不应再把它当成 Axvisor 的主入口。

## 2. 环境准备

建议预留至少 10GB 磁盘空间，因为 StarryOS rootfs 和 Axvisor Guest 镜像会额外占用空间。

### 2.1 基础工具

```bash
sudo apt update
sudo apt install -y \
    build-essential cmake clang curl file git libssl-dev libudev-dev \
    pkg-config python3 qemu-system-arm qemu-system-riscv64 qemu-system-x86 \
    xz-utils
```

### 2.2 Rust 工具链

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

常用辅助工具：

```bash
cargo install cargo-binutils
cargo install ostool
```

### 2.3 可选：Musl 交叉工具链

如果你要为 StarryOS rootfs 里的用户态程序构建静态二进制文件，通常还需要 Musl 交叉工具链。首次跑通 ArceOS 不需要它。

### 2.4 WSL2 提示

WSL2 下可以跑纯软件 QEMU，但通常不要指望 KVM 或宿主机硬件虚拟化加速可用。

## 3. 克隆仓库

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

## 4. ArceOS 快速上手

ArceOS 当前根 CLI 的真实子命令是 `build`、`qemu`、`uboot`。常用参数只有 `--package`、`--target`、`--config` 和 `--plat_dyn`。

### 4.1 最小示例

```bash
cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf
```

### 4.2 只构建 / U-Boot 路径

```bash
cargo arceos build --package ax-helloworld --target riscv64gc-unknown-none-elf
cargo arceos uboot --package ax-helloworld --target aarch64-unknown-none-softfloat
```

### 4.3 关于高级功能开关

根 CLI 不再直接暴露 `--net`、`--blk`、`--features`、`--platform`、`--smp` 这类旧参数。  
如果你要调网络、块设备、平台或 SMP，请改应用目录下的 `.build-<target>.toml` / `build-<target>.toml`，或者使用 `os/arceos/Makefile` 本地入口。

## 5. StarryOS 快速上手

StarryOS 当前根 CLI 的真实子命令是 `build`、`qemu`、`rootfs`、`uboot`。包名固定为 `starryos`，CLI 不需要也不接受 `--package`。

### 5.1 预热 rootfs

`rootfs` 会把镜像准备到工作区 target 目录下，对应文件名为 `rootfs-<arch>.img`：

```bash
cargo xtask starry rootfs --arch riscv64
```

这一步适合首次预热或单独检查镜像，但不是每次运行前都必须手工执行，因为 `qemu` 会在需要时自动补齐 rootfs。

### 5.2 运行 StarryOS

```bash
cargo starry qemu --arch riscv64
```

### 5.3 其他架构

```bash
cargo starry qemu --arch loongarch64
```

如果你走 `os/StarryOS/Makefile` 路径，使用的则是 `os/StarryOS/make/disk.img`。

## 6. Axvisor 快速上手

Axvisor 当前根 CLI 的真实子命令是 `build`、`qemu`、`uboot`、`defconfig`、`config`、`image`。推荐先走 QEMU AArch64 路径。

### 6.1 先生成板级配置并准备 Guest 资源

最稳妥的流程不是手工拼参数，而是先生成板级配置，再调用官方脚本准备镜像、VM 配置和 rootfs：

```bash
cargo axvisor defconfig qemu-aarch64
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
```

`setup_qemu.sh` 会自动完成三件事：

1. 下载并解压 Guest 镜像到 `/tmp/.axvisor-images/`
2. 生成 `os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
3. 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

### 6.2 启动 Axvisor

```bash
cargo axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果启动成功，ArceOS Guest 会输出 `Hello, world!`。

### 6.3 常见失败原因

如果 `cargo axvisor qemu` 失败，优先检查：

1. `os/axvisor/.build.toml` 是否已经由 `cargo axvisor defconfig qemu-aarch64` 生成
2. `os/axvisor/tmp/rootfs.img` 是否已经由 `setup_qemu.sh` 准备好
3. `os/axvisor/tmp/vmconfigs/*.generated.toml` 是否存在，且里面的 `kernel_path` 指向真实镜像

### 6.4 统一测试命令

```bash
cargo axvisor test qemu --target aarch64
```

这条命令属于根工作区测试矩阵，会走自己的测试逻辑。

## 7. 开发闭环建议

不要一上来跑全量测试。先选离你改动最近的消费者做验证。

| 改动位置 | 先做什么 | 再做什么 |
| --- | --- | --- |
| `components/axerrno`、`components/kspin`、`components/percpu` 这类基础 crate | `cargo test -p <crate>` | 再跑一个最小 ArceOS 或 StarryOS 路径 |
| `os/arceos/modules/*` 或 `os/arceos/api/*` | `cargo arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf` | 再补 `cargo arceos test qemu --target riscv64gc-unknown-none-elf` |
| `components/starry-*` 或 `os/StarryOS/kernel/*` | `cargo starry qemu --arch riscv64` | 再补 `cargo starry test qemu --target riscv64` |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`os/axvisor/src/*` | `cargo axvisor build --config os/axvisor/.build.toml` | 需要 Guest 时先运行 `(cd os/axvisor && ./scripts/setup_qemu.sh arceos)`，再执行 `cargo axvisor qemu --config ... --qemu-config ... --vmconfigs ...` |

### 7.1 提交前的统一测试

```bash
cargo xtask test
cargo arceos test qemu --target riscv64gc-unknown-none-elf
cargo starry test qemu --target riscv64
cargo axvisor test qemu --target aarch64
```

## 8. 后续学习路径

| 你想继续看什么 | 下一篇建议文档 |
| --- | --- |
| 继续做 ArceOS 示例、模块或平台 | [arceos-guide.md](arceos-guide.md) |
| 理解 ArceOS 的分层、feature 装配和启动路径 | [arceos-internals.md](arceos-internals.md) |
| 修改 StarryOS 内核、rootfs 或 syscall | [starryos-guide.md](starryos-guide.md) |
| 理解 StarryOS 的 syscall、进程和 rootfs 装载链路 | [starryos-internals.md](starryos-internals.md) |
| 搞清楚 Axvisor 的板级配置、VM 配置和虚拟化组件 | [axvisor-guide.md](axvisor-guide.md) |
| 理解 Axvisor 的 VMM、vCPU 与配置生效路径 | [axvisor-internals.md](axvisor-internals.md) |
| 从组件视角理解三个系统的关系 | [components.md](components.md) |
| 理解工作区、xtask、Makefile 和测试矩阵 | [build-system.md](build-system.md) |

## 9. 常见问题

### 9.1 `rust-lld` 或目标工具链缺失

```bash
rustup target list --installed
```

如果缺少目标，重新执行：

```bash
rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

### 9.2 StarryOS 提示找不到 rootfs

先执行：

```bash
cargo xtask starry rootfs --arch riscv64
```

然后确认目标产物目录下的 `rootfs-<arch>.img` 是否已生成。只有本地 Makefile 路径才检查 `os/StarryOS/make/disk.img`。

### 9.3 Axvisor 启动不了 Guest

优先检查：

1. `os/axvisor/tmp/rootfs.img` 是否已经由 `(cd os/axvisor && ./scripts/setup_qemu.sh arceos)` 准备好
2. `os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 是否已经生成

### 9.4 在 WSL2 下速度很慢

WSL2 下运行缓慢通常不是仓库配置问题，而是纯软件仿真导致的。先确保你没有依赖硬件加速，再尽量从最小示例开始。
