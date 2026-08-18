# PR 提交前检查规范（PR #2062 及相关 `ax-driver` 变更）

目标：避免在 GitHub CI 里再次触发 `std tests failed for 1 package(s): ax-driver` 这类回归未被本地发现的问题。

## 一、强制执行顺序（每次提交前必须完成）

1. 代码一致性

```bash
cargo fmt --all -- --check
git diff --check
```

2. 关注包级最小闭环（先跑标准测试）

```bash
cargo test -p ax-driver --locked -- --nocapture
```

3. 关注所有特征路径（若变更牵涉 FDT/PCI/测试基线）

```bash
cargo test -p ax-driver --all-features --locked -- --nocapture
```

4. 跑本次主题测试（至少以下之一，按本次变更选做）

```bash
cargo test -p ax-driver --test binding_info --locked -- --nocapture
cargo test -p ax-driver --test fdt_relation_binding --locked -- --nocapture
cargo test --manifest-path drivers/ax-driver/Cargo.toml --features starfive-jh7110-dwmmc --test fdt_irq_capability --locked -- --nocapture
cargo test --manifest-path drivers/ax-driver/Cargo.toml --all-features --test pci_fdt_irq_capability --locked -- --nocapture
```

5. 关键回归（确认不引入仓库层面回归）

```bash
cargo xtask test
```

### `ax-driver` 的 workspace std profile

`scripts/axbuild/src/test/std.rs` 对 `ax-driver` 选择的是
`starfive-jh7110-dwmmc`，因此必须额外执行下面的**同一条命令**；
`--all-features` 不能替代它：

```bash
cargo test -p ax-driver --features starfive-jh7110-dwmmc --locked -- --nocapture
```

该命令必须覆盖 library 与已启用的 integration tests。不得为了让
combined-feature 测试通过而用 feature gate 隐藏既有单元测试；若失败，
保留首条编译错误或失败测试及原始 exit code。

若仓库当前资源限制不能完整运行，至少要求提交：

- 以上 1～4 步全部通过；
- `ax-driver` 所有测试通过，并保留日志；
- 明确记录为何不能跑 full `xtask`（环境、命令和错误码）。

## 二、失败即中止规则

出现以下任一项，**禁止提交**：

- `std tests failed for 1 package(s): ax-driver`
- `merge test` / `test result: FAILED`
- `cargo fmt` 或 `git diff --check` 非 0
- 任何与本 PR 修改域相关的测试出现非预期 `exit 1`

一律要求先补齐最小失败切片，不能仅以“疑似 CI 随机失败”跳过。

## 三、最小复现与归因规范

当 CI 失败时，优先按以下顺序贴齐证据（按原始命令+exit code）：

- `Tests / Workspace / std tests` 失败段：定位 `ax-driver` 首条 `FAILED` 的测试名与 panic/err 来源。
- `cargo test -p ax-driver ...` 三段式复测：
  - `--locked`
  - `--all-features`（如上一步已执行可引用）
  - 集成测试（`binding_info` / `fdt_relation_binding`）
- `cargo xtask test` 的失败段

## 四、提交前附录（PR 评论可直接复用）

提交时附上：

- 已执行命令列表
- 每条命令的 exit code
- 失败测试（如有）及 root cause
- 未执行命令与原因（含客观环境限制）

## 五、PR 归并前红线

默认不能以“历史日志”作为依据判断通过。  
以当前 `HEAD` 对应的最新 CI 结果为准；若有变更后再跑 CI，即使旧日志显示通过，也不能跳过上述本地复核。
