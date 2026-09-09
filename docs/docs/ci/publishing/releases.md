---
sidebar_position: 3
sidebar_label: "软件包发布"
---

# 软件包发布

`.github/workflows/release-plz.yml` 使用 Release-plz 执行软件包发布和发布 PR 维护。两项工作由不同 job 完成，使用根目录 `release-plz.toml`，不属于主 CI 的 `Preflight` 或测试矩阵。

## 1. 事件与仓库

发布工作流会产生外部写入，其事件条件、工具链和权限与一般检查工作流不同。

### 1.1 触发条件

自动入口为 `dev` 分支 push，没有路径过滤。`workflow_dispatch` 提供手动入口；两个 job 都要求 `github.repository == 'rcore-os/tgoskits'`，因此 fork 不能仅凭继承工作流文件启用主仓发布 job。

job 条件允许手动事件，或 ref 名称为 `dev`。手动事件并未进一步限制到 `dev`，所以手动运行时的选定引用会影响实际处理内容，不能把它当成无副作用的状态查询。

### 1.2 队列与工具链

concurrency group 为 `release-plz-${{ github.ref }}`，配置 `queue: max`。同 ref 的发布工作流按自己的队列运行，不与主 CI 的 concurrency group 共用队列。

`release-plz-release` 设置 `RUSTUP_TOOLCHAIN=stable`，通过 `dtolnay/rust-toolchain@stable` 准备发布工具链。`release-plz-pr` 使用 `rustup show active-toolchain` 安装并选择 `rust-toolchain.toml` 固定的 nightly，再通过 `GITHUB_ENV` 设置 `RUSTUP_TOOLCHAIN`，使仓库外的临时基线构建也使用同一工具链。API 检查会编译依赖，不能统一强制使用 stable；例如 `x86_64` 的 nightly 功能依赖较新的 `Step` 接口。

## 2. 发布任务

`release-plz-release` 和 `release-plz-pr` 之间没有 `needs`，也没有依赖主 CI 的检查结果。它们可以独立调度、分别成功或失败。

### 2.1 实际发布

`release-plz-release` 以完整 Git 历史 checkout，并设置 `persist-credentials=false`。随后先执行 `cargo publish --workspace --dry-run --no-verify`，再通过 `release-plz/action@v0.5` 运行 `command: release`。

前置命令只做发布预检，不真正上传软件包，也不验证软件包构建。真正的发布由后续 Release-plz 步骤处理；`release_always=false` 要求当前提交关联发布 PR，普通开发提交不会直接触发上传。这样可以先由发布 PR 协调软件包版本和依赖约束，避免新增软件包先于其依赖所需的新接口发布。

### 2.2 发布 PR

`release-plz-pr` 复用 checkout，准备仓库固定的 nightly 后执行 `command: release-pr`，用于创建或更新版本及发布相关改动的 PR。该 job 没有实际发布 job 中的 `Preflight workspace packages` 步骤。

发布 PR 创建成功不等于软件包已经上传；实际发布 job 成功也不等于另一个 job 已创建所需 PR。二者的日志、输出和外部状态分别构成结果证据。

## 3. 配置与权限

根目录 `release-plz.toml` 设置 workspace 级选项，并通过 `[[package]]` 为无法使用默认自动 API 检查的软件包定义例外。未显式覆盖的行为由所用 Release-plz 版本解释。

### 3.1 workspace 选项

当前配置包含以下三个字段，它们影响发布准备和实际发布行为。

| 字段 | 当前值 | 作用 |
| --- | --- | --- |
| `dependencies_update` | `true` | 在发布准备中启用依赖更新 |
| `publish_no_verify` | `true` | 发布时不执行 Cargo 的软件包构建验证 |
| `release_always` | `false` | 仅在当前提交关联发布 PR 时尝试发布 |

Release-plz 通过关联 PR 的分支前缀识别发布 PR，仓库使用默认的 `release-plz-` 前缀。手动触发工作流也不绕过这一条件。配置和预检没有执行主 CI 的 Rust 静态检查、std 测试、QEMU 或板卡测试，不能用发布成功代替这些验证。字段语义见 [Release-plz 官方配置](https://release-plz.dev/docs/config#the-release_always-field)。

### 3.2 API 检查边界

其余软件包继续使用默认的自动 semver 检查。`release-plz.toml` 中的包级 `semver_check=false` 只处理已确认的工具或基线限制，不表示这些软件包已经通过兼容性检查。维护者需要核对公开接口，并在有破坏性变化时使用 `release-plz set-version <package>@<version>` 修正版本及依赖约束。

| 例外软件包 | 默认检查无法运行的原因 |
| --- | --- |
| `arm-gic-driver` | 需要 AArch64，Release-plz 未提供包级 `--target` 配置 |
| `ax-cpu`、`ax-hal`、`axplat-dyn`、`ax-runtime`、`ax-std` | 默认功能并集同时启用互斥的 `tls` 和 `uspace`；应分别检查合法组合 |
| `x86_vcpu` | 检查器收集私有实现后，VMCB 宏生成的深层 `FieldValueEnumTypes` 超过 `cargo-semver-checks 0.50` 的 JSON 解析能力；这些寄存器类型未通过公开 API 暴露 |
| `ax-posix-api`、`ax-libc`、`axvm` | 已发布的 `ax-posix-api 0.5.34` 缺少仓库外构建所需的 `src/ctypes_gen.rs` |
| `axbuild`、`axvisor` | 已发布的 `axbuild 0.5.3` 引用了包外的 review-bench 素材，Axvisor 宿主入口依赖它 |

包级覆盖使用 [Release-plz 官方支持的配置](https://release-plz.dev/docs/config#the-semver_check-field)。历史包缺失文件并未因此得到修复；发布可独立构建的基线后，应重新启用相应检查。目标架构、互斥功能和解析器限制也应在工具支持后重新评估。

### 3.3 外部写入

两个 job 的 GitHub 权限不同。发布使用的 registry token 通过环境传入，不应写入仓库或诊断日志。

| Job | GitHub 权限 | 凭据 | 外部操作 |
| --- | --- | --- | --- |
| `release-plz-release` | `contents: write`、`pull-requests: read` | `GITHUB_TOKEN`、`CARGO_REGISTRY_TOKEN` | 发布软件包及 Release-plz 管理的相关发布资产 |
| `release-plz-pr` | `contents: write`、`pull-requests: write` | `GITHUB_TOKEN`、`CARGO_REGISTRY_TOKEN` | 创建或更新发布分支和 PR |

工作流没有等待主 CI 成功的 `workflow_run` 或跨工作流门禁，也没有声明发布 environment 审批。仓库保护、凭据权限及组织策略可能施加额外约束，但不能仅从主 CI 和发布工作流同属一个仓库推断它们存在依赖。

修改发布文档不需要运行这些有外部写入的步骤。本地站点构建可以检查文档，不能验证 registry 上传、发布 PR 权限或生产发布状态。
