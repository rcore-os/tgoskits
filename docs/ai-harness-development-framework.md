# 基于 AI 的 StarryOS Syscall 与 qperf 自治开发框架说明

## 1. 文档目的

本文档说明当前在 TGOSKits 仓库中构建的 StarryOS 自治开发 harness。该框架面向两个长期目标：

1. 让智能开发代理能够自动发现 StarryOS 与 Linux 标准语义之间的 syscall 行为差异，并生成可复现、可审计的差异报告。
2. 让智能开发代理能够围绕 qperf 性能画像自动定位 StarryOS 热点路径，形成性能分析、可视化、报告生成、瓶颈修复建议和回归验证的闭环。

该框架不是单个测试脚本，而是一套面向代理自动化和人工交互的工作平台。它包含 CLI harness、MCP 工具、Codex skill、本地 Web UI、Docker 隔离执行、结构化报告、qperf flamegraph、热点规则和 PR 工作流约束。

本文档重点展示框架设计、已完成工作量、自动化能力边界、可信性保障和后续演进方向，可作为评审、汇报、交接和继续扩展的基础材料。

## 2. 背景与问题

StarryOS 作为 Linux 兼容目标，需要长期面对两类复杂问题。

第一类是 syscall 语义兼容问题。单个 syscall 的错误返回值、errno、边界条件、文件描述符状态或 flag 校验顺序只要与 Linux 标准行为不一致，就可能导致上层应用异常。这类问题通常有以下特点：

- 问题粒度细，人工 review 难以覆盖全部边界条件。
- 行为差异往往只在特定参数组合下出现。
- 单纯看源码不一定能判断 Linux 真实行为。
- 修复后需要重新对拍验证，避免通过修改测试掩盖问题。

第二类是性能问题。StarryOS 性能优化不能只依靠直觉或局部代码审查，需要能够回答以下问题：

- 当前 StarryOS 运行时热点实际集中在哪里。
- VirtIO、内存映射、调度、锁、copy 路径是否占据异常比例。
- 优化前后热点是否真的下降。
- 是否有可保存、可比较、可视化的证据。

因此，开发框架必须将“发现问题、生成证据、定位代码、提出修复、验证回归、形成 PR”串联为可重复执行的闭环，而不是依赖一次性命令。

## 3. 设计目标

### 3.1 自动化目标

框架支持智能代理默认通过 CLI/MCP 自动调用，不依赖人工点击页面即可完成批量扫描、性能采样和报告读取。

核心自动化能力包括：

- 自动检查 Docker 镜像和工具链是否可用。
- 自动编译 Linux 侧 syscall probe。
- 自动编译目标架构 StarryOS probe。
- 自动构建 StarryOS rootfs。
- 自动将 probe 注入 rootfs。
- 自动启动 StarryOS QEMU。
- 自动解析 Linux 与 StarryOS 输出。
- 自动生成 JSON/Markdown/CSV 报告。
- 自动运行 qperf TCG plugin 采样。
- 自动解析 qperf raw samples 为 folded stack。
- 自动生成 flamegraph SVG。
- 自动提取 top functions、top stacks 和 fix candidates。
- 自动比较两次 profile 的热点变化。
- 自动将结果暴露给 MCP 客户端。

### 3.2 可信性目标

框架不能为了“自动化”降低结果可信度。设计上遵循以下原则：

- Linux probe 输出作为 syscall 语义参考基线。
- StarryOS 构建、rootfs、QEMU、qperf 相关流程必须在 Docker 中执行。
- probe 输出采用稳定 key-value 格式，避免 fd 编号、时间戳、地址、调度时序等非确定信息进入比较。
- syscall 修复必须修实现，不通过削弱 probe 或忽略差异来通过测试。
- qperf 结果作为性能优化证据，而不是直接替代代码审查。
- 性能 fix candidates 是规则辅助，不是自动套用补丁。
- 所有报告产物都落盘，便于复核和 PR 说明。

### 3.3 交互目标

默认路径面向智能代理自动调用；同时提供本地浏览器 UI，便于人工交互式查看和触发任务。

UI 支持：

- Doctor 检查。
- syscall 扫描与修复上下文查看。
- qperf 性能分析。
- flamegraph 查看。
- perf diff 比较。
- 后台任务日志跟踪。
- 已生成报告和 artifact 读取。

### 3.4 PR 工作流目标

框架服务于真实项目协作，因此 PR 流程必须可审计：

- 提交 PR 前对齐 upstream 目标分支。
- 在干净分支上提交修复。
- PR 描述中列明背景、修改、验证命令和关键结果。
- 对 StarryOS 相关构建测试坚持 Docker 执行。
- 对 Rust 逻辑变更运行 targeted clippy。

## 4. 总体架构

框架由六层组成：

```text
┌──────────────────────────────────────────────────────────────┐
│ Agent / Human Operator                                       │
│ - MCP 自动调用                                                │
│ - CLI 直接调用                                                │
│ - Local Web UI 交互调用                                       │
└───────────────────────────────┬──────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────┐
│ Harness Entry Points                                          │
│ tools/starry-syscall-harness/harness.py                       │
│ tools/starry-syscall-harness/mcp_server.py                    │
│ tools/starry-syscall-harness/ui_server.py                     │
└───────────────────────────────┬──────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────┐
│ Docker Execution Boundary                                     │
│ ghcr.io/rcore-os/tgoskits-container:latest                    │
│ StarryOS build / rootfs / QEMU / qperf all run inside Docker  │
└───────────────────────────────┬──────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────┐
│ Syscall Differential Engine                                   │
│ Linux probe vs StarryOS probe                                 │
│ report.json: cases, differences, markers, artifacts           │
└───────────────────────────────┬──────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────┐
│ qperf Performance Engine                                      │
│ qperf plugin / analyzer / folded stack / flamegraph / diff    │
└───────────────────────────────┬──────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────┐
│ Reports And Feedback                                          │
│ JSON / Markdown / CSV / SVG / logs / PR validation evidence   │
└──────────────────────────────────────────────────────────────┘
```

这种架构的关键点是：agent 不直接拼接复杂 StarryOS 构建命令，也不直接操作 QEMU。agent 通过 harness 的稳定接口发起任务，由 harness 负责参数规范化、Docker 重入、输出目录管理、报告生成和 artifact 回收。

## 5. 代码与产物布局

### 5.1 Harness 主目录

```text
tools/starry-syscall-harness/
  README.md
  harness.py
  mcp_server.py
  ui_server.py
  probes/
    syscall_probe.c
  web/
    index.html
    styles.css
    app.js
```

各文件职责如下：

| 文件 | 职责 |
|---|---|
| `harness.py` | CLI 主入口，提供 doctor、discover、perf-profile、perf-diff、ui 子命令 |
| `mcp_server.py` | MCP server，将 harness 能力暴露为智能代理可调用工具 |
| `ui_server.py` | 本地 HTTP server，提供任务 API、报告 API、artifact 文件读取 |
| `probes/syscall_probe.c` | syscall 对拍 probe，输出稳定 `CASE` 行 |
| `web/index.html` | 本地 UI 页面结构 |
| `web/styles.css` | 本地 UI 视觉布局 |
| `web/app.js` | 本地 UI 调用 API、轮询任务、渲染报告和 flamegraph |
| `README.md` | harness 使用说明 |

### 5.2 qperf 相关目录

```text
tools/qperf/
  Cargo.toml
  src/
    profiler.rs
    reg.rs
  analyzer/
    Cargo.toml
    src/main.rs
```

qperf 集成还涉及：

```text
scripts/axbuild/src/starry/perf.rs
```

该文件将 qperf plugin/analyzer 构建、StarryOS 构建、rootfs 准备、QEMU 运行、raw sample 分析和 flamegraph 输出接入 `cargo xtask starry perf`。

### 5.3 报告目录

默认报告目录为：

```text
target/starry-syscall-harness/
```

syscall 扫描产物：

```text
target/starry-syscall-harness/<arch>/latest/
  report.json
  linux.stdout
  linux.stderr
  starry.stdout
  starry.stderr
  qemu.toml
  rootfs-<arch>-probe.img
```

qperf 性能产物：

```text
target/starry-syscall-harness/perf/<arch>/latest/
  report.json
  report.md
  hotspots.csv
  profile.stdout
  profile.stderr
  qperf/
    qemu.toml
    qperf.bin
    stack.folded
    flamegraph.svg
    summary.txt
```

perf diff 产物：

```text
target/starry-syscall-harness/perf-diff/
  report.json
```

UI 任务日志：

```text
target/starry-syscall-harness/ui/jobs/<job-id>.log
```

## 6. CLI 能力

### 6.1 Doctor

```bash
python3 tools/starry-syscall-harness/harness.py doctor
```

Doctor 用于确认当前环境是否具备运行 harness 的基本条件。它检查：

- Docker 命令是否可用。
- 默认镜像是否存在。
- 容器内是否具备 `debugfs`、交叉编译器、QEMU 和 Cargo。

输出为 JSON，便于 agent 解析。

### 6.2 Syscall Discover

```bash
python3 tools/starry-syscall-harness/harness.py discover --arch riscv64
```

该命令完成 Linux-vs-StarryOS syscall 对拍。默认情况下，如果命令在宿主机执行，会自动重入 Docker，Docker 内再执行真实 StarryOS 构建、rootfs 和 QEMU 流程。

支持架构：

- `riscv64`
- `aarch64`
- `loongarch64`
- `x86_64`

关键参数：

| 参数 | 作用 |
|---|---|
| `--arch` | 目标架构 |
| `--timeout` | QEMU probe 超时 |
| `--fail-on-diff` | 如果发现语义差异则返回失败 |
| `--output-dir` | 报告输出目录 |

### 6.3 qperf Profile

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile --arch riscv64 --timeout 20 --format all
```

该命令执行 StarryOS qperf profile，并生成结构化性能报告。

支持架构：

- `riscv64`
- `loongarch64`

关键参数：

| 参数 | 作用 |
|---|---|
| `--format folded` | 只生成 folded stack |
| `--format svg` | 生成 SVG flamegraph |
| `--format all` | 生成 folded stack、报告和 SVG |
| `--freq` | 采样频率 |
| `--max-depth` | 最大栈深 |
| `--mode tb` | 以 QEMU translation block 维度采样 |
| `--mode insn` | 以指令维度采样 |
| `--top` | 报告中展示的热点数量 |
| `--min-percent` | fix candidate 阈值 |
| `--debug` | 使用 debug build，便于符号分析 |
| `--kernel-filter` | 仅保留检测到的 kernel `.text` 范围样本 |

### 6.4 perf-diff

```bash
python3 tools/starry-syscall-harness/harness.py perf-diff \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline \
  --compare target/starry-syscall-harness/perf/riscv64/latest
```

该命令用于比较两次 profile 的 folded stack，输出热点函数百分比变化，用于验证优化是否有效。

### 6.5 Local UI

```bash
python3 tools/starry-syscall-harness/harness.py ui --host 127.0.0.1 --port 8765 --open
```

UI 是可选入口。默认自动化仍建议 agent 走 CLI/MCP；当需要人工观察报告、查看 flamegraph 或手动触发任务时启动 UI。

## 7. Syscall 语义对拍闭环

### 7.1 Probe 输出规范

syscall probe 使用 C 编写，输出形如：

```text
CASE ftruncate_readonly_fd ret=-1 errno=22
CASE pwritev2_writes_data ret=2 errno=0 read_ret=2 read_errno=0 data=5859
```

格式设计要点：

- 每个 case 单独一行。
- 使用 `CASE <name> key=value ...`。
- 不输出随机地址、时间戳、fd 编号等不稳定数据。
- 只输出与语义判断有关的字段。
- Linux 与 StarryOS 侧使用同一份 probe 源码。

### 7.2 Linux 基线

host Linux 执行 probe，产生参考输出：

```text
linux.stdout
linux.stderr
```

除非某个 case 明确记录为架构差异，否则 Linux 输出即为目标语义。

### 7.3 StarryOS 执行

harness 在 Docker 内完成：

1. 使用目标架构交叉编译 probe。
2. 通过 `cargo xtask starry rootfs` 准备 rootfs。
3. 使用 `debugfs` 将 probe 注入 rootfs。
4. 写入专用 QEMU config。
5. 通过 `cargo xtask starry qemu` 启动 StarryOS。
6. 捕获 StarryOS 输出。
7. 解析 `CASE` 行并与 Linux 输出比较。

### 7.4 差异报告

`report.json` 包含：

```json
{
  "arch": "riscv64",
  "linux": {},
  "starry": {},
  "differences": [],
  "markers": {
    "starry_begin": true,
    "starry_end": true
  },
  "artifacts": {}
}
```

字段含义：

| 字段 | 含义 |
|---|---|
| `linux` | Linux probe 结果 |
| `starry` | StarryOS probe 结果 |
| `differences` | 两边不一致的 case |
| `markers` | StarryOS probe 是否完整启动和结束 |
| `artifacts` | rootfs、qemu config、报告路径 |

### 7.5 已验证修复示例

框架已经发现并驱动修复了一个实际 syscall 语义差异：

- case：`ftruncate_readonly_fd`
- 问题：只读普通文件 fd 上执行 `ftruncate` 时 errno 与 Linux 不一致。
- 修复：调整 `sys_ftruncate` 对只读普通文件 fd 的 errno 映射，同时保留 `O_PATH` fd 的 `EBADF` 行为。
- 验证：重新运行 syscall discover 后，riscv64 报告无语义差异，StarryOS begin/end marker 均正常。

该示例说明 harness 不是只生成测试框架，而是已经完成了“发现问题 -> 定位语义 -> 修复实现 -> 回归验证”的完整闭环。

## 8. qperf 性能分析闭环

### 8.1 qperf 接入目标

qperf 集成目标不是简单生成一个 profile 文件，而是为 agent 提供可解析、可比较、可视化、可转化为修复任务的性能证据。

完整路径如下：

```text
StarryOS build
  -> rootfs ready
  -> QEMU with qperf plugin
  -> qperf.bin raw samples
  -> qperf-analyzer resolve
  -> stack.folded
  -> flamegraph.svg
  -> report.json / report.md / hotspots.csv
  -> fix candidates
  -> code inspection and optimization
  -> rerun profile
  -> perf-diff
```

### 8.2 qperf plugin 改造

qperf plugin 侧围绕稳定性和长期自动化做了增强：

- 使用 bounded queue，避免 writer backlog 无界增长。
- QEMU 执行路径使用 non-blocking send，降低采样对 guest 执行的扰动。
- 支持 `max_depth` 限制栈展开深度。
- frame pointer unwind 增加边界检查。
- 采样失败不 panic，计入失败统计。
- 使用 buffered writer 写 raw samples。
- 支持 TB/insn 两种采样模式。
- 支持 kernel text range 过滤参数。
- 支持物理地址 alias 映射，将 QEMU callback 中的物理地址样本映射回 ELF 虚拟地址。
- timeout 结束时尽量保留已采集样本。

### 8.3 qperf analyzer 改造

analyzer 侧围绕自动报告和可视化做了增强：

- 解析 qperf raw sample，输出 folded stack。
- 支持 symbol cache，减少重复地址解析成本。
- 支持 symtab fallback，DWARF 信息不足时仍尽量给出符号。
- 对 trailing partial record 容错，避免 timeout 截断导致整次分析失败。
- 输出 top hottest functions。
- 支持 diff 模式，比较两份 folded stack。
- 支持内置 inferno flamegraph feature，`--format all/svg` 时不依赖容器额外安装外部 flamegraph 命令。
- flamegraph 生成参数已优化为更宽画布、更高 frame、稳定 hash 配色，提升可读性。

### 8.4 Flamegraph 可视化优化

当前 flamegraph 生成配置：

```text
title = "StarryOS qperf Flame Graph"
image_width = 3200
frame_height = 24
font_size = 13
min_width = 0.35
hash = true
deterministic = true
```

这样做的目的：

- 固定宽画布避免热点块挤压在窄面板中。
- 更高 frame 让多层栈更容易辨认。
- 更低 `min_width` 保留更多小热点。
- hash 配色让相邻函数块更容易区分。
- deterministic 保证不同运行之间颜色更稳定，便于比较。

UI 侧也做了配套优化：

- flamegraph iframe 不再强行压缩到面板宽度。
- 外层容器支持横向滚动。
- 前端读取 SVG `width` / `height`，动态调整 iframe 尺寸。
- 如果 SVG 没有生成，页面显示具体原因，而不是只显示空白。

### 8.5 性能报告结构

`perf-profile` 生成的 `report.json` 包含：

```json
{
  "arch": "riscv64",
  "result": "ok",
  "parameters": {},
  "hotspots": {
    "total_samples": 350,
    "top_functions": [],
    "top_stacks": []
  },
  "summary": {},
  "plugin_summary": {},
  "fix_candidates": [],
  "linux_alignment": {},
  "artifacts": {}
}
```

核心字段说明：

| 字段 | 说明 |
|---|---|
| `result` | `ok` 或 `incomplete` |
| `parameters` | profile 参数，便于复现 |
| `hotspots.total_samples` | 样本数 |
| `hotspots.top_functions` | 函数维度热点 |
| `hotspots.top_stacks` | 栈维度热点 |
| `summary` | qperf summary 信息 |
| `plugin_summary` | plugin 侧统计，timeout 时可能不可用 |
| `fix_candidates` | 规则推导的优化候选 |
| `linux_alignment` | 后续与 Linux baseline 或优化前 baseline 对齐的入口 |
| `artifacts` | folded stack、SVG、Markdown、CSV 等路径 |

### 8.6 Fix Candidate 规则

当前实现了一组规则化的性能修复候选，用于把热点函数映射到可能的代码区域和优化策略。

示例规则类型：

- `virtio_vsock_locking`
- `virtio_net_shared_state`
- `virtio_block_sync_queue`
- `lock_contention`
- `copy_overhead`

候选输出包含：

```json
{
  "id": "copy_overhead",
  "trigger": "...memcpy...",
  "samples": 4,
  "percent": 1.14,
  "files": [],
  "strategy": "Inspect repeated buffer copies..."
}
```

该能力对 agent 很重要，因为它把“采样结果”转化成“下一步代码审查方向”，降低从性能数据到修复任务之间的语义鸿沟。

## 9. MCP 集成

MCP server 位于：

```text
tools/starry-syscall-harness/mcp_server.py
```

注册命令：

```bash
codex mcp add starry-syscall-harness -- \
  python3 /home/cg24/tgoskits/tools/starry-syscall-harness/mcp_server.py \
  --repo /home/cg24/tgoskits
```

当前 MCP tools：

| Tool | 功能 |
|---|---|
| `starry_syscall_doctor` | 检查 Docker、镜像和容器工具链 |
| `starry_syscall_discover` | 运行 syscall Linux-vs-StarryOS 对拍 |
| `starry_perf_profile` | 运行 qperf profile 并返回报告 |
| `starry_perf_diff` | 比较两次 profile |
| `starry_harness_ui_command` | 返回本地 UI 启动命令 |

MCP 层的价值在于：agent 不需要知道底层命令细节，只需选择工具并传入结构化参数，即可触发完整流程。

## 10. Codex Skill 集成

本地 skill 位于：

```text
/home/cg24/.codex/skills/starry-syscall-harness/SKILL.md
```

skill 记录了 agent 在使用 harness 时必须遵守的行为约束：

- StarryOS 构建、rootfs、QEMU、syscall probe、qperf profile 均通过 Docker 执行。
- syscall 语义以 Linux probe 输出为参考。
- qperf 热点和 fix candidates 只作为 triage 输入。
- 修复必须改实现，不削弱 probe。
- 性能优化必须 rerun profile，并结合 perf-diff 验证。
- 本地 UI 只作为可选交互入口，默认自动化路径仍是 CLI/MCP。

Skill 的作用不是提供代码，而是把项目经验固化为 agent 的操作准则，降低未来自动化执行时误用命令、绕过 Docker 或错误解释报告的风险。

## 11. 本地 Web UI

### 11.1 启动方式

```bash
python3 tools/starry-syscall-harness/harness.py ui --host 127.0.0.1 --port 8765 --open
```

默认绑定 `127.0.0.1`，避免把本地任务 API 暴露到网络。

### 11.2 API 设计

UI server 使用 Python 标准库实现，无 Node runtime 依赖。

核心 endpoint：

| Endpoint | 方法 | 功能 |
|---|---|---|
| `/` | GET | 静态 UI |
| `/api/status` | GET | repo、镜像、报告、任务状态 |
| `/api/jobs` | POST | 启动后台任务 |
| `/api/jobs/<id>` | GET | 查询任务状态和日志 |
| `/api/report?kind=...` | GET | 读取 syscall/perf/perf-diff 报告 |
| `/api/file?path=...` | GET | 读取 harness artifact 文件 |

### 11.3 后台任务模型

UI 后台任务支持：

- `doctor`
- `discover`
- `perf-profile`
- `perf-diff`

每次任务会生成：

- job id
- command
- status
- returncode
- duration
- output tail
- log file
- report path

为了避免多个重型 StarryOS/QEMU 任务并发抢占资源，UI 当前限制同一时间只运行一个 active job。

### 11.4 Artifact 安全边界

UI 文件读取做了路径限制：

- 只允许读取仓库内路径。
- 只允许读取 harness artifact 根目录下的文件。
- Docker 内报告常见的 `/work/...` 路径会映射回宿主机 repo root。
- 非文件或越界路径会拒绝。

这保证 UI 能读取 Docker 生成的报告，同时不会成为任意文件读取接口。

## 12. Docker 执行模型

所有 StarryOS 相关测试都必须在 Docker 中完成。harness 的 Docker re-exec 模型如下：

1. 用户或 agent 在宿主机运行 harness。
2. harness 检查当前是否在 Docker 内。
3. 如果不在 Docker 内，自动执行：

```bash
docker run --rm \
  -v "$repo_root:/work" \
  -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'python3 tools/starry-syscall-harness/harness.py "$@" ...'
```

4. Docker 内执行真实 StarryOS 构建、rootfs、QEMU 或 qperf。
5. 任务结束后 chown artifact，避免宿主机留下 root-owned 文件。

这种设计同时满足：

- 环境一致性。
- StarryOS 运行隔离。
- agent 命令简洁。
- artifact 可在宿主机读取。
- 不在本地裸跑 StarryOS 测试。

## 13. 自动修复工作流

### 13.1 Syscall 修复闭环

推荐 agent 工作流：

```text
doctor
  -> discover
  -> read report.json
  -> inspect differences
  -> locate syscall implementation
  -> patch implementation
  -> cargo fmt
  -> Docker clippy for changed crate
  -> discover rerun
  -> commit
  -> rebase upstream/dev
  -> push PR branch
```

关键约束：

- `differences` 必须有明确 Linux 参考。
- 修复应限制在对应 syscall 或参数校验路径。
- 不应把 probe 改成 StarryOS 当前行为。
- rerun discover 后才能认为修复完成。

### 13.2 性能优化闭环

推荐 agent 工作流：

```text
perf-profile baseline
  -> read report.json/report.md/flamegraph.svg
  -> inspect top functions/top stacks/fix candidates
  -> inspect code path
  -> patch suspected bottleneck
  -> cargo fmt
  -> Docker clippy for changed crate
  -> perf-profile compare
  -> perf-diff baseline compare
  -> decide whether improvement is real
  -> commit
  -> update PR evidence
```

关键约束：

- 性能修复必须由样本支持。
- fix candidate 只是线索，不能替代代码审查。
- 优化前后必须保留 folded stack 或 report 作为比较依据。
- Flamegraph 用于视觉定位，JSON/CSV 用于机器解析。

## 14. PR 自动化与仓库协作

框架已按真实 PR 流程验证：

- 目标仓库：`rcore-os/tgoskits`
- 目标分支：`dev`
- 工作分支：`fix/starry-syscall-harness`
- PR：`feat(starry): add syscall and qperf harness`

PR 准则：

- 提交前 fetch upstream。
- 在本地分支 rebase 到 `upstream/dev`。
- 使用 `--force-with-lease` 更新已存在 PR 分支。
- PR 描述包含背景、修改、验证命令和关键结果。
- 对 GitHub CLI GraphQL 兼容问题，改用 REST issue API 更新 PR body。

这种流程说明框架不是停留在本地实验，而是已经接入真实开源协作路径。

## 15. 已完成工作清单

### 15.1 Syscall Harness

- 建立 `tools/starry-syscall-harness/harness.py`。
- 实现 Docker re-exec。
- 实现 Doctor 检查。
- 实现 Linux probe 编译与运行。
- 实现 StarryOS probe 交叉编译。
- 实现 rootfs 准备。
- 实现 debugfs probe 注入。
- 实现 QEMU config 生成。
- 实现 StarryOS QEMU 运行与输出捕获。
- 实现 ANSI 清理、CASE 解析和差异比较。
- 实现 syscall report JSON。
- 实现 `--fail-on-diff`。
- 实现多架构入口。
- 使用 harness 发现并修复 `ftruncate_readonly_fd` 差异。

### 15.2 qperf Performance Harness

- 将 qperf plugin/analyzer 纳入仓库工具链。
- 接入 `cargo xtask starry perf`。
- 支持 TB/insn 采样模式。
- 支持 `--freq`。
- 支持 `--max-depth`。
- 支持 `--timeout`。
- 支持 release/debug build。
- 支持 kernel text range 检测。
- 支持物理地址 alias 映射。
- 支持 broad sampling 默认策略。
- 支持 optional kernel filter。
- 支持 folded stack 输出。
- 支持 flamegraph SVG 输出。
- 修复 analyzer flamegraph feature 构建问题。
- 优化 flamegraph 可读性。
- 生成 `report.json`。
- 生成 `report.md`。
- 生成 `hotspots.csv`。
- 生成 fix candidates。
- 实现 perf diff。

### 15.3 MCP

- 实现 MCP initialize。
- 实现 tools/list。
- 实现 tools/call。
- 暴露 syscall doctor。
- 暴露 syscall discover。
- 暴露 qperf profile。
- 暴露 perf diff。
- 暴露 UI launch command。
- 完成本地 MCP 注册验证。

### 15.4 Local UI

- 实现 `harness.py ui` 子命令。
- 实现 Python 标准库 HTTP server。
- 实现任务 API。
- 实现报告 API。
- 实现 artifact 文件 API。
- 实现后台 job 线程。
- 实现 job 日志落盘。
- 实现 active job 防并发。
- 实现 `/work/...` Docker artifact 路径映射。
- 实现 syscall 页面。
- 实现 qperf 页面。
- 实现 perf diff 页面。
- 实现 job log 页面。
- 实现 flamegraph iframe 展示。
- 实现 flamegraph 空状态诊断。
- 实现 flamegraph 动态宽高和横向滚动。

### 15.5 Skill 与文档

- 创建本地 `starry-syscall-harness` skill。
- 记录 Docker 约束。
- 记录 syscall 工作流。
- 记录 performance 工作流。
- 记录 MCP 注册方法。
- 记录 UI 使用方法。
- 增加 harness README。
- 增加项目 docs 中 syscall/performance harness 说明。
- 增加本文档，用于描述整体 AI 开发框架。

## 16. 验证记录

已执行过的关键验证包括：

```bash
python3 -m py_compile \
  tools/starry-syscall-harness/harness.py \
  tools/starry-syscall-harness/mcp_server.py \
  tools/starry-syscall-harness/ui_server.py
```

```bash
node --check tools/starry-syscall-harness/web/app.js
```

```bash
docker run --rm -v "$PWD":/work -w /work \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo xtask clippy --package axbuild'
```

```bash
docker run --rm -v "$PWD":/work -w /work \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo clippy --manifest-path tools/qperf/analyzer/Cargo.toml --features flamegraph --all-targets -- -D warnings'
```

qperf profile 验证：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /home/cg24/tgoskits \
  --arch riscv64 \
  --timeout 10 \
  --format all \
  --freq 49 \
  --max-depth 32 \
  --mode tb \
  --top 8 \
  --min-percent 1 \
  --output-dir target/starry-syscall-harness
```

验证结果示例：

```text
result: ok
samples: 350
flamegraph_generated = true
flamegraph.svg width = 3200
```

UI API 验证：

```bash
curl http://127.0.0.1:8765/api/status
curl 'http://127.0.0.1:8765/api/report?kind=perf&arch=riscv64'
curl 'http://127.0.0.1:8765/api/file?path=/home/cg24/tgoskits/target/starry-syscall-harness/perf/riscv64/latest/qperf/flamegraph.svg'
```

## 17. 可信性与风险控制

### 17.1 避免环境漂移

StarryOS 相关流程统一在 Docker 镜像中执行，避免宿主机工具链、QEMU 版本、交叉编译器差异影响结果。

### 17.2 避免测试变弱

syscall probe 作为 Linux 对拍依据，不应为适配 StarryOS 当前错误行为而削弱。若 Linux 与 StarryOS 不一致，默认应修 StarryOS 实现。

### 17.3 避免性能误判

qperf profile 默认使用 release build。`--debug` 仅用于符号细节调查，不作为默认性能比较基准。

### 17.4 避免 UI 越权

UI 默认绑定本地地址，并限制 artifact 文件读取范围，避免开放任意文件读取。

### 17.5 避免 PR 噪声

Docker 命令可能重写 `Cargo.lock` 或生成 root-owned target 文件。工作流中明确恢复非预期 `Cargo.lock` 改动，并在 Docker re-exec 结束后 chown artifact。

## 18. 当前能力边界

框架已经具备完整闭环，但仍有明确边界：

- syscall probe 覆盖范围仍需持续扩充。
- qperf 当前主要验证 riscv64，loongarch64 路径具备入口但仍需更多实测。
- `pprof` 格式保留为未来能力，目前未完整支持。
- fix candidates 仍是规则驱动，后续可加入更丰富的代码索引和历史修复模式。
- Linux 性能 baseline 尚未完全自动化纳入，需要后续定义可比 workload。
- 自动“修复”仍应由 agent 基于报告和代码审查生成补丁，而不是盲目模板化修改。

## 19. 后续扩展路线

建议后续按以下方向演进。

### 19.1 扩展 syscall 语义覆盖

- 增加文件系统 syscall 边界 case。
- 增加 fd lifecycle case。
- 增加 signal、poll、epoll、eventfd、timerfd 等常见 Linux app 依赖路径。
- 增加 mmap/mprotect/munmap 边界 case。
- 增加权限、flag 和 errno 顺序 case。

### 19.2 强化自动修复

- 将 difference case 映射到 syscall 实现文件。
- 为常见 errno/flag 差异建立修复模板。
- 自动生成最小补丁候选。
- 自动 rerun discover 并保留 before/after report。
- 自动生成 PR 描述中的语义证据。

### 19.3 强化性能基线

- 引入标准 workload 集合。
- 保存 baseline profile。
- 自动比较 Linux baseline、StarryOS baseline 和优化后 StarryOS。
- 增加性能阈值判断。
- 增加趋势报告。

### 19.4 强化可视化

- 在 UI 中加入 flamegraph zoom 状态提示。
- 加入 top function 到 flamegraph 搜索的快捷跳转。
- 加入 baseline/compare flamegraph 并排查看。
- 加入 diff flamegraph。
- 加入热点文件和候选修复区域的跳转。

### 19.5 强化 PR 自动化

- 自动汇总本轮 discover/perf-profile/perf-diff 结果。
- 自动生成 PR body。
- 自动附加关键 artifact 路径。
- 自动检查目标分支是否最新。
- 自动标记需要人工 review 的风险点。

## 20. 结论

当前 harness 已经形成一套面向 StarryOS 的 AI 开发工作框架。它将传统上分散的任务串联成可执行闭环：

```text
发现 syscall 差异
  -> 生成 Linux 对拍证据
  -> 定位和修复 StarryOS 实现
  -> Docker 内回归验证
  -> 提交 PR

发现性能热点
  -> qperf 采样
  -> folded stack / flamegraph / JSON 报告
  -> 生成修复候选
  -> 优化代码
  -> perf diff 验证
  -> 提交 PR
```

框架同时覆盖自动化和交互式使用：

- agent 默认使用 CLI/MCP。
- 人工需要观察时启动本地 UI。
- 所有 StarryOS 执行留在 Docker 内。
- 所有关键结果以结构化 artifact 保存。
- PR 流程对齐真实 upstream 目标分支。

这套工作说明了一个可持续扩展的方向：让智能开发代理不只是“写代码”，而是围绕可复现证据执行系统化工程任务，包括发现问题、验证语义、定位热点、生成报告、提出修复、回归确认和推动 PR。
