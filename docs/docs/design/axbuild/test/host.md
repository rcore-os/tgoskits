---
sidebar_position: 8
sidebar_label: "Host 端"
---

# Host 端测试

Host 端测试不涉及 OS 编译或 QEMU 运行，而是直接在宿主机上执行验证。包括三类：**std 白名单测试**（验证 workspace 中的 Rust crate 在标准库环境下能正确编译和通过测试）、**clippy 检查**（静态代码质量分析）和 **sync-lint**（并发安全性审查）。这三类测试共同确保代码库的基础质量。

## std 白名单测试

```text
cargo xtask test
```

对 `scripts/test/std_crates.csv` 白名单中的每个 crate 执行 `cargo test -p <package>`。

CSV 格式：每行一个 crate 名，`#` 开头为注释行。

白名单机制是必要的，因为 workspace 中包含大量 `no_std` crate，它们无法在标准 `cargo test` 环境下运行。白名单只包含已知能在 host 环境中通过测试的 crate（如纯算法库、工具库等），避免因平台不兼容导致测试失败。

## Clippy 检查

```text
cargo xtask clippy [--all | --package <name>]
```

基于 `scripts/test/clippy_crates.csv` 白名单，对每个包检查所有 feature 组合和 `docs.rs` 目标平台。

Clippy 检查不仅运行默认 lint 规则，还会遍历每个包的所有 feature 组合，确保 feature 门控代码也符合 lint 规范。此外，还会检查 `docs.rs` 目标平台构建，验证文档生成不会因平台特定代码报错。`--all` 跳过白名单检查所有 workspace 包，`--package` 只检查指定包。

## sync-lint

```text
cargo xtask sync-lint
```

扫描 workspace 中所有 Rust 源文件，检测可疑的 `Relaxed` 原子序使用。

在内核和裸机环境中，内存排序的正确性至关重要——不恰当的 `Ordering::Relaxed` 可能导致难以复现的并发 bug。sync-lint 扫描所有 `Ordering::Relaxed` 使用点，帮助开发者审查每个使用场景是否真正只需要 Relaxed 语义，还是应该使用更强的排序保证（如 `Acquire`/`Release`）。
