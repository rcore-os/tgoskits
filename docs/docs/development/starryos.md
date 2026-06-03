# StarryOS 开发指南

StarryOS 是构建在 ArceOS 模块层之上的 Linux 兼容操作系统。本文档面向在 TGOSKits 工作区内进行 StarryOS 相关开发的场景，覆盖开发环境、内核开发规范、Syscall 开发流程、用户态程序开发、rootfs 管理、测试策略、调试技巧和多架构注意事项。

> 架构分层、syscall 分发和进程模型见 [StarryOS 架构](/docs/architecture/starryos)。
> 最短命令和快速启动见 [快速开始](/docs/quickstart/overview)。
> 构建系统总览见 [构建与运行](/docs/build/overview)。

---

## 1. 开发环境

### 1.1 工具链

StarryOS 共享 TGOSKits 工作区的统一工具链（`nightly-2026-04-27`），无需额外配置。详见 [ArceOS 开发指南 → 开发环境](/docs/development/arceos#1-开发环境)。

### 1.2 QEMU

StarryOS 需要更多内存（推荐 ≥ 512M）和可能的网络/块设备：

```bash
# 基本验证
cargo xtask starry qemu --arch riscv64

# aarch64
cargo xtask starry qemu --arch aarch64
```

### 1.3 交叉编译工具链（用户态程序开发）

开发用户态测试程序时需要交叉编译器：

| 架构 | 工具链包 | 前缀 |
|------|---------|------|
| aarch64 | `gcc-aarch64-linux-gnu` | `aarch64-linux-gnu-gcc` |
| riscv64 | `gcc-riscv64-linux-gnu` | `riscv64-linux-gnu-gcc` |

安装示例：

```bash
sudo apt install gcc-aarch64-linux-gnu gcc-riscv64-linux-gnu
```

如果使用 musl 静态链接：

```bash
# 安装 musl 交叉工具链
sudo apt install musl-tools
# 或使用 musl-cross-make 获取交叉版本
```

---

## 2. 目录结构总览

```
os/StarryOS/
├── starryos/          # StarryOS 启动包
│   ├── Cargo.toml     # 包级 feature：qemu, smp, rknpu
│   └── src/
│       └── main.rs    # 入口
├── kernel/            # StarryOS 内核（starry-kernel）
│   ├── Cargo.toml     # 内核 feature：memtrack, input, vsock, rknpu
│   └── src/
│       ├── entry.rs       # 初始进程创建，加载 /bin/sh
│       ├── lib.rs         # crate root
│       ├── config/        # 内核配置
│       ├── file/          # 文件描述符表、文件操作
│       ├── mm/            # 内存管理、用户地址空间、ELF 加载
│       ├── pseudofs/      # 伪文件系统（devfs, procfs 等）
│       ├── syscall/       # Syscall 完整实现
│       │   ├── mod.rs         # Syscall 分发
│       │   ├── fs/            # 文件系统相关 syscall
│       │   ├── task/          # 进程/线程相关 syscall
│       │   ├── mm/            # 内存管理相关 syscall
│       │   ├── net/           # 网络 socket syscall
│       │   ├── signal/        # 信号相关 syscall
│       │   ├── sync/          # futex、mutex、信号量
│       │   ├── ipc/           # pipe、shm、消息队列
│       │   ├── io_mpx/        # epoll、poll、select
│       │   ├── time/          # 时间相关 syscall
│       │   ├── resources/     # rlimit、prctl、getcpu
│       │   └── sys/           # uname、sysinfo、getpid 等
│       ├── task/          # 线程/进程数据、futex、信号、凭证
│       ├── time.rs        # 时间管理
│       └── trap.rs        # Trap/异常处理
└── Makefile            # 构建 rootfs 和运行
```

StarryOS 专用组件（位于 `components/`）：

| 组件 | 版本 | 职责 |
|------|------|------|
| `starry-process` | v0.4.5 | 进程生命周期、父子关系、进程组、会话 |
| `starry-signal` | v0.6.0 | 信号投递、信号处理、架构相关信号帧 |
| `starry-vm` | v0.5.6 | 用户地址空间管理、虚拟内存抽象 |

---

## 3. Feature 配置

### 3.1 启动包 Feature（`starryos/Cargo.toml`）

| Feature | 说明 |
|---------|------|
| `qemu` | 启用默认平台、PCI 总线、显示、输入、vsock、网络 |
| `smp` | 多核支持 |
| `rknpu` | Rockchip NPU 驱动支持 |

### 3.2 内核 Feature（`kernel/Cargo.toml`）

| Feature | 说明 |
|---------|------|
| `dev-log` | 开发日志 |
| `input` | 输入设备支持 |
| `memtrack` | 内存追踪（gimli-based） |
| `rknpu` | Rockchip NPU 驱动 |
| `vsock` | VSOCK 支持 |

内核默认启用：`fp-simd`, `irq`, `uspace`, `page-alloc-4g`, `alloc-slab`, `multitask`, `task-ext`, `sched-rr`, `rtc`, `fs-ng-ext4`, `net-ng`。

### 3.3 KCOV 暂不引入

StarryOS 目前不暴露 `kcov` feature，也不注册 `/dev/kcov`。此前尝试引入 Linux KCOV 兼容接口时，需要同时改动编译插桩参数、`ax-hal` 架构 trampoline、文件描述符状态、设备 mmap、任务生命周期和测试矩阵，侵入面过大。

另一个阻塞点是多核语义：KCOV 的 per-fd / per-task 状态需要和调度、抢占、线程退出、fork 以及中断上下文保持一致，当前实现还不能稳定覆盖 SMP 场景。因此在形成更小的边界和可靠的多核方案前，暂不把 KCOV 纳入 StarryOS。

---

## 4. Syscall 开发

### 4.1 Syscall 分发机制

Syscall 入口在 `kernel/src/syscall/mod.rs` 的 `handle_syscall()` 函数：

```rust
pub fn handle_syscall(uctx: &mut UserContext) {
    let Some(sysno) = Sysno::new(uctx.sysno()) else { ... };
    let result = match sysno {
        Sysno::ioctl => sys_ioctl(uctx.arg0(), uctx.arg1(), uctx.arg2()),
        Sysno::chdir => sys_chdir(uctx.arg0()),
        // ... 数百个 syscall
    };
}
```

`Sysno` 枚举来自 `syscalls` crate，覆盖 Linux 标准系统调用号。

### 4.2 添加新 Syscall 完整流程

以添加 `sys_mycall` 为例：

**步骤 1：在分发函数中添加 match arm**

编辑 `kernel/src/syscall/mod.rs`：

```rust
Sysno::mycall => sys_mycall(uctx.arg0(), uctx.arg1()),
```

> 如果 `Sysno` 枚举中尚无此 syscall 号，需更新 `syscalls` crate 或手动定义。

**步骤 2：选择合适的子模块实现**

根据 syscall 功能类别放入对应子模块：

| 类别 | 文件位置 | 典型 syscall |
|------|---------|-------------|
| 文件操作 | `syscall/fs/` | `open`, `read`, `write`, `ioctl`, `stat` |
| 进程/线程 | `syscall/task/` | `clone`, `execve`, `exit`, `wait4` |
| 内存管理 | `syscall/mm/` | `mmap`, `mprotect`, `munmap`, `brk` |
| 网络 | `syscall/net/` | `socket`, `bind`, `listen`, `accept` |
| 信号 | `syscall/signal/` | `sigaction`, `sigprocmask`, `kill` |
| 同步 | `syscall/sync/` | `futex`, `mutex` |
| IPC | `syscall/ipc/` | `pipe`, `shmget`, `msgsnd` |
| I/O 多路复用 | `syscall/io_mpx/` | `epoll_create`, `poll`, `select` |
| 时间 | `syscall/time/` | `clock_gettime`, `nanosleep` |
| 资源/系统 | `syscall/resources/` | `getrlimit`, `prctl`, `getcpu` |
| 系统信息 | `syscall/sys/` | `uname`, `sysinfo`, `getpid` |

**步骤 3：实现 syscall 函数**

```rust
// 例：在 syscall/fs/mycall.rs 中
pub fn sys_mycall(arg0: usize, arg1: usize) -> isize {
    // 参数解析和安全性检查
    // 实现逻辑
    // 返回值：0 表示成功，负数表示错误（-errno）
    0
}
```

**步骤 4：准备用户态测试程序**

编写最小 C 程序触发新 syscall：

```c
// test_mycall.c
#include <stdio.h>
#include <unistd.h>
#include <sys/syscall.h>

long mycall(int arg0, int arg1) {
    return syscall(SYS_mycall, arg0, arg1);
}

int main() {
    long ret = mycall(1, 2);
    printf("mycall returned: %ld\n", ret);
    return 0;
}
```

交叉编译并放入 rootfs：

```bash
# aarch64
aarch64-linux-gnu-gcc -static -o test_mycall test_mycall.c

# riscv64
riscv64-linux-gnu-gcc -static -o test_mycall test_mycall.c
```

**步骤 5：启动验证**

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64
```

在 StarryOS shell 中运行测试程序。

**步骤 6：添加到 test-suit**

将测试程序加入 `test-suit/starryos/normal/` 对应目录，编写配置文件。

### 4.3 Syscall 实现注意事项

- **参数安全性**：所有来自用户空间的指针必须验证可访问性，使用 `UserPtr` 或手动 `copy_from_user` / `copy_to_user`
- **错误返回**：使用负数返回 `-errno`，而非设置 `errno` 全局变量
- **锁使用**：内核代码运行在 IRQ 上下文时注意锁的使用，参考 `kspin` 组件
- **与 Linux 对齐**：参考 Linux 内核对应 syscall 的行为和边界条件，注意 `man 2 <syscall>` 中的错误情况

---

## 5. 进程与信号开发

### 5.1 进程管理（`starry-process`）

进程相关的核心数据结构：

- **`Process`**：进程结构体，管理地址空间、文件描述符表、子进程列表
- **`ProcessGroup`**：进程组，支持信号组播
- **`Session`**：会话，管理控制终端

开发流程：

1. 在 `components/starry-process/src/` 中修改进程数据结构或逻辑
2. 在 `kernel/src/task/` 中调整与内核的集成
3. 通过 `clone` / `execve` / `exit` 等 syscall 路径验证

### 5.2 信号机制（`starry-signal`）

信号投递链路：

```
用户调用 kill() / sigaction()
  → syscall 分发
    → starry-signal: 设置 pending 或修改 disposition
      → 返回用户空间前检查 pending
        → 构造信号帧（架构相关）
          → 跳转到用户信号处理函数
```

信号处理涉及架构相关代码（信号帧布局），位于 `starry-signal/src/arch/`。

### 5.3 地址空间管理（`starry-vm`）

`starry-vm` 提供两种模式：

- **`alloc` feature（默认）**：完整的用户地址空间管理
- **no-alloc（thin 模式）**：轻量级封装

开发时关注：

- `mmap` / `munmap` 系统调用如何通过 `starry-vm` 操作地址空间
- ELF 加载器如何使用 `starry-vm` 映射程序段
- 写时复制（COW）在 `fork` 中的实现

---

## 6. rootfs 管理

### 6.1 rootfs 概述

StarryOS 使用预构建的 rootfs 镜像（如 `rootfs-aarch64-alpine.img`），基于 Alpine Linux，包含基本的用户态工具（busybox、shell 等）。

### 6.2 下载和准备 rootfs

```bash
# 通过 xtask 下载 rootfs
cargo xtask starry rootfs --arch riscv64
cargo xtask starry rootfs --arch aarch64
```

镜像放置位置：`target/rootfs/`。

### 6.3 查看和修改 rootfs 内容

```bash
# 创建挂载点
mkdir -p /mnt/rootfs

# 挂载 rootfs 镜像
sudo mount -o loop target/rootfs/rootfs-riscv64-alpine.img /mnt/rootfs

# 查看内容
ls /mnt/rootfs
ls /mnt/rootfs/bin
ls /mnt/rootfs/usr

# 添加自定义程序
sudo cp test_mycall /mnt/rootfs/root/

# 卸载
sudo umount /mnt/rootfs
```

### 6.4 构建自定义 rootfs

如需更完整的 rootfs（包含额外工具或库），可以基于 Alpine 构建自定义镜像：

```bash
# 安装 alpine-make-rootfs
# 使用 debootstrap 或手动构建
# 核心思路：创建最小文件系统 → 安装必要包 → 打包为 ext4 镜像
```

### 6.5 xtask 与 Makefile 的 rootfs 差异

| 入口 | rootfs 位置 | 说明 |
|------|------------|------|
| `cargo xtask starry rootfs` | `target/rootfs/` | 根目录统一入口 |
| `os/StarryOS/Makefile` | `os/StarryOS/make/disk.img` | 本地 Makefile 入口 |

**重要**：两者不互通。一边下载过的 rootfs，另一边不会自动复用。建议统一使用 xtask 入口。

---

## 7. 用户态程序开发

### 7.1 静态链接 C 程序

推荐使用静态链接，避免依赖 rootfs 中的动态库版本：

```bash
# aarch64
aarch64-linux-gnu-gcc -static -o mytest mytest.c

# riscv64
riscv64-linux-gnu-gcc -static -o mytest mytest.c
```

### 7.2 使用 musl 静态链接

```bash
# 使用 musl-gcc
musl-gcc -static -o mytest mytest.c

# 交叉编译需要 musl-cross 工具链
aarch64-linux-musl-gcc -static -o mytest mytest.c
```

### 7.3 放入 rootfs

```bash
# 挂载 rootfs
sudo mount -o loop target/rootfs/rootfs-riscv64-alpine.img /mnt/rootfs

# 复制程序
sudo cp mytest /mnt/rootfs/root/

# 确保可执行
sudo chmod +x /mnt/rootfs/root/mytest

# 卸载
sudo umount /mnt/rootfs
```

### 7.4 Python 脚本测试

rootfs 中包含 Python 环境，可直接运行 Python 测试脚本：

```bash
# 在 StarryOS shell 中
python3 /root/test.py
```

---

## 8. 测试

### 8.1 测试套件结构

`test-suit/starryos/` 分为两组：

**`normal/`** — 标准测试：

| 目录 | 内容 |
|------|------|
| `qemu-smp1/` | 单核 QEMU 测试（smoke, busybox, python-hello, syscall, bugfix, usb 等） |
| `qemu-smp4/` | 多核 QEMU 测试（affinity, test-shm-deadlock） |
| `qemu-dhcp/` | DHCP 网络测试 |
| `board-orangepi-5-plus/` | OrangePi-5-Plus 物理板测试（net-smoke, npu-yolov8, pcie-enumerate） |

**`stress/`** — 压力测试：

| 目录 | 内容 |
|------|------|
| `postgresql/` | PostgreSQL 工作负载 |
| `stress-ng-0/` | stress-ng 测试 |

### 8.2 测试配置格式

StarryOS 测试配置与 ArceOS 类似，但增加了 shell 交互：

```toml
# build config
env = {AX_IP = "10.0.2.15", AX_GW = "10.0.2.2"}
features = [
  "ax-feat/rtc",
  "ax-driver/serial",
  "ax-driver/virtio-blk",
  "ax-driver/virtio-net",
]
log = "Warn"
plat_dyn = true
target = "riscv64gc-unknown-none-elf"
```

```toml
# qemu runtime config
args = ["-nographic", "-m", "512M", "-cpu", "cortex-a53", ...]
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 5
```

**与 ArceOS 测试的关键差异**：StarryOS 使用 `shell_prefix` + `shell_init_cmd` 与已启动的 Linux shell 交互，而非仅匹配输出。

关键字段：

| 字段 | 说明 |
|------|------|
| `shell_prefix` | Shell 提示符匹配模式 |
| `shell_init_cmd` | 在 shell 中执行的命令 |
| `timeout` | 超时时间（秒） |
| `success_regex` | 匹配成功的正则 |
| `fail_regex` | 匹配失败的正则 |

### 8.3 运行测试

```bash
# 通过 xtask 运行 StarryOS QEMU 测试
cargo xtask starry test qemu --target riscv64
cargo xtask starry test qemu --target aarch64-unknown-none-softfloat
```

### 8.4 添加新测试用例

1. 在 `test-suit/starryos/normal/qemu-smp1/`（或对应目录）下创建测试
2. 准备测试程序（C/Python/Shell），放入 rootfs 或通过 `shell_init_cmd` 直接执行
3. 编写 `build-<target>.toml` 和 `qemu-<arch>.toml`
4. 确认 `shell_prefix` 与实际 shell 提示符匹配
5. 通过 `cargo xtask starry test qemu` 验证

---

## 9. 调试

### 9.1 日志

```bash
# 使用 Makefile
cd os/StarryOS
make ARCH=riscv64 LOG=debug run
```

内核 feature `dev-log` 可提供更详细的内核日志。

### 9.2 GDB

```bash
cd os/StarryOS
make ARCH=riscv64 debug
```

在另一个终端连接 GDB：

```bash
riscv64-unknown-elf-gdb <binary>
(gdb) target remote :1234
(gdb) break handle_syscall
(gdb) continue
```

### 9.3 Syscall 追踪

StarryOS 目前没有实现 `strace` 等价工具，但可以通过以下方式观察 syscall 行为：

1. 在 `handle_syscall()` 中临时添加 `info!` 日志
2. 启用 `LOG=debug` 查看内核层面的调用序列
3. 在特定 syscall 实现中添加参数/返回值日志

```rust
// 在 syscall/mod.rs 中临时添加
Sysno::mycall => {
    info!("mycall({}, {})", uctx.arg0(), uctx.arg1());
    let ret = sys_mycall(uctx.arg0(), uctx.arg1());
    info!("mycall returned {}", ret);
    ret
}
```

### 9.4 排错清单

StarryOS 未按预期启动时，按以下顺序排查：

1. **rootfs 是否存在**：`ls target/rootfs/`
2. **rootfs 路径是否正确**：xtask 路径 vs Makefile 路径
3. **改动范围**：共享组件？ArceOS 模块？StarrryOS 内核？
4. **架构是否匹配**：rootfs 架构与 QEMU 架构一致
5. **内存是否足够**：StarryOS 推荐 ≥ 512M

---

## 10. 多架构注意事项

### 10.1 架构差异

| 方面 | aarch64 | riscv64 |
|------|---------|---------|
| syscall 入口 | `svc #0` 指令 | `ecall` 指令 |
| 信号帧布局 | aarch64 专用 | riscv64 专用 |
| 页表格式 | 4 级或 3 级页表 | Sv39 / Sv48 |
| 中断控制器 | GIC | PLIC |
| 默认 QEMU CPU | cortex-a53 | 默认 |

### 10.2 验证矩阵

建议在两个架构上都验证 StarryOS 改动：

```bash
# riscv64
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64

# aarch64
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64
```

---

## 11. 与 ArceOS 的关系

StarryOS 复用了大量 ArceOS 模块，改动共享模块时的验证策略：

| 改动位置 | 先验证 | 再验证 |
|----------|--------|--------|
| `components/axerrno`、`kspin` 等基础 crate | `cargo test -p <crate>` | ArceOS helloworld + StarryOS qemu |
| `os/arceos/modules/axhal` | ArceOS helloworld | StarryOS qemu |
| `os/arceos/modules/axtask` | ArceOS helloworld | StarryOS qemu |
| `components/starry-process` 等 Starrry 专用 | StarryOS qemu | — |
| `os/StarryOS/kernel/*` | StarryOS qemu | — |

---

## 12. 推荐阅读

- [StarryOS 架构](/docs/architecture/starryos): 叠层架构、syscall 分发、进程与地址空间机制
- [组件开发指南](/docs/development/components): 共享依赖如何落到 StarryOS
- [构建与运行](/docs/build/overview): rootfs 位置、xtask 和 Makefile 边界
- [ArceOS 开发指南](/docs/development/arceos): 当改动落在 ArceOS 共享模块层时
