---
sidebar_position: 8
sidebar_label: "Spin Lint"
---

# Spin Lint

`cargo xtask spin-lint` 用于守护 workspace 对上游 `spin` crate 的统一使用约束。当前仓库直接使用 crates.io 的 `spin` 0.12.2，并关闭默认 feature。`ax-kspin` 在唯一受审计的 raw algorithm 边界组合 FIFO ticket mutex、raw rwlock 与 `lock_api`；其余 crate 只能使用 `ax-kspin` 的安全封装。

> spin-lint 检查的是仓库级依赖与同步原语不变量，不是一般代码风格。

## 检查内容

`lint_workspace` 依次执行以下检查，所有 finding 会累积后统一报告：

| 检查 | 约束 |
|------|------|
| 本地 package | workspace 内不能出现名为 `spin` 的本地 package，避免重新引入 vendored 副本 |
| 根 manifest | `[workspace.dependencies]` 必须精确声明 crates.io `spin =0.12.2`，关闭默认 feature，并启用 `lazylock`、`lock_api`、`mutex`、`once`、`rwlock`、`use_ticket_mutex`；不能设置 `path`、`git`、`registry` 或 `package` |
| crates.io patch | 根 manifest 不能通过 `[patch.crates-io]` 替换 `spin` |
| 成员 manifest | 推荐使用 `spin = { workspace = true }`；显式依赖也必须固定 `=0.12.2`、关闭默认 feature，且只能启用白名单 feature |
| 源码用法 | 除 `components/kspin/src/raw.rs` 外，Rust 源码不能出现 `spin::RwLock`、`spin::rwlock` 或 `use spin::RwLock` |
| lockfile | 每个 `spin` 条目都必须是 crates.io 的 0.12.2，并带 registry source 与 checksum |

扫描会跳过 `.git`、`target`、`tmp` 和 `.cache`。源码扫描还会跳过 `docs` 以及 `spin_lint.rs` 自身，避免示例文字和规则字符串触发误报。

## 合法依赖写法

根 `Cargo.toml`：

```toml
[workspace.dependencies]
spin = { version = "=0.12.2", default-features = false, features = ["lazylock", "lock_api", "mutex", "once", "rwlock", "use_ticket_mutex"] }
```

成员 crate 优先继承 workspace 配置：

```toml
[dependencies]
spin = { workspace = true }
```

成员 crate 如果必须显式声明，只能使用相同精确版本、关闭默认 feature，并选择白名单 feature 的子集：

```toml
[dependencies]
spin = { version = "=0.12.2", default-features = false, features = ["once"] }
```

以下写法会失败：

```toml
# 字符串依赖会启用上游默认 feature。
spin = "0.12"

# workspace 继承项不能增加未经审计的 feature。
spin = { workspace = true, features = ["portable-atomic"] }

# 不允许改名或覆盖来源。
spin_compat = { package = "spin", version = "=0.12.2" }
spin = { path = "components/spin" }
```

## RwLock 约束

spin-lint 对 Rust 源码逐行检查以下模式：

```rust
const FORBIDDEN_SPIN_RWLOCK_PATTERNS: &[&str] =
    &["spin::RwLock", "spin::rwlock", "use spin::RwLock"];
```

普通 crate 需要非睡眠读写锁时使用：

```rust
use ax_kspin::SpinRwLock;
```

唯一例外是 `components/kspin/src/raw.rs`。该文件直接适配 `spin::mutex::TicketMutex` 和 `spin::rwlock::RwLock` 到 `lock_api`，例外按完整仓库相对路径精确匹配，不会放宽其他消费者。实时关键路径应优先使用 FIFO ticket mutex，因为上游 raw rwlock 不保证有界 writer wait。

## 报告格式

每条 finding 包含路径、TOML 位置或源码行号、错误说明与修复建议：

```text
<path>: <location>: <message>
  help: <修复建议>
```

存在任何 finding 时命令会以非零状态退出。

## 用法

```bash
cargo xtask spin-lint
```

CI 会运行该命令，防止依赖版本、feature、来源或 raw lock 边界回退。修改 `spin` 版本、feature 或 ax-kspin 算法边界时，必须同步更新门禁与本文档。
