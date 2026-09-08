---
sidebar_position: 6
sidebar_label: "应用验证"
---

# 应用验证

`.github/workflows/starry-apps.yml` 为 Starry 应用提供独立于主 CI 的定时和手动验证。工作流使用 `ci_plan.py` 的 `starry-apps` 模式读取专用检查清单，再调用共享矩阵执行器；它不使用主 CI 的 PR 变更范围或 push/PR 去重结果。

## 1. 事件与选择

应用工作流只有 `schedule` 和 `workflow_dispatch` 两种入口。其检查集合由 `.github/ci/checks/starry-apps.toml` 和布尔输入共同决定。

### 1.1 定时运行

cron 表达式为 `0 18 * * *`，按 UTC 每日 18:00 调度，对应北京时间次日 02:00。`build_starry_apps_plan()` 选择四架构应用 smoke、NixOS 场景和完整 Clippy；实际启动时间仍受 GitHub 调度及执行资源影响。

定时任务设置 `save_cache=true`，允许声明了 `cache_key` 的矩阵行保存 Rust cache。这里的缓存配置与主 CI 的分支条件相互独立。

### 1.2 手动运行

`run_clippy_all` 默认关闭。启用时，规划步骤向 `ci_plan.py` 传入 `--boolean-input run_clippy_all`；`_is_enabled()` 将事件匹配和布尔输入匹配作为可替代的选中条件。

无论是否启用 Clippy，四架构 smoke 和 NixOS 检查仍保留。手动运行设置 `save_cache=false`，但不因此禁用已配置的缓存恢复。

## 2. 检查内容

`starry-apps.toml` 使用 `phase=starry_apps`。它没有主 CI 的 `static` 和 `test` 阶段划分，所有选中项进入同一个矩阵。

### 2.1 应用 smoke

四个架构分别调用任务工具的应用入口，`--all` 由应用运行器展开，而不是由 GitHub 工作流逐个枚举应用。

| 架构 | 矩阵命令 | 环境 |
| --- | --- | --- |
| x86_64 | `cargo xtask starry app qemu --all --arch x86_64` | `ubuntu-base` |
| AArch64 | `cargo xtask starry app qemu --all --arch aarch64` | `ubuntu-base` |
| RISC-V | `cargo xtask starry app qemu --all --arch riscv64` | `ubuntu-base` |
| LoongArch | `cargo xtask starry app qemu --all --arch loongarch64` | `ubuntu-base` |

这些行设置 `apk_region=us` 和 `container_preflight=qemu-user`。应用检查的结果不能替代主 CI 的系统套件、板卡测试或软件包发布结果。

### 2.2 NixOS 与 Clippy

NixOS 行使用 `ubuntu-host`，命令在宿主上启动 `docker.io/nixos/nix:2.33.1` 容器，挂载工作区后进入 `nix develop`，执行 `cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix`。该行的 `timeout_minutes=45` 限制整个 job，包括容器与开发环境准备。

完整 Clippy 行执行 `cargo xtask clippy --all`。虽然手动输入的描述提到在应用检查前审计，实际 `checks` job 没有为这一行建立单独的 `needs` 门禁；它与应用行处于同一个矩阵，可以并行执行。

## 3. 调度与结果

`plan` 在一个 GitHub-hosted runner 上完成 checkout 和规划，`checks` 等待规划成功后调用 `reusable-check-matrix.yml`。应用矩阵不下载主 CI 的 `tg-xtask-bin` artifact，直接使用各行声明的命令。

### 3.1 并发策略

workflow 使用 `starry-apps-${{ github.ref }}` 作为 concurrency group，设置 `cancel-in-progress=false`。这避免新事件主动取消同组正在运行的任务，但没有声明主 CI 的 `queue: max` 保留策略，不能据此承诺每次待执行事件都完整排队。

矩阵设置 `fail_fast=false`。一个应用或架构失败，不会通过矩阵 fail-fast 取消其他行；其余行仍可产生独立结果。

### 3.2 权限与失败

工作流只声明 `contents: read` 和 `packages: read`，没有软件包或站点发布步骤。规划失败会阻止整个应用矩阵，环境准备失败和应用命令失败则记录在对应行中。

主 CI 和应用工作流不存在相互等待关系。确认应用版本可运行时，需要检查本工作流的具体提交和矩阵结果，不能只引用主 CI 的绿色状态。
