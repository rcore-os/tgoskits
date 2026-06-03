# StarryOS 自编译：问题与解决方案

在 StarryOS 内部使用 cargo 编译 StarryOS 自身——支持 riscv64 和 x86_64 架构。

## 概览

### 跨架构自编译状态

| 架构 | 种子内核 | QEMU 启动 | rootfs | 自编译 | 耗时 | 备注 |
|------|---------|----------|--------|--------|------|------|
| riscv64 | ✅ | ✅ | 已就绪 | ✅ | ~100 min | TCG 模拟，SMP=1, 12GB RAM |
| x86_64 | ✅ | ✅ (KVM) | 已就绪 | ✅ | 6m53s | KVM + SMP=1, 12GB RAM, 301 crates |

### 测试链路

```
Host (Linux)
  └─ scripts/self-compile.sh --arch <arch>
      ├─ cargo xtask starry build        (种子内核)
      ├─ loopback mount → inject files   (脚本/配置注入)
      └─ expect + QEMU (-m 12G)
          └─ StarryOS 内核
              └─ Debian rootfs (ext4)
                  └─ /usr/bin/self-compile-inner.sh
                      ├─ mount tmpfs (12G)
                      ├─ filter-workspace.sh (架构过滤)
                      └─ cargo build -p starryos --offline
```

## 前置依赖 PR

| PR | 内容 | 关联 |
|----|------|------|
| #797 | 信号传递修复：`interrupt_waker.wake()` 唤醒被 `future_blocked_resched` 移出运行队列的任务 | 无此修复，cargo 子进程（build script）挂起，父进程 waitpid 永远阻塞 |
| #1007 | 页回收：内存压力下驱逐干净文件支持页面，`try_page_reclaim()` 最多重试 4 次 | 无此修复，编译 `syn` 时 OOM panic（大量源码/产物占满文件缓存） |
| #971 | rsext4 clock LRU 缓存（4 入口/16 KiB），减少 virtio 块设备 round-trip | 加速离线 registry 读取，将依赖解析从分钟级降到秒级 |

## 共通阻塞点（riscv64 + x86_64）

### 1. 内存检测仅识别 512MB

**现象**: QEMU `-m 12G` 但内核只识别 ~510MB。

**根因**: `axplat-riscv64-qemu-virt` 的 `axconfig.toml` 硬编码 `phys-memory-size = 0x2000_0000`。

**修复**: 改为 `phys-memory-size = "0x2_0000_0000"` (8GB)。x86_64 使用 `axplat-dyn` + `somehal::mem::memory_map()` 动态检测，无此问题。**注**: 实际测试中 8GB 存在 OOM 风险，建议使用 `0x3_0000_0000` (12GB)。

**注**: PR #987 重构了 ax-alloc，移除了旧 bitmap 页分配器（及 `page-alloc-*` 特性），改用 TLSF/buddy-slab。TLSF 无硬编码容量限制，不再需要 `page-alloc-64g` passthrough。

### 2. TMPFS 挂载失败

**现象**: `mount -t tmpfs -o size=12G tmpfs /tmp` 失败。

**根因**: mount(8) 优先使用新版 mount API（`fsopen`/`fsconfig`/`fsmount`）。StarryOS 将 `fsopen` 实现为 `sys_dummy_fd`（返回伪 fd），mount(8) 误以为挂载成功但后续操作失败，不会回退到传统 `mount(2)`。

**修复**: 将 `fsopen`/`fspick`/`open_tree` 返回 `ENOSYS`，mount(8) 收到后回退到传统 `mount(2)` 调用。

```rust
Sysno::fsopen | Sysno::fspick | Sysno::open_tree => Err(AxError::Unsupported),
```

### 3. 链接器 `_ex_table_end` 未定义

**现象**: 所有 crate 编译通过，但最终链接失败: `undefined symbol: _ex_table_end`。

**根因**: 自编译环境中 `.cargo/config.toml` 未传递 `-Tlinker.x`。`linker.ld` 使用 `INSERT AFTER .data;` 期望 `linker.x` 先定义 `.data` 段（含 `_ex_table_end`），但缺少 linker.x 时符号未定义。

**修复** (`os/StarryOS/starryos/linker.ld`):
```ld
PROVIDE(_ex_table_start = 0);
PROVIDE(_ex_table_end = 0);

SECTIONS { /* 原有内容 */ }
INSERT AFTER .data;
```

### 4. 测试正则误匹配

**现象**: 编译 crate `axpanic` 时 cargo 输出 `panic v0.1.0`，触发 fail_regex。

**修复**: `\bpanic` → `\bpanicked\b`（仅匹配内核 panic 消息）。

### 5. Workspace 架构过滤

**现象**: `cargo build --offline` 解析失败——workspace 包含其他架构的 crate（如 `arm_vcpu`、`loongarch_vcpu`）。

**根因**: 这些 crate 依赖当前目标架构不可用的平台库（如 `aarch64-cpu`），在 `--offline` 模式下无法解析。

**修复**: `scripts/filter-workspace.sh` — 基于目标架构从 `Cargo.toml` 的 workspace members 中精确移除不兼容的行：

```bash
filter-workspace.sh x86_64 Cargo.toml
# 移除: arm_vcpu, arm_vgic, aarch64_sysreg, kasm-aarch64, riscv_*, loongarch_vcpu 等
# 保留: x86_vcpu, x86_vlapic 及所有公共 crate
```

## x86_64 专有阻塞点

### 6. PCI BAR 64位地址导致 Page Fault

**现象**: 内核启动时 `#PF` panic——64-bit PCI BAR（28GB+ 地址）未映射到页表。

**根因**: QEMU q35 机型的 PCI 设备 BAR 可分配在 4GB 以上地址空间，但页表未建立相应映射。

**修复**: 驱动初始化调用 `ax_mm::iomap()` 动态映射 BAR 物理地址到虚拟地址空间。

### 7. QEMU 镜像文件排他锁

**现象**: QEMU 启动时报文件锁冲突。

**修复**: `-drive id=disk0,if=none,format=raw,file=$IMG,file.locking=off`

### 8. ext4 兼容性 Bug 系列

Linux host 内核 ext4 驱动与 StarryOS rsext4 之间存在多个不兼容点：

| Bug | 现象 | 根因 | 修复 |
|-----|------|------|------|
| #1 | mount 后 checksum 失败 | `metadata_csum` 被 `debugfs -w` 破坏 | `mkfs.ext4 -O ^metadata_csum,^metadata_csum_seed` |
| #2 | Cargo.toml 被截断为 0 字节 | busybox grep 不支持 `[[:space:]]` | 用 `[ ]` 替代 + `[ -s ]` 安全检查 |
| #3 | 目录项读取 ENOENT | `debugfs -w` 写入目录项不可靠 | 使用 loopback mount + `cp` 替代 debugfs |
| #4 | 反复 mount/e2fsck 累积损坏 | 多次循环后目录结构不一致 | prepare 阶段（nspawn）完成所有写入，minimize host 修改 |
| #5 | `--offline` 缺少 crate | Cargo.lock 引用所有平台依赖 | 全量 `cargo fetch`（无 `--target` 过滤） |
| #6 | init 进程退出，QEMU 终止 | POSIX shell 重定向失败导致 shell 退出 | `: > file` → `touch file 2>/dev/null \|\| true` |

### 9. SMP 死锁

**现象**: SMP=4 + KVM 时，ext4 写入操作后系统冻结；SMP=1 正常完成。

**根因**: `axfs-ng` ext4 状态使用 `SpinNoPreempt` mutex。多 vCPU 并发访问时发生锁顺序死锁：线程 A 持锁等 I/O，线程 B 自旋等锁。

**当前解决方案**: SMP=1 + KVM（性能足够：6m53s 编译 301 crates）。

**未来方向**: 将 `SpinNoPreempt` 替换为 `Spin`（允许抢占），或审计 `sync_to_disk()` 消除锁重入。

### 10. rustc 版本不满足 MSRV

**现象**: Debian 系统 rustc (1.85) 无法编译要求 nightly 特性的代码。

**修复**: 在 rootfs 准备阶段通过 `rustup` 安装 nightly-2026-04-27 工具链（~6.9GB）。

### 11. USB UVC 未供应商化依赖

**现象**: `cargo build --offline` 报 `no matching package named 'qoi' found`。

**根因**: `drivers/usb/usb-device/uvc` (crab-uvc) 的 dev-dependency 引用了未缓存的 `qoi` crate。

**修复**: `filter-workspace.sh` 中移除 `drivers/usb/usb-device/uvc` member 行。该驱动不参与内核编译。

## riscv64 专有阻塞点

### 12. Bitmap 容量溢出（已淘汰）

**现象**: 8GB RAM 下 panic: `need 3145728 pages but CAP is 1048576`。

**根因**: 旧 `page-alloc-4g` 使用 `BitAlloc1M`（1M bits = 4GB 最大容量）。

**状态**: PR #987 移除了整个 bitmap 分配器，改用 TLSF/buddy-slab。TLSF 无硬编码容量限制，**此问题已不存在**。

### 13. 动态 RAM 检测失败

**现象**: 无法通过 someboot 传递实际 FDT 内存大小给静态平台。

**根因**: someboot（MMU 关闭阶段）写入共享内存的地址，在 StarryOS（MMU 开启阶段）无法直接访问——地址空间不一致。

**当前方案**: 硬编码 `phys-memory-size = 0x2_0000_0000` 为实用方案。

## 脚本编排

| 脚本 | 功能 | 运行环境 |
|------|------|---------|
| `scripts/prepare-selfhost-rootfs.sh` | 创建 Debian rootfs（debootstrap + rustup + cargo fetch + 预解压 .crate） | Host (sudo) |
| `scripts/self-compile.sh` | 构建种子内核 → 注入文件 → QEMU expect 自动化 → 验证产物 | Host |
| `scripts/run-selfbuilt-kernel.sh` | 提取并启动自编译的内核 | Host |
| `scripts/filter-workspace.sh` | 从 Cargo.toml 移除架构不兼容的 workspace members | Host + Guest |

### 使用流程

```bash
# 1. 准备 rootfs（首次，每架构一次）
sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64

# 2. 自编译
./scripts/self-compile.sh --arch x86_64 --smp 4

# 3. 启动自编译内核
./scripts/run-selfbuilt-kernel.sh --arch x86_64
```

## 测试配置

测试用例位于 `apps/starry/selfhost/`，通过 Starry app 系统运行。

```
apps/starry/selfhost/
├── build-riscv64gc-unknown-none-elf.toml      # 构建配置
├── selfhost-full-kernel/                      # 完整编译测试（timeout=7200s）
│   ├── qemu-riscv64.toml
│   └── sh/self-compile.sh                     # Guest 内执行的编译脚本
└── test-selfhost-check/                       # 快速工具检查（timeout=120s）
    └── qemu-riscv64.toml
```

**CI 不运行的原因**: Debian rootfs 镜像（~8-12GB）未上传到 tgosimages release，CI 容器无法下载。

**手动运行**:
```bash
# 完整自编译
cargo xtask starry app qemu --arch riscv64 --app-case selfhost/selfhost-full-kernel

# 快速工具检查
cargo xtask starry app qemu --arch riscv64 --app-case selfhost/test-selfhost-check
```

## 已知限制

1. **`phys-memory-size` 硬编码**: 动态 RAM 检测因启动阶段地址空间不一致无法实现。自编译需要 `-m 12G` + `axconfig_overrides = ["plat.phys-memory-size=0x3_0000_0000"]`。8GB 在实测中出现 OOM，不建议使用。
2. **自编译测试不在标准 CI 中运行**: 需要 8-12GB rootfs 镜像，仅支持本地手动测试。
3. **SMP > 1 未验证**: ext4 `SpinNoPreempt` 死锁 workaround 为 SMP=1。x86_64 通过 KVM 加速弥补单核性能。
4. **aarch64 引导已验证**: rootfs 准备 + 种子内核引导 + shell 可用均通过，完整编译因 TCG 模拟性能限制（预计 4-8h）未运行。需 `plat_dyn=true` + PIE 目标（`--config test-suit/starryos/normal/qemu-smp1/build-aarch64-unknown-none-softfloat.toml`）。
5. **页面回收仅支持干净页**: 脏页在极端压力下作为最后手段回收（记录 warning），缺少脏页写回机制。

## 环境要求

- **QEMU**: riscv64 (TCG) / x86_64 (KVM) / aarch64 (TCG), `-m 12G`
- **内核**: StarryOS (dev 分支)
- **根文件系统**: Debian (per-arch), ext4, rustc nightly-2026-04-27
- **Host 依赖**: `qemu-system-*`, `expect`, `sudo`（免密）, `systemd-nspawn`
- **源码**: StarryOS monorepo (离线，预取依赖)
