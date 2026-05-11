# StarryOS 开发指南

StarryOS 建立在 ArceOS 模块层之上的 Linux 兼容系统。本文聚焦**改了什么之后该如何验证**的开发闭环、rootfs 管理要点和调试技巧。

> 架构分层、syscall 分发和进程模型见 [StarryOS 架构](/docs/architecture/starryos)。  
> 最短命令和快速启动见 [StarryOS 快速上手](/docs/quickstart/starryos)。  
> 构建系统总览见 [build-system.md](/docs/build/overview)。

## 常见开发动作

### 修改共享基础能力

如果你改的是：

- `components/axerrno`、`components/kspin` 这类基础 crate
- 或 `os/arceos/modules/axhal`、`axtask`、`axdriver`、`axnet`

建议先确认 ArceOS 最小路径仍然工作，再回到 StarryOS：

```bash
cargo xtask arceos qemu --package ax-helloworld --arch aarch64
cargo xtask starry qemu --arch riscv64
```

### 修改 Starry 专用组件或内核逻辑

如果你改的是：

- `components/starry-process`、`starry-signal`、`starry-vm`
- `os/StarryOS/kernel/*`

直接从 StarryOS 路径验证：

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64
```

### 增加 syscall 或用户可见行为

闭环步骤：

1. 在内核里完成实现
2. 准备最小用户态程序触发它
3. 把程序放入 rootfs
4. 启动 StarryOS 验证行为

### 修改启动包和 feature 组合

- `os/StarryOS/starryos/Cargo.toml` 定义包级 feature：`qemu`、`smp`、`rknpu`
- `os/StarryOS/kernel/Cargo.toml`（`starry-kernel`）定义内核 feature：`memtrack`、`input`、`vsock`、`rknpu`

如果改动更像"启动形态"而非"内核算法"，先看 `starryos/Cargo.toml`。

## rootfs 相关要点

### xtask 路径和 Makefile 路径不共享默认镜像位置

- 根目录 `cargo xtask starry rootfs` / `cargo xtask starry qemu` → `rootfs-<arch>.img`
- `os/StarryOS/Makefile` → `os/StarryOS/make/disk.img`

两者不互通：一边下载过 rootfs，不代表另一边会自动复用。

### 查看 rootfs 内容

本地 Makefile 路径：

```bash
mkdir -p /mnt/rootfs
sudo mount -o loop os/StarryOS/make/disk.img /mnt/rootfs
ls /mnt/rootfs
sudo umount /mnt/rootfs
```

根目录 xtask 路径需先确认实际 `rootfs-<arch>.img` 位置，再按同样方式挂载。

## 调试建议

### 看更详细的日志

```bash
cd os/StarryOS
make ARCH=riscv64 LOG=debug run
```

### 使用 GDB

```bash
cd os/StarryOS
make ARCH=riscv64 debug
```

### 常见排查顺序

如果 StarryOS 没有按预期启动，优先检查：

1. rootfs 是否存在
2. 当前使用的是根目录 xtask 路径还是本地 Makefile 路径
3. 最近的改动到底在共享组件、ArceOS 模块还是 StarryOS 内核

## 继续往哪里读

- [StarryOS 架构](/docs/architecture/starryos): 叠层架构、syscall 分发、进程与地址空间机制
- [components.md](/docs/components/overview): 共享依赖如何落到 StarryOS
- [build-system.md](/docs/build/overview): rootfs 位置、xtask 和 Makefile 边界
- [arceos-guide.md](/docs/development/arceos): 当改动落在 ArceOS 共享模块层时
