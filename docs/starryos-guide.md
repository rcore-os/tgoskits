# StarryOS 开发指南

在 TGOSKits 里，StarryOS 不是一套完全孤立的内核，而是建立在 ArceOS 模块层之上的 Linux 兼容系统。理解 StarryOS 的关键是把三层连起来看：共享基础组件提供通用能力，ArceOS 模块层提供操作系统底层支撑，StarryOS 自己的内核逻辑和 rootfs 用户态实现 Linux 兼容行为。本文档介绍 StarryOS 在仓库中的位置、运行入口、典型开发流程和调试方法。

## 1. StarryOS 在仓库里的位置

StarryOS 的代码分散在 `os/StarryOS/`、`components/` 和 `os/arceos/` 三个区域。理解哪些目录属于"StarryOS 自己"、哪些属于"被 StarryOS 复用的共享层"，是判断改动影响面的关键。

| 路径 | 角色 | 什么时候会改到 |
| --- | --- | --- |
| `os/StarryOS/kernel/` | StarryOS 内核实现 | syscall、进程、内存、文件系统、驱动接入 |
| `os/StarryOS/starryos/` | 启动包与 feature 组合 | 改启动入口、包级 feature、平台筛选 |
| `components/starry-*` | Starry 专用复用组件 | `starry-process`、`starry-signal`、`starry-vm`、`starry-smoltcp` 等 |
| `components/axpoll`、`components/rsext4` 等 | Starry 常用共享组件 | I/O 多路复用、文件系统等 |
| `os/arceos/modules/*` | StarryOS 复用的底层能力 | HAL、任务、驱动、网络、内存 |
| `test-suit/starryos/` | 系统测试入口 | 回归测试 |

## 2. 运行入口

StarryOS 提供两种运行方式：仓库根目录的 `cargo xtask starry` 统一入口和 `os/StarryOS/` 下的本地 Makefile 入口。两者使用不同的 rootfs 镜像位置（详见第 6 节），首次上手建议统一使用根目录入口。

### 仓库根目录的推荐入口

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

根目录入口的特点：

- `rootfs` 会把镜像准备到 Starry 的目标产物目录
- `run` 在发现磁盘镜像缺失时也会自动补准备
- 默认包是 `starryos`

首次上手建议统一使用 `riscv64`。如果你已经熟悉流程，可以尝试：

```bash
cargo xtask starry run --arch loongarch64 --package starryos
```

### `os/StarryOS/` 里的本地入口

```bash
cd os/StarryOS
make rootfs ARCH=riscv64
make ARCH=riscv64 run
```

常用快捷命令：

```bash
make rv
make la
```

本地 Makefile 路径的特点是：

- rootfs 固定复制到 `os/StarryOS/make/disk.img`
- 更适合调试 StarryOS 自己的 `make/` 行为

## 3. StarryOS 如何复用 ArceOS 和组件

StarryOS 的能力来自三个层次：`components/` 下的共享基础 crate、`components/starry-*` 下的 StarryOS 专用组件、以及 `os/arceos/modules/` 下的 ArceOS 内核模块。下面的流程图展示了这些层次之间的关系。理解这条链路，有助于判断你的改动会从哪一层开始传播。

```mermaid
flowchart TD
    SharedCrates["components/* shared crates"]
    ArceosModules["os/arceos/modules/*"]
    StarryCrates["components/starry-*"]
    StarryKernel["os/StarryOS/kernel/*"]
    StarryPackage["os/StarryOS/starryos"]
    RootfsUserspace["rootfs programs"]
    StarryTests["test-suit/starryos"]

    SharedCrates --> ArceosModules
    SharedCrates --> StarryCrates
    ArceosModules --> StarryKernel
    StarryCrates --> StarryKernel
    StarryKernel --> StarryPackage
    StarryPackage --> RootfsUserspace
    StarryPackage --> StarryTests
```

这条链路里最关键的判断是：

- 如果你改的是底层通用能力，通常先看 `components/*` 或 `os/arceos/modules/*`
- 如果你改的是 Linux 兼容行为，通常要看 `components/starry-*` 或 `os/StarryOS/kernel/*`
- 如果你改的是启动包、feature 组合或目标平台范围，要看 `os/StarryOS/starryos`

## 4. 常见开发动作

本节列出 StarryOS 开发中最常见的几类改动。无论你是修改共享基础能力、StarryOS 专用组件、内核逻辑还是启动配置，都应该按照推荐的顺序逐步验证，先确保底层消费者工作正常，再验证上层行为。

### 4.1 修改共享基础能力

如果你改的是：

- `components/axerrno`、`components/kspin` 这类基础 crate
- 或 `os/arceos/modules/axhal`、`axtask`、`axdriver`、`axnet`

建议先确认 ArceOS 最小路径仍然工作，再回到 StarryOS：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

### 4.2 修改 Starry 专用组件或内核逻辑

如果你改的是：

- `components/starry-process`
- `components/starry-signal`
- `components/starry-vm`
- `components/starry-smoltcp`
- `os/StarryOS/kernel/*`

那就直接从 StarryOS 路径开始验证：

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

### 4.3 增加 syscall 或用户可见行为

这类改动通常会同时触及：

- `os/StarryOS/kernel/` 里的 syscall / 进程 / 文件系统逻辑
- rootfs 中的测试程序或用户态验证脚本

推荐闭环是：

1. 先在内核里完成实现
2. 准备一个最小用户态程序去触发它
3. 把程序放入 rootfs
4. 启动 StarryOS 验证行为

如果你使用 Musl 工具链编译静态测试程序，最常见的做法是把它复制进挂载后的 rootfs 镜像中。

### 4.4 修改启动包和 feature 组合

`os/StarryOS/starryos/Cargo.toml` 里定义了包级 feature，例如 `qemu`、`smp`、`vf2`。如果你的改动更像"启动形态"而不是"内核算法"，先看这里而不是直接进 kernel。

## 5. 验证入口

StarryOS 提供了从日常运行到系统测试的多层验证入口。根目录 xtask 适合快速迭代，本地 Makefile 适合需要精细控制的场景，系统测试则用于自动化回归。

### 日常运行

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

### 系统测试

```bash
cargo xtask test starry --target riscv64gc-unknown-none-elf
```

根测试入口跑的其实是 `test-suit/starryos` 下的 `starryos-test` 包，而不是普通的 `starryos` 包。它更适合做自动化回归。

### 本地 Makefile 路径

```bash
cd os/StarryOS
make rootfs ARCH=riscv64
make ARCH=riscv64 run
make ARCH=riscv64 debug
```

## 6. rootfs 相关要点

StarryOS 的 rootfs 管理有两点需要特别注意：根目录 xtask 路径和本地 Makefile 路径使用不同的镜像位置，且彼此不会自动共享。理解这一点可以避免"明明下载过 rootfs 却还是报找不到镜像"的困惑。

### 两种路径不共享默认镜像位置

- 根目录 `cargo xtask starry rootfs` 使用目标产物目录下的 `disk.img`
- `os/StarryOS/Makefile` 使用 `os/StarryOS/make/disk.img`

这意味着：

- 你在根目录下载过 rootfs，不代表 `make rootfs` 一定会省略复制
- 你在本地 Makefile 路径准备过 rootfs，也不代表根目录 xtask 一定会直接复用

### 如何查看 rootfs 内容

如果你使用的是本地 Makefile 路径，最常见的是直接挂载 `os/StarryOS/make/disk.img`：

```bash
mkdir -p /mnt/rootfs
sudo mount -o loop os/StarryOS/make/disk.img /mnt/rootfs
ls /mnt/rootfs
sudo umount /mnt/rootfs
```

如果你使用的是根目录 xtask 路径，请先确认实际生成的 `disk.img` 位于哪个目标产物目录，再按同样方式挂载。

## 7. 调试建议

StarryOS 的调试手段与 ArceOS 类似，支持日志级别调整和 GDB 调试。当你需要排查启动问题时，建议先确认 rootfs 是否存在、使用的是哪种运行路径，然后根据最近的改动范围缩小排查方向。

### 看更详细的日志

本地 Makefile 路径最直接：

```bash
cd os/StarryOS
make ARCH=riscv64 LOG=debug run
```

### 使用 GDB

本地入口已经带有 `debug` / `justrun` 路径，比自己拼 QEMU 参数更稳妥：

```bash
cd os/StarryOS
make ARCH=riscv64 debug
```

### 常见排查顺序

如果 StarryOS 没有按预期启动，优先检查：

1. rootfs 是否存在
2. 当前使用的是根目录 xtask 路径还是本地 Makefile 路径
3. 最近的改动到底在共享组件、ArceOS 模块还是 StarryOS 内核

## 8. 继续阅读

以下是深入理解 StarryOS 及其上下文的推荐阅读顺序。

- [starryos-internals.md](starryos-internals.md): 系统理解 StarryOS 的叠层架构、syscall 分发、进程与地址空间机制
- [components.md](components.md): 从组件视角理解共享依赖如何落到 StarryOS
- [build-system.md](build-system.md): 理解 rootfs 位置、xtask 和 Makefile 的边界
- [arceos-guide.md](arceos-guide.md): 当你的改动落在 ArceOS 共享模块层时
