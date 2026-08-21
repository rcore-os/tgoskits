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

容器发布由 `.github/workflows/container-publish.yml` 独立处理。同仓分支 push 是
对应 head commit 的首选验证；pull request 准备阶段会短暂重试尚未可见的同 SHA push
run。匹配的 push 仍在 queued 或 in_progress 时，它已经拥有该 commit 的验证，pull
request 将自己的后续矩阵标记为 skipped；push 已 completed 时，仍必须存在非 Plan、
非 skipped/cancelled 的真实矩阵 job 才能复用。这样既避免 push 与 pull request 同时
执行，也不会把历史上的 Plan-only push 当成完整验证。查询失败、fork PR、completed
Plan-only push 或已取消的 push 都会 fail-open，由 pull request 正常规划并执行。

pull request 不会取消当前 SHA 的 push，因此 GitHub 不会把正常去重显示为
failing/cancelled checks。pull request run 仍只取消同一 PR 的旧 pull request run，
即使本次矩阵复用 push 而 skipped，也会清理旧 commit 尚未完成的 pull request run；
新 commit 的 push 同样会取消同一分支的旧 push run。`main` 和 `dev` 是例外：每个
push commit 的 CI 都完整保留，后续提交不会取消仍在运行的旧 CI。旧 run 先收到普通
cancel；短暂复查仍未完成时再 force-cancel，避免分组 job 的 `always()` 条件阻止清理。

## 触发条件

| 事件 | 行为 |
|------|------|
| push 到 `main` / `dev` | 非文档变更运行完整矩阵，并保留每个 commit 的完整 CI |
| 其他分支 push | 非文档变更运行完整矩阵，作为同仓 PR 的首选验证 |
| 首次创建 pull request | 同 SHA push 已拥有验证时跳过，否则按三点 diff 规划 |
| 更新或重开 pull request | 取消旧 commit 的同事件 run；同 SHA push 已拥有验证时跳过，否则按三点 diff 规划 |
| workflow dispatch | 使用指定的 `since_sha`，但仍运行完整矩阵 |

纯 Markdown 变更不触发主 CI。`push` 和 `workflow_dispatch` 不缩小测试矩阵。

## 执行阶段与名称

```text
Plan CI
Preflight / <purpose>
Workspace / <purpose>
ArceOS / <platform> <arch-or-board> · <purpose>
Starry / <platform> <arch-or-board> · <purpose>
AxVisor / <platform> <arch-or-board> · <purpose>
```

`Workspace`、`ArceOS`、`Starry` 和 `AxVisor` 都在 `Preflight` 成功或按计划跳过
后启动。每个名称都是一个可展开的 reusable workflow 分组，平台写在架构之前，例如：

```text
Preflight / Formatting + publish dry-run
Workspace / Incremental Clippy
ArceOS / QEMU aarch64 · GICv2 SMP4 boot + suites + axtest
Starry / Board VisionFive 2 · Suites
AxVisor / VMX x86_64 · Smoke
```

`Preflight` 包含 formatting/publish dry-run 和 incremental sync-lint。`Workspace`
包含跨 workspace 的 clippy 和 std tests，其余三个分组包含各自
注册的 QEMU、KVM 和真机检查。实际行数由 manifests 动态生成，不在文档中维护易过期
的固定总数。

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
<OS> / <platform> <arch-or-board> · <case>
```

例如：

```text
ArceOS / QEMU riscv64 · rust/task-ipi
Starry / QEMU aarch64 · qemu/system
AxVisor / VMX x86_64 · direct-acpi-vmx
Starry / Board OrangePi 5 Plus · native-hardware-smoke
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
| `qcs` | `self-hosted, linux, qcs` | workflow 在 fork 仓库运行时回退到 base container |
| `board` | `self-hosted, linux, board` | 仅 `rcore-os` owner |
| `kvm-intel` | `self-hosted, linux, intel, kvm` | 仅 `rcore-os`，要求 KVM |
| `kvm-amd` | `self-hosted, linux, amd, kvm` | 仅 `rcore-os`，要求 KVM |

Runner 路由以 workflow 执行仓库的 `github.repository_owner` 为准。fork
仓库自行执行 CI 时使用 GitHub-hosted fallback；在 `rcore-os/tgoskits`
中执行的 pull request workflow 使用完整的组织 Runner 矩阵。外部 PR 在使用
self-hosted Runner 前应先经过 workflow 审批；应在仓库设置中为外部贡献者启用
该审批，不能用 PR 源仓库 owner 改写 Runner 路由。

self-hosted 检查的 `cache_key` 必须为空；省略时默认就是空字符串。GitHub-hosted
检查只有显式非空 `cache_key` 才启用 `Swatinem/rust-cache`。

## 可复用矩阵

`reusable-check-matrix.yml` 只包含一个 matrix job。planner 分别输出
`workspace_matrix`、`arceos_matrix`、`starry_matrix` 和 `axvisor_matrix`，顶层同名
caller 分别调用该执行器，从而在 Actions 左栏形成可展开分组。每个分组内部保留
fail-fast；一个分组失败不会取消其他分组。矩阵行仍包含完全展开的 runner labels、
container image、preflight、cache、checkout depth、timeout、artifact 和命令字段。

普通完整/增量运行中，sync-lint 生成 `tg-xtask-bin` artifact，使用容器的测试行可
下载复用。exclusive test-suit 不运行 Preflight，因此动态行直接使用 `cargo xtask`
并禁用 artifact 下载。

## 本地检查

```bash
python3 -m unittest \
  scripts/test/test_ci_impact.py \
  scripts/test/test_ci_plan.py \
  scripts/test/test_ci_routing.py
python3 scripts/test/check_ci_routing.py
actionlint
```

本仓库的 planner 需要 Python 3.11 或更新版本。
