# StarryOS qperf 采样问题处理记录

本文记录在把 qperf 接入 StarryOS harness 过程中实际遇到的采样、符号解析、火焰图和自动化运行问题，以及背后的 OS 语义。这里的重点不是“怎么跑出一张图”，而是说明为什么这些图一开始会不可信，以及如何让它们变成可用于定位内核瓶颈的证据。

当前 qperf 链路如下：

```text
StarryOS build/rootfs
  -> QEMU system-mode run
  -> QEMU TCG plugin callbacks from tools/qperf
  -> qperf.bin raw guest stack samples
  -> qperf-analyzer symbolization
  -> stack.folded / flamegraph.svg / report.json
  -> harness report and optional Web UI
```

这条链路不是 host `perf`。host `perf` 看到的主要是 QEMU 进程、TCG dispatch、host libc 和模拟器内部函数；qperf 看到的是 QEMU 暴露出来的 guest PC 和 guest 栈。因此，qperf 的核心问题是 OS instrumentation，而不是普通应用 profiler 集成。

## 1. 为什么不能直接用 host perf

### 现象

如果直接在宿主机上 profile QEMU，热点会落到 QEMU 自身，例如 TCG 翻译缓存、dispatch loop、helper 函数或 host libc。这样的结果回答的是：

```text
QEMU 这个宿主进程在哪里耗时？
```

但我们真正想回答的是：

```text
StarryOS 这个 guest kernel 在哪些内核路径上执行得最多？
```

### OS 语义

StarryOS 运行在 QEMU system-mode 里。guest kernel 的函数不是 host 进程里的 native symbol。QEMU 把 guest basic block 翻译成 host code cache 后执行，host PC 指向的是 QEMU 生成的代码；真正有意义的 StarryOS PC 是 guest PC。

因此采样点必须放在 QEMU TCG plugin 层：既不能太高，只看到 host QEMU；也不能太低，丢失 guest OS 的函数语义。

### 处理方式

`cargo xtask starry perf` 在 QEMU 命令里注入：

```text
qemu-system-<arch> -plugin libqperf.so,...
```

qperf 通过 translation block 或 instruction callback 采 guest IP，再由 analyzer 按 StarryOS kernel ELF 解析符号。

## 2. guest IP 不一定等于 ELF 链接地址

### 现象

早期 qperf 输出里出现过这类热点：

```text
70.10% _start+0x1000
7.75%  0x800065ae
0.56%  0x800004f8
```

这说明采样不是空的，但符号化质量很差。火焰图也会被 `_start+offset` 或裸地址占据，无法定位到真正的 Rust/内核函数。

### 根因

StarryOS kernel ELF 按 kernel virtual base 链接，符号表和 DWARF 都以虚拟链接地址为基准。但 QEMU plugin callback 中拿到的 guest IP 可能落在 kernel `.text` 的物理别名范围。

从 CPU/MMU 的角度看，这些地址都可能指向同一段内核代码；但从 ELF symbolizer 的角度看，只有链接时的虚拟地址能直接查到函数名。

这是典型内核 profiling 问题：运行时执行地址、物理装载地址、虚拟链接地址不是同一个概念。

### 修复

`scripts/axbuild/src/starry/perf.rs` 现在会检测：

- kernel ELF 的 `.text` 虚拟范围；
- `plat.kernel-base-vaddr`；
- `plat.kernel-base-paddr`；
- `.text` 对应的物理别名范围；
- 从物理别名映射回 ELF 虚拟地址所需的 offset。

然后把这些参数传给 qperf plugin：

```text
filter_start=0x<text-vaddr>
filter_end=0x<text-vaddr-end>
filter_alias_start=0x<text-paddr>
filter_alias_end=0x<text-paddr-end>
filter_alias_offset=0x<vaddr-paddr-delta>
```

plugin 采样时做 canonicalization：

```text
physical .text alias + (kernel_vaddr - kernel_paddr) -> ELF virtual address
```

这样 analyzer 收到的地址才和 ELF/DWARF 的地址空间一致。

### OS 结论

内核性能分析必须先解决地址空间语义。没有这一步，profile 文件可能格式正确、样本数量也不少，但语义上是错的。

## 3. 过早 kernel filter 会丢掉真实样本

### 现象

开启 kernel-only filter 后，有时 `stack.folded` 样本很少，甚至看起来像没有采到内核热点。

### 根因

如果只按 ELF 虚拟 `.text` 范围过滤，就默认 QEMU 回调中的所有 kernel PC 都落在虚拟地址范围。但物理 alias 存在时，合法 kernel 样本可能先以物理地址出现。如果在 canonicalize 前把它丢掉，就永远无法恢复。

### 修复

harness 默认不强制 kernel-only filter，而是先广泛采样，再对已知 kernel `.text` alias 做地址规范化。只有显式传入 `--kernel-filter` 时才过滤，而且过滤条件同时接受：

- `.text` virtual range；
- `.text` physical alias range。

### OS 结论

对内核来说，“这是不是 kernel text”不能只看一个地址区间。需要同时知道物理装载、虚拟映射和链接地址三者之间的关系。

## 4. TB 采样和 instruction 采样的取舍

### 现象

instruction mode 更细，但 QEMU 明显更慢；TB mode 开销低，但粒度更粗。

### 根因

QEMU TCG 的 translation block 是一段 guest 指令的翻译单元。对每条指令注册 callback 会显著增加回调次数。对于内核 workload，这会改变 guest 的运行节奏，影响调度、时钟中断、I/O 批处理和锁竞争。

### 处理方式

qperf 支持两种模式：

```text
mode=tb
mode=insn
```

harness 默认使用 `tb`。它足够发现大块热点，例如锁、copy、VirtIO、页表或文件系统路径。`insn` 保留给短时间、针对性更强的调查。

### OS 结论

profile 不能变成 workload 本身。一个 profiler 如果改变了 guest kernel 的阻塞点、调度点或 I/O 时序，就可能制造出它正在报告的瓶颈。

## 5. 采样频率会和 guest 周期行为混叠

### 现象

某些整数频率下，热点看起来异常稳定地集中在少量路径上，像是采样总打在同一个运行阶段。

### 根因

采样频率如果和 guest timer tick、QEMU 调度间隔或 workload 周期对齐，就会发生 sampling aliasing。采样结果会过度代表某个相位。

### 处理方式

默认频率使用 `99 Hz`，避免最简单的 `100 Hz` 周期锁定。harness 同时暴露 `--freq`，当结果可疑时可以换一个频率复跑。

### OS 结论

采样 profile 是统计证据，不是精确 trace。要考虑偏差、方差和观测者效应。

## 6. 栈回溯依赖 ABI、frame pointer 和页表可见性

### 现象

部分样本只能解析 top frame；有些 stack 很短，甚至中间断掉。

### 根因

qperf 的 stack unwind 不是读 DWARF unwind info，而是读取 guest frame pointer 并沿 guest stack 走 frame record。这依赖：

- 目标架构 ABI；
- 编译器是否保留 frame pointer；
- kernel stack 当前是否通过 guest virtual address 可读；
- frame record 中 saved FP / saved RA 的布局；
- 当前执行点是否处于普通函数栈帧中。

以 RISC-V 为例，`s0/fp` 是常用 frame pointer。plugin 通过 QEMU register API 读 FP，再用 `qemu_plugin_read_memory_vaddr` 读 guest 栈内存。如果 FP 指向未映射地址、栈帧损坏、进入异常路径或优化破坏了常规 frame layout，回溯就必须停止。

### 修复

`tools/qperf/src/profiler.rs` 里做了保守检查：

- 使用 target-specific register/frame 描述；
- 要求 FP 对齐；
- 用 `seen_fps` 防止循环；
- next FP 必须向前推进；
- guest memory 读取失败就停止；
- saved return address 读不到可执行地址就停止；
- 用 `max_depth` 限制最大深度。

这里宁可少给一层 stack，也不能伪造错误的调用链。

### OS 结论

内核栈只是按 ABI 解释的一段内存。profiler 走栈时，本质上也是 MMU/page table 和 ABI 的使用者。

## 7. return address 符号化需要 IP 修正

### 现象

caller frame 有时会解析到 call 之后的位置，甚至落到相邻符号。

### 根因

保存的 return address 指向 call 返回后的下一条指令。符号化 caller 时，更有意义的是 call site，因此通常要用 `return_address - 1`。但 top frame 是当前采样 PC，不应该减 1。

### 修复

`qperf-analyzer` 中：

- 第一个 IP 作为当前 PC 原样解析；
- 非 top frame 用 `ip - 1` 查符号。

这不是为了精确还原指令长度，而是把地址拉回 call 指令所在的符号范围。

## 8. DWARF 不足时需要 symtab fallback

### 现象

即使地址已经规范化，仍然会出现裸地址或 `??`。

### 根因

release build 更接近真实性能，但 debug info 可能不完整；Rust 泛型、内联、monomorphization 和链接优化也会让 `addr2line` 无法给出完整函数名。

### 修复

analyzer 解析顺序：

```text
addr2line frame lookup
  -> 失败或为空时，查 ELF symtab 中 <= IP 的最近 text symbol
  -> 输出 symbol+offset
  -> 仍失败时，输出 0x<ip>
```

这样即使没有完整行号，也尽量保留函数级定位能力。

### OS 结论

原始 IP 是事实，函数名是解释。内核性能分析要把符号化结果当作 best effort，而不是绝对真相。

## 9. timeout 会截断 raw sample

### 现象

QEMU 被 timeout 停掉时，可能有 `qperf.bin`，但没有完整 plugin summary；或者 analyzer 在文件尾部 decode 失败。

### 根因

自动化 harness 必须有 timeout，否则 agent 可能卡死。但 timeout 终止 QEMU 不等于 guest 正常关机：

- QEMU plugin shutdown 可能来不及完成；
- writer thread 可能没写完 summary；
- 最后一个 bincode record 可能只写了一半；
- QEMU 返回非零退出码，但已经产生了有效样本。

### 修复

链路上做了几层容错：

- plugin writer 每写一个 sample 就 flush；
- plugin 正常退出时写 `qperf.summary.txt`；
- `cargo xtask starry perf` 在 QEMU 非零退出但 raw sample 非空时继续分析；
- analyzer 如果已经解出至少一个 record，则把尾部 decode error 当作 partial tail；
- summary 中显式记录 plugin summary 是否缺失。

### OS 结论

性能 harness 是系统程序，必须有 timeout-aware artifact 语义。部分 profile 只要被明确标注，就比整次样本丢失更有价值。

## 10. callback 路径不能阻塞 vCPU

### 现象

如果直接在 callback 中写文件，QEMU vCPU 会被 I/O 阻塞，profile 会明显扰动 guest。

### 根因

QEMU plugin callback 位于 guest translated code 执行路径上。它类似一个 probe/interrupt path：不能做重 I/O，不能长时间持锁，也不能在高频路径上产生不可控分配。

### 修复

plugin callback 只把 stack sample `try_send` 到 bounded channel，后台 writer thread 负责写 `qperf.bin`。队列满时丢样本，并记录 `dropped_samples`。

当前队列大小：

```text
queue_size = 4096
```

这个设计的原则是：宁可丢样本，也不能把 guest CPU 卡在 profiler 上。

### OS 结论

采样路径要像中断路径一样短。丢事件是可度量误差；阻塞 vCPU 会改变系统行为。

## 11. raw sample 编码必须版本一致

### 现象

review 中指出 qperf plugin 和 analyzer 不应分别硬编码 `bincode = "2.0.1"`。

### 根因

`qperf.bin` 是 plugin 写、analyzer 读的私有二进制格式。如果编码库版本或配置漂移，结果可能无法解析，或者更糟糕地被错误解析。

### 修复

两个 qperf crate 都使用 workspace 依赖：

```toml
bincode.workspace = true
```

同时 summary 中保留 `qperf_format_version = 1`，给后续格式演进留入口。

### OS 结论

profiling artifact 是工具链 ABI。producer 和 consumer 的格式一致性应该像 syscall ABI 一样被认真维护。

## 12. analyzer 没启用 feature 时不会生成 flamegraph

### 现象

`--format all` 跑完后有 folded stack，但前端性能页没有 flame graph。

### 根因

内置 flamegraph 依赖 analyzer 的 `flamegraph` feature。如果 `qperf-analyzer` 构建时没启用这个 feature，传入 `--flamegraph` 也不会产生 SVG，只能依赖外部 `flamegraph.pl` 或 `inferno-flamegraph`。

在容器化 harness 里，依赖外部命令很脆弱。

### 修复

当 `cargo xtask starry perf` 的输出格式是 `svg` 或 `all` 时，构建 analyzer 会自动加：

```text
--features flamegraph
```

外部 generator 仍作为 fallback，但主路径已经自包含。

## 13. 火焰图有了，但视觉上挤在一起

### 现象

SVG 能生成，但前端展示时 frame 很窄，多个路径挤在一起，不容易看出调用层级。

### 根因

短时间 qperf run 里，样本可能集中在几个大 bucket，同时有许多小 stack。默认 flamegraph 参数会把这些 frame 压缩到较窄画布里，视觉上像是所有东西堆在一起。

### 修复

analyzer 使用更适合本场景的 inferno 参数：

```text
image_width = 3200
frame_height = 24
font_size = 13
min_width = 0.35
hash = true
deterministic = true
```

UI 按 SVG 自身宽度展示，并允许横向滚动，而不是强行缩放进 viewport。

### OS 结论

可视化也是 measurement pipeline 的一部分。如果 UI 把 stack 结构压没了，后续优化就容易对错路径下手。

## 14. stale StarryOS defconfig 会让 qperf 跑错机器

### 现象

rebase 到 dynamic platform 相关改动后，qperf 路径曾经因为旧生成配置引用了已经不存在的 QEMU feature 而失败。

### 根因

`tmp/axbuild` 下的配置是生成状态。平台配置变更后，如果继续复用旧 defconfig/build config，实际构建出来的内核配置可能不是当前源码期望的配置。

对 qperf 来说，这非常危险：profile 必须绑定明确的机器模型、设备和内核配置。

### 修复

harness 在 rootfs 和 `starry perf` 前刷新 `qemu-*` defconfig。

### OS 结论

profiling 前必须固定 boot/configuration state。用旧平台配置跑出来的 profile，本质上是在 profile 另一台机器。

## 15. release 和 debug 回答的是不同问题

### 现象

debug build 符号更多，但热点不代表真实性能；release build 更真实，但符号化难度更高。

### 根因

编译优化会改变：

- 函数边界；
- 内联；
- 寄存器分配；
- 锁和 copy fast path；
- 栈帧形态；
- panic/debug 路径的存在感。

debug profile 往往在观察“可调试内核”，release profile 才更接近“实际运行内核”。

### 处理方式

harness 默认使用 release profile。`--debug` 只用于调查符号化、栈形态或构建问题，不用于最终性能结论。

### OS 结论

内核性能结论必须来自接近真实运行状态的二进制。debug 信息多不等于性能证据强。

## 16. qperf profile 不是 Linux baseline

### 现象

有了 StarryOS flamegraph 后，很容易误以为这已经能说明“和 Linux 对齐”。

### 根因

qperf profile 表示 StarryOS 在 QEMU TCG 下的 guest sample 分布。Linux 对齐需要可比较 workload 和 baseline。单次 StarryOS profile 只能说明 StarryOS 自己的热点形状，不能直接说明 Linux 同 workload 下会怎样。

### 处理方式

harness report 中明确保留：

```text
linux_alignment.status = baseline_required
```

性能优化闭环应该是：

```text
baseline profile
  -> patch
  -> candidate profile
  -> perf-diff
  -> code inspection
  -> rerun
```

### OS 结论

性能是比较问题。火焰图是生成假设的工具，不是证明 OS 已经达到 Linux 性能水平的结论。

## 17. 如何判断一份 qperf 报告是否可信

在根据热点改代码前，应先检查：

1. `samples` 是否大于 0。
2. `dropped_samples` 是否很高；高丢样本说明 profile 可能偏。
3. `plugin_summary` 是否存在；缺失通常表示 QEMU 被 timeout 停掉。
4. top functions 是真实函数名，还是 `_start+offset` / 裸地址。
5. stderr/summary 里 `.text` virtual range 和 physical alias 是否合理。
6. 当前是 release 还是 debug build。
7. 是否可能和 guest timer/workload 周期混叠；可换 `--freq` 复跑。
8. 是否开启了 `--kernel-filter`；若开启，需要确认 alias mapping 没把样本过滤掉。
9. rule-based fix candidate 只当 triage，不直接当结论。
10. 修改后必须用下一次 qperf profile 和 `perf-diff` 验证。

## 18. 产物语义

harness 性能 run 的关键文件：

```text
target/starry-syscall-harness/perf/<arch>/latest/
  report.json
  report.md
  hotspots.csv
  profile.stdout
  profile.stderr
  qperf/
    qperf.bin
    qperf.summary.txt
    summary.txt
    stack.folded
    flamegraph.svg
```

各文件含义：

- `qperf.bin`：plugin 写出的原始 guest stack sample stream。
- `qperf.summary.txt`：plugin 正常 shutdown 时写出的采样统计。
- `summary.txt`：`cargo xtask starry perf` 写出的运行级摘要。
- `stack.folded`：analyzer 输出的 folded stack，是 flamegraph 和 diff 的输入。
- `hotspots.csv`：便于脚本或表格处理的热点列表。
- `report.json`：harness 级机器可读报告，给 MCP/agent/UI 使用。
- `profile.stdout` / `profile.stderr`：保留完整底层命令输出，用于排查 QEMU、地址范围、符号化和 feature 构建问题。

## 19. 推荐命令

常规 profile：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20
```

短时间 instruction mode profile：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 10 \
  --format folded \
  --freq 101 \
  --max-depth 64 \
  --mode insn \
  --top 20
```

比较两次 profile：

```bash
python3 tools/starry-syscall-harness/harness.py perf-diff \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline \
  --compare target/starry-syscall-harness/perf/riscv64/latest \
  --top 20
```

所有 StarryOS/QEMU/qperf 运行仍应通过 harness 进入 Docker，不应直接在宿主机跑 StarryOS 测试。

## 20. 总结

这次 qperf 接入中，真正关键的问题不是“生成 flamegraph”，而是让 flamegraph 的每一层都有正确 OS 语义：

- IP 属于 guest，不属于 host QEMU；
- guest IP 需要映射回 kernel ELF 的虚拟链接地址；
- kernel text 可能有物理 alias；
- stack unwind 依赖 ABI、frame pointer 和 guest 页表可读性；
- callback 路径不能阻塞 guest vCPU；
- timeout 会造成 partial artifact，工具链必须能诚实处理；
- release/debug profile 不能混为一谈；
- StarryOS profile 不是 Linux baseline，只能作为对齐流程中的一环。

处理完这些问题后，qperf 才能成为自动化性能优化闭环的一部分：采样 guest kernel，解析内核调用路径，生成可视化和结构化热点，提出候选瓶颈，修改代码，再用下一次 profile 和 diff 验证。
