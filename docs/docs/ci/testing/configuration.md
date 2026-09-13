---
sidebar_position: 5
sidebar_label: "配置维护"
---

# CI 配置维护

检查能力放在 `.github/ci/checks/`，机器和环境能力放在 `.github/ci/runner-profiles.toml`。`ci_plan.py` 在执行前校验二者的契约，新增检查应沿用这个边界，而不是继续向工作流堆叠独立 job。

## 1. 检查清单

`load_catalog()` 读取 `MAIN_MANIFESTS` 或 `STARRY_APPS_MANIFEST`，展开 profile 后执行字段、影响范围、artifact 和 suite 注册校验。仅把新的 TOML 文件放进目录，不会自动进入 `MAIN_MANIFESTS`。

### 1.1 文件与字段

manifest 使用 `schema_version = 3`，声明 `phase`、`group` 和非空 `check` 数组。主 CI 使用 `static`、`test`，定时应用清单使用 `starry_apps`；旧 schema 和未知字段会被拒绝。

| 字段 | 作用与约束 |
| --- | --- |
| `id`、`name`、`command` | 必填且非空；`id` 在整个目录载入结果中唯一，`name` 用于显示 |
| `default_runner`、`runner` | 文件默认 profile 和单项覆盖；省略时使用 `ubuntu-base` |
| `impact_targets`、`impact_packages` | 声明平台或软件包影响；非 Workspace 的 OS 测试必须至少声明一类 |
| `pull_request_command` | 非全量 PR 选择时替换普通命令，不影响 push 的命令 |
| `fetch_depth` | 非负深度或 `full`；默认 `1` |
| `timeout_minutes` | 整个 job 的正整数超时；默认 `360` |
| `cache_key` | 默认空；自托管行不允许非空值 |
| `upload_xtask_bin_artifact`、`download_xtask_bin_artifact` | 任务工具 producer/consumer，单项不能同时启用两者 |
| `xtask_bin_artifact_name` | 默认 `tg-xtask-bin`，消费者必须匹配 producer |
| `container_preflight` | `none`、`qemu-user` 或 `full`，省略时按环境推导 |
| `events`、`enable_boolean_input` | 满足声明事件或启用布尔输入时选中，用于定时/手动检查 |
| `required_base_branch` | 仅匹配指向指定 base 的 PR |
| `wifi_secrets`、`apk_region` | Wi-Fi 凭据开关和 APK 区域环境值 |

`runs_on`、`environment`、owner 和 `require_kvm` 不能直接写进 check；它们来自 runner profile。这样同一种机器的约束不会在各 OS 清单中复制并逐渐分叉。

### 1.2 suite 注册

能运行某类套件的检查通过 `[[check.suite]]` 声明能力。下面是现有 ArceOS x86_64 注册的简化摘录，用于说明字段关系，不应再添加一个同名检查。

```toml
[[check]]
id = "test-arceos-x86-64-qemu"
impact_targets = ["arceos:x86_64"]
name = "QEMU x86_64 · Suites"
command = "cargo xtask arceos test qemu --arch x86_64"

[[check.suite]]
kind = "arceos-qemu"
arch = "x86_64"
```

`kind` 必须属于 `ci_suite.py` 的 `SUPPORTED_SUITE_KINDS`。QEMU 注册要求 `arch`，board 注册要求 `board`；`cases` 可以限制覆盖的 case。注册用于把套件路径映射到已有运行能力，不只是给 Actions 增加一个标签。

### 1.3 增加检查

维护时先确认原始测试入口存在且具有确定的成功、失败含义，再修改 CI。推荐按下面的顺序核对执行链路。

1. 在对应 OS 或组件测试入口证明目标行为，确认命令能通过 `cargo xtask` 被发现和执行。
2. 选择已有 profile；只有机器能力确实不同才新增 profile。
3. 在所属 manifest 添加唯一 `id`、显示名称、命令及影响范围；需要精确 suite 路由时同步注册 `check.suite`。
4. 核对 artifact、缓存、超时和凭据。主 CI 必须恰好有一个静态阶段的任务工具 producer，不能让新消费者引用不存在的 artifact。
5. 修改路由逻辑时补充必要的选择回归，并运行 `scripts/test` 下的 CI 配置回归检查受影响测试。

检查数量不是覆盖完整性的证明。一个新 case 即使能在本地运行，没有对应 CI suite 注册时仍可能在精确规划阶段失败；反过来，注册一个名称也不能代替真实测试入口。

## 2. runner 与环境

`ci_runner_profiles.py` 的 `load_runner_profiles()` 校验 profile，`ci_plan.py` 的 `_is_enabled()` 和 `_normalize_check()` 分别决定是否启用检查及其最终环境。它们处理当前执行仓库的配置，不把主仓 job 转发到 fork。

### 2.1 profile 选择

七种 profile 的标签、环境、任务分配和实机核查方法统一列在[执行环境](runners.md)。新增配置时优先复用已有 profile，不根据一次 job 的显示名称推断机器型号或容量。

`self_hosted_owner` 配合 `fallback_environment` 表示其他 owner 可以转到自己的托管环境；`required_owner` 则表示不匹配时不启用检查。新增 profile 时必须明确选择这两种语义，不能把板卡或 KVM 检查无条件转到普通托管环境。

来源门禁与 owner 路由是两层不同判断：主仓先拒绝跨仓 PR；允许运行的仓库再按自己的 owner 选择 profile。不能把 PR 的源 owner 冒充工作流 owner，也不能把 GitHub-hosted 等同于 fork 自己的资源。

### 2.2 缓存约束

自托管 `cache_key` 必须为空，planner 会拒绝非空值。这保留持久化 runner 的现有工具链和 Cargo 状态，不能为了加速而直接复制托管行的缓存设置。

托管行只有显式非空键才启用 `Swatinem/rust-cache`。`save_cache` 决定是否保存，不决定是否尝试恢复；主 CI 允许 `main`、`dev` 的 push 保存，Starry Apps 则只在定时事件允许保存。任务工具 artifact 的生产与恢复规则见[矩阵执行](execution.md)。
