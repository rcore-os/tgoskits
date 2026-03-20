# StarryOS 开发指南

StarryOS 是一个基于 ArceOS 构建的教学操作系统，旨在提供一个 Linux 兼容的单体内核。

## 📋 目录

- [简介](#简介)
- [快速开始](#快速开始)
- [系统架构](#系统架构)
- [开发流程](#开发流程)
- [功能特性](#功能特性)
- [调试技巧](#调试技巧)
- [进阶主题](#进阶主题)

## 简介

### 特性

- ✅ **多架构支持**: RISC-V 64, LoongArch64, AArch64 (x86_64 开发中)
- ✅ **Linux 兼容**: 支持 Linux 系统调用接口
- ✅ **进程管理**: 完整的进程生命周期管理
- ✅ **内存管理**: 虚拟内存、分页机制
- ✅ **文件系统**: ext4 等文件系统支持
- ✅ **网络支持**: TCP/IP 协议栈
- ✅ **信号机制**: 进程间通信

### 适用场景

- **教学**: 操作系统课程实践
- **研究**: 操作系统原型验证
- **学习**: 理解 Linux 内核原理
- **开发**: 实验性操作系统开发

## 快速开始

### 方法一：在 TGOSKits 中构建（推荐）

```bash
# 1. 进入 TGOSKits 根目录
cd /path/to/tgoskits

# 2. 准备 rootfs（首次运行需要）
cargo xtask starry rootfs --arch riscv64

# 3. 运行 StarryOS
cargo xtask starry run --arch riscv64 --package starryos

# 4. 其他架构
cargo xtask starry run --arch loongarch64 --package starryos
cargo xtask starry run --arch aarch64 --package starryos
```

### 方法二：使用 Makefile

```bash
# 1. 进入 StarryOS 目录
cd os/StarryOS

# 2. 准备 rootfs
make rootfs ARCH=riscv64

# 3. 构建并运行
make ARCH=riscv64 run

# 快捷命令
make rv  # 等同于 make ARCH=riscv64 run
make la  # 等同于 make ARCH=loongarch64 run
```

### 方法三：使用 Docker

```bash
cd os/StarryOS

# 国内用户
docker pull docker.cnb.cool/starry-os/arceos-build
docker run -it --rm -v $(pwd):/workspace -w /workspace docker.cnb.cool/starry-os/arceos-build

# 在容器内执行
make rootfs
make ARCH=riscv64 run

# 海外用户
docker pull ghcr.io/arceos-org/arceos-build
docker run -it --rm -v $(pwd):/workspace -w /workspace ghcr.io/arceos-org/arceos-build
```

## 系统架构

### 整体架构

```
┌─────────────────────────────────────────┐
│         User Applications               │
├─────────────────────────────────────────┤
│         System Call Interface           │
├─────────────────────────────────────────┤
│  Kernel Services                        │
│  ├── Process Management                 │
│  ├── Memory Management                  │
│  ├── File System                        │
│  ├── Network Stack                      │
│  └── Signal Handling                    │
├─────────────────────────────────────────┤
│         ArceOS Modules                  │
│  ├── axhal (HAL)                        │
│  ├── axtask (Tasks)                     │
│  ├── axmm (Memory)                      │
│  ├── axdriver (Drivers)                 │
│  └── axnet (Network)                    │
├─────────────────────────────────────────┤
│         Hardware                        │
└─────────────────────────────────────────┘
```

### 核心组件

| 组件 | 路径 | 说明 |
|------|------|------|
| **starry-kernel** | `os/StarryOS/kernel` | 内核核心 |
| **starry-process** | `components/starry-process` | 进程管理 |
| **starry-signal** | `components/starry-signal` | 信号机制 |
| **starry-vm** | `components/starry-vm` | 虚拟内存 |
| **starry-smoltcp** | `components/starry-smoltcp` | 网络协议栈 |
| **axpoll** | `components/axpoll` | I/O 多路复用 |
| **rsext4** | `components/rsext4` | ext4 文件系统 |

### 与 ArceOS 的关系

StarryOS 基于 ArceOS 构建，复用了许多 ArceOS 的组件：

- **axhal**: 硬件抽象层
- **axtask**: 任务和线程管理
- **axmm**: 内存管理
- **axdriver**: 设备驱动
- **axnet**: 网络协议栈

同时添加了 StarryOS 特有的组件：

- **进程管理**: Linux 风格的进程模型
- **系统调用**: Linux 兼容的系统调用接口
- **信号**: POSIX 信号机制
- **文件系统**: 完整的文件系统支持

## 开发流程

### 目录结构

```
os/StarryOS/
├── kernel/               # 内核实现
│   ├── src/
│   │   ├── syscall/     # 系统调用实现
│   │   ├── process/     # 进程管理
│   │   ├── memory/      # 内存管理
│   │   └── fs/          # 文件系统
│   └── Cargo.toml
├── starryos/            # 主程序入口
├── configs/             # 配置文件
├── make/                # 构建系统
└── Makefile             # 顶层 Makefile
```

### 构建选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `ARCH` | 目标架构 | `riscv64` |
| `LOG` | 日志级别 | `warn` |
| `DWARF` | DWARF 调试信息 | `y` |
| `MEMTRACK` | 内存跟踪 | `n` |
| `BLK` | 块设备支持 | `y` |
| `NET` | 网络支持 | `y` |
| `MEM` | 内存大小 | `1G` |

### 修改代码后重新构建

```bash
# 在 TGOSKits 根目录
vim os/StarryOS/kernel/src/syscall/mod.rs

# 重新构建并运行
cargo xtask starry run --arch riscv64 --package starryos

# 或者使用 Makefile
cd os/StarryOS
make ARCH=riscv64 run
```

### 添加新的系统调用

1. **定义系统调用号** (在 `kernel/src/syscall/num.rs`)

```rust
pub const SYS_MY_SYSCALL: usize = 500;
```

2. **实现系统调用**

```rust
// kernel/src/syscall/mod.rs
pub fn sys_mysyscall(args: [usize; 6]) -> isize {
    let arg1 = args[0];
    let arg2 = args[1];
    
    // 实现逻辑
    debug!("mysyscall called: {}, {}", arg1, arg2);
    
    0
}
```

3. **注册到系统调用表**

```rust
// kernel/src/syscall/mod.rs
fn syscall_handler(num: usize, args: [usize; 6]) -> isize {
    match num {
        SYS_MY_SYSCALL => sys_mysyscall(args),
        // 其他系统调用...
        _ => -1,
    }
}
```

4. **测试系统调用**

```c
// test.c
#include <unistd.h>
#include <stdio.h>

int main() {
    long ret = syscall(500, 1, 2);
    printf("syscall returned: %ld\n", ret);
    return 0;
}
```

```bash
# 编译测试程序
riscv64-linux-musl-gcc -static test.c -o test

# 复制到 rootfs
cp test /path/to/rootfs/root/

# 运行 StarryOS 并测试
./test
```

## 功能特性

### 进程管理

```rust
// 创建进程
let pid = current_process().fork();

// 执行新程序
current_process().exec("/bin/sh", &["sh"], &[]);

// 等待子进程
let exit_code = current_process().waitpid(pid);
```

### 内存管理

```rust
// 分配内存
let ptr = axalloc::alloc_pages(4);

// 映射虚拟内存
let vaddr = current_process().vm_map(vaddr, size, prot);

// 取消映射
current_process().vm_unmap(vaddr, size);
```

### 文件系统

```rust
// 打开文件
let file = axfs::open("/etc/passwd", OpenOptions::new().read(true))?;

// 读取文件
let mut buf = [0u8; 1024];
let n = file.read(&mut buf)?;

// 写入文件
file.write(b"hello")?;
```

### 网络

```rust
// 创建 socket
let socket = axnet::socket(SocketType::Tcp)?;

// 连接服务器
socket.connect("10.0.2.2:80")?;

// 发送数据
socket.send(b"GET / HTTP/1.1\r\n")?;

// 接收数据
let mut buf = [0u8; 1024];
let n = socket.recv(&mut buf)?;
```

## 调试技巧

### 启用调试日志

```bash
# 方法1：通过 Makefile
make ARCH=riscv64 LOG=debug run

# 方法2：通过环境变量
export LOG=debug
cargo xtask starry run --arch riscv64 --package starryos
```

### 使用 GDB 调试

```bash
# 1. 启动 QEMU 并等待 GDB
cd os/StarryOS
make ARCH=riscv64 justrun

# 2. 在另一个终端连接 GDB
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/release/starryos

# GDB 命令
(gdb) target remote :1234
(gdb) break syscall_handler
(gdb) continue
(gdb) info registers
(gdb) backtrace
```

### 查看 rootfs 内容

```bash
# 挂载 rootfs 镜像
mkdir /mnt/rootfs
sudo mount -o loop os/StarryOS/make/disk.img /mnt/rootfs

# 查看内容
ls /mnt/rootfs

# 卸载
sudo umount /mnt/rootfs
```

### 内存调试

```bash
# 启用内存跟踪
make ARCH=riscv64 MEMTRACK=y run

# 查看内存使用情况
# 在 StarryOS shell 中运行
cat /proc/meminfo
```

## 进阶主题

### 添加新的文件系统

1. **实现文件系统 trait**

```rust
use axfs_vfs::VfsNodeOps;

pub struct MyFileSystem {
    // 文件系统状态
}

impl VfsNodeOps for MyFileSystem {
    // 实现必要的方法...
}
```

2. **注册文件系统**

```rust
use axfs::register_filesystem;

register_filesystem!("myfs", MyFileSystem::new());
```

### 添加新的驱动

```rust
use axdriver_base::DeviceDriver;

pub struct MyDevice {
    // 设备状态
}

impl DeviceDriver for MyDevice {
    fn name(&self) -> &str {
        "my-device"
    }
    
    fn init(&mut self) -> Result<()> {
        // 初始化设备
        Ok(())
    }
}
```

### 性能优化

1. **使用 release 模式**

```bash
make ARCH=riscv64 MODE=release run
```

2. **禁用调试信息**

```bash
make ARCH=riscv64 DWARF=n run
```

3. **调整内存大小**

```bash
make ARCH=riscv64 MEM=2G run
```

### 支持新架构

1. **添加架构支持到 ArceOS**
2. **实现架构特定的内核代码**
3. **更新构建配置**

```toml
# os/StarryOS/starryos/Cargo.toml
[package.metadata.vendor-filter]
platforms = [
    "riscv64gc-unknown-none-elf",
    "loongarch64-unknown-none-softfloat",
    "aarch64-unknown-none-softfloat",
    "new-arch-unknown-none-elf",  # 添加新架构
]
```

## 常见问题

### Q: rootfs 下载失败

**A:** 手动下载并放置：

```bash
# 从 GitHub 下载
wget https://github.com/Starry-OS/rootfs/releases/download/20260214/rootfs-riscv64.img.xz
xz -d rootfs-riscv64.img.xz
cp rootfs-riscv64.img os/StarryOS/make/disk.img
```

### Q: 运行时找不到 /init

**A:** 确保 rootfs 正确：

```bash
# 检查 rootfs
file os/StarryOS/make/disk.img

# 重新准备 rootfs
make rootfs ARCH=riscv64
```

### Q: 如何在 StarryOS 中运行自定义程序

**A:** 将程序添加到 rootfs：

```bash
# 编译静态链接程序
riscv64-linux-musl-gcc -static myapp.c -o myapp

# 挂载 rootfs
sudo mount -o loop os/StarryOS/make/disk.img /mnt/rootfs

# 复制程序
sudo cp myapp /mnt/rootfs/usr/bin/

# 卸载
sudo umount /mnt/rootfs
```

## 参考资源

- [StarryOS 仓库](https://github.com/Starry-OS/StarryOS)
- [Linux 系统调用手册](https://man7.org/linux/man-pages/man2/syscalls.2.html)
- [ArceOS 文档](https://arceos-org.github.io/arceos/)
- [rCore Tutorial](https://rcore-os.cn/rCore-Tutorial-Book-v3/)

---

**下一步**: 学习 [Axvisor 开发指南](axvisor-guide.md) 了解虚拟化技术
