---
sidebar_position: 6
sidebar_label: "应用验证"
---

# 应用验证

`.github/workflows/starry-apps.yml` 为 Starry 应用提供独立于主 CI 的定时和手动验证。工作流使用 `ci_plan.py` 的 `starry-apps` 模式只读取专用检查清单，再调用共享矩阵执行器；它不使用主 CI 的 PR 变更范围或 push/PR 去重结果，也不执行共享 `benchmarks.toml` 中的性能 check。

## 1. 事件与选择

应用工作流只有 `schedule` 和 `workflow_dispatch` 两种入口。smoke 集合由 `.github/ci/checks/starry-apps.toml` 和布尔输入共同决定；性能 check 由独立的 [基准验证](benchmarks.md) 工作流承担，不在本工作流中规划。

### 1.1 定时运行

cron 表达式为 `0 18 * * *`，按 UTC 每日 18:00 调度，对应北京时间次日 02:00。`build_starry_apps_plan()` 选择四架构应用 smoke、NixOS 场景和完整 Clippy；实际启动时间仍受 GitHub 调度及执行资源影响。

定时运行检出 `dev`，规划时固定其提交 SHA；后续矩阵和结果汇总使用同一版本。

定时任务设置 `save_cache=true`，允许声明了 `cache_key` 的矩阵行保存 Rust cache。这里的缓存配置与主 CI 的分支条件相互独立。

### 1.2 手动运行

`run_clippy_all` 默认关闭。启用时，规划步骤向 `ci_plan.py` 传入 `--boolean-input run_clippy_all`；`_is_enabled()` 将事件匹配和布尔输入匹配作为可替代的选中条件。

未指定测试项时，四架构 smoke 和 NixOS 检查全部保留；指定 `apps` 或 `arches` 时只规划所选范围。`run_clippy_all` 仍是独立开关。手动运行设置 `save_cache=false`，但不因此禁用已配置的缓存恢复。

手动运行检出所选分支在触发时的提交 `github.sha`，再把实际 SHA 传给全部矩阵行；结果中的 `tested revision` 是本次测试的源码版本。在 Actions 页面选择 **Starry Apps → Run workflow**，选择功能分支并按需启用 `run_clippy_all`。也可以执行：

```bash
gh workflow run starry-apps.yml --ref <branch> -f run_clippy_all=true
```

## 2. 检查内容

`starry-apps.toml` 使用 `phase=starry_apps`，只提供 smoke、NixOS 和 Clippy 行，没有主 CI 的 `static` 和 `test` 阶段划分。共享的 `benchmarks.toml` 使用 `phase=benchmark`，由基准工作流通过 `build_benchmarks_plan()` 读取。

### 2.1 应用 smoke

四个架构分别调用任务工具的应用入口，`--all` 由应用运行器展开，而不是由 GitHub 工作流逐个枚举应用。

| 架构 | 矩阵命令 | 环境 |
| --- | --- | --- |
| x86_64 | `cargo xtask starry app qemu --all --arch x86_64` | `ubuntu-base` |
| AArch64 | `cargo xtask starry app qemu --all --arch aarch64` | `ubuntu-base` |
| RISC-V | `cargo xtask starry app qemu --all --arch riscv64` | `ubuntu-base` |
| LoongArch | `cargo xtask starry app qemu --all --arch loongarch64` | `ubuntu-base` |

这些行设置 `apk_region=us` 和 `container_preflight=qemu-user`。应用检查的结果不能替代主 CI 的系统套件、板卡测试或软件包发布结果。

`--all` 按顺序运行当前架构下的应用。某个应用构建、准备或运行失败时，记录应用名和完整错误链，继续运行后续应用，最后汇总成功、失败以及能力不足而跳过的数量；任何实际失败都会使该架构命令非零退出。全部应用均因能力不足而跳过时也不会报告成功。显式指定 `-t <应用>` 时直接返回该应用的结果。

当前自动验收暂时排除以下已知失败项，保留应用代码与手动完整入口：

| 应用 | 自动执行范围 | 排除原因与记录 |
| --- | --- | --- |
| `nix`（x86_64/AArch64） | `test_nix.sh --ci` 保留预装检查、无沙箱、nixpkgs | 沙箱阶段依赖未完成的 UID/GID 映射；显式 `NIX_SANDBOX_TEST_SKIPPED`，汇总 3 通过、1 跳过 |
| `py-sci2`（x86_64/AArch64） | `run-pysci2.sh --ci` 保留七个模块 | Matplotlib/Statsmodels 受 NumPy 多维索引异常影响；显式两项跳过，成功为 `7/7`，跳过不计通过 |
| `web-browser-firefox` | 通过 `apps/.ignore` 移出默认发现，显式 `-t` 可选 | 导航后渲染观察超时；不进入批次，不计为通过 |

Nix 和 py-sci2 的客户机脚本不带 `--ci` 时仍执行完整检查。恢复自动验收前，须分别
验证映射语义、原两科学计算模块及 Firefox 完整渲染判定。单独 NixOS Stage-2、
其他科学计算模块和已修复的基本功能测试继续保留。

smoke 矩阵只来自 `starry-apps.toml`；`build_starry_apps_plan()` 返回 `starry_apps_matrix`，其中只有应用 smoke、NixOS 和可选 Clippy 行。性能 QEMU 与板卡矩阵已经迁移到 [基准验证](benchmarks.md)，因此应用工作流不再准备 `tg-xtask` artifact，也不会因为性能用例失败而影响应用 smoke 的调度边界。

### 2.2 NixOS 与 Clippy

NixOS 行使用 `ubuntu-host`，命令在宿主上启动 `docker.io/nixos/nix:2.33.1` 容器，挂载工作区后进入 `nix develop`，执行 `cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix`。该行的 `timeout_minutes=45` 限制整个 job，包括容器与开发环境准备。

完整 Clippy 行执行 `cargo xtask clippy --all`，与应用行处于同一个矩阵，可以并行执行；它没有单独的 `needs` 门禁，不会阻止应用行启动。

### 2.3 手动指定应用和架构

在 **Starry Apps → Run workflow** 选择分支，使用两个可选输入：

- `apps`：逗号分隔的应用名，与 `cargo xtask starry app qemu -t` 一致，
  例如 `cpu-video-test`、`go-lang`、`qemu/findutils`。
- `arches`：逗号分隔的架构名，可选 `x86_64`、`aarch64`、`riscv64`、`loongarch64`。

两个输入留空时执行原完整矩阵。只填 `apps` 时，为所选应用自动选择它提供标准
`qemu-<arch>.toml` 的架构；只有预构建脚本、使用默认 QEMU 配置的应用按全部架构
规划。只填 `arches` 时执行这些架构的默认应用批次，选择包含 x86_64 时保留独立
NixOS 检查。两者都填时，每个应用必须支持指定的每个架构，否则规划失败。

```bash
# 在指定分支上跑完整 Apps。
gh workflow run starry-apps.yml --ref <branch>

# 只测试两个架构上的视频应用。
gh workflow run starry-apps.yml --ref <branch> \
  -f apps=cpu-video-test -f arches=aarch64,riscv64

# 只跑 AArch64 的 Go 和 top。
gh workflow run starry-apps.yml --ref <branch> \
  -f apps=go-lang,top -f arches=aarch64

# 只测试 RISC-V 默认应用批次。
gh workflow run starry-apps.yml --ref <branch> -f arches=riscv64
```

每个明确指定的应用/架构组合使用独立 job，复用正常 Apps 的容器、运行器、缓存、
网络区域及预检查，直接调用原应用命令；失败不取消其他 job，最终仍汇总失败。
`apps=nixos` 使用原 NixOS 容器流程，只支持 x86_64；显式选择会执行原入口，即使
应用在默认 `apps/.ignore` 中。需要特殊能力或非标准 QEMU 配置的应用仍受原运行器
约束，不能据此保证任意应用在所有架构上运行。

不存在的应用、无效架构或不支持的组合会明确报错。`run_clippy_all=true` 可额外
运行完整 Clippy。定时触发不接受应用/架构筛选，始终检出 `dev` 并执行完整矩阵；
手动触发固定所选分支在触发时的实际提交。输出见各 job 的 `Run command` 日志。

## 3. 调度与结果

`plan` 在一个 GitHub-hosted runner 上完成 checkout 和规划，`checks` 等待规划成功后调用 `reusable-check-matrix.yml`。应用矩阵不下载主 CI 的 `tg-xtask-bin` artifact，直接使用各行声明的命令。

### 3.1 并发策略

workflow 使用 `starry-apps-${{ github.ref }}` 作为 concurrency group，设置 `cancel-in-progress=false`。这避免新事件主动取消同组正在运行的任务，但没有声明主 CI 的 `queue: max` 保留策略，不能据此承诺每次待执行事件都完整排队。

矩阵设置 `fail_fast=false`，一个架构失败不会取消其他行；每行内的 `--all` 同样继续执行其他独立应用。任务整体超时、执行环境故障或人为取消仍可能阻止完整运行，须结合逐项 `RUN` 和最终汇总确认实际覆盖范围。

### 3.2 权限与失败

工作流只声明 `contents: read` 和 `packages: read`，没有软件包或站点发布步骤。规划失败会阻止整个应用矩阵，环境准备失败和应用命令失败则记录在对应行中。

主 CI 和应用工作流不存在相互等待关系。确认应用版本可运行时，需要检查本工作流的具体提交和矩阵结果，不能只引用主 CI 的绿色状态。
