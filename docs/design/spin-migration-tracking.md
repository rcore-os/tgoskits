# `spin` 迁移历史

> 本文只保留早期迁移的历史结论，不再描述当前接口或可执行步骤。当前锁架构、
> API、边界约束和验证命令以 [`ax-sync` 锁架构设计](./design/ax-sync-lock-architecture.md)
> 与 [`lock-lint`](./docs/build/lock_lint.md) 文档为准。

项目最初同时使用 crates.io `spin`、`ax-kspin`、`ax-kernel-guard`、`ax-lockdep`
和旧 `ax-sync`。迁移曾分阶段把外部 `spin` 的 `Mutex`、`RwLock`、`Once` 和
`LazyLock` 收口到仓库内实现，再统一锁的上下文语义。

该迁移现已结束：

- `ax-sync` 是第一方代码唯一的公共锁 crate；
- `SpinLock` 和 `SpinRwLock` 由获取方法区分 preempt、IRQ-save 和 raw 上下文；
- `Mutex` 固定为可睡眠 mutex；
- lockdep 和 guard 能力已并入 `ax-sync`；
- `ax-kspin`、`ax-kernel-guard`、`ax-lockdep` 以及第一方 crates.io `spin` 依赖已删除；
- no-std 一次性初始化使用 `ax-lazyinit`，std 组件使用 `std::sync`；
- 原 `spin-lint` 已由覆盖完整锁边界的 `cargo xtask lock-lint` 取代。

历史方案中的 vendored `components/spin`、旧锁类型名和 `spin-lint` 命令均已失效，
不应再作为新代码或验证流程的依据。传递依赖仍可由第三方 crate 引入 `spin`；
`lock-lint` 禁止的是第一方 manifest 和源码直接使用。
