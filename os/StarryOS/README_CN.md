# StarryOS

[English](README.md) | 中文

StarryOS 是集成在 TGOSKits 根工作区 `os/StarryOS` 下的 Linux 兼容内核。Cargo 工作区、`cargo xtask` 入口和依赖锁定都由仓库根目录维护，因此本文的命令均应在 TGOSKits 检出目录的根执行。

## 1. 工作区定位

StarryOS 在 ArceOS 提供的运行时、硬件抽象和设备能力之上，实现进程、地址空间、信号、文件系统和 Linux 系统调用语义。它由下列根工作区包组成，`os/StarryOS` 目录本身没有独立的 Cargo 工作区。

### 1.1 软件包

根 `Cargo.toml` 将 `kernel`、`starryos`、`signal` 和 `vm` 登记为 workspace 成员；下表按包名列出源码目录与职责。包名由各自目录中的 `Cargo.toml` 定义，构建时由根工作区统一解析依赖和 feature。

| 包 | 目录 | 职责 |
| --- | --- | --- |
| `starry-kernel` | `kernel/` | Linux 兼容内核主体 |
| `starryos` | `starryos/` | 可启动的 StarryOS 目标 |
| `starry-signal` | `signal/` | 信号管理抽象 |
| `starry-vm` | `vm/` | 虚拟内存管理抽象 |

### 1.2 源码目录

`configs/board/` 保存 QEMU 与实体板卡配置，其中 QEMU 配置覆盖 `qemu-aarch64`、`qemu-loongarch64`、`qemu-riscv64` 和 `qemu-x86_64`。`lkm/` 保存可加载内核模块，`docs/` 保存子系统说明，`scripts/` 保存 StarryOS 专用辅助脚本。

StarryOS 应用和运行场景位于仓库根目录的 `apps/starry/`。ArceOS 提供的运行时、驱动、文件系统和网络 crate 仍位于各自的工作区目录，并通过 Cargo feature 参与 StarryOS 构建。

## 2. 快速开始

集成流程从 TGOSKits 根目录开始。项目容器已经包含 Rust 工具链、QEMU 和常用交叉编译依赖。

### 2.1 环境准备

克隆集成仓库并进入根目录：

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

使用项目容器时，把当前检出目录挂载到 `/workspace`，并把该目录作为工作目录：

```bash
docker pull ghcr.io/rcore-os/tgoskits-container:latest
docker run -it --rm \
  -v "$(pwd)":/workspace \
  -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest
```

手工配置环境时，按根目录 `README.md` 安装仓库级依赖；运行 QEMU 还需要所选架构对应的 `qemu-system-<arch>`。

### 2.2 准备并运行

目标不是默认配置时，先选择板级配置，再准备托管 rootfs 并启动 QEMU。以下示例使用 x86_64：

```bash
cargo xtask starry defconfig qemu-x86_64
cargo xtask starry rootfs --arch x86_64
cargo xtask starry qemu --arch x86_64
```

其他 QEMU 目标使用同一流程，例如 AArch64：

```bash
cargo xtask starry defconfig qemu-aarch64
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64
```

`cargo xtask starry qemu` 会在需要时重新构建。`--rootfs <IMAGE>` 可以选择托管镜像别名或显式路径，`--rootfs-write-policy persist` 可以让来宾磁盘写入跨运行保留。

### 2.3 验证入口

验证使用仓库根目录的任务工具，不使用原生全工作区 `cargo test` 或 `cargo clippy`：

```bash
cargo xtask clippy --package starry-kernel
cargo xtask test
cargo xtask starry test qemu --arch x86_64
```

`cargo xtask test` 运行标准库测试白名单中当前登记的包；改动已经提交后，可以用 `cargo xtask test --since <REF>` 选择受影响的包。QEMU 测试验证对应运行路径，包级测试和静态检查分别覆盖各自的既定边界。

## 3. 文档

快速开始、构建和运行参考位于文档站点。OpenRC 启动等子系统说明保存在 `docs/`。

- [StarryOS 快速开始](../../docs/docs/quickstart/starryos.md)
- [StarryOS 构建概览](../../docs/docs/build/starry/overview.md)
- [OpenRC 启动说明](./docs/openrc.md)
- [上游项目](https://github.com/Starry-OS/StarryOS)

## 4. 许可证

StarryOS 采用 Apache License 2.0。详情见 [LICENSE](./LICENSE) 和 [NOTICE](./NOTICE)。
