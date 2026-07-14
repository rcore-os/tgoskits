---
sidebar_position: 8
sidebar_label: "Spin Lint"
---

# Spin Lint

`cargo xtask spin-lint` 固定并审计 workspace 使用的 crates.io `spin 0.12.0`。仓库不再维护本地 `spin` fork；`ax-kspin` 在唯一受审计的 raw algorithm 边界上组合 `spin::TicketMutex`、`spin::RwLock` 与 `lock_api`，其余 crate 只能使用 `ax-kspin` 的安全封装。

## 检查内容

门禁同时检查 manifest、源码和 lockfile：

- workspace 根必须以精确版本 `=0.12.0`、`default-features = false` 声明 `spin`；允许的 feature 固定为 `lazylock`、`lock_api`、`mutex`、`once`、`rwlock` 和 `use_ticket_mutex`。
- 成员 crate 应写 `spin = { workspace = true }`，不得覆盖版本、来源、默认 feature 或 feature 集合。
- 不允许本地同名 package、`[patch.crates-io]`、git/path/registry override 或依赖重命名。
- 除 `components/kspin/src/raw.rs` 外，源码不得直接引用 `spin::RwLock` 或 `spin::rwlock`；普通消费者应使用 `ax_kspin::SpinRwLock`，实时关键路径优先使用 FIFO ticket mutex。
- `Cargo.lock` 中只允许 crates.io 的 `spin 0.12.0`，且必须带 registry checksum。

该例外按完整仓库相对路径精确匹配，不会放宽其他 crate。对应单测同时验证普通消费者会被拒绝、ax-kspin raw 实现会被接受。

## 用法

```bash
cargo xtask spin-lint
```

任何 finding 都会打印文件、字段或源码行号以及修复建议，并以非零状态退出。修改 `spin` 版本、feature 或 ax-kspin 算法边界时，必须同步更新门禁与本文档。
