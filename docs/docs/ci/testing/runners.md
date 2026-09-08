---
sidebar_position: 4
sidebar_label: "执行环境"
---

# 执行环境

TGOSKits 使用 GitHub-hosted runner 和组织自托管 runner 执行 CI。`.github/ci/runner-profiles.toml` 定义七种调度 profile，检查清单选择 profile，`ci_plan.py` 展开为 `matrix.runs_on` 和环境字段。本页区分配置中的任务要求、GitHub 注册的 runner 实例和实际物理机器，避免把三者混为一谈。

## 1. 配置清单

profile 是可复用的调度配置，不是一台机器。多个 profile 可以使用同一种 GitHub-hosted runner，也可能有多台自托管实例满足同一组标签；实际分配仍受仓库权限、runner group、标签和可用容量约束。

### 1.1 完整 profile

下表逐项对应 `runner-profiles.toml`。自托管标签列表表示需要同时满足的条件，`environment` 是本项目的执行环境字段，不是 GitHub Environment 审批配置。

| Profile | `runs_on` | `environment` | owner 条件 | 其他 owner 的行为 |
| --- | --- | --- | --- | --- |
| `ubuntu-base` | `ubuntu-latest` | `base` | 无 | 在自己的仓库上下文运行 |
| `ubuntu-host` | `ubuntu-latest` | `host` | 无 | 在自己的仓库上下文运行 |
| `ubuntu-axvisor-lvz` | `ubuntu-latest` | `axvisor-lvz` | 无 | 在自己的仓库上下文运行 |
| `qcs` | `self-hosted, linux, qcs` | `host` | `self_hosted_owner=rcore-os` | 回退为 `ubuntu-latest` 和 `base` |
| `board` | `self-hosted, linux, board` | `host` | `required_owner=rcore-os` | 不启用该检查 |
| `kvm-intel` | `self-hosted, linux, intel, kvm` | `host` | `required_owner=rcore-os` | 不启用该检查 |
| `kvm-amd` | `self-hosted, linux, amd, kvm` | `host` | `required_owner=rcore-os` | 不启用该检查 |

两个 KVM profile 还声明 `require_kvm=true`。`qcs` 和 `board` 没有这一声明，不能仅因它们是自托管 runner 就认为已经满足 KVM 要求。`qcs` 在配置中只是标签，不能由此推断云厂商、CPU 型号或机器规格。

### 1.2 环境含义

`_container_image()` 根据当前执行仓库生成镜像名称。`ubuntu-base` 和 `ubuntu-host` 虽然都请求 `ubuntu-latest`，但前者在 job 容器中运行命令，后者直接使用宿主环境。

| 环境 | job 容器 | 需要准备的内容 |
| --- | --- | --- |
| `host` | 不设置容器镜像 | 自托管机器预装依赖，或由托管 job 自行准备 |
| `base` | `ghcr.io/<repository>-container:latest` | 当前仓库的 base 镜像及读取权限 |
| `axvisor-lvz` | `ghcr.io/<repository>-container-axvisor-lvz:latest` | 当前仓库的 AxVisor LoongArch LVZ 镜像及读取权限 |

QEMU 的目标架构不等于 runner 的宿主架构。例如 AArch64 测试可以由交叉编译和模拟器执行，`linux` 标签也不等价于 `x64` 标签。配置没有声明的 CPU 架构、核数和内存，需要从实际机器核实。

### 1.3 不经过 profile 的 job

并非所有 runner 分配都来自检查清单。以下准备或配套 job 在工作流中直接声明 `runs-on: ubuntu-latest`，也应计入资源占用。

| 工作流与 job | 作用 | 调度关系 |
| --- | --- | --- |
| `ci.yml / plan_ci` | 去重、清理旧 run、配置校验和规划 | 主 CI 矩阵之前执行；跨仓 PR 不分配 |
| `starry-apps.yml / plan` | 定时或手动应用矩阵规划 | 应用矩阵之前执行 |
| `container-publish.yml / publish` | 构建并发布测试容器 | 独立工作流，不是主 CI 的前置 job |
| `docs.yml / build` | 构建文档并上传 Pages artifact | 文档工作流的构建阶段 |
| `docs.yml / deploy` | 部署 Pages artifact | 等待文档构建成功 |
| `release-plz.yml / release-plz-release` | 发布符合条件的软件包及相关发布资产 | 独立于主 CI 和 release PR job |
| `release-plz.yml / release-plz-pr` | 创建或更新软件包发布 PR | 与实际发布 job 没有 `needs` 依赖 |

这些准备和发布 job 不计入检查清单的 profile 数量。旧任务清理已经复用 `plan_ci`，没有单独的清理 runner；不要为同一职责重新添加 `ci-pr-cleanup.yml`。

## 2. 任务分配

`.github/ci/checks/*.toml` 通过单项 `runner`、文件 `default_runner` 或全局默认值选择 profile。下表统计当前清单的静态 check 声明，便于核对任务落点；不是本次矩阵行数，更不是机器数量。

### 2.1 分配总览

主 CI 与 Starry Apps 分开载入清单。PR 过滤、事件开关、owner 条件和精确 suite 展开都会改变真正运行的行，不能把静态计数当成每次 CI 的资源需求。

| Profile | 主 CI 声明数 | Starry Apps 声明数 | 任务范围 |
| --- | --- | --- | --- |
| `qcs` | 10 | 0 | Formatting/publish 预检，Workspace Clippy/std，ArceOS 四架构套件，AxVisor AArch64/RISC-V 场景 |
| `ubuntu-base` | 6 | 5 | sync-lint、qperf、Starry 四架构套件；定时完整 Clippy 和四架构应用 smoke |
| `ubuntu-host` | 0 | 1 | Starry NixOS x86_64 Stage-2 |
| `ubuntu-axvisor-lvz` | 1 | 0 | AxVisor LoongArch QEMU 套件 |
| `kvm-intel` | 3 | 0 | VMX、ACPI/MP/OVMF 和 AxLoader UEFI HTTP 启动 |
| `kvm-amd` | 2 | 0 | SVM、ACPI/OVMF 和 PCI 枚举 |
| `board` | 11 | 0 | Starry 原生板卡测试和 AxVisor 板卡 guest 场景 |

修改清单时应同步更新这张表。`qcs` 在普通外部 fork 上回退为托管环境，表中仍按声明的 profile 分类；它不表示外部 fork 可以使用组织的 QCS 机器。

### 2.2 QCS 和托管任务

`qcs` 承担较通用的构建与运行工作，但不是“所有测试的默认机器”。`arceos.toml`、`workspace.toml` 和 `axvisor.toml` 使用它作为文件默认值，单项仍可以覆盖；全局默认其实是 `ubuntu-base`。

ArceOS 的四个架构聚合检查使用 QCS。AxVisor 的两个 AArch64 检查分别覆盖 smoke/virtio-blk/axtest/timer stress 和 panic/HTTP 控制面/浏览器控制台/IVC，另一个 RISC-V 检查覆盖 smoke、IPI 与 panic 模式。`static.toml` 只有 formatting/publish 使用 QCS，sync-lint 走托管 base 环境。

Starry 的四个架构 QEMU 套件使用托管 base 环境，并在各自行中追加内核测试；不能因为它们运行内核就推断必须申请自托管 runner。`workspace.toml` 中的 qperf 同样显式选择 `ubuntu-base`。

### 2.3 KVM 任务

Intel 和 AMD 使用不同标签，测试命令也分别选择 VMX 或 SVM 场景。`axvisor.toml` 中的实际检查分配如下。

| Profile | Check ID | 主要内容 |
| --- | --- | --- |
| `kvm-intel` | `test-axvisor-self-hosted-x86-64-vmx-smoke-pci-enumeration` | VMX smoke、通用 PCI 枚举 |
| `kvm-intel` | `test-axloader-http-smoke` | AxLoader 的 x86_64 UEFI HTTP 启动 |
| `kvm-intel` | `test-axvisor-x86-64-acpi-direct-and-ovmf-boot-vmx` | direct ACPI、MP fallback、OVMF ACPI |
| `kvm-amd` | `test-axvisor-self-hosted-x86-64-svm-smoke-acpi` | SVM smoke、direct ACPI、OVMF ACPI |
| `kvm-amd` | `test-axvisor-x86-64-pci-enumeration-svm` | SVM 通用 PCI 枚举 |

`require_kvm` 只让执行器检查 `/dev/kvm` 可读写，并未完整验证 CPU 虚拟化特性、嵌套虚拟化、固件镜像或 guest 能力。预检通过后仍可能在特定启动场景失败，应以相应 case 的日志定位。

### 2.4 板卡任务

`board` 是执行板卡测试命令的 Linux runner，不是板卡型号标签。具体目标由 `cargo xtask ... test board --board ...`、测试配置和板卡服务决定；不同板卡共享 `board` 标签，不代表互相可替换。

| 目标板卡或场景 | Starry 清单 | AxVisor 清单 |
| --- | --- | --- |
| OrangePi 5 Plus | 原生套件 | Linux guest、StarryOS guest、AXIVC Zephyr-Starry benchmark |
| OrangePi 5 Plus robot | 原生套件 | StarryOS guest 和 Linux guest，分别独立声明 |
| AKA-00 SG2002 | 原生套件，启用 Wi-Fi 凭据 | 未声明对应行 |
| VisionFive 2 | 原生套件 | 未声明对应行 |
| JL LSGD2K10 | 原生套件 | 未声明对应行 |
| ROC-RK3568-PC | 未声明对应行 | Linux guest |
| Phytium Pi | 未声明对应行 | Linux guest |
| ASUS NUC15CRH | 未声明对应行 | Linux guest |

这张表描述已注册的测试目标，不证明板卡当前在线或可用。维护时要分别检查 runner 是否空闲、板卡服务是否可达、对应板卡是否可取得会话，以及测试资产是否齐备；增加 Linux runner 数量不会自动增加物理板卡容量。

## 3. 宿主要求与容量

profile 只描述调度条件，实际任务还依赖工具链、模拟器、存储和网络。`reusable-check-matrix.yml` 的预检是早期诊断，不是完整的机器验收程序。

### 3.1 预检范围

执行器目前进行以下检查。排查缺失依赖时，要区分“代码明确检查的内容”和“实际命令仍然需要、但预检未覆盖的内容”。

| 环境或开关 | 执行器明确检查 | 仍需按任务核实 |
| --- | --- | --- |
| 自托管 | `rustc`、`cargo` 存在并输出版本 | 固定工具链、目标、系统 QEMU、链接器、磁盘与网络 |
| `require_kvm=true` | `/dev/kvm` 可读写 | VMX/SVM、嵌套能力及具体 guest 启动条件 |
| `container_preflight=qemu-user` | Git、工作目录及四架构用户态 QEMU | 系统模拟器和任务自身的其他依赖 |
| `container_preflight=full` | 上述检查，加四架构 musl 编译器 | 任务需要的固件、rootfs、特殊模拟器和资产 |
| `container_preflight=none` | 不执行容器预检步骤 | 由该任务的命令负责准备或校验 |

自托管行禁止非空 `cache_key`。不能为了套用托管缓存策略而开启 `rust-cache`，也不能把容器或缺少 fork secrets 当作保护宿主机器的充分隔离措施。缓存、artifact 和凭据传递详见[矩阵执行](execution.md)。

### 3.2 调度上限

`max_parallel=256` 是单个矩阵的并发上限，不是 runner 预留数。主 CI 的四个测试分组独立调度，合计负载还会叠加其他 run、定时应用和共享组织中的其他任务。

同一个 profile 下出现 queued 时，先区分 runner group 权限、标签不匹配、离线、实例忙碌和底层硬件不可用。`main`、`dev` 的 workflow 队列是另一层限制；降低矩阵并发或添加 runner 都不会消除等待前一个主分支 run 的约束。

同仓 push/PR 去重减少重复矩阵，旧任务清理复用 `plan_ci`，artifact 减少任务工具重复编译。这些机制优化的是不同开销，不应通过取消主分支每次提交的验证或复用未经验证的旧结果来进一步减少占用。

## 4. 实际机器清单

仓库没有在 `runner-profiles.toml` 中声明实例名称、数量、CPU、内存、磁盘或在线状态，因此本页不为这些信息填写猜测值。实际盘点应由有权限的维护者查询 GitHub 注册信息，并与主机及板卡资产记录核对。

### 4.1 查询注册与访问权限

组织级 runner 和仓库级 runner 必须分别查询。仓库级接口返回空列表，不能据此认定组织没有向仓库共享 runner；403 表示当前凭据无权读取，不表示实例不存在。

```bash
gh api --paginate 'orgs/rcore-os/actions/runners?per_page=100' \
  --jq '.runners[] | {id, name, os, status, busy, labels: [.labels[].name]}'
gh api --paginate 'repos/rcore-os/tgoskits/actions/runners?per_page=100' \
  --jq '.runners[] | {id, name, os, status, busy, labels: [.labels[].name]}'
gh api --paginate 'orgs/rcore-os/actions/runner-groups?per_page=100' \
  --jq '.runner_groups[] | {id, name, visibility, allows_public_repositories,
    restricted_to_workflows, selected_workflows}'
```

这些命令只读，不扩权、不修改 runner group。查询 group 后还需核对它向哪些仓库开放，不能只看组名就推断 `tgoskits` 有访问权限。应使用组织管理员授权的凭据，不在文档或日志中记录 token。

### 4.2 盘点字段

GitHub 的实例数据只回答部分问题。尤其不能把 runner 实例数直接当作物理机器数，也不能把一个时刻的 `busy=false` 当作长期可用容量。

| 要记录的信息 | 事实来源 | 维护注意事项 |
| --- | --- | --- |
| 实例 ID、名称、OS、标签 | GitHub runner 列表 | 与 profile 的标签逐项匹配 |
| online/offline、busy | 查询时的 GitHub 状态 | 附采集时间，属于动态状态 |
| runner group、仓库和工作流权限 | 组织访问配置 | 关系到是否真正可以调度和安全边界 |
| 物理主机、CPU、内存、磁盘 | 主机或资产管理记录 | 区分多个实例是否共享同一主机资源 |
| VMX/SVM 与嵌套虚拟化 | 主机配置和对应测试结果 | `/dev/kvm` 存在不代表全部场景可用 |
| 板卡型号、会话与服务容量 | 板卡服务和资产记录 | 不等同于 `board` runner 实例数量 |

如果要在后续维护中发布固定机器清单，应先取得上述事实并明确更新责任；私有地址、凭据和敏感运维细节不应放进公开文档。当前能由仓库可靠维护的是 profile、检查落点和环境契约，实机容量须单独核实。
