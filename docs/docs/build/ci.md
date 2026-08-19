---
sidebar_position: 20
sidebar_label: "自动 CI 测试"
---

# 自动 CI 测试

主 CI 由以下几层组成：

- `.github/workflows/ci.yml` 解析增量基线并调用 planner；
- `.github/ci/checks/*.toml` 以 schema v3 声明检查、影响范围和已注册 test-suit 能力；
- `.github/ci/runner-profiles.toml` 集中声明 runner、环境、owner 和 KVM 要求；
- `scripts/test/ci_plan.py` 展开配置并生成矩阵；
- `.github/workflows/reusable-check-matrix.yml` 执行展开后的矩阵行。

容器发布由 `.github/workflows/container-publish.yml` 独立处理。非主线分支 push
先经过 `.github/workflows/ci-branch-push.yml`，已有 open PR 时不重复调度主 CI。

## 触发条件

| 事件 | 行为 |
|------|------|
| push 到 `main` / `dev` | 非文档变更运行完整矩阵 |
| 其他分支 push | 没有 open PR 时通过 `workflow_dispatch` 运行完整矩阵 |
| pull request | planner 根据三点 diff 生成增量矩阵 |
| workflow dispatch | 使用指定的 `since_sha`，但仍运行完整矩阵 |

纯 Markdown 变更不触发主 CI。`push` 和 `workflow_dispatch` 不缩小测试矩阵。

## 执行阶段与名称

```text
Cancel stale CI runs
  -> Plan CI
     -> Preflight / <purpose>
        -> Verification / <OS> / <platform> <arch-or-board> · <purpose>
```

固定阶段名称为 `Preflight` 和 `Verification`。平台写在架构之前，例如：

```text
Preflight / Formatting + publish dry-run
Verification / ArceOS / QEMU aarch64 · GICv2 SMP4 boot + suites + axtest
Verification / AxVisor / VMX x86_64 · Smoke
Verification / Starry / Board VisionFive 2 · Suites
```

`Preflight` 包含 formatting/publish dry-run、incremental sync-lint 和 locking
policy。`Verification` 包含 Workspace、ArceOS、AxVisor 和 Starry 检查。全量运行
展开为 3 个 Preflight 行和 32 个 Verification 行。

## PR 影响路由

planner 将输入分为三类：

1. workspace crate 通过四个 bare-metal target 的 Cargo metadata 计算反向依赖；
2. test-suit 和 OS 配置使用精确输入路由；
3. 全局、未知或无法安全解释的输入回退到完整矩阵。

只要 crate 在任意架构影响 ArceOS、Starry 或 AxVisor 根 package，就会运行该
OS 当前注册的全部 QEMU、KVM 和真机检查，不再只运行最初命中的架构。独立
axtest package 仍按其声明的 target 选择 ArceOS QEMU 检查，AxLoader 仍由自己的
package impact 单独选择。

命名明确的 QEMU/board 配置只选择对应平台。无法精确解释但能确定所属 OS 的配置
会回退到该 OS 的全部注册检查。`apps/**` 默认不扩大运行时覆盖，但
`apps/arceos/virtio-blk-test/**` 是 AxVisor aarch64 CI 的直接输入，会选择对应 QEMU
检查。

解析失败、未知源码路径、Cargo/toolchain、CI 配置、workflow、planner、xtask 或
`scripts/axbuild/**` 变更都会回退完整矩阵。planner 会在 Actions summary 中写出
changed paths、affected OS、精确输入、test-suit 选择、selected/skipped checks 和
回退原因。

## test-suit 精确运行

如果 PR 的有效改动全部位于 `test-suit/**`，planner 进入 exclusive 模式：

- `Preflight` 不执行；
- Workspace 和其他 OS 的聚合检查不执行；
- 每个变更只生成对应的已注册 case 行；
- 多个 case 取稳定去重并集；
- 没有 CI runner/template 注册的 case 使 `Plan CI` 明确失败。

动态名称遵循：

```text
Verification / <OS> / <platform> <arch-or-board> · <case>
```

例如：

```text
Verification / ArceOS / QEMU riscv64 · rust/task-ipi
Verification / Starry / QEMU aarch64 · qemu/system
Verification / AxVisor / VMX x86_64 · direct-acpi-vmx
Verification / Starry / Board OrangePi 5 Plus · native-hardware-smoke
```

Starry `qemu/system/<subcase>` 源码变更使用 `qemu/<subcase>` selector，并为拥有
`qemu-<arch>.toml` 且已注册的架构分别生成一行。共享 build wrapper 变更会展开到
该 wrapper 下所有已注册 case。suite 与 crate/OS 输入混合时，更宽的 OS 规则优先，
不会重复生成同一 OS 的精确 suite 行。

## Manifest schema v3

每个 check manifest 必须声明 `schema_version = 3`。schema v2 不再兼容。

全局默认 runner 是 `ubuntu-base`。同一 manifest 大部分检查使用另一 runner 时，可用
`default_runner`；单项差异使用 `runner`。默认 runner 或默认字段不需要重复书写。
check 中禁止直接声明 `runs_on`、`environment`、owner 或 `require_kvm`，这些字段只
来自 runner profile。

当前 profiles：

| Profile | Runner / 环境 | 约束 |
|---------|---------------|------|
| `ubuntu-base` | `ubuntu-latest` + base container | 全局默认 |
| `ubuntu-host` | `ubuntu-latest` host | 不使用 container |
| `ubuntu-axvisor-lvz` | `ubuntu-latest` + LVZ container | LoongArch AxVisor |
| `qcs` | `self-hosted, linux, qcs` | fork 回退到 base container |
| `board` | `self-hosted, linux, board` | 仅 `rcore-os` owner |
| `kvm-intel` | `self-hosted, linux, intel, kvm` | 仅 `rcore-os`，要求 KVM |
| `kvm-amd` | `self-hosted, linux, amd, kvm` | 仅 `rcore-os`，要求 KVM |

self-hosted 检查的 `cache_key` 必须为空；省略时默认就是空字符串。GitHub-hosted
检查只有显式非空 `cache_key` 才启用 `Swatinem/rust-cache`。

## 可复用矩阵

`reusable-check-matrix.yml` 只包含一个 matrix job。planner 始终输出完全展开的
runner labels、container image、preflight、cache、checkout depth、timeout、artifact
和命令字段。

普通完整/增量运行中，sync-lint 生成 `tg-xtask-bin` artifact，使用容器的测试行可
下载复用。exclusive test-suit 不运行 Preflight，因此动态行直接使用 `cargo xtask`
并禁用 artifact 下载。

## 本地检查

```bash
python3 -m unittest scripts/test/test_ci_impact.py scripts/test/test_ci_plan.py
python3 scripts/test/check_ci_paths.py
python3 scripts/test/check_ci_routing.py
python3 scripts/test/check_workflow_layout.py
actionlint
```

本仓库的 planner 需要 Python 3.11 或更新版本。
