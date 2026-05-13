---
sidebar_position: 8
sidebar_label: "Host 端检查"
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
cargo xtask clippy [--all | --package <name> | --since <ref>]
```

基于 `scripts/test/clippy_crates.csv` 白名单，对每个包执行多维度的 clippy 检查。

### 检查展开

对每个包，clippy 会自动展开为多组检查：

1. **基础检查**（`ClippyCheckKind::Base`）：`cargo clippy -p <package> -- -D warnings`
2. **Feature 检查**（`ClippyCheckKind::Feature`）：对每个非 default feature 执行 `cargo clippy -p <package> --no-default-features --features <feature> -- -D warnings`
3. **docs.rs 目标平台检查**：如果包的 `Cargo.toml` 中 `[package.metadata.docs.rs]` 含 `targets` 数组，则为每个目标平台额外执行上述所有检查

例如，一个有 3 个 feature 且配置了 2 个 docs.rs 目标平台的包，会展开为 `(1 + 3) × 2 = 8` 组 clippy 检查。

### 增量选择

- `--since <ref>`：通过 `git diff --name-only <ref>..HEAD` 获取变更路径，定位到具体包后沿依赖图反向遍历（reverse dependency walk）找到所有受影响的包，再与白名单取交集。如果变更路径位于 workspace 包之外，则回退到完整白名单

### 结果报告

执行完成后输出结构化报告：通过/失败的包数、失败包中每个失败的检查项。当使用 `--all` 时还会输出通过包的 CSV 列表，方便更新白名单。

## sync-lint

```text
cargo xtask sync-lint [--since <ref>]
```

扫描 workspace 中 Rust 源文件，检测可疑的 `Relaxed` 原子序使用。

- 不带参数：扫描整个 workspace（并行执行，线程数 = `available_parallelism()` 与文件数的较小值）
- `--since <ref>`：仅检查自指定 git ref 以来变更的 Rust 文件（增量检查）。如果变更路径位于 workspace 包之外，则回退到全量扫描

### 检查规则

sync-lint 使用 AST 访问器（基于 `syn` crate）分析代码，检测三类问题：

| 规则标签 | 说明 |
|----------|------|
| `suspicious_relaxed_wait_condition` | 等待条件（如 `while load(Relaxed)`）中使用 Relaxed 加载 |
| `suspicious_relaxed_publish_before_notify` | Relaxed 写入后紧跟 wake/notify 操作 |
| `suspicious_relaxed_mixed_ordering` | 同一同步变量上混用 Relaxed 和更强排序 |

第三条规则（`mixed_ordering`）通过跨整个函数体收集所有原子访问，在函数结束时检查是否存在同一变量既被 Relaxed 访问又被更强排序访问的情况。

### 忽略机制

在可疑行前最多 3 行内添加包含 `sync-lint: ignore` 注释可抑制报告。支持精确规则过滤（如 `// sync-lint: ignore suspicious_relaxed_wait_condition`）或忽略全部规则（`// sync-lint: ignore` 后不跟具体规则名）。

### 输出格式

```
path:line:col: message [rule-label]
```

在有发现问题时以错误退出码退出。
