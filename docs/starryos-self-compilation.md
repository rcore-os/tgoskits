# StarryOS 自编译：问题与解决方案

在 StarryOS 内部使用 cargo 编译 StarryOS 自身——支持 riscv64 和 x86_64 架构。

## 概览

### 跨架构自编译状态

| 架构 | 种子内核 | QEMU 启动 | rootfs | 自编译 | 耗时 | 备注 |
|------|---------|----------|--------|--------|------|------|
| riscv64 | ✅ | ✅ | 已就绪 | ✅ | ~100 min | TCG 模拟，SMP=1；种子/guest 均为静态平台 bare-metal，构建流程一致 |
| x86_64 | ✅ | ✅ (KVM) | 已就绪 | ✅ | ~10 min | KVM + SMP=4, 448 crates; guest 构建走 xtask musl-PIE std 流程 (plat-dyn) |

> **x86_64 自编译已通过运行时验证（2026-06）**: 完整闭环已验证——guest 驱动 `cargo xtask starry build`（经 `tg-xtask` + musl-PIE std 流程 + `-Zbuild-std` + linker wrapper + `-u _head`），编译全部 448 crate + 链接 + kallsyms（10737 符号） + objcopy，产出 16 MB ELF。产物经 OVMF/UEFI 启动到 StarryOS shell 提示符。详细修复路径见下方 §x86_64 自编译构建流程不匹配（已修复）。riscv64 不受影响（构建流程一致）。

### 测试链路

```
Host (Linux)
  └─ scripts/self-compile.sh --arch <arch>
      ├─ cargo xtask starry build        (种子内核)
      ├─ cp rootfs → 临时工作副本        (蓝本永不污染)
      ├─ apps/starry/selfhost/selfhost-full-kernel/prebuild.sh  (生成所有 overlay 文件)
      ├─ debugfs -w → rm + write         (幂等 overlay 注入)
      └─ cargo xtask starry app qemu -t selfhost/selfhost-full-kernel (16G, OVMF UEFI)
          └─ StarryOS 内核
              └─ selfhost rootfs (ext4)
                  └─ /usr/bin/self-compile-inner.sh
                      ├─ filter-workspace.sh (架构过滤)
                      └─ cargo build --ignore-rust-version -p starryos --offline
                              │
                              ▼ 成功
                      debugfs dump → tmp/starryos-selfbuilt-<arch>
```

## x86_64 自编译构建流程不匹配（已修复）

**现象**: guest 内自编译在编译完整个 workspace（425/426 crate）后，最终链接 `starryos`
二进制失败：

```
rust-lld: error: someboot.x:9 ENTRY(_head); :109 ABSOLUTE(kernel_entry)
  -> symbol not found: _head / kernel_entry
```

**根因**: 种子内核构建与 guest 自编译走了**两套不同的构建流程**。

- x86_64 的 `plat_dyn` 默认为 `true`（`build-x86_64-unknown-none.toml` 未声明该字段，
  `resolve_effective_plat_dyn` 对 `x86_64-*` 返回 `true`）。因此**种子构建**经 axbuild
  改写为 ArceOS std/PIE 流程：有效目标是 `scripts/targets/std/pie/x86_64-unknown-linux-musl.json`，
  `-Zbuild-std`，并通过**自定义链接器 wrapper**（`std_build.rs`）将所有 rlib 包进单个
  `--start-group/--end-group`、强制 `-pie` 与 `-u _head`。`_head`/`kernel_entry` 仅被
  `someboot.x` 的 `ENTRY`/`ABSOLUTE` 引用、无 `#[used]`、无 Rust 调用方，**只有 `-u _head`
  这类命令行 undefined 才能强制抽取 someboot 归档成员**。
- 而 **guest 自编译**（`selfhost-full-kernel/prebuild.sh` 生成的 inner script）手写了一个
  `cargo build -p starryos --target x86_64-unknown-none`（裸 bare-metal 目标，plain rust-lld，
  无 wrapper、无 archive grouping、无 `-u _head`，且用 glibc gcc symlink 为 musl-gcc）。
  这套简化构建避开了 musl 工具链 / `-Zbuild-std`，但也因此**无法抽取 someboot 的
  `_head`/`kernel_entry`，最终链接失败**。

riscv64 不受影响：其 `build-riscv64gc-unknown-none-elf.toml` 显式 `plat_dyn = false` + 静态
平台，种子与 guest 均为 bare-metal `riscv64gc-unknown-none-elf`，**两套流程一致**，故链接通过。

**修复方式**（已实现并端到端验证）：

1. inner script 改为调用种子流程：先行构建 `tg-xtask`（gnu host triple），再通过
   `CFLAGS=-fno-stack-protector cargo xtask starry build -c apps/starry/selfhost/build-x86_64-unknown-none.toml --arch x86_64`
   驱动 musl-PIE std 流程，产物从 `target/x86_64-unknown-linux-musl/release/starryos` 拷贝到
   `/opt/starryos-selfbuilt`。`SELF_COMPILE_SUCCESS` 在 ELF 检测后立即发出。
2. `prepare-selfhost-rootfs.sh` 已补充：`musl-tools`/`musl-dev`（+ `x86_64-linux-musl-{cc,gcc,ar}` symlinks）、
   `llvm-tools-preview` + `cargo-binutils` + `ksym`（kallsyms 工具）、AIC8800 firmware blobs、
   `pkgconf libudev-dev`、`/bin/sh → /bin/bash`（linker wrapper 兼容）及完整源码与离线依赖闭包。
   该脚本作为维护者工具；reviewer 验证路径使用制备好的 blueprint image。

## 前置依赖 PR

| PR | 内容 | 关联 |
|----|------|------|
| #797 | 信号传递修复：`interrupt_waker.wake()` 唤醒被 `future_blocked_resched` 移出运行队列的任务 | 无此修复，cargo 子进程（build script）挂起，父进程 waitpid 永远阻塞 |
| #1007 | 页回收：内存压力下驱逐干净文件支持页面，`try_page_reclaim()` 最多重试 4 次 | 无此修复，编译 `syn` 时 OOM panic（大量源码/产物占满文件缓存） |
| #971 | rsext4 clock LRU 缓存（4 入口/16 KiB），减少 virtio 块设备 round-trip | 加速离线 registry 读取，将依赖解析从分钟级降到秒级 |

## 共通阻塞点（riscv64 + x86_64）

### 1. 内存检测仅识别 512MB

**现象**: QEMU `-m 8G` 但内核只识别 ~510MB。

**根因**: 早期已淘汰的平台配置路径在 `axconfig.toml` 中硬编码 `phys-memory-size = 0x2000_0000`。

**修复**: 当时改为 `phys-memory-size = "0x2_0000_0000"` (8GB)。当前构建链固定使用 `axplat-dyn` + `somehal::mem::memory_map()` 动态检测，无此问题。

**注**: PR #987 重构了 ax-alloc，移除了旧 bitmap 页分配器（及 `page-alloc-*` 特性），改用 TLSF/buddy-slab。TLSF 无硬编码容量限制，不再需要 `page-alloc-64g` passthrough。

### 2. TMPFS 挂载失败（已移除）

**现象**: `mount -t tmpfs -o size=8G tmpfs /tmp` 失败。

**根因**: mount(8) 优先使用新版 mount API（`fsopen`/`fsconfig`/`fsmount`）。StarryOS 将 `fsopen` 实现为 `sys_dummy_fd`（返回伪 fd），mount(8) 误以为挂载成功但后续操作失败，不会回退到传统 `mount(2)`。

**修复**: 将 `fsopen`/`fspick`/`open_tree` 返回 `ENOSYS`，mount(8) 收到后回退到传统 `mount(2)` 调用。

```rust
Sysno::fsopen | Sysno::fspick | Sysno::open_tree => Err(AxError::Unsupported),
```

**状态**: 自编译脚本中原先尝试挂载 tmpfs（registry 1500M + workspace 100M）作为 ext4 缓存一致性 workaround，但两个挂载始终失败（tmpfs 在 StarryOS 中不可用）。tmpfs 挂载代码已移除——缓存一致性问题的真正根因已通过 journal coherence 修复解决（见 §8 bug #7）。

### 3. 链接器 `_ex_table_end` 未定义

**现象**: 所有 crate 编译通过，但最终链接失败: `undefined symbol: _ex_table_end`。

**根因**: 自编译环境中 `.cargo/config.toml` 未传递 `-Tlinker.x`。`ext_linker.ld` 使用 `INSERT AFTER .data;` 期望 `linker.x` 先定义 `.data` 段（含 `_ex_table_end`），但缺少 linker.x 时符号未定义。

**修复** (`os/StarryOS/starryos/ext_linker.ld`):
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
| #7 | cargo build ENOENT（`log`/`cfg-if` 编译失败） | `Jbd2Dev::read_blocks()` 未检查 journal commit_queue，inode table 的 read-modify-write 基于磁盘 stale 数据构建，导致同一 block 的前一个 inode 修改（如 `i_mode`）被静默丢弃 | `read_blocks()` 逐 block 检查 journal commit_queue，匹配 `read_block_direct` 行为；同时移除 `-Cincremental=false`（该 flag 类型为 `Option<Path>`，"false" 被解析为字面目录名） |

### 9. SMP 死锁（已修复）

**现象**: SMP=4 + KVM 时，ext4 写入操作后系统冻结；SMP=1 正常完成。

**根因**: VFS 锁使用 `SpinNoIrq`（禁用中断），I/O 期间定时器/IPI/virtio 完成通知被阻塞。与缓存锁（`SpinNoPreempt`）不一致。

**修复**:
1. VFS 锁从 `SpinNoIrq` 改为 `SpinNoPreempt`，与三个缓存锁一致，I/O 期间中断保持开启
2. 三个缓存层（DataBlockCache/InodeCache/BitmapCache）的 `evict()`/`flush()`/`flush_all()` 均已在 I/O 前释放 spinlock（lock-drop-during-I/O 模式）

**当前状态**: SMP=4 正常工作。

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

### 13. 动态 RAM 检测失败（早期静态平台路径，已淘汰）

**现象**: 早期静态平台路径无法通过 someboot 传递实际 FDT 内存大小，导致 RAM 大小只能来自固定配置。

**根因**: someboot（MMU 关闭阶段）写入共享内存的地址，在 StarryOS（MMU 开启阶段）无法直接访问——地址空间不一致。

**当前方案**: 当前构建链固定走 `axplat-dyn`，由 `somehal::mem::memory_map()` 提供运行时内存图，不再依赖静态平台 axconfig 或 `.axconfig.toml`。

## 脚本编排

| 脚本 | 功能 | 运行环境 |
|------|------|---------|
| `scripts/prepare-selfhost-rootfs.sh` | 维护者工具：创建 selfhost Debian rootfs blueprint（debootstrap + tous 前置 + 离线依赖闭包）。需 sudo + systemd-nspawn；非 reviewer/CI 验证路径 | Host (maintainer, sudo) |
| `scripts/self-compile.sh` | 构建种子内核 → 注入 overlay → QEMU app runner → 验证产物 | Host |
| `scripts/run-selfbuilt-kernel.sh` | 提取并启动自编译的内核 | Host |
| `scripts/filter-workspace.sh` | 从 Cargo.toml 移除架构不兼容的 workspace members | Host + Guest |

### 使用流程

```bash
# 1. 准备 rootfs blueprint（维护者一次性操作，通过 prepare-selfhost-rootfs.sh 或后续
#    QEMU Debian bootstrap 制备；制备后置于 tmp/axbuild/rootfs/rootfs-x86_64-selfhost.img）
#
#    self-compile.sh 每次运行从 blueprint 克隆临时工作副本，不会污染 blueprint。

# 2. 自编译（产物自动缓存到 tmp/starryos-selfbuilt-<arch>）
./scripts/self-compile.sh --arch x86_64 --smp 4

# 3. 启动自编译内核（默认使用缓存，也可 --kernel 指定路径）
./scripts/run-selfbuilt-kernel.sh --arch x86_64
# 或显式指定内核：
./scripts/run-selfbuilt-kernel.sh --arch x86_64 --kernel tmp/starryos-selfbuilt-x86_64
```

## 测试配置

自编译入口脚本位于 `scripts/self-compile.sh`，app 模板配置位于 `apps/starry/selfhost/`，**不在** `test-suit/` 下，不参与标准 CI。

```
scripts/
├── self-compile.sh          # 主入口：xtask app runner → QEMU → cargo build → debugfs 提取
├── prepare-selfhost-rootfs.sh  # 准备包含编译依赖的 Debian rootfs 镜像（需要 sudo）
├── run-selfbuilt-kernel.sh     # 提取并启动自编译内核（OVMF UEFI for x86_64）
└── filter-workspace.sh      # 架构过滤：从 Cargo.toml 移除不兼容的 workspace members

apps/starry/selfhost/
├── build-x86_64-unknown-none.toml   # x86_64 bare-metal build config（axplat-dyn/efi）
├── selfhost-bootstrap/
│   ├── prebuild.sh                  # 生成 bootstrap overlay（Alpine base → selfhost rootfs）
│   └── qemu-x86_64.toml             # QEMU 配置（4G, apk + rustup, shell_init_cmd）
└── selfhost-full-kernel/
    ├── prebuild.sh                  # 生成所有 overlay（inner script, linker.x, axconfig）
    └── qemu-x86_64.toml             # QEMU 配置（16G, cache=writeback, OVMF UEFI）
```

**CI 不运行的原因**: selfhost rootfs 镜像制备后约 8-12GB（含 rustup nightly 工具链 ~6.9GB、预缓存 crate、系统包），未上传到 tgosimages release，CI 容器无法下载。

**手动运行**:
```bash
# Blueprint: 将 selfhost rootfs 置于 tmp/axbuild/rootfs/rootfs-x86_64-selfhost.img。
# 维护者通过 prepare-selfhost-rootfs.sh 制备（需 sudo、debootstrap、systemd-nspawn；
# 后续计划提供 QEMU Debian bootstrap 或可下载/可校验的预制 blueprint）。

# 运行自编译（x86_64 KVM, SMP=4）
./scripts/self-compile.sh --arch x86_64 --smp 4

# 指定特定 commit 和 ref（用于产物溯源的 .expected-commit 文件）
./scripts/self-compile.sh --arch x86_64 --commit $(git rev-parse HEAD) --ref dev

# 指定 cargo 并行编译 job 数（默认与 --smp 相同）
./scripts/self-compile.sh --arch x86_64 --smp 4 --jobs 8
```

## 已知限制

1. **`phys-memory-size` 硬编码 8GB (riscv64)**: 动态 RAM 检测因启动阶段地址空间不一致无法实现。x86_64 使用 axplat-dyn + somehal 动态检测，无需硬编码。
2. **自编译测试不在标准 CI 中运行**: 需要 selfhost rootfs 镜像（制备后 ~8-12GB），CI 环境无法承载，仅支持本地手动测试。
3. **QEMU 内存配置**: riscv64 使用 `-m 8G`，x86_64 使用 `-m 16G`（qemu-x86_64.toml 中配置）。
4. **aarch64 引导已验证**: rootfs 准备 + 种子内核引导 + shell 可用均通过，完整编译因 TCG 模拟性能限制（预计 4-8h）未运行。需动态平台默认配置 + PIE 目标（`--config test-suit/starryos/qemu-smp1/build-aarch64-unknown-none-softfloat.toml`）。
5. **页面回收仅支持干净页**: 脏页在极端压力下作为最后手段回收（记录 warning），缺少脏页写回机制。

## 环境要求

- **QEMU**: riscv64 (TCG) / x86_64 (KVM) / aarch64 (TCG)，内存按 arch 配置（riscv64: 8G, x86_64: 16G）
- **内核**: StarryOS (dev 分支)
- **根文件系统**: Debian 或 Alpine (per-arch), ext4, rustc nightly-2026-04-27
- **Host 依赖**: `qemu-system-*`, `debugfs`（来自 e2fsprogs），x86_64 额外需要 `objcopy`（binutils）和 OVMF firmware（edk2-ovmf）。`self-compile.sh` 与 `run-selfbuilt-kernel.sh` 无需 sudo；rootfs blueprint 制备由维护者通过 `prepare-selfhost-rootfs.sh` 完成（该脚本需 sudo 和 systemd-nspawn）。
- **源码**: StarryOS monorepo (离线，预取依赖)
- **SMP**: 已验证 SMP=4 正常（VFS 锁 `SpinNoIrq` → `SpinNoPreempt` + journal coherence 修复）
