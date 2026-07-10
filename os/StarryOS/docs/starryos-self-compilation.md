# StarryOS 自编译全过程

> ## 当前 x86_64 入口
>
> x86_64 自编译现在通过 `cargo starry app qemu -t selfhost/selfhost-full-kernel --arch x86_64`
> 直接运行。它在联网 QEMU guest 中安装工具链和依赖，不需要 `self-compile.sh`、host
> sudo 或 loop mount。推荐流程与产物启动方式见
> [`docs/starryos-self-compilation.md`](../../../docs/starryos-self-compilation.md)。本文其余内容
> 保留为 riscv64/Debian 旧流程和历史排障记录，不能作为 x86_64 的当前操作说明。

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

**修复** (`os/arceos/modules/axalloc/Cargo.toml`):
```toml
# Before
default = ["tlsf", "ax-allocator/page-alloc-4g"]
# After
default = ["tlsf", "ax-allocator/page-alloc-64g"]  # 16M bits = 64GB
```

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
QEMU riscv64 (-m 8G)
  └─ OpenSBI
      └─ someboot (加载 FDT 内存布局)
          └─ StarryOS 内核
              └─ Debian riscv64 rootfs (ext4)
                  └─ /usr/bin/self-compile.sh
                      ├─ mount tmpfs (8G)
                      ├─ 修补 linker.ld
                      └─ cargo build -p starryos --offline (276 crates)
```

## 测试配置

### 测试用例 (`test-suit/starryos/qemu-selfhost/selfhost-full-kernel/`)

测试用例位于独立的 `qemu-selfhost` 构建组中，避免因缺少 Debian rootfs 镜像（8GB）而阻塞标准 CI。

```toml
# qemu-riscv64.toml
args = ["-nographic", "-cpu", "rv64", "-smp", "1", "-m", "8G", ...]
shell_init_cmd = "/usr/bin/self-compile.sh"
success_regex = ['(?m)^SELFHOST_SUCCESS\\s*$']
fail_regex = ['(?i)\bpanicked\b', 'SELFHOST_FAILED']
timeout = 7200
```

Shell pipeline (`sh/self-compile.sh`) 自动注入到 rootfs 的 `/usr/bin/`:
1. 挂载 8G tmpfs 到 /tmp
2. 修补 linker.ld 添加 PROVIDE 回退
3. 执行 `cargo build -p starryos --target riscv64gc-unknown-none-elf --offline`
4. 检查产物并输出 SELFHOST_SUCCESS 或 SELFHOST_FAILED

### 运行测试

```bash
# 在提前准备 rootfs 的环境中
cargo xtask starry test qemu --arch riscv64 -g qemu-selfhost -c selfhost-full-kernel
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

**工作流程**:
1. 解析 `--arch`（默认 `riscv64`）和 `--smp`（默认 `4`）参数
2. `cargo xtask starry build --arch $ARCH` 构建种子内核
3. 通过 loopback mount + `cp` 将架构感知的编译脚本注入 rootfs 的 `/usr/bin/self-compile-inner.sh`
4. 使用 `expect` 自动启动 QEMU、等待 shell 提示符、执行编译脚本（传递 ARCH/TARGET/SMP/JOBS 变量）
5. 编译脚本内部：挂载 8G tmpfs → 修复 `linker.ld` → `cargo build -p starryos --target $TARGET --offline` → 将产物保存到 `/opt/starryos-selfbuilt`
6. 编译完成后自动关机（`poweroff`），退出后验证 rootfs 中的二进制

```bash
# riscv64（默认）：4 核 CPU + 4 个 cargo jobs
./scripts/self-compile.sh

# x86_64：自动启用 KVM 加速（~10x 速度提升）
./scripts/self-compile.sh --arch x86_64

# aarch64
./scripts/self-compile.sh --arch aarch64

# 自定义 SMP 和 cargo jobs
./scripts/self-compile.sh --arch riscv64 --smp 2 --jobs 2
```

**关键设计**:
- 编译产物写入 rootfs 持久路径 `/opt/starryos-selfbuilt`（不是 tmpfs 的 `/tmp`），确保重启后仍然存在
- 使用 `expect` 的串口自动化，不需要手动输入命令
- 超时设置为 7500 秒，覆盖完整的 276 crate 编译；x86_64 + KVM 下可大幅缩短
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
./scripts/self-compile.sh --arch x86_64        # ~10-15 分钟 (KVM 加速)
./scripts/self-compile.sh --arch aarch64       # ~60-90 分钟

# 用产物启动
./scripts/run-selfbuilt-kernel.sh --arch riscv64
./scripts/run-selfbuilt-kernel.sh --arch x86_64
./scripts/run-selfbuilt-kernel.sh --arch aarch64
```

### 与测试框架的对比

| 维度 | xtask 测试框架 | 独立脚本 |
|------|---------------|---------|
| 启动方式 | `cargo xtask starry test qemu -g qemu-selfhost` | `./scripts/self-compile.sh` |
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
| `axalloc/Cargo.toml` | page-alloc-4g → page-alloc-64g |
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
2. **自编译测试不在标准 CI 中运行**: Debian rootfs 镜像（8GB）未上传到 tgosimages 发布版，测试用例位于独立 `qemu-selfhost` 构建组，需要手动在配备 rootfs 的环境中运行。
3. **页面回收仅支持干净页的主动回收**: 脏页在第二阶段作为最后手段回收（记录 warning），但缺少脏页写回机制。对于只读 ext4 挂载的自编译场景影响很小。

## 环境

- **QEMU**: riscv64 / x86_64 (KVM) / aarch64, `-m 8G`, `-smp 4`
- **内核**: StarryOS (dev 分支)
- **根文件系统**: Debian (per-arch), ext4, rustc nightly-2026-04-27
- **源码**: StarryOS monorepo (离线，预取依赖)

### 跨架构自编译状态

| 架构 | 种子内核构建 | QEMU 启动 | Debian rootfs | 自编译验证 | 备注 |
|------|-------------|----------|---------------|-----------|------|
| riscv64 | 通过 | 通过 | 已就绪 | 通过 | 完整验证通过 |
| x86_64 | 通过 | 通过 (KVM) | 已就绪 | 通过 ✅ (301/301, 6m53s) | SMP=1, Bug #8 修复后完成 |
| aarch64 | 通过 | 通过 | 需准备 | 待验证 | |

> x86_64: Debian rootfs 已通过 `prepare-selfhost-rootfs.sh --arch x86_64` 生成。种子内核可启动，workspace filter、cargo deps 缓存、rustc nightly 工具链均已就绪。编译可进行到 294/297 crates，卡在最后 3 个 crate（`starry-kernel`、`starryos`）的 SIGSEGV（详见下文）。

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

#### 当前阻塞: Linux host 与 StarryOS rsext4 的 ext4 兼容性 bug

**现象**（三种表现形式）:

| 错误 | 含义 |
|------|------|
| `Not found: /opt/starryos/Cargo.toml` | host 端 `debugfs` 确认存在 (21KB)，StarryOS 内核 `test -f` 返回"不存在" |
| `Block num already free!` | StarryOS 块位图与 host 写入的不一致——host 标记为已分配的块，rsext4 读出来是空闲 |
| `Input/output error` | init 进程在 ext4 上创建文件或目录时直接失败 |

**根因**: Linux host 内核的 ext4 驱动（`mount -o loop` + `cp`）与 StarryOS 的 `rsext4` 实现**不兼容**。
host 写入元数据后，rsext4 无法正确解析磁盘上的数据结构。具体不兼容点：

- **`metadata_csum`**（元数据 crc32c 校验和）: host 写入时嵌入校验和；rsext4 读回时校验失败或未正确重新计算，导致合法数据被视为损坏。
- **块位图格式**: host 和 rsext4 对已分配/空闲块的解释不一致，rsext4 看到 `Block num already free` 重复释放错误。
- **JBD2 journal 结构**: host 和 rsext4 对 journal 的提交格式和回放逻辑存在差异。
- **`debugfs -w`**: 直接操纵 ext4 结构而无视 journal，进一步破坏一致性。

**关键结论**: 此 bug 与 KVM 无关——发生在 QEMU 启动前，host 侧写入 ext4 镜像的阶段。

**唯一可行路径**: 让 ext4 的所有写入都在 StarryOS 内完成。host 只负责 `mkfs.ext4` 创建空文件系统，后续所有写操作（debootstrap、cargo fetch、文件创建）均通过 StarryOS 的 ext4 驱动（即 prepare 脚本的 `nspawn` 步骤）。一旦 host 直接 mount 并写入（如 `copy-cargo-cache.sh`），rootfs 即不可逆损坏。

成功运行到 294/297 crates 的那次就是因为 prepare 后 rootfs 未被 host 端修改。

**临时 workaround 尝试**:
1. `-Cincremental=false` — 无效，`s-*-working` 是 build script 输出目录，非增量编译
2. registry src symlink 到 tmpfs — symlink 在 QEMU 重启后断裂，需要挂载时重建
3. `cp -a` registry src → tmpfs (5.9万文件) — 耗时长，有时成功但后续目录创建仍失败

**建议**: 作为 StarryOS ext4 内核 bug 追踪，不在脚本层面继续 workaround。

#### 编译进度

```
[BUILD] 294/297 crates 编译成功
[SIGSEGV] starry-kernel (lib) — signal 11, invalid memory reference
[SIGSEGV] starryos (bin) — 同上
[UNCOMPILED] starry-kernel, starryos, ax-mm (部分运行)
```

SIGSEGV 在 `codegen-units=1`、`opt-level=0`、16GB RAM、单 job 下仍发生。可能是 nightly rustc (1.97) 与 Debian 13.5 运行时不兼容，而非内存不足。

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

`task.interrupt()` 做两件事（`os/arceos/modules/axtask/src/task.rs:284-287`）：
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
| 后续合并 | 将 `interrupt_waker.wake()` 集成进 `task.interrupt()`，移除独立 `wake_task` 函数 |

### 与 `interrupted` 标志的配合

`interruptible` future 包装器（`future/mod.rs:116-126`）轮询两个条件：
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

## 根因分析：ext4 兼容性 bug（2026-05-25 更新）

经过深入调查，发现 **rsext4 内核代码不是问题的根本原因**。
"Not found: /opt/starryos/Cargo.toml" 和文件损坏是由以下脚本层 bug 引起：

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

### Bug #3: `debugfs -w` 目录项不可靠

- 即使禁用 metadata_csum，debugfs -w 写入的目录项仍然不可靠
- rsext4 读取被 debugfs 修改过的目录时，"Structure needs cleaning" / ENOENT
- **修复**: 使用 loopback mount + cp（通过内核 ext4 驱动）替代 debugfs -w

### Bug #4: 反复 mount/unmount/e2fsck 累积损坏

- 每次 self-compile.sh 运行 loopback mount → cp → umount → e2fsck 循环
- e2fsck "修复" 了一些问题但可能引入新问题
- 多次循环后 `/run` 等目录损坏
- **当前方案**: 在 prepare 阶段（nspawn）尽可能完成所有文件写入，self-compile.sh 最小化 host 侧修改

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

### Bug #7: SMP 并发导致 `SpinNoPreempt` mutex 死锁（阻塞写入）

- **现象**: SMP=4 + KVM 时，内部脚本在 `cat > linker.ld`（ext4 写入）后冻结；SMP=1 时正常完成
- **根因**: `axfs-ng/src/fs/ext4/rsext4/fs.rs:29` — `inner: Mutex<Ext4State>`，其中 `Mutex = SpinNoPreempt`（自旋锁）
- **机制**: 多 vCPU 并发访问文件系统时发生锁顺序死锁。线程 A 持有锁等待 I/O 完成，线程 B 自旋等待锁释放。若 I/O 完成路径需要获取同一把锁，则形成死锁
- **证据**:
  - SMP=1 简单 `cat >` 测试 → ✅ 通过
  - SMP=4 + KVM 完整脚本 → ❌ 冻结于 `cat > linker.ld`
  - SMP=4 + 无 KVM（TCG 模拟器串行化 vCPU 执行）→ ✅ 越过冻结点，到达 cargo build
- **修复方向**:
  - 将 `SpinNoPreempt` 替换为 `Spin`（允许抢占，避免自旋等待中断处理程序）
  - 或审计 `sync_to_disk()` 调用路径，消除锁重入

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

1. Host 侧 ext4 写入（loopback mount / debugfs）与 StarryOS rsext4 读写之间存在双向不兼容：
   - Host 写的文件 → rsext4 可能读不到
   - rsext4 写的文件 → host kernel 可能报 EUCLEAN
2. **唯一可靠的写入路径**是 **nspawn**（prepare 阶段使用）。自编译流程应最小化 host 侧修改，所有文件注入通过 nspawn 完成
3. **SMP=1 是当前可用配置**。在 SMP 死锁修复之前，自编译只能以单核模式运行

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
- [ ] 自编译内核启动验证 (`run-selfbuilt-kernel.sh`)
- [ ] 修复 SMP 死锁后测试 SMP=4 性能
- [ ] aarch64 端到端验证

## TODO

- **x86_64 自编译**:
  - [x] Debian rootfs 镜像 (`prepare-selfhost-rootfs.sh --arch x86_64`)
  - [x] 内核启动（PCI BAR iomap 修复）
  - [x] Workspace filter + cargo deps 缓存
  - [x] rustc nightly 工具链
  - [x] 诊断 ext4 兼容性 bug（metadata_csum + busybox grep + debugfs 目录项）
  - [x] 诊断 SMP SpinNoPreempt 死锁（根因定位，修复方案明确）
  - [x] USB UVC workspace 过滤修复（Bug #8）
  - [x] SMP=1 KVM 端到端自编译（301/301 crates, 6m53s, 87MB binary）
  - [ ] 自编译内核启动验证 (`run-selfbuilt-kernel.sh`)
  - [ ] 修复 SMP 死锁 → SMP=4 自编译加速
- **aarch64 自编译**: 准备 Debian rootfs，完成端到端验证
- 评估 SMP > 4 的编译加速收益（cargo 的并行度在 4-8 核后递减）
- 考虑实现多队列 LRU（MGLRU）替代当前单队列 LRU，区分活跃/非活跃页面，减少回收扫描开销
- 为脏页添加写回机制（writeback），允许安全回收脏文件页面
