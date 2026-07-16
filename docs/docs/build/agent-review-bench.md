---
sidebar_position: 14
sidebar_label: "Review Benchmark"
---

# Review Benchmark

`cargo xtask agent-review-bench` 是历史 PR review 的离线 benchmark 工具。它读取 `scripts/agent-review-bench/cases/*.toml`，准备 base/head/fixed-by Git 快照，在隔离目录中调用指定 review CLI，并将报告与预期 finding 比较后输出 recall 和额外 finding 数量。

它不参与 ArceOS、StarryOS 或 Axvisor 的构建、运行和 CI 产物生成；用途是维护 review 能力的可重复评估。

## 1. 命令接口

该命令将只读校验与实际 reviewer 执行分开，便于先验证历史快照可复现，再消耗 reviewer 资源。`list` 和 `check` 不调用 reviewer CLI，`run` 才会创建隔离工作目录并评分。

### 1.1 命令形式

三个子命令共用 case 选择和 artifact 路径约定，但只有 `run` 接收 reviewer 选择、模型和 recall 门槛。

```bash
# 列出 case ID、PR 和预期 finding 数量
cargo xtask agent-review-bench list

# 只校验 TOML schema、Git commit 和预期行是否有效
cargo xtask agent-review-bench check

# 执行所选或全部 case
cargo xtask agent-review-bench run \
  [--case <ID>...] [--pr <NUMBER>...] \
  [--agent codex|claude] [--model <MODEL>] \
  [--reasoning-effort <LEVEL>] [--timeout-secs <SECONDS>] \
  [--min-recall <0-100>] [--output <DIR>]
```

### 1.2 选择规则

未传 `--case` 或 `--pr` 时，`run` 执行全部 case；两个选择器可以重复使用，选择结果为并集。`--timeout-secs` 默认 1800 秒且必须大于 0；`--min-recall` 设定总 recall 的失败门槛。

## 2. 用例契约

每个 case 通过 TOML 声明：

```toml
id = "example-pr"
pr = 123
title = "Example"
remote = "https://github.com/owner/repo.git"
base = "<40-char SHA>"
head = "<40-char SHA>"
fixed_by = "<40-char SHA>"
source = "historical PR snapshot"

[[expected]]
id = "missing-validation"
path = "crate/src/lib.rs"
line = 42
severity = "major"
description = "Expected review finding"
```

加载时会验证 ID、SHA、远程 URL、预期 finding 去重和路径安全性。`check` 还确保三个 commit 可获得、`base` 是 `head` 的祖先、预期文件确实由 base..head 修改，并且预期行位于 HEAD 一侧的变更 hunk 内。

## 3. 评分产物

运行默认将结果写入 workspace 内的 benchmark artifact 目录；`--output` 可覆盖。每个 case 产生 `review.json`、`grade.json`、`result.json`，总目录产生 `summary.json`。终端输出包含命中数、总预期 finding 数、recall、额外 finding 数和 artifact 位置。

评分以 case 的 `expected` 作为基准；当设置 `--min-recall` 且总 recall 低于门槛时，命令以失败状态退出。
