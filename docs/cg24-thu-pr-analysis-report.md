# cg24-THU 在 rcore-os/tgoskits 中的 PR 分析报告

## 1. 分析范围与方法

* 仓库：<https://github.com/rcore-os/tgoskits>
* 作者：`cg24-THU`
* 分析时间：2026-05-29，Asia/Shanghai；GitHub 元数据时间按 UTC 记录。
* PR 查询方式：`gh pr list --repo rcore-os/tgoskits --author cg24-THU --state all --limit 200`。
* 单 PR 数据获取：`gh pr view <number> --repo rcore-os/tgoskits --json ...`、`gh pr diff <number> --repo rcore-os/tgoskits`、`gh pr checks <number> --repo rcore-os/tgoskits`、`gh api repos/rcore-os/tgoskits/pulls/<number>/comments --paginate`。
* 本地代码上下文：`git fetch upstream dev pull/<number>/head:refs/remotes/upstream/pr-<number>`，并用 `git show` / `git diff` 阅读关键文件。
* 数据来源：PR 描述、commit 列表、文件列表、review summary、inline comments、CI/check rollup、PR head diff、本地代码上下文。
* 局限性：本报告没有重新运行每个 PR 的完整 QEMU、clippy 或 CI；CI 结论来自 GitHub checks 和 review 记录。部分 closed PR 的分支包含旧基线或后续 merge commit，本文会区分 PR 总 diff 与核心 commit。

## 2. PR 总览表

| PR 编号 | 标题 | 状态 | 创建时间 | 是否合入 | 主要改动领域 | 简要结论 |
|---:|---|---|---|---|---|---|
| [#268](https://github.com/rcore-os/tgoskits/pull/268) | starryos: reject invalid pipe2 flags and add a userspace regression test | MERGED | 2026-04-18T14:01:20Z | 是 | StarryOS syscall | 最终合入 `pipe2` 无效 flags 严格校验；测试基础设施改动在 rebase 后移除。 |
| [#476](https://github.com/rcore-os/tgoskits/pull/476) | feat(starryos): integrate qperf profiling | CLOSED | 2026-05-09T09:21:41Z | 否 | syscall、qperf、BusyBox | 技术点有价值，但范围混杂且 syscall 测试未接入 CI，最终因长期未更新关闭。 |
| [#663](https://github.com/rcore-os/tgoskits/pull/663) | Integrate qperf profiling for StarryOS | CLOSED | 2026-05-16T04:47:42Z | 否 | 非项目代码/文档 | PR 描述声称 qperf 集成，实际 diff 是 `BigLabA-task4/` 和 `.DS_Store`，关闭合理。 |
| [#665](https://github.com/rcore-os/tgoskits/pull/665) | feat(starryos): integrate qperf and expand busybox tests | MERGED | 2026-05-16T05:23:09Z | 是 | syscall、qperf、测试、文档 | 将 #476 中成熟部分合入：vectored I/O 兼容性、qperf 初版、BusyBox 测试增强。 |
| [#783](https://github.com/rcore-os/tgoskits/pull/783) | perf(starryos): 优化 virtio-blk 队列大小与内存屏障 | CLOSED | 2026-05-19T16:35:14Z | 否 | VirtIO block 性能 | 优化方向明确，但 vendoring 整个 `virtio-drivers` 不适合本仓库，维护者建议投上游。 |
| [#940](https://github.com/rcore-os/tgoskits/pull/940) | feat(qperf): TCG hotspot profiling tool for StarryOS | OPEN | 2026-05-25T19:22:38Z | 否 | qperf 性能工具 | 增强 qperf 的地址过滤、TB 采样、symtab fallback、diff、flamegraph；已获 approve。 |
| [#990](https://github.com/rcore-os/tgoskits/pull/990) | feat(starry): add syscall and qperf harness | OPEN | 2026-05-27T10:42:20Z | 否 | syscall harness、qperf、MCP/UI | 建立 Linux/Starry 对拍和 qperf harness；review 已 approve，但与 #940 重叠且最新 checks 有失败项。 |

## 3. 单个 PR 详细分析

### PR #268：starryos: reject invalid pipe2 flags and add a userspace regression test

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/268>
* 状态：MERGED
* 创建 / 更新时间：2026-04-18T14:01:20Z / 2026-05-07T11:59:55Z
* merge 情况：2026-05-07T11:59:55Z 由 `ZR233` 合入，merge commit `2c850f7863907560f1d1fc5b6021634d1812c68b`
* commit 数量：1
* 修改文件数量：1

#### 背景与目标

PR 描述指出 `sys_pipe2()` 使用 `PipeFlags::from_bits_truncate(flags)`，会静默丢弃未知 bit。Linux `pipe2(2)` 对未知 flags 应返回 `EINVAL`，因此旧行为会让 `pipe2(..., O_CREAT)` 一类错误调用错误地成功创建 pipe。

#### 核心改动

最终合入只修改 `os/StarryOS/kernel/src/syscall/fs/pipe.rs`。核心 diff 是把 `PipeFlags::from_bits_truncate(flags)` 改为 `PipeFlags::from_bits(flags).ok_or_else(...) ?`。未知 flags 现在返回 `AxError::InvalidInput`，合法的 `O_CLOEXEC` / `O_NONBLOCK` 路径保持不变。

#### 设计分析

这是典型的 ABI 兼容性修复：用 bitflags 的严格解析替代容错截断，不引入新抽象，不改变合法 flags 行为。`AxError::InvalidInput` 对应 Linux `EINVAL`，与 PR 目标一致。

#### 影响范围

影响面集中在 StarryOS pipe syscall。非法 flags 从“成功并创建 pipe”变为“失败返回 `EINVAL`”；合法 flags 行为不变。PR 描述中提到的 userspace regression test 和 per-case rootfs 注入，最终没有出现在合入 diff 中。

#### 评审与讨论

早期 review 指出测试基础设施问题：`build.sh` 硬编码 `riscv64-unknown-elf-gcc`，success gating 可能在程序打印 `FAIL` 后仍把 case 记为 ok，且旧 rootfs asset 流程与当时 `dev` 冲突。rebase 后只保留核心 `pipe.rs` 修复，review 转为 approve。

#### 风险与不足

主要不足是合入版缺少回归测试，后续仍应按当前 Starry test-suit C pipeline 补一个 `pipe2` invalid flags case。代码本身风险较低，行为变化是向 Linux 语义收敛。

#### 结论

#268 是一个小而清晰的 syscall 修复。它提升了 `pipe2` Linux 兼容性，但测试部分未随最终版本合入。该 PR 也暴露出当时 Starry test-suit 对新 C 用例接入方式的演进问题。

### PR #476：feat(starryos): integrate qperf profiling

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/476>
* 状态：CLOSED
* 创建 / 更新时间：2026-05-09T09:21:41Z / 2026-05-25T01:24:58Z
* merge 情况：未合入
* commit 数量：4
* 修改文件数量：21
* diff 规模：`+2609/-23`

#### 背景与目标

PR 描述包含三个方向：修复 `preadv2` / `pwritev2` Linux 兼容性，引入 `cargo starry perf` qperf profiling，扩展 BusyBox 兼容性测试。每个方向本身都有价值，但放在一个 PR 中导致 review 面过大。

#### 核心改动

* `os/StarryOS/kernel/src/syscall/mod.rs`：调整 `preadv` / `pwritev` / `preadv2` / `pwritev2` dispatcher 参数传递。
* `os/StarryOS/kernel/src/syscall/fs/io.rs`：新增 `offset_from_hilo()`、RWF flags 校验，支持 `preadv2` / `pwritev2` 的 `offset == -1` 当前文件偏移语义。
* `os/StarryOS/kernel/src/mm/io.rs`：`IoVectorBuf::new()` 增加 `check_access()`、`checked_add()` 和 `isize::MAX` 总长度限制。
* `test-suit/starryos/syscall/preadv_pwritev2.c`：新增 C 回归测试源码。
* `scripts/axbuild/src/starry/perf.rs` 和 `tools/qperf/`：新增 qperf plugin/analyzer 与 StarryOS perf 子命令。
* BusyBox 脚本和 qemu 配置：增加部分语义测试与 fail matcher。

#### 设计分析

syscall 部分的设计方向正确：显式表达 Linux ABI 的 `pos_l` / `pos_h` / `flags`，并把 iovec 地址和长度检查前置。qperf 部分建立了“xtask 编排、QEMU plugin 采样、analyzer 后处理”的工具层分工。

问题是边界不清。syscall ABI 修复、qperf 工具链和 BusyBox 测试互不依赖，应拆成独立 PR，以便单独验证和回滚。

#### 影响范围

影响 `readv/writev/preadv/pwritev/preadv2/pwritev2` 及复用 `IoVectorBuf` 的路径；新增 qperf 工具链；新增 BusyBox 测试与文档。但新增 syscall C 文件没有被 test-suit 可发现 case 引用。

#### 评审与讨论

review 主要阻塞在三点：

* `preadv2/pwritev2` ABI 和 x86_64 参数映射需要更精确处理。
* `test-suit/starryos/syscall/preadv_pwritev2.c` 没有 `CMakeLists.txt`、子目录和 `qemu-*.toml` 引用，CI 不会运行。
* qperf 约 2000 行代码与 syscall 修复混在一起，应拆到独立 PR。

最后 `ZR233` 以“长时间未修改”关闭。`gh pr checks` 显示该分支没有 checks。

#### 风险与不足

最大风险是测试没有进入 CI，无法防止 syscall 行为回退。PR 范围过大也导致 review 成本和合入风险上升。报告中还出现过与 base 代码不完全一致的描述，例如旧 `pwritev2` 是否走 read path，后续 #665 review 再次指出需修正。

#### 结论

#476 是一次重要探索，但不是合格的合入单元。其核心成果后来以 #665 的形式进入项目；#476 本身由于范围混杂、测试接入缺失和长期未更新而关闭。

### PR #663：Integrate qperf profiling for StarryOS

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/663>
* 状态：CLOSED
* 创建 / 更新时间：2026-05-16T04:47:42Z / 2026-05-16T04:55:58Z
* merge 情况：未合入
* commit 数量：2
* 修改文件数量：19
* diff 规模：`+7034/-0`

#### 背景与目标

PR 描述称要把 qperf profiler 接入 StarryOS，并新增 `cargo starry perf`。但实际 diff 与描述完全不一致。

#### 核心改动

文件列表全部集中在 `.DS_Store` 和 `BigLabA-task4/`，包括课程/实验文档、harness 设计文档、Node.js MCP server 示例等。没有 `scripts/axbuild/src/starry/perf.rs`，没有 `tools/qperf/`，也没有 StarryOS CLI 改动。

#### 设计分析

实际 diff 不属于 tgoskits 主项目功能，不具备 qperf 集成的代码路径。因此无法按 PR 描述进行 qperf 设计评估。若按实际提交评估，它缺少项目边界、构建接入、测试接入和维护策略。

#### 影响范围

如果合入，会引入大量非项目目录和 macOS 元数据，对主项目无直接收益，并污染仓库。

#### 评审与讨论

review 明确指出“PR 描述与实际更改严重不符”，并要求移除 `.DS_Store`、移除 `BigLabA-task4/`、修正标题描述，若要提交 qperf 应另开 PR。PR 创建后约 8 分钟关闭。

#### 风险与不足

风险是仓库污染和 review 误导。CI 只显示路径检测与 skip 类检查，没有对实际内容提供有意义验证。

#### 结论

#663 是一次分支或提交选择错误。其主要意义是说明 qperf 工作需要重新整理为真实项目代码提交；后续 #665 才是真正的 qperf 集成 PR。

### PR #665：feat(starryos): integrate qperf and expand busybox tests

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/665>
* 状态：MERGED
* 创建 / 更新时间：2026-05-16T05:23:09Z / 2026-05-18T09:36:55Z
* merge 情况：2026-05-18T09:36:55Z 合入
* commit 数量：6
* 修改文件数量：23
* diff 规模：`+3122/-24`

#### 背景与目标

该 PR 继承 #476 的主要工作，并明确包含四项 StarryOS-facing 更新：`preadv2` / `pwritev2` 兼容性修复、qperf profiling 集成、BusyBox 语义测试扩展、BusyBox 中文兼容性报告。

#### 核心改动

syscall 相关：

* `os/StarryOS/kernel/src/syscall/mod.rs`：为 `preadv` / `pwritev` / `preadv2` / `pwritev2` 正确传递 `pos_h` 和 flags。
* `os/StarryOS/kernel/src/syscall/fs/io.rs`：新增 `offset_from_hilo(pos_l, pos_h)`；32 位下组合高低位 offset，64 位下忽略 `pos_h`；`preadv2` / `pwritev2` 支持 `offset == -1` 使用当前文件位置；当前未支持的非零 RWF flags 返回不支持。
* `os/StarryOS/kernel/src/mm/io.rs`：`IoVectorBuf::new()` 对 iovec 个数、负长度、非零长度用户地址、长度累计溢出和 `isize::MAX` 上限做前置校验。

qperf 相关：

* `scripts/axbuild/src/starry/mod.rs`：新增 `Perf(ArgsPerf)` 子命令，暴露 `--arch`、`--freq`、`--out`、`--format`、`--max-depth`、`--timeout`。
* `scripts/axbuild/src/starry/perf.rs`：串联 qperf plugin/analyzer 构建、StarryOS debug kernel 构建、rootfs 准备、QEMU `-plugin` 注入、raw samples、folded stack 和 summary 输出。
* `tools/qperf/src/profiler.rs`：实现 QEMU TCG plugin，使用 bounded channel 与 `try_send` 避免阻塞 QEMU 执行路径，基于 frame pointer 回溯栈。
* `tools/qperf/analyzer/src/main.rs`：读取 `qperf.bin`，用 `addr2line` 解析符号，输出 folded stack。

测试与文档：

* `test-suit/starryos/syscall/preadv_pwritev2.c`：新增 C 测试源码。
* `test-suit/starryos/normal/qemu-smp1/busybox/sh/busybox-tests.sh`：新增 12 个 BusyBox 稳定语义测试。
* BusyBox 四个架构 qemu 配置增加 `(?m)^FAIL: ` fail matcher，并增加 `Test run completed` success marker。
* 新增 qperf 集成报告、BusyBox 兼容性报告和 syscall 修改报告。

#### 设计分析

syscall 修复把 Linux ABI 细节显式化，降低参数错位风险。`IoVectorBuf` 前置校验提升边界安全性。qperf 设计保持在工具层，不侵入内核本体，通过 xtask 复用现有 build/rootfs/QEMU 流程。

不足是 PR 仍包含四个独立方向。review 虽然 approve，但也建议后续拆分，以降低 review 和回滚风险。

#### 影响范围

* 内核行为：vectored I/O 的 offset、flags、错误码和 iovec 检查时机变化。
* 工具链：新增可执行的 StarryOS qperf profiling 工作流。
* 测试：BusyBox case 更严格，脚本内 `FAIL:` 会被 QEMU harness 捕获。
* 文档：新增性能工具与 BusyBox 兼容性说明。

#### 评审与讨论

review 认可实现逻辑，但记录了注意事项：PR 范围偏大；syscall QEMU 端到端验证曾因 rootfs 下载阻塞而不完整；BusyBox 新测试主要验证 riscv64；`O_APPEND + positioned write` 仍有已知 Linux 兼容限制；修改报告中关于旧 `pwritev2` 走 read path 的说法不准确。

CI/check 显示合入前 container 路径多项通过，包括 clippy、sync-lint、std test、多架构 Starry/ArceOS/AxVisor QEMU 等。

#### 风险与不足

主要风险是范围大。`RWF_*` flags 当前统一返回不支持，是阶段性兼容实现。新增 syscall C 测试文件是否完整纳入后续常规 Starry CI 路径，需要继续确认。

#### 结论

#665 是作者最重要的已合入贡献之一。它同时提升 StarryOS Linux 兼容性、性能分析能力和 BusyBox 测试质量。虽然提交边界偏大，但最终通过 review 和 CI，被项目接受，并成为后续 #940/#990 的基础。

### PR #783：perf(starryos): 优化 virtio-blk 队列大小与内存屏障

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/783>
* 状态：CLOSED
* 创建 / 更新时间：2026-05-19T16:35:14Z / 2026-05-20T07:12:24Z
* merge 情况：未合入
* commit 数量：6
* 修改文件数量：75
* PR 总 diff 规模：`+17058/-25`

#### 背景与目标

PR 目标是优化 StarryOS virtio-blk I/O 路径。描述基于 qperf 和 Linux 对照，认为 virtio-blk queue size 太小、`VirtQueue::add()` 内存屏障过强是可优化点。

#### 核心改动

核心性能 commit `584dcaffd00a4e917e3ca16e9c754c5d6e76703b` 包含：

* `Cargo.toml`：新增 `[patch.crates-io] virtio-drivers = { path = "third_party/virtio-drivers" }`。
* `third_party/virtio-drivers/src/device/blk.rs`：`QUEUE_SIZE` 设置为 `256`。
* `third_party/virtio-drivers/src/queue.rs`：available index 更新前使用 `fence(Ordering::Release)`，注释说明该屏障保证 descriptor table 和 avail ring 写入先于 `avail.idx` store 可见。
* `test-suit/starryos/normal/qemu-smp1/bench-virtio-blk/`：新增 C benchmark，覆盖 10MB 顺序读写、不同 block size 和随机 4K read。
* `docs/virtio_qperf_analysis.md`：新增中文性能分析报告。

PR 总 diff 还包含 #665/#476 的 syscall、qperf 和 BusyBox 历史内容，这是分支基线导致的噪声，不是 virtio-blk 优化核心。

#### 设计分析

从 virtio 语义看，queue size 从 16 提到 256 有助于更多在途请求；`Release` fence 也更接近“发布此前写入给设备”的单向排序需求，比 `SeqCst` 更弱、潜在开销更低。

设计问题在提交落点。为两处核心修改 vendoring 整个 `virtio-drivers` crate，并在 tgoskits 中 patch crates.io，会带来上游同步成本。该类变更更适合先提交到 `rcore-os/virtio-drivers`。

#### 影响范围

影响所有使用 patched `virtio-drivers` 的 virtqueue 行为，并改变依赖来源。新增 benchmark 只配置了 riscv64 QEMU。文档提供了性能数据和分析方法。

#### 评审与讨论

review 认可 queue size 和 fence 优化方向，但指出 PR 范围过大、vendoring 整个 crate 需要维护策略。`elliott10` 也指出 `tools/qperf/` 和 `third_party/virtio-drivers/` 是否应长期维护需要确认。维护者 `ZR233` 最终要求向 <https://github.com/rcore-os/virtio-drivers> 提交 PR，并关闭本 PR。该分支没有 GitHub checks。

#### 风险与不足

* vendoring 整个驱动 crate 的维护成本高。
* PR 总 diff 混入无关历史内容。
* benchmark 只覆盖 riscv64，跨架构影响未验证。
* 优化应上游化后再回到 tgoskits 更新依赖。

#### 结论

#783 的技术判断有价值，但仓库边界不合适。它未合入，但为后续 `virtio-drivers` 上游优化提供了补丁方向、benchmark 和分析材料。

### PR #940：feat(qperf): TCG hotspot profiling tool for StarryOS

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/940>
* 状态：OPEN
* 创建 / 更新时间：2026-05-25T19:22:38Z / 2026-05-28T12:48:32Z
* merge 情况：未合入
* commit 数量：4
* 修改文件数量：9
* diff 规模：`+881/-33`

#### 背景与目标

#940 在 #665 的 qperf 初版基础上增强热点分析能力，重点解决 OpenSBI 样本噪声、指令级采样开销、release kernel 符号解析和优化前后 diff 问题。

#### 核心改动

* `scripts/axbuild/src/starry/mod.rs`：`ArgsPerf` 新增 `--mode` 和 `--top`。
* `scripts/axbuild/src/starry/perf.rs`：用 `object::{Object, ObjectSection}` 读取 kernel ELF `.text` 范围，并传给 qperf plugin 作为 `filter_start/filter_end`；qperf 构建使用 `tools/qperf/target`；analyzer 改为 `resolve` 子命令并支持 `--top`、`--flamegraph`。
* `tools/qperf/src/profiler.rs`：新增 TB/insn 两种采样模式，支持地址过滤，TB 模式在 translation block 上注册回调。
* `tools/qperf/analyzer/src/main.rs`：新增 `resolve` / `diff` 子命令，使用 `.symtab` fallback，聚合 top hottest functions，支持 folded stack diff 和可选 flamegraph。
* `docs/qperf-virtio-optimization-report.md`：新增基于 qperf 的 VirtIO 性能优化分析文档。

#### 设计分析

#940 把 qperf 从“能跑通”推进到“能定位热点”。`.text` 过滤解决固件样本淹没，TB 采样降低开销，`.symtab` fallback 提升 release kernel 可用性，diff 模式支持优化验证。这些改动都保持在工具层，不改变内核语义。

#### 影响范围

影响 `cargo starry perf` 的输出质量、采样开销和分析能力。新增文档把 qperf 用于 VirtIO vsock/net/blk 锁竞争分析。构建上增加 analyzer 依赖进入 `Cargo.lock`。

#### 评审与讨论

前两轮 review 指出格式、clippy、编译和 merge conflict 问题，包括 `ObjectSection` trait import、`collapsible_if`、target dir、Cargo.lock 冲突等。后续修复后，review 转为 approve。

最新 `gh pr checks` 显示 container 路径通过，包括 formatting、clippy、std test、多架构 Starry/ArceOS/AxVisor QEMU 等；大量 host 路径 skipped。

#### 风险与不足

PR 仍未合入。review 提到 `truncate_str()` 截断时没有加 `...` 前缀，是非阻塞小问题。`--format pprof` 仍是预留能力。文档和工具代码放在同一 PR，会增加 review 噪声但不构成阻塞。

#### 结论

#940 是 #665 qperf 初版后的聚焦增强。它提高了 qperf 的准确性、可读性和对比能力，review 已批准。当前主要问题是与 #990 的重叠和后续合入顺序。

### PR #990：feat(starry): add syscall and qperf harness

#### 基本信息

* 链接：<https://github.com/rcore-os/tgoskits/pull/990>
* 状态：OPEN
* 创建 / 更新时间：2026-05-27T10:42:20Z / 2026-05-28T06:15:01Z
* merge 情况：未合入
* commit 数量：7
* 修改文件数量：24
* diff 规模：`+3936/-101`

#### 背景与目标

#990 新增 StarryOS syscall/qperf harness，用于在 Docker 内执行 Linux 语义对拍、StarryOS QEMU 运行和 qperf 性能画像。它同时修复 harness 发现的 `ftruncate` 只读 fd errno 问题，并提供 CLI、MCP server 和本地浏览器 UI。

#### 核心改动

syscall 修复：

* `os/StarryOS/kernel/src/syscall/fs/io.rs`：`sys_ftruncate()` 对 `FileFlags::PATH` 或缺少 `FileFlags::WRITE` 的 fd 显式返回 `AxError::BadFileDescriptor`，对齐 Linux/POSIX 的 `EBADF`。
* `test-suit/starryos/normal/qemu-smp1/syscall/test-ftruncate/c/src/main.c`：只读 fd 场景从宽松接受 `EBADF || EINVAL` 改为严格检查 `EBADF`。

qperf 扩展：

* `scripts/axbuild/src/starry/mod.rs`：`ArgsPerf` 使用 typed `PerfMode`，新增 `--debug` 和 `--kernel-filter`。
* `scripts/axbuild/src/starry/perf.rs`：支持 debug/release profile，检测 kernel `.text` 虚拟范围和物理别名范围，传递 `filter_alias_start/end/offset`；analyzer 可直接生成 flamegraph。
* `tools/qperf/src/profiler.rs` 和 `tools/qperf/analyzer/src/main.rs`：增强 timeout flush、物理地址别名映射、top hotspot、diff 和 flamegraph 输出。

harness 工具：

* `tools/starry-syscall-harness/harness.py`：提供 `doctor`、`discover`、`perf-profile`、`perf-diff`、`ui`。`discover` 构建 syscall probe，在 Linux 与 StarryOS 中运行并比较 `CASE` 输出；`perf-profile` 运行 `cargo xtask starry perf`，解析 folded stack，输出 `report.json`、`report.md`、`hotspots.csv`。
* `tools/starry-syscall-harness/probes/syscall_probe.c`：覆盖 `pipe2` invalid flags、`eventfd2` invalid flags、`memfd_create` invalid flags、`dup3` same fd、`pwritev2` 写入数据、`ftruncate` readonly fd。
* `tools/starry-syscall-harness/mcp_server.py`：暴露 `starry_syscall_doctor`、`starry_syscall_discover`、`starry_perf_profile`、`starry_perf_diff`、`starry_harness_ui_command` 五个工具，并自动定位仓库根。
* `tools/starry-syscall-harness/ui_server.py` 与 `web/`：提供本地 UI、job queue、report/file API；artifact 文件服务限制在 harness artifact root 下。
* `.claude/skills/starry-syscall-harness/SKILL.md`、`AGENTS.md`、`docs/starry-syscall-harness.md`：补充使用文档和项目技能说明。
* `test-suit/starryos/normal/qemu-smp1/apk-curl/qemu-*.toml`：改用 `printf` 输出 PASS/FAIL marker，避免 shell trace 下 `echo` 标记误匹配。

#### 设计分析

#990 把前面几类工作串成自动化闭环：syscall probe 发现语义差异，StarryOS 修复由 test-suit 回归锁住，qperf profile/diff 支撑性能迭代，MCP/UI 提供交互入口。工具位于 `tools/`，不侵入内核。UI server 对 repo path 和 artifact path 做白名单限制，安全设计较清楚。

主要设计风险是与 #940 重叠。review 明确指出 #940 是 qperf 基础增强，#990 在其上扩展 harness 和更多 qperf 能力，存在合并顺序依赖。

#### 影响范围

* syscall：`ftruncate` 只读 fd errno 收敛为 `EBADF`。
* 测试：`test-ftruncate` 更严格；`apk-curl` marker 规避误判。
* 工具链：新增 Docker harness、MCP server、本地 UI 和 qperf report pipeline。
* 文档：新增 harness README、项目 skill 和用户文档。

#### 评审与讨论

前两轮 review 请求修改：`sys_ftruncate` 只读 fd 应返回 `EBADF`；缺少回归测试；`mcp_server.py` 和文档中有 `/home/cg24/tgoskits` 硬编码；`bincode` 应保留 workspace dependency。后续 commit 修复后 review 转为 approve。

最后一轮 review 明确指出与 #940 的文件级重叠和合并顺序依赖。CI/check 方面，分析时 `gh pr checks` 显示 `Test starry riscv64 qemu / run_container` 失败，`Run clippy / run_container` 也有失败或取消记录；其他多项检查通过或 skipped。因此该 PR 虽获 approve，但当前不应直接合入。

#### 风险与不足

* PR 范围很大，包含 syscall 修复、qperf 增强、harness、UI、MCP、文档和 test marker 修复。
* 与 #940 重叠，需要明确先后或合并策略。
* Docker harness、UI、MCP 增加长期维护面，需要持续审计命令执行边界。
* `ftruncate` 修复可以独立拆出，避免被工具链大 PR 阻塞。
* 最新 CI 仍有失败项。

#### 结论

#990 是作者从单点修复走向可重复验证框架的标志性 PR。它的工程化价值高，但当前最大问题是范围过大、与 #940 重叠和 checks 未全绿。建议先拆分或明确 #940/#990 合并策略。

## 4. 整体贡献脉络

这些 PR 呈现出递进关系：

* Linux syscall 兼容性：#268 修 `pipe2` invalid flags；#476/#665 推进 `preadv/pwritev2` ABI 和 iovec 边界；#990 修 `ftruncate` readonly fd errno。
* 测试体系：#268 早期尝试回归测试但未合入；#665 强化 BusyBox 测试和 fail matcher；#990 建立 Linux/Starry syscall differential harness。
* 性能工具：#665 合入 qperf 初版；#940 增强热点分析；#990 把 qperf 纳入 harness、UI 和 MCP。
* 性能优化探索：#783 基于 qperf 分析尝试 virtio-blk 优化，但应上游到 `virtio-drivers`。
* 工程边界演进：#476/#663/#783 暴露范围混杂、分支错误和上游归属问题；#665/#940/#990 的质量逐步提升，但仍需进一步拆分。

## 5. 技术主题归类

* 内核机制：#268、#665、#990。
* 用户态支持：#665 的 BusyBox 测试和 #990 的 syscall probe。
* 驱动 / 设备：#783 的 virtio-blk 优化探索；#940 文档中的 VirtIO 锁竞争分析。
* 文件系统：#665 的 vectored I/O、#990 的 `ftruncate`。
* 构建与工具链：#665 的 `cargo starry perf`、#940 的 qperf 增强、#990 的 Docker harness/MCP/UI。
* 测试与 CI：#476 的测试接入失败、#665 的 BusyBox matcher、#990 的 differential harness。
* 文档与工程化：#665、#783、#940、#990 都包含较多分析和使用文档。

## 6. 综合评价

`cg24-THU` 的贡献集中在 StarryOS Linux 兼容性、测试体系和性能分析工具链。已合入的 #268 和 #665 对项目有直接贡献：前者修复明确 syscall 语义错误，后者提升 vectored I/O 兼容性、qperf 工具能力和 BusyBox 测试质量。

未合入 PR 也有技术价值，但暴露出工程边界问题。#476 和 #783 都有可取技术点，却因范围混杂、测试接入不足或上游归属不合适而关闭。#663 是明显的分支内容错误。#940 和 #990 是更成熟的后续工作，review 已基本认可方向，但需要处理合并顺序和 CI 状态。

整体看，这些 PR 把 tgoskits 的 StarryOS 工作从“修具体兼容 bug”推进到“用测试和 profiling 工具持续发现问题”。qperf 与 syscall harness 的结合，为后续性能优化和 Linux 语义对齐提供了可重复方法。

## 7. 后续建议

1. 明确 #940/#990 的合并策略：优先合入 #940 后 rebase #990，或确认 #990 完整覆盖 #940 后关闭 #940。
2. 将 #990 中 `ftruncate` errno 修复拆成小 PR，避免被 harness 大 PR 阻塞。
3. 为 #268 补当前 C pipeline 下的 `pipe2` invalid flags 回归测试。
4. 将 `preadv_pwritev2.c` 迁移到可发现、可编译、可运行的 Starry test-suit case，并在 qemu config 中明确引用。
5. #783 的 virtio-blk queue/fence 优化应转投 `rcore-os/virtio-drivers`，上游合入后再更新 tgoskits 依赖。
6. 为 qperf/harness 增加最小 CI：Python `py_compile`、Node `--check`、qperf clippy、`cargo xtask starry perf --help`。
7. 对 harness UI/MCP 持续审计命令参数、artifact 读取范围和长任务并发，避免工具链成为维护风险。
