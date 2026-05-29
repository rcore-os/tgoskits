# StarryOS 自编译全过程

在 riscv64 Debian Linux 上运行 StarryOS，并在 StarryOS 内部使用 cargo 编译 StarryOS 自身。

## 前置依赖 PR

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

### PR #1007 — 页回收

**现象**: 编译 `syn`、`proc-macro2` 等大型 crate 时 OOM panic。

**根因**: 8GB 物理内存下，cargo build 会产生大量文件缓存页面（源码、中间产物）。当可用内存不足时，没有机制回收不再使用的干净文件页面，导致帧分配器返回 `NoMemory`。

**修复文件**:
| 文件 | 变更 |
|------|------|
| `axalloc/src/lib.rs` | 注册 `page_cache_reclaim` 回调，分配失败时尝试回收 |
| `axalloc/src/buddy_slab.rs` | 分配重试逻辑（最多 4 次），每次失败后触发回收 |
| `axfs-ng/src/highlevel/file.rs` | LRU 页面缓存驱逐：回收干净的文件支持页面 |
| `axsync/src/mutex.rs` | 移除 `try_lock` 路径中的 `might_sleep()`（try_lock 是单次 CAS，永不应阻塞） |
| `entry.rs` | 启动时注册回收回调 |

**关联**: 无此修复，`syn` crate（编译第 7/276）会因 OOM 而 panic。

## 阻塞点及修复

### 1. 内存检测：仅识别 512MB

**现象**: QEMU `-m 8G`，但 `phys_ram_ranges()` 只返回 `[0x804f0000, 0xa0000000)`（~510MB）。

**根因**: 静态平台 `axplat-riscv64-qemu-virt` 的 `axconfig.toml` 中 `phys-memory-size = 0x2000_0000`（512MB）是硬编码常量。`phys_ram_ranges()` 使用该常量计算可用内存，忽略 FDT 中的实际物理 RAM。

**修复** (`components/axplat_crates/platforms/axplat-riscv64-qemu-virt/axconfig.toml`):
```toml
# Before
phys-memory-size = 0x2000_0000       # 512M
# After
phys-memory-size = 0x2_0000_0000     # 8G
```

同时修改 `platform/axplat-dyn/src/mem.rs` 的 `phys_ram_ranges()`，从 `somehal::mem::memory_map()` 动态读取 Free 区域（plat_dyn 路径用）。

**关于动态 RAM 检测**: 尝试通过共享物理内存（someboot 写、静态平台读）实现动态检测，但遭遇 someboot（MMU 关闭阶段）与 starryOS（MMU 开启阶段）地址空间不一致的根本性困难。目前采用 hardcoded 8G 作为实用方案。

**注意**: PR #987 重构了 ax-alloc，移除了旧的 bitmap 页分配器（及 `page-alloc-*` 特性），改用 TLSF/buddy-slab 后端。TLSF 没有硬编码容量限制，因此不再需要 `page-alloc-64g` feature passthrough。只需修改 `phys-memory-size` 即可支持 8GB 内存。

### 2. TMPFS 挂载失败

**现象**: `mount -t tmpfs` 失败，Debian 根文件系统中 /tmp 不可写。

**根因**: mount(8) 优先使用新版 mount API（fsopen/fsconfig/fsmount）。StarryOS 将 `fsopen` 等实现为 `sys_dummy_fd`（返回伪 fd），mount(8) 误以为挂载成功，不会回退到传统 mount(2)。

**修复** (`os/StarryOS/kernel/src/syscall/mod.rs`):
```rust
// 将 fsopen/fspick/open_tree 从 sys_dummy_fd 改为返回 ENOSYS
// mount(8) 收到 ENOSYS 后回退到传统 mount(2) 调用来挂载 tmpfs
Sysno::fsopen | Sysno::fspick | Sysno::open_tree => Err(AxError::Unsupported),
```

**注意**: 此修复已在 upstream/dev 中存在。

### 3. 最终链接: _ex_table_end 未定义

**现象**: 所有 crate 编译通过，但 starryos 二进制链接失败:
```
rust-lld: error: undefined symbol: _ex_table_end
```

**根因**: 自编译环境中 `.cargo/config.toml` 未传递 `-Tlinker.x`（host 编译通过 `--config rustflags` 传递）。`ext_linker.ld` 使用 `INSERT AFTER .data;` 期望 `linker.x` 先定义 `.data` 段（含 `_ex_table_end`），但缺少 linker.x 时符号不存在。

**修复** (`os/StarryOS/starryos/ext_linker.ld`):
```ld
PROVIDE(_ex_table_start = 0);
PROVIDE(_ex_table_end = 0);

SECTIONS {
    /* ... 原有内容 ... */
}
INSERT AFTER .data;
```

`PROVIDE` 仅在符号未定义时提供回退值（空异常表，不影响正常运行）。

### 4. 测试正则误匹配

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
                      ├─ 修补 ext_linker.ld (如需要)
                      └─ cargo build -p starryos --offline (276 crates)
```

## 测试配置

### 测试用例 (`test-suit/starryos/selfhost-manual/selfhost-full-kernel/`)

测试用例位于独立的 `selfhost-manual` 目录中（非 `normal/`），避免因缺少 Debian rootfs 镜像（12GB）而阻塞标准 CI。

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
2. 使用 sed 向 ext_linker.ld 前置 PROVIDE 回退（保留已有 section）
3. 执行 `cargo build -p starryos --target riscv64gc-unknown-none-elf --offline`
4. 检查产物并输出 SELFHOST_SUCCESS 或 SELFHOST_FAILED

### 运行测试

```bash
# 在提前准备 rootfs 的环境中
cargo xtask starry test qemu --arch riscv64 --test-suite test-suit/starryos/selfhost-manual -g selfhost-manual -c selfhost-full-kernel
```

## 构建耗时

| 阶段 | 耗时 |
|------|------|
| Debian 启动 | ~5 分钟 |
| cargo build (276 crates) | ~95 分钟 |
| 总计 | ~100 分钟 |

## 完整变更清单

### 前置依赖 PR
| PR | 内容 | 关键文件 |
|----|------|----------|
| #797 | 信号传递修复：wake_task + dumpable/no_new_privs | `task/mod.rs`, `task/signal.rs` |
| #1007 | 页回收：内存压力下驱逐干净文件页面 | `axalloc/`, `axfs-ng/`, `axsync/` |

### 自编译 PR
| 文件 | 变更 |
|------|------|
| `axconfig.toml` | phys-memory-size: 512M → 8G |
| `syscall/mod.rs` | fsopen/fspick/open_tree → ENOSYS（已在 upstream/dev 中） |
| `ext_linker.ld` | PROVIDE _ex_table_start/end |
| `axplat-dyn/src/mem.rs` | phys_ram_ranges 从 memory_map 动态读取 |
| `selfhost-full-kernel/` | 测试用例及构建脚本 |
| `scripts/self-compile.sh` | 主机端自动化构建脚本 |

**注意**: 块引用 #1 中的 `axconfig.toml` 已在上游修改为 8G（`0x2_0000_0000`）。块引用 #2（`page-alloc-64g` passthrough）不再需要，因为 PR #987 移除了旧 bitmap 分配器及其特性。

## 已知限制

1. **`phys-memory-size` 硬编码 8G**: 动态 RAM 检测（someboot→OS 共享内存）因启动阶段地址空间不一致无法实现。使用少于 8G QEMU RAM 的标准测试会 panic。
2. **自编译测试不在标准 CI 中运行**: Debian rootfs 镜像（12GB）未上传到 tgosimages 发布版。测试用例位于 `test-suit/starryos/selfhost-manual/`，不在 `normal/` 目录下，不会阻塞 CI；需要手动在配备 rootfs 的环境中运行。

## 环境

- **QEMU**: riscv64, `-m 8G`, virt machine
- **内核**: StarryOS (dev 分支)
- **根文件系统**: Debian riscv64, ext4, rustc nightly-2026-04-27
- **源码**: StarryOS monorepo (离线，预取依赖)
