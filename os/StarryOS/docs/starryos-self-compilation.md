# StarryOS 自编译全过程

在 riscv64 Debian Linux 上运行 StarryOS，并在 StarryOS 内部使用 cargo 编译 StarryOS 自身。前置依赖 PR

实现自编译需要两个基础设施 PR 作为前置条件。

### PR #797 — 信号传递修复

**现象**: cargo/rustc 在执行子进程（build script、proc-macro 等）时，子进程随机挂起无法退出。

**根因**: 信号传递后缺少 `wake_task` 调用，被信号唤醒的进程没有被加入调度队列。同时缺少 `dumpable`/`no_new_privs` 字段，导致 ptrace 和 `/proc/self/` 相关操作失败——构建工具链（cc、rustc）依赖这些接口来管理子进程。

**修复文件**:
| 文件 | 变更 |
|------|------|
| `task/mod.rs` | 添加 `dumpable`/`no_new_privs` 字段到进程结构 |
| `task/signal.rs` | 信号传递后调用 `wake_task` 唤醒目标进程 |
| `syscall/signal.rs` | 修正信号相关系统调用的 uid 参数校验 |

**关联**: 无此修复，cargo 在执行 build script 时会因子进程挂起而永远等待，自编译无法启动。

### PR #804 — 页回收

**现象**: 编译 `syn`、`proc-macro2` 等大型 crate 时 OOM panic。

**根因**: 8GB 物理内存下，cargo build 会产生大量文件缓存页面（源码、中间产物）。当可用内存不足时，没有机制回收不再使用的干净文件页面，导致帧分配器返回 `NoMemory`。

**修复文件**:
| 文件 | 变更 |
|------|------|
| `axalloc/src/lib.rs` | 注册 `page_cache_reclaim` 回调，分配失败时尝试回收 |
| `axalloc/src/buddy_slab.rs` | 分配重试逻辑（最多 4 次），每次失败后触发回收 |
| `axalloc/src/default_impl.rs` | 同上 |
| `axfs-ng/src/highlevel/file.rs` | LRU 页面缓存驱逐：回收干净的文件支持页面 |
| `axsync/src/mutex.rs` | 移除 `try_lock` 路径中的 `might_sleep()`（try_lock 是单次 CAS，永不应阻塞） |
| `entry.rs` | 启动时注册回收回调 |

**关联**: 无此修复，`syn` crate（编译第 7/276）会因 OOM 而 panic。

## 阻塞点及修复

### 1. 内存检测：仅识别 512MB

**现象**: QEMU `-m 8G`，但 `phys_ram_ranges()` 只返回 `[0x804f0000, 0xa0000000)`（~510MB）。

**根因**: 早期已淘汰的 RISC-V QEMU 平台配置路径依赖 axconfig 中的固定内存大小，`phys_ram_ranges()` 会按该常量计算可用内存，忽略 FDT 中的实际物理 RAM。

**修复**: RISC-V QEMU 默认构建改走 `axplat-dyn`，`axplat-dyn/src/mem.rs` 的 `phys_ram_ranges()` 从 `somehal::mem::memory_map()` 动态读取 Free 区域。

### 2. Bitmap 容量溢出

**现象**: 修改 axconfig 为 8G 后，内核 panic:

```
bitmap capacity exceeded: need 3145728 pages but CAP is 1048576
```

**根因**: 默认 `page-alloc-4g` 使用 `BitAlloc1M`（1M bits = 4GB 最大容量）。8GB 需要 2M pages > 1M CAP。

**修复**: 早期方案是把 `os/arceos/modules/axalloc/Cargo.toml` 的默认特性从 `ax-allocator/page-alloc-4g` 提升到 `page-alloc-64g`（16M bits = 64GB）。该 crate 之后被重构——当前 `default = []`，仅用 TLSF/buddy-slab，无 `page-alloc-*` 特性、无 `ax-allocator` 依赖；TLSF 没有硬编码的页容量上限，因此不再需要 `page-alloc-*` 透传。

### 3. TMPFS 挂载失败

**现象**: `mount -t tmpfs` 失败，Debian 根文件系统中 /tmp 不可写。

**根因**: mount(8) 优先使用新版 mount API（fsopen/fsconfig/fsmount）。StarryOS 将 `fsopen` 等实现为 `sys_dummy_fd`（返回伪 fd），mount(8) 误以为挂载成功，不会回退到传统 mount(2)。

**修复** (`os/StarryOS/kernel/src/syscall/mod.rs`):
```rust
// 将 fsopen/fspick/open_tree 从 sys_dummy_fd 改为返回 ENOSYS
// mount(8) 收到 ENOSYS 后回退到传统 mount(2) 调用来挂载 tmpfs
Sysno::fsopen | Sysno::fspick | Sysno::open_tree => Err(AxError::Unsupported),
```

### 4. 最终链接: _ex_table_end 未定义

**现象**: 所有 276 个 crate 编译通过，但 starryos 二进制链接失败:
```
rust-lld: error: undefined symbol: _ex_table_end
```

**根因**: 自编译环境中 `.cargo/config.toml` 未传递最终 `-Tlinker.x`。`linker.x` 期望先通过 `runtime.x` 定义 runtime 公共段（含 `_ex_table_end`），但缺少最终 linker 脚本时符号不存在。

**修复** (`os/StarryOS/starryos/linker.ld`):
```ld
PROVIDE(_ex_table_start = 0);
PROVIDE(_ex_table_end = 0);

SECTIONS {
    /* ... 原有内容 ... */
}
INSERT AFTER .data;
```

`PROVIDE` 仅在符号未定义时提供回退值（空异常表，不影响正常运行）。

### 5. 测试正则误匹配

**现象**: 编译 crate `axpanic` 时，cargo 输出 `panic v0.1.0`，触发 fail_regex `\bpanic`。

**修复**: 改为 `\bpanicked\b` 仅匹配真正的内核 panic 消息。

## 测试链路

```
QEMU riscv64 (-m 12G)
  └─ OpenSBI
      └─ someboot (加载 FDT 内存布局)
          └─ StarryOS 内核
              └─ Debian riscv64 rootfs (ext4)
                  └─ /usr/bin/self-compile.sh
                      ├─ mount tmpfs (12G)
                      ├─ 修补 linker.ld
                      └─ cargo build -p starryos --offline (276 crates)
```

## 测试配置

### 测试用例 (`apps/starry/selfhost/selfhost-full-kernel/`)

测试用例是一个 self-compile app，通过 `cargo xtask starry app qemu -t selfhost/selfhost-full-kernel` 发现和运行（`cargo xtask starry app list` 显示为 `qemu selfhost/selfhost-full-kernel prebuild`），避免因缺少 Debian rootfs 镜像而阻塞标准 CI。

```toml
# qemu-riscv64.toml
args = ["-nographic", "-cpu", "rv64", "-smp", "1", "-m", "12G", ...]
shell_init_cmd = "/usr/bin/self-compile.sh"
success_regex = ['(?m)^SELFHOST_SUCCESS\\s*$']
fail_regex = ['(?i)\bpanicked\b', 'SELFHOST_FAILED']
timeout = 7200
```

Shell pipeline (`sh/self-compile.sh`) 自动注入到 rootfs 的 `/usr/bin/`:
1. 挂载 12G tmpfs 到 /tmp
2. 修补 linker.ld 添加 PROVIDE 回退
3. 执行 `cargo build -p starryos --target riscv64gc-unknown-none-elf --offline`
4. 检查产物并输出 SELFHOST_SUCCESS 或 SELFHOST_FAILED

### 运行测试

```bash
# 在提前准备 rootfs 的环境中
cargo xtask starry app qemu -t selfhost/selfhost-full-kernel --arch riscv64
```

## 独立脚本

除了通过 xtask 测试框架运行外，还提供了两个独立脚本用于自编译和启动。

### scripts/prepare-selfhost-rootfs.sh — 准备自编译 rootfs 镜像（统一脚本）

**前提**: 
- riscv64: 已有 Debian riscv64 基础镜像（`tmp/axbuild/rootfs/rootfs-riscv64-debian.img`）
- x86_64: `sudo pacman -S debootstrap`（Arch Linux），本机架构无需 QEMU
- aarch64: `sudo pacman -S debootstrap qemu-user-static-binfmt`

**工作流程** (自动完成):
1. 创建/复制基础 Debian rootfs（x86_64/aarch64 用 debootstrap，riscv64 用已有镜像）
2. 安装 rustc + cargo + build-essential
3. 扩展镜像至 ~8GB
4. 注入 StarryOS 源码（git archive）
5. 配置 `/root/.cargo/config.toml`（offline + 对应 target）
6. 预取全部 workspace crate 依赖（cargo fetch）
7. 验证关键文件存在

```bash
# 三种架构用法一致
sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64
sudo ./scripts/prepare-selfhost-rootfs.sh --arch riscv64
sudo ./scripts/prepare-selfhost-rootfs.sh --arch aarch64
```

> **注意**: x86_64 和 aarch64 需要 `sudo`（debootstrap 需要 root 权限创建 rootfs）。riscv64 如已有基础镜像则不需要 sudo。

### scripts/self-compile.sh — 在 StarryOS 内编译自身

**前提**: 已运行 `scripts/prepare-selfhost-rootfs.sh` 生成含源码和依赖的 rootfs 镜像。

**工作流程**（本脚本是 `cargo xtask starry app qemu` 的薄封装）:
1. 解析 `--arch`（默认 `riscv64`）和 `--smp`（默认 `4`）参数，导出 `SELF_COMPILE_*` 环境变量；x86_64 会从 blueprint 克隆一份工作副本 rootfs
2. 调用 `cargo xtask starry app qemu -t selfhost/selfhost-full-kernel --arch $ARCH`。app runner 负责：构建种子内核 → 运行 `selfhost-full-kernel/prebuild.sh` 生成架构感知的 overlay（含 `self-compile-inner.sh`）→ 通过 **debugfs** 将 overlay 注入 rootfs（无 loopback mount、无 expect）→ 启动 QEMU（x86_64 走 OVMF/UEFI，riscv64 走 `-kernel`）
3. guest 内 `self-compile-inner.sh` 执行：riscv64 挂载 12G tmpfs → 修复 `linker.ld` → `cargo build --offline` → 产物写入 rootfs 持久路径 `/opt/starryos-selfbuilt`
4. QEMU 退出后，宿主机用 `debugfs -R "dump /opt/starryos-selfbuilt ..."` 从 rootfs 镜像提取自编译内核到 `tmp/starryos-selfbuilt-<arch>`

```bash
# riscv64（默认）：4 核 CPU + 4 个 cargo jobs
./scripts/self-compile.sh

# x86_64：自动启用 KVM 加速（~10x 速度提升）
./scripts/self-compile.sh --arch x86_64

# aarch64：自编译暂不支持（self-compile.sh 直接报错：无 qemu-aarch64.toml，仅 riscv64/x86_64 有效）

# 自定义 SMP 和 cargo jobs
./scripts/self-compile.sh --arch riscv64 --smp 2 --jobs 2
```

**关键设计**:
- 编译产物写入 rootfs 持久路径 `/opt/starryos-selfbuilt`（不是 tmpfs 的 `/tmp`），确保重启后仍然存在
- 串口自动化由 xtask app runner 完成（`cargo xtask starry app qemu`，通过 app 的 `prebuild.sh` + `shell_init_cmd` 在 guest 内驱动编译），不需要手动输入命令，也不使用 `expect`
- QEMU 超时设置为 7200 秒（见 `qemu-{riscv64,x86_64}.toml` 的 `timeout`）；x86_64 + KVM 下可大幅缩短
- **SMP 默认值提升至 4**：QEMU `-smp 4` + `CARGO_BUILD_JOBS=4`，利用多核加速编译
- x86_64 自动检测 `/dev/kvm` 并启用硬件加速，KVM 不可用时回退到 TCG 模拟

### scripts/run-selfbuilt-kernel.sh — 用自编译内核启动

**前提**: 已成功运行 `scripts/self-compile.sh`，rootfs 中存在 `/opt/starryos-selfbuilt`。

**工作流程**:
1. 通过 `debugfs` 从 rootfs 镜像提取 `/opt/starryos-selfbuilt` → `tmp/starryos-selfbuilt-${ARCH}`
2. 缓存提取的内核（per-arch），后续运行直接使用缓存（删除缓存文件可强制重新提取）
3. 用架构对应的 QEMU 启动提取的内核，使用同一个 rootfs 镜像

```bash
# 用法与 self-compile.sh 一致
./scripts/run-selfbuilt-kernel.sh --arch riscv64
./scripts/run-selfbuilt-kernel.sh --arch x86_64
./scripts/run-selfbuilt-kernel.sh --arch aarch64 --smp 4
```

**验证自编译内核**:
```
# 启动后进入 StarryOS shell，验证内核版本
root@starry:~# uname -a
# 或执行 shell 命令确认系统正常运行
root@starry:~# ls /opt/starryos/
```

### 完整工作流

```bash
# 一次性准备 rootfs（需要 sudo，首次约 30-60 分钟）
sudo ./scripts/prepare-selfhost-rootfs.sh --arch riscv64   # riscv64 (需已有 Debian 镜像)
sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64    # x86_64 (原生 debootstrap，最快)
sudo ./scripts/prepare-selfhost-rootfs.sh --arch aarch64       # aarch64 (交叉 debootstrap)

# 每次自编译
./scripts/self-compile.sh --arch riscv64       # ~60 分钟 (SMP 4)
./scripts/self-compile.sh --arch x86_64        # ~25 分钟 (KVM 加速)
# aarch64 自编译暂不支持（self-compile.sh 直接 error 退出，仅 riscv64/x86_64 有效）

# 用产物启动
./scripts/run-selfbuilt-kernel.sh --arch riscv64
./scripts/run-selfbuilt-kernel.sh --arch x86_64
# aarch64：boot 路径可用，但 self-compile.sh 尚不能产出 aarch64 自编译内核
```

### 与测试框架的对比

| 维度 | xtask 测试框架 | 独立脚本 |
|------|---------------|---------|
| 启动方式 | `cargo xtask starry app qemu -t selfhost/selfhost-full-kernel` | `./scripts/self-compile.sh` |
| 编译产物 | 保留在 tmpfs（重启丢失） | 持久化到 rootfs `/opt/` |
| 产物复用 | 不支持 | 缓存到 `tmp/` 目录 |
| 适用场景 | CI 验证"能否编译" | 开发验证"编译结果能否启动" |
| 需要 test-suit 配置 | 是（在 PR 分支） | 否（独立可用） |

## 构建耗时

| 架构 | SMP | KVM | 预计耗时 |
|------|-----|-----|----------|
| riscv64 | 4 | N/A | ~50-60 分钟 |
| x86_64 | 4 | yes | ~10-15 分钟 |
| x86_64 | 4 | no | ~60-90 分钟 |
| aarch64 | 4 | N/A | ~60-90 分钟 |

> 注：riscv64 单核耗时 ~100 分钟，4 核约 50-60 分钟。x86_64 + KVM 由于运行在宿主机本机指令集上，性能接近原生编译。

## 完整变更清单

### 前置依赖 PR
| PR | 内容 | 关键文件 |
|----|------|----------|
| #797 | 信号传递修复：wake_task + dumpable/no_new_privs | `task/mod.rs`, `task/signal.rs` |
| #804 | 页回收：内存压力下驱逐干净文件页面 | `axalloc/`, `axfs-ng/`, `axsync/` |

### 自编译 PR (#881)
| 文件 | 变更 |
|------|------|
| 旧 `axconfig.toml` | phys-memory-size: 512M → 8G |
| `axalloc/Cargo.toml` | page-alloc-4g → page-alloc-64g（后经重构改为 TLSF，`default = []`，无 page-alloc 特性） |
| `syscall/mod.rs` | fsopen/fspick/open_tree → ENOSYS |
| `linker.ld` | PROVIDE _ex_table_start/end |
| `axplat-dyn/src/mem.rs` | phys_ram_ranges 从 memory_map 动态读取 |
| `selfhost-full-kernel/` | 测试用例及构建脚本 |

### 页回收机制改进

基于 PR #804 的初始实现，对页面缓存回收机制进行了增强：

**修改文件**: `os/arceos/modules/axfs-ng/src/highlevel/file.rs`

**改进项**:

| 改进 | 原值 | 新值 | 说明 |
|------|------|------|------|
| 回收批次上限 | 256 页 (1MB) | 2048 页 (8MB) | 单次调用可回收更多页面，减少重试次数 |
| 目标放大因子 | ×2 | ×4 (上限 8192) | 更积极预回收，降低后续内存压力 |
| LRU 容量 | 64 页/文件 | 256 页/文件 | 减少顺序读取时的 LRU 抖动 |
| 脏页回退 | 无 | 两阶段回收 | 极端压力下允许回收脏页（记录 warning） |

**两阶段回收策略**:

```
第 1 阶段: try_evict_clean_pages()
  └─ 仅回收干净页面（无数据丢失风险）
  
如果回收量 < 请求量:
第 2 阶段: try_evict_pages(allow_dirty=true)
  └─ 回收包括脏页在内的所有页面（记录 warning）
  └─ StarryOS 自编译使用只读 ext4 挂载，脏页极少见
```

**代码位置**: `os/arceos/modules/axfs-ng/src/highlevel/file.rs:492-540`

## 已知限制

1. **`phys-memory-size` 硬编码 8G**: 动态 RAM 检测（someboot→OS 共享内存）因启动阶段地址空间不一致无法实现。使用少于 8G QEMU RAM 的标准测试会 panic。
2. **自编译测试不在标准 CI 中运行**: Debian rootfs 镜像未上传到 tgosimages 发布版，测试用例是 `apps/starry/selfhost/` 下的 self-compile app（通过 `cargo xtask starry app qemu -t selfhost/...` 运行），需要手动在配备 rootfs 的环境中运行。
3. **页面回收仅支持干净页的主动回收**: 脏页在第二阶段作为最后手段回收（记录 warning），但缺少脏页写回机制。对于只读 ext4 挂载的自编译场景影响很小。

## 环境

- **QEMU**: riscv64 (`-m 12G`, `-smp 1`) / x86_64 (KVM, `-m 16G`, `-smp 4`) / aarch64
- **内核**: StarryOS (dev 分支)
- **根文件系统**: Debian (per-arch), ext4, rustc nightly-2026-04-27
- **源码**: StarryOS monorepo (离线，预取依赖)

### 跨架构自编译状态

| 架构 | 种子内核构建 | QEMU 启动 | Debian rootfs | 自编译验证 | 备注 |
|------|-------------|----------|---------------|-----------|------|
| riscv64 | 通过 | 通过 | 已就绪 | 通过 | 完整验证通过 |
| x86_64 | 通过 | 通过 (KVM) | 已就绪 | 通过 ✅ (301/301, 6m53s) | SMP=1, Bug #8 修复后完成 |
| aarch64 | 通过 | 通过 | 需准备 | 待验证 | |

> x86_64: Debian rootfs 已通过 `prepare-selfhost-rootfs.sh --arch x86_64` 生成。种子内核可启动，workspace filter、cargo deps 缓存、rustc nightly 工具链均已就绪，自编译已端到端通过（301/301），产物经 debugfs 从 rootfs 提取。

### x86_64 自编译详解

#### 已解决的阻塞

| # | 问题 | 根因 | 修复 |
|---|------|------|------|
| 1 | 内核启动 `#PF` panic | 64-bit PCI BAR (28GB+) 未映射到页表 | `axdriver` 调用 `ax_mm::iomap()` 动态映射 BAR |
| 2 | QEMU 镜像文件锁 | 默认排他写锁 | `-drive file.locking=off` |
| 3 | cargo `--offline` 解析失败 | Workspace 包含 `arm_vcpu` 等架构专属 crate | `filter-workspace.sh`：grep 精确匹配，只删 member 行保 deps |
| 4 | cargo deps 缓存缺失 | `cargo fetch` 在 chroot 内写入错误目录 (`CARGO_HOME` 问题) | `copy-cargo-cache.sh`：从宿主机直接复制 2095 crates 到 rootfs |
| 5 | rustc 1.85 太旧 (MSRV 不满足) | Debian 系统 rustc 版本低 | 宿主机 nightly-2026-04-27 工具链复制到 rootfs (6.9GB) |
| 6 | ext4 `mkdir` 失败 (crate 9: `log`) | rustc 在 registry `src/` 中创建嵌套工作目录失败 | 整体 source tree + registry 移到 tmpfs |

#### 当前状态: 自编译端到端通过（debugfs 注入路径）

x86_64 与 riscv64 自编译均已完整通过。种子内核构建后，`prebuild.sh` 生成的 overlay 通过 `debugfs` 写入 rootfs 镜像，QEMU 内 guest 离线编译出完整内核，QEMU 退出后 host 再用 `debugfs dump` 提取 `/opt/starryos-selfbuilt`（见 `scripts/self-compile.sh` 与 `selfhost-full-kernel/qemu-*.toml`）。整个流程不做 loopback mount、不用 expect。debugfs 写入的文件在 guest 内可正常读取，rsext4 对 host 写入的 ext4 元数据解析正确。

#### 编译进度

```
[BUILD] 301/301 crates 编译成功（x86_64，6m53s）
[OK]    starry-kernel / starryos 均编译并链接成功
[EXTRACT] host 经 debugfs 从 rootfs 提取 /opt/starryos-selfbuilt（~16 MB, ELF64, 10737 kallsyms）
[BOOT]  自建内核在 OVMF/UEFI 下启动至 root@starry 提示符
```

riscv64 同样端到端通过。早期在 `starry-kernel`/`starryos` 观察到的 SIGSEGV 已随 Bug #8 修复解决。

### 动态内存检测

| 平台路径 | 检测方式 | 状态 |
|------|---------|------|
| x86_64 `axplat-dyn` | UEFI / runtime platform discovery | 已支持 |
| RISC-V QEMU `axplat-dyn` | `somehal::mem::memory_map()` | 已支持 |
| AArch64 `axplat-dyn` | `somehal::mem::memory_map()` | 已支持 |
| `axplat-dyn` (all arches) | `somehal::mem::memory_map()` | 已支持 |

`axplat-dyn` 的 `phys_ram_ranges()` 在启动时从 somehal memory map 动态发现物理 RAM。这使得在不同 RAM 大小的 QEMU 上运行自编译时不需重新编译内核。

## PR797 深入分析：为什么需要 wake_task，是被什么阻塞的？

### 信号传递与调度唤醒的完整链路

当 cargo/rustc 在 StarryOS 上执行子进程（build script、proc-macro）时，进程间通过信号协调生命周期：

```
cargo (父进程)
  ├─ spawn → build-script (子进程)
  ├─ waitpid(子进程) ───→ block_on(interruptible(wait_future))
  │                         └─ future_blocked_resched → 状态=Blocked，脱离运行队列
  └─ 信号到达(SIGCHLD等) → 须将父进程唤醒回运行队列，让 waitpid 返回
```

### 阻塞点：future_blocked_resched 将任务移出运行队列

`block_on` 的实现（`os/arceos/modules/axtask/src/future/mod.rs:55-95`）：

1. 轮询 future → 返回 `Poll::Pending`
2. 检查 `axwaker.woke` 锁 → 仍为 false
3. 调用 `rq.future_blocked_resched(woke)` → **将任务状态设为 Blocked，从运行队列移除，yield 到调度器**

此时任务**不在运行队列中**。没有任何调度器会再检查它的 `interrupted` 标志。

### 为何只有 flag 不够：interrupt_waker 的注册-唤醒闭环

`task.interrupt()` 做两件事（`os/arceos/modules/axtask/src/task.rs:338-340`）：
```rust
pub fn interrupt(&self) {
    self.interrupted.store(true, Ordering::Release);  // ① 设置标志
    self.interrupt_waker.wake();                       // ② 唤醒 waker
}
```

`interrupt_waker.wake()` 触发 `AxWaker::wake_by_ref()` → `rq.unblock_task(task, false)`，将任务状态从 `Blocked` 变回 `Ready` 并放回运行队列。

**修复前的 `task.interrupt()` 只有第①步（设置标志）。** 任务被 `future_blocked_resched` 移出运行队列后，没有任何机制将它放回——标志虽已设置，但调度器永远不会再检查它。

### 具体阻塞场景

在自编译过程中，子进程被阻塞的典型场景：

| 阻塞 syscall | 阻塞对象 | 阻塞在何处 | 触发信号 |
|-------------|---------|-----------|---------|
| `waitpid` | 子进程未退出 | `block_on(interruptible(wait_future))` | SIGCHLD |
| `futex` | 互斥锁/条件变量 | `block_on(interruptible(futex_future))` | 任意信号 |
| `read` (pipe) | 管道空，writer 关闭 | `block_on(interruptible(read_future))` | SIGPIPE |
| `sigtimedwait` | 无 pending signal | `block_on(interruptible(signal_future))` | 目标信号 |

当子进程也因同一 bug 而挂起时，父进程在 `waitpid` 中永远等不到子进程退出——而子进程恰好在等某个信号来唤醒自己。系统形成死锁循环。

### 与 I/O future 的区别（为何 I/O 不会被此 bug 影响）

I/O future（如 block read）有自己的 waker 回调（由设备驱动挂载）。即使 `interrupt_waker.wake()` 不触发，I/O 完成时设备驱动调用 `AxWaker::wake_by_ref()` 同样会走 `unblock_task` 把任务放回运行队列。任务恢复运行后，`poll_interrupt` 检测到 `interrupted` 标志，返回 `Interrupted`。

但对于 waitpid / futex / sigtimedwait，**没有外部设备驱动来触发 waker**。这些 future 的完成完全依赖另一个用户态进程或线程——如果那个进程/线程因同样的 bug 而挂起，没有任何人或硬件来拯救它。

### 修复演进历史

| 提交 | 变更 |
|------|------|
| `04686fbd5` | 新增 `ax_task::wake_task()` 函数，在信号传递后显式调用 `unblock_task` |
| `10e6008f2` | 修复 `poll_interrupt` 的 race：先注册 waker 再检查 flag（防止 wake 丢失） |
| `0e2341f8e` | 条件化 wake_task：仅在 `send_signal` 返回 true（信号可递达，非 blocked）时唤醒 |
| `ce6105da6` | 移除冗余 `task.interrupt()` 调用（`wake_task` 内部已包含） |
| 后续合并 | 将 `interrupt_waker.wake()` 集成进 `task.interrupt()`（`task.rs:338-340`）；独立的 `ax_task::wake_task()` 仍保留（`api.rs:528`），作为阻塞于裸 `WaitQueue` 的任务（pipe read / futex）的兜底强制唤醒路径 |

### 与 `interrupted` 标志的配合

`interruptible` future 包装器（`future/mod.rs:120-130`）轮询两个条件：
1. `curr.poll_interrupt(cx)` — 检查标志 + 注册 waker
2. `f.as_mut().poll(cx)` — 原始 future

当信号触发 `task.interrupt()`：
- wake 将任务放回运行队列 → 调度器选中 → `block_on` 循环重新轮询
- `poll_interrupt` 发现 `interrupted = true` → 返回 `Err(Interrupted)`
- 外层 syscall 返回 `-EINTR`
- `user.rs` 的 signal drain loop 调用 `check_signals()` → 递达 pending signal
- 后续由 `SA_RESTART` 逻辑决定是否重启 syscall

### 关键文件

| 文件 | 角色 |
|------|------|
| `os/arceos/modules/axtask/src/future/mod.rs` | `block_on` + `interruptible` + `AxWaker` |
| `os/arceos/modules/axtask/src/task.rs` | `TaskInner::interrupt()` — flag + wake |
| `os/arceos/modules/axtask/src/run_queue.rs` | `future_blocked_resched` / `unblock_task` |
| `os/StarryOS/kernel/src/task/signal.rs` | `send_signal_to_process` — 信号入队 + 唤醒目标 |
| `os/StarryOS/kernel/src/task/user.rs` | syscall 返回路径的 signal drain loop |

## 根因分析历史：早期「ext4 兼容性」误诊（已解决）

早期将 `Not found: /opt/starryos/Cargo.toml` 与文件损坏归因于 host ext4 驱动与 rsext4「不兼容」。经复核，**rsext4 内核代码正确**，也能正确读取 host 经 `debugfs` 注入的文件；实际问题全部位于脚本层，且已逐一修复，x86_64 自编译现已端到端通过。下列 Bug #1–#8 保留为已修复问题的历史记录：

### Bug #1: `metadata_csum` 与 `debugfs -w` 不兼容

- `mkfs.ext4` 在较新的 Linux（Debian Trixie）上默认启用 `metadata_csum`
- rsext4 支持 metadata_csum 的校验，所以 mount 不会报错
- 但 `debugfs -w` 写裸块时不计算 crc32c 校验和
- 后续读取时，rsext4 验证校验和失败 → EIO / EUCLEAN
- **修复**: `mkfs.ext4 -O ^metadata_csum,^metadata_csum_seed` 在创建镜像时禁用

### Bug #2: busybox grep 不支持 `[[:space:]]`

- `prepare-selfhost-rootfs.sh` 在 nspawn 内（Debian busybox grep）过滤 workspace
- `grep -v '^[[:space:]]*"components/name"'` 中的 `[[:space:]]` 不被 busybox grep 支持
- grep 输出空文件 → `mv filtered Cargo.toml` 截断 Cargo.toml 为 0 字节
- **修复**: 将 `[[:space:]]` 替换为 `[ ]`（literal space in brackets），并添加 `[ -s ]` 安全检查

### Bug #3:（已废弃）debugfs 目录项曾被怀疑不可靠

- 早期怀疑 debugfs 写入的目录项 rsext4 读不到，一度改用 loopback mount + cp
- 现状：debugfs 注入是当前规范路径（`scripts/axbuild/src/rootfs/inject.rs` 逐文件生成 debugfs 命令注入 overlay），guest rsext4 读取正常，loopback mount 已不再需要

### Bug #4:（已废弃）loopback mount/e2fsck 循环累积损坏

- 早期 self-compile.sh 反复 loopback mount→cp→umount→e2fsck，e2fsck『修复』可能引入新问题
- 现状：overlay 一次性经 debugfs 注入、提取亦走 debugfs，不再有 mount/e2fsck 循环，该问题不再出现

### Bug #5: Cargo.lock 跨平台依赖

- `cargo fetch` 在 prepare 阶段基于 full workspace 下载
- Cargo.lock 引用所有平台的 crate（包括 aarch64-cpu）
- `cargo build --offline --target x86_64` 仍需这些 crate 在 local registry 中
- **最终方案**: 全量 `cargo fetch`（无 `--target` 过滤），确保 Cargo.lock 中所有架构依赖均在本地缓存

### Bug #6: POSIX shell 重定向失败导致 init 进程退出

- StarryOS 的 `init.sh` 使用 `: > /run/udev/data/c226:0` 创建设备初始化标记文件
- POSIX shell 规范：非交互式 shell 中，重定向失败会导致 shell 退出
- rsext4 无法读取 host 侧创建的目录 → `mkdir /run/udev/data` 失败 → `: >` 重定向失败 → init 进程退出 → QEMU 终止
- `2>/dev/null` 无法阻止此行为——重定向失败是致命的，不等 `|| true` 执行
- **修复**: 将 `: > file` 替换为 `touch file 2>/dev/null || true`（`touch` 是命令，不会导致 shell 退出）

### Bug #7: 早期 SMP 文件系统锁竞争（已由细粒度锁解决）

- 早期 SMP>1 时 rsext4 在并发写入下可能停顿，曾以 SMP=1 规避
- 当前实现：`axfs-ng/src/fs/ext4/rsext4/fs.rs` 的 `Ext4Filesystem.inner` 使用 `SleepMutex`（见文件头 `os::sync::{SleepMutex as Mutex}`），并由 PR #1057 引入细粒度锁提升 SMP 可扩展性
- 结论：不再需要 SMP=1 规避，自旋锁重入死锁的旧根因已不适用

### Bug #8: USB UVC workspace member pulls in unvendored `qoi` dependency

- **现象**: `cargo build --offline` fails with `error: no matching package named 'qoi' found`
- **根因**: `crab-uvc` (`drivers/usb/usb-device/uvc`) 是 workspace member，其 dev-dependency `image = "0.24"` 依赖 `qoi` crate。`cargo fetch` 在 rootfs 准备阶段未成功下载 `qoi`（该 crate 可能未被正确 fetcher 获取或存在网络问题），导致 offline build 时 registry 中缺失该 crate
- **影响**: x86_64 自编译的 workspace 解析阶段立即失败，编译完全无法开始
- **修复**: 在 workspace 过滤阶段移除 `drivers/usb/usb-device/uvc` 成员行（类似于已过滤的 `drivers/usb/test_crates/`）。`crab-uvc` 是 USB 摄像头设备驱动，不参与内核编译
- **修复文件**: `scripts/self-compile.sh`（内建 grep 过滤器）、`scripts/prepare-selfhost-rootfs.sh`（cargo fetch 阶段 sed 过滤器）、`scripts/filter-workspace.sh`（独立过滤脚本）
- **关联**: 无此修复，所有 x86_64 QEMU 自编译尝试都在 cargo workspace 解析阶段失败

### 其他发现

- **rustc 版本**: Debian rustc 1.85.0 过旧（多个依赖需要 >= 1.85.1/1.86）。需通过 rustup 安装 nightly（已验证 rustc 1.97.0-nightly 可用）
- **CA 证书**: minbase 安装的 ca-certificates 包需要手动运行 `update-ca-certificates` 生成 `/etc/ssl/certs/ca-certificates.crt` 才能使用 curl
- **Debian 镜像**: `deb.debian.org` CDN 和 `ftp.us.debian.org` 均存在间歇性连接超时；清华源 `mirrors.tuna.tsinghua.edu.cn` 稳定可靠

### 关键认知

1. Host 侧 ext4 与 StarryOS rsext4 **完全兼容**。overlay 文件由 `debugfs -w` 直接写入 rootfs 镜像（`scripts/axbuild/src/rootfs/inject.rs` 的 `inject_overlay`），guest 内 rsext4 可正常读取；自编译产物也由 `debugfs dump` 从镜像提取回 host。早期『host 写入损坏 rootfs、rsext4 读不到』的现象经复核为误判（多为 tmpfs/RAM 混淆及 apk 写入中途取快照），并非文件系统缺陷。
2. **标准文件注入路径是 debugfs overlay 注入**，由 app runner（`cargo xtask starry app qemu -t selfhost/selfhost-full-kernel`）调用 `inject::inject_overlay` 完成——无 loopback mount、无 nspawn、无 expect。
3. **SMP=4 是当前默认配置**（`scripts/self-compile.sh` 默认 `SMP=4`）；rsext4 的 `inner` 已改用 `SleepMutex`，旧的自旋锁死锁不再适用。x86_64 自编译已端到端跑通并成功启动自编译内核。

## 当前状态

### x86_64 自编译（2026-05-26）✅ 完成

**配置**：SMP=1 + KVM + nspawn 文件注入 + 清华源 + nightly toolchain

**最终结果**：301 crates 编译成功，6m53s，产物 84MB

**已验证的里程碑**：
- ✅ Debian Trixie rootfs 制备（minbase, metadata_csum 禁用, 清华源）
- ✅ 内核启动（PCI BAR iomap 修复, init.sh touch 修复）
- ✅ Workspace 过滤 + 全量 cargo deps 缓存（383 crates, 97MB）
- ✅ rustc nightly-2026-04-27 安装（1.97.0-nightly）
- ✅ SMP=1 简单 `cat >` 测试
- ✅ USB UVC workspace 过滤修复（Bug #8: `qoi` 依赖缺失）
- ✅ nspawn 验证：cargo check 通过（1m14s）
- ✅ SMP=1 KVM QEMU 完整端到端自编译（301/301 crates, 6m53s, 87MB binary）

**待完成**：
- [x] 自编译内核启动验证 (`run-selfbuilt-kernel.sh`)——自建内核在 OVMF/UEFI 下启动至 `root@starry` 提示符
- [x] SMP=4 自编译（`inner` 改用 `SleepMutex` 后无死锁，默认 SMP=4 通过）
- [ ] aarch64 端到端验证

## TODO

- **x86_64 自编译**:
  - [x] Debian rootfs 镜像 (`prepare-selfhost-rootfs.sh --arch x86_64`)
  - [x] 内核启动（PCI BAR iomap 修复）
  - [x] Workspace filter + cargo deps 缓存
  - [x] rustc nightly 工具链
  - [x] 诊断并修复早期脚本层问题（metadata_csum + busybox grep + debugfs 注入路径），确认非 rsext4/ext4 内核缺陷
  - [x] 解决早期 SMP 文件系统锁竞争（rsext4 `inner` 改用 `SleepMutex` + PR #1057 细粒度锁，SMP=4 通过）
  - [x] USB UVC workspace 过滤修复（Bug #8）
  - [x] SMP=1 KVM 端到端自编译（301/301 crates, 6m53s, 87MB binary）
  - [x] 自编译内核启动验证 (`run-selfbuilt-kernel.sh`)——自建内核 OVMF/UEFI 启动至 `root@starry`
  - [x] SMP=4 自编译（`inner` 改用 `SleepMutex`，默认 SMP=4）
- **aarch64 自编译**: 准备 Debian rootfs，完成端到端验证
- 评估 SMP > 4 的编译加速收益（cargo 的并行度在 4-8 核后递减）
- 考虑实现多队列 LRU（MGLRU）替代当前单队列 LRU，区分活跃/非活跃页面，减少回收扫描开销
- 为脏页添加写回机制（writeback），允许安全回收脏文件页面
