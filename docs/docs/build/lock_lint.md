---
sidebar_position: 8
sidebar_label: "Lock Lint"
---

# Lock Lint

`cargo xtask lock-lint` 守护全项目统一到 `ax-sync` 后的锁边界。它检查仓库级依赖、
源码导入和 runtime provider 不变量，不是一般代码风格检查。

## 检查内容

| 检查 | 约束 |
|------|------|
| 已移除 package | workspace、成员 manifest 和 `Cargo.lock` 不能重新出现 `ax-kspin`、`ax-kernel-guard` 或 `ax-lockdep` |
| 第一方 `spin` | manifest 不能直接依赖 crates.io `spin`，Rust 源码不能直接导入或调用 `spin::*` |
| StarryOS | kernel 生产源码只能经 `crate::sync` 使用锁；`src/sync.rs` 是唯一允许直接导入 `ax-sync` 的 facade |
| Axvisor | AxVM/Axvisor 不得直接依赖或导入 `ax-sync`；普通路径使用 `std::sync`，特殊上下文经 `ax_std::os::arceos::sync` |
| runtime provider | `CriticalSectionOps`、`MutexRuntimeOps`、`LockdepOps` 的生产实现必须各有且仅有一个，并位于 `ax-runtime/src/sync.rs` |

扫描跳过 `.git`、`target`、`tmp`、`.cache`、文档和 lint 实现自身。测试 provider 只允许出现在
`ax-sync` 的明确测试位置；第三方依赖树中的传递 `spin` package 不属于第一方直接依赖，允许保留。

## 合法边界

可移植的 `no_std` 组件直接依赖 workspace `ax-sync`：

```toml
[dependencies]
ax-sync = { workspace = true }
```

```rust
use ax_sync::SpinLock;

let state = lock.lock();
let irq_shared = irq_lock.lock_irqsave();
```

StarryOS kernel 从本地 facade 导入：

```rust
use crate::sync::{Mutex, SpinLock};
```

AxVM 普通任务上下文使用 `std::sync`；只有 IRQ、guest-entry 或 no-preempt 路径使用：

```rust
use ax_std::os::arceos::sync::IrqSafeMutex;
```

以下写法会失败：

```toml
spin = "0.12"
ax-kspin = { workspace = true }
ax-lockdep = { workspace = true }
```

```rust
use spin::Once;
use ax_kspin::SpinNoIrq;
use ax_lockdep::HeldLock;
```

`no_std` 的一次初始化使用 `ax_lazyinit::{OnceLock, LazyLock}`；有 Rust `std` 的组件使用
`std::sync::{OnceLock, LazyLock}`。

## 报告格式

每条 finding 包含路径、TOML 位置或源码行号、错误说明与修复建议：

```text
<path>: <location>: <message>
  help: <修复建议>
```

存在任何 finding 时命令以非零状态退出。

## 用法

```bash
cargo xtask lock-lint
```

CI 会运行该命令，防止旧锁 crate、第一方直接 `spin`、OS facade 绕过和重复 runtime provider
重新进入仓库。
