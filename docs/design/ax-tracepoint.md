# ax-tracepoint 组件提取设计

## 问题

迁移前，StarryOS 从 crates.io 使用 `ktracepoint 0.6.0`。该 crate 通过
`static-keys` 在运行中的内核文本上切换 tracepoint 快路径，因此每个 OS 宿主都必须
提供写内核 text、指令缓存同步和跨 CPU 可见性能力。TGOSKits 已经拥有 tracepoint
定义、tracefs 控制、perf/BPF callback 和 trace pipe 消费者，但核心事件定义仍由仓库外
crate 拥有，无法在 workspace 内统一审核公共 API、linker 契约和宿主同步所有权。

具体场景是管理员向
`/sys/kernel/debug/tracing/events/<system>/<event>/enable` 写 `1` 后触发事件，
并从 `trace_pipe` 消费文本，或通过 perf/raw-tracepoint fd 挂载 BPF callback。期望启用、
触发、filter 和 fd 关闭注销都不要求改写运行中的 kernel text。若不实施，Starry 每次
启停仍依赖架构相关 text 写入和 cache 同步，workspace 也无法修复上游 safe API 中的
malformed record panic/UB 边界。

PR #1775 的提交 `ec879a74fa1b094c25e8ceda052a3fc3cc314a7e` 包含一个可独立提取的
`ktracepoint` 组件版本：它用运行时原子 gate 代替 live text patching，并把 callback
状态发布交给 OS 宿主。本设计把该组件提取到 `components/ax-tracepoint`，crate/package
名改为 `ax-tracepoint`，Rust 导入名为 `ax_tracepoint`，并迁移当前 StarryOS 消费者。

## 用户与成功标准

直接用户是 StarryOS tracefs、perf tracepoint、raw tracepoint/BPF 和仓库内定义静态事件
的内核模块。后续 ArceOS 或其它内核镜像也可以通过 `KernelTraceOps` 接入自己的状态
registry 和 trace pipe。

成功标准：

- workspace 不再解析 crates.io 的 `ktracepoint`，所有现有事件改用本地
  `ax-tracepoint`；
- callback 从零变为非零时，完整状态先对读者可见，再打开 gate；callback 变为空时，
  读者不能执行已移除 callback；
- 启停 tracepoint 不再写内核 text，也不要求架构相关 cache flush；
- 保持当前 Starry tracefs 路径、event ID/format/filter、perf/BPF 接入和
  `sched_switch` 调用上下文；
- malformed trace record 和非法 filter 返回 typed error，不触发 panic 或未定义行为；
- host example、crate 单测、Starry 多架构 clippy 和 QEMU axtest 通过。

验收动作与期望：

| 输入或动作 | 期望结果 |
| --- | --- |
| enable 文件写 `1`，随后触发事件 | callback 集合发布后 gate 打开，trace pipe 收到记录 |
| enable 文件写 `0` 或关闭 perf/raw fd | callback 从集合移除，空集合对应 gate 关闭 |
| 先写合法 filter，再写非法表达式 | 第二次返回错误，上一次 compiled filter 继续生效 |
| 解析短记录或未知 event ID | 返回 `TraceParseError`，Starry 丢弃记录且不 panic |
| host example 定义两种 `KernelTraceOps` | 每次初始化只发现自身类型的 linker metadata |

## 非目标

- 不提取 #1775 的 `ax-task`、调度策略、IPI、timer、RT/Deadline 或 runtime 重构；
- 不把 callback 移出 Starry 当前的 `NoPreemptMutex` 临界区，不引入 generation/RCU
  registry、retire worker 或新的 trace ingress ring；
- 不新增 tracefs 文件，不改变 syscall/Linux ABI，不改变现有事件字段布局；
- 不承诺与 crates.io `ktracepoint` 的源码级名称兼容；仓库内消费者一次性迁移到新名。

## Prior art 与约束

Linux tracepoint 的关键语义是：未启用时只保留轻量条件检查；启用后 probe 在触发者的
执行上下文中运行；注册和注销必须对并发触发者安全。查阅记录与结论：

- Linux kernel current documentation（2026-08-19 查阅）：
  <https://docs.kernel.org/trace/tracepoints.html>。probe 在触发者上下文运行，关闭路径只需
  轻量 enabled 检查，注册/注销必须与并发触发者同步。
- Linux ftrace current documentation（2026-08-19 查阅）：
  <https://docs.kernel.org/trace/ftrace.html>。tracefs 负责控制与输出，`trace_pipe` 是
  消费型流接口；dynamic text patch 是降低开销的实现方式，不是事件 ABI。
- crates.io `ktracepoint 0.6.0`（`Cargo.lock` 迁移前版本及本地 cargo registry source）。
  事件宏、schema/filter/pipe API 可复用；`static-keys`、公开 erased function 字段和
  未校验 record decode 不应原样进入新的 workspace 公共 crate。
- PR #1775 的 `ec879a74fa1b094c25e8ceda052a3fc3cc314a7e`。逐文件 diff 表明
  `components/ktracepoint` 可手工提取；同提交的 Starry generation registry 依赖该 PR
  更早的 ingress/scheduler/runtime 改动，不能独立 cherry-pick。

本组件采用 #1775 的原子 gate。与 Linux 的差异是关闭路径使用普通原子分支，不做
jump-label/text patch；callback 注销安全由宿主 registry 的锁或更强发布机制保证。

## 方案与替代方案

### 采用方案：本地组件加宿主发布边界

`ax-tracepoint` 拥有静态事件描述、linker metadata、callback/filter runtime value 和
原子 callback gate。Starry 调用链是
`KernelTraceOps::write_tracepoint_state -> KernelExtTracePoint::update`；后者是唯一实际
mutation owner：

1. 以 `NoPreemptMutex` 保护 `ExtTracePoint`；
2. 在同一锁内完成注册、注销或其它状态修改；
3. 依据修改后的 callback 集合以 Release store 更新 gate；
4. 快路径以 Acquire load 检查 gate，再通过同一 registry 锁读取并执行 callback。

从空集合启用时，callback 已在锁保护的状态中，再打开 gate。禁用时即使读者先观察到
旧的 `true`，它随后获取同一锁时也只会看到空集合。该边界保持当前 Starry callback
执行上下文，不依赖 #1775 更早提交中的 ingress 和调度器改造。

callback 在 `KernelExtTracePoint::read` 持 `NoPreemptMutex` 时执行，这是当前 Starry
语义：callback 不得注册/注销 callback、更新 filter，或递归触发同一 registry 的事件。
默认 trace pipe、perf 与 raw-BPF 路径继续遵守已有上下文约束；放宽该限制需要独立的
generation/ingress 设计，不能在本次 crate rename 中隐式改变。

### 未采用：保留 crates.io ktracepoint

这会继续把 live kernel-text 修改和架构 cache 同步暴露给每个宿主，也无法在 workspace
内收紧 linker、unsafe 和错误 API。

### 未采用：直接 cherry-pick #1775 的 tracepoint 提交

该提交的 Starry registry 部分依赖 #1775 更早已经存在的 trace ingress、deferred
`sched_switch` 和 runtime API；直接 cherry-pick 会隐式带入大范围 task/runtime 重构，
不再是独立组件提取。

### 未采用：在当前 dev 上只搬 generation registry

generation registry 会把 callback 移出 `NoPreemptMutex`，但当前 `sched_switch` callback
仍沿用现有触发上下文和 trace pipe 路径。缺少配套 IRQ-safe ingress 时，单独改变锁边界
会扩大上下文语义，不能作为本 PR 的安全迁移。

### 未采用：保留兼容别名 ktracepoint

同时维护两个 crate 名会延长重复接口并隐藏未迁移消费者。本次是 workspace 内一次性
rename，所有实际调用点同时迁移，不增加兼容 shim。

## 公共 API 与所有权

- `KernelTraceOps` 只描述宿主能力：PID、trace pipe、cmdline cache，以及 runtime state
  的读写入口；删除 `write_kernel_text`。
- `TracePoint` 拥有不可变事件描述和原子 gate；`ExtTracePoint` 是可克隆、尚未发布的
  callback/filter 状态值。
- `TracePointMap` 只提供查询、迭代和长度 API，不通过 `DerefMut` 暴露内部 `BTreeMap`。
- `TraceFilterError`、`TraceParseError` 和 `TraceInitError` 是可匹配的 typed error。
- cooked event callback 接收当前调用独占的 `&mut [u8]`；每个 callback 都编码独立
  record，Starry BPF adapter 不从共享引用构造可变切片。
- filter 更新是事务性的：编译失败或空表达式记录错误，但不清除上一个有效 compiled
  filter；仅精确的 `0`（允许外围空白）清除 filter。
- StarryOS 只通过 `KernelExtTracePoint::update` 修改 callback state，避免调用方忘记同步
  gate。

`KernelTraceOps::{read,write}_tracepoint_state` 对未知 ID 使用 `expect`，因为宏生成的
内部调用只会在 `global_init_events` 建表并安装 registry 后使用同一静态 event ID；这是
内核内部初始化 invariant。来自用户输入的 perf/raw event ID 不走该入口，先通过
`lookup_ext_tracepoint`/`find_ext_tracepoint_by_name` 返回 `Option` 并转换为 typed error。

## Linker 与 unsafe 边界

`define_event_trace!` 把统一的 `CommonTracePointMeta` 放进 `.tracepoint`。metadata 携带
`KernelTraceOps` type tag；`global_init_events::<K>` 只恢复 tag 匹配的 `TracePoint<K>`，
因此同一最终镜像可包含多个宿主类型而不会跨类型解释静态引用。最终 linker
script 必须定义 `__start_tracepoint`/`__stop_tracepoint`、KEEP 输入段并满足 metadata
对齐；`my_section.ld` 提供参考片段。初始化时先检查地址顺序、对齐和整项长度，再建立
只读 slice，并对 metadata 引用排序，不写 linker section。

宏只接受实现 `TraceField` 的 entry 字段。该 unsafe trait 的契约是 Copy、无需 drop 且
所有 bit pattern 有效；内置实现仅覆盖整数及其数组。formatter 检查 payload 长度后用
unaligned copy 构造 entry，因此安全 API 不会从任意 `Vec<u8>` 建立未对齐引用。

不同事件 callback 的函数签名仍需要类型擦除。`TraceDefaultFunc` 的字段保持私有，只有
带 `# Safety` 契约的 hidden constructor 能接收 erased pointer；生成的注册与 dispatch
函数成对擦除和恢复同一个签名，普通调用方不能通过安全字段构造错误签名。

## 兼容性、失败与回滚

Starry 的 tracefs 用户可见路径和格式不变。crate 名和 Rust import 是有意的源码级破坏；
workspace 内消费者在同一提交完成迁移。关闭路径从 static key 变为 Acquire 原子分支，
可能有可测但预期很小的性能差异，应由后续 benchmark 决定是否需要架构级 fast path，
不能在本次重新引入 text patch。

解析到短记录或未知 event ID 时，Starry 丢弃该损坏记录并记录 warning，避免 trace pipe
读路径 panic。filter 写失败保持先前活动 filter，并由既有 VFS 边界返回 `EINVAL`。

若需要回滚，可以在一个提交内恢复 workspace 的 crates.io dependency、旧 import 和
`write_kernel_text` 宿主实现；本 PR 不改变持久化状态或外部 ABI，不需要数据迁移。

## 调研与重复性检查

设计时基线是当时最新 `origin/dev`；交付前必须再次 fetch/rebase 并记录最终 commit。
已检查：

- 当前 workspace 只有 crates.io `ktracepoint` 这一迁移前实现，没有第二个本地
  tracepoint component；
- GitHub open PR/issue 中没有与“独立提取并 rename ax-tracepoint”重复的交付；#1775
  是来源 PR，范围还包含大量 task/runtime 重构；
- 当前 Starry 所有实际消费者、linker script 和 Cargo dependency 已枚举，迁移不存在
  仅保留旧名的 compatibility shim；
- `starry-syscall-compatibility` 技能不适用：本次不改变系统调用编号、参数、错误码、检查
  顺序或 Linux ABI，仅替换既有 tracepoint 内部组件与损坏记录失败策略。

## 验证证据与剩余计划

- red：`empty_filter_is_rejected_without_panicking` 在原实现对空 slice 索引 panic；
  `invalid_filter_preserves_the_previous_compiled_expression` 在原实现清空 compiled state；
  `unknown_tracepoint_record_is_rejected_without_panicking` 在原实现的 `expect` panic；
  padding 回归在直接暴露 `repr(C)` object representation 时读到未初始化尾部；独占
  callback 回归在旧 `&[u8]` 接口上无法编译。
- green：`cargo test -p ax-tracepoint` 当前 8/8，通过上述回归、短 record、零初始化
  padding、独占可变 callback 和两种 `KernelTraceOps` metadata 隔离。
- green：`cargo run -p ax-tracepoint --example usage`，实际发现两个 event，完成 callback、
  filter、raw/event dispatch 和 6 条文本记录格式化。
- green：`cargo clippy -p ax-tracepoint --all-targets -- -D warnings`，覆盖 macro 的测试与
  example 展开，不产生 `redundant_field_names`。
- green：`cargo xtask clippy --package ax-tracepoint --package starry-kernel`，覆盖四种
  target、逐 feature 与系统配置的完整矩阵，99/99。
- green：`cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64`，
  42/42，终端输出 `AXTEST_SUITE_OK`。
- 已 rebase 到验证时最新的 `origin/dev`（`8618959d29`）；提交 PR 后等待 CI terminal。

由于本次新增公共 crate、unsafe linker 边界和跨 crate 依赖，按
`feature-development` 技能归类为高风险功能，需要组件与 Starry 边界维护者独立审核。
