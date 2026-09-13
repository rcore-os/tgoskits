---
sidebar_position: 14
sidebar_label: "评审基准"
---

# 评审基准

`cargo xtask agent-review-bench` 是历史拉取请求评审的离线基准工具。它读取 `scripts/agent-review-bench/cases/*.toml`，准备基准与待审提交的 Git 快照，在隔离目录中调用指定评审命令行程序，并将报告与预期问题比较后输出召回率和额外问题数量。

它不参与 ArceOS、StarryOS 或 Axvisor 的构建、运行和持续集成产物生成；用途是维护评审能力的可重复评估。

## 1. 命令接口

该命令将只读校验与实际评审执行分开，便于先验证历史快照可复现，再消耗评审资源。`list` 和 `check` 不调用评审命令行程序，`run` 才会创建隔离工作目录并评分。

### 1.1 命令形式

三个子命令共用用例选择和产物路径约定，但只有 `run` 接收评审程序选择、模型和召回率门槛。

```bash
# 列出用例标识、拉取请求和预期问题数量
cargo xtask agent-review-bench list

# 只校验 TOML schema、Git commit 和预期行是否有效
cargo xtask agent-review-bench check

# 执行所选或全部用例
cargo xtask agent-review-bench run \
  [--case <ID>...] [--pr <NUMBER>...] \
  [--agent codex|claude] [--model <MODEL>] \
  [--reasoning-effort <LEVEL>] [--timeout-secs <SECONDS>] \
  [--min-recall <0-100>] [--output <DIR>]
```

### 1.2 选择规则

未传 `--case` 或 `--pr` 时，`run` 执行全部用例；两个选择器可以重复使用，选择结果为并集。`--timeout-secs` 默认 1800 秒且必须大于 0；`--min-recall` 设定总召回率的失败门槛。

## 2. 用例契约

每个用例通过 TOML 声明：

```toml
id = "example-pr"
pr = 123
title = "Example"
remote = "https://github.com/owner/repo.git"
base = "<40-char SHA>"
head = "<40-char SHA>"
source = "historical PR snapshot"

[[expected]]
id = "missing-validation"
path = "crate/src/lib.rs"
line = 42
severity = "major"
description = "Expected review finding"
```

加载时会验证标识、提交散列值、远程网址、预期问题去重和路径安全性。`check` 还确保两个提交可获得、`base` 是 `head` 的祖先、预期文件确实由 `base..head` 修改，并且预期行位于 `HEAD` 一侧的变更块内。

## 3. 评分产物

运行默认将结果写入工作区内的基准产物目录；`--output` 可覆盖。每个用例产生 `review.json`、`grade.json`、`result.json`，总目录产生 `summary.json`。终端输出包含命中数、总预期问题数、召回率、额外问题数和产物位置。

评分以用例的 `expected` 作为基准；当设置 `--min-recall` 且总召回率低于门槛时，命令以失败状态退出。
