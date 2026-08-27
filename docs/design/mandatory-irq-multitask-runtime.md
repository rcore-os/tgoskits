# IRQ 与多任务运行时基础能力必选化

## 背景与问题

仓库过去通过 `irq` 和 `multitask` Cargo feature 同时表达平台能力、运行时装配和
应用选择。关闭这些 feature 后，各层会进入彼此不一致的降级路径：timer timeout 被
忽略、等待退化成 busy-wait、设备 IRQ 被静默丢弃、block registrar 为空、Klib 返回
`Unsupported`，pthread/futex 则使用固定 PID 或空实现。这些路径不能构成可用的平台或
运行时，却扩大了公共接口和启动组合的验证空间。

## 决策

`irq` 与 `multitask` Cargo feature 被彻底删除，不提供 deprecated 或 no-op 兼容层。
所有真实平台必须实现 IRQ framework、timer 和公共 dispatch 能力；所有 ArceOS、
StarryOS 与 Axvisor 构建都使用完整多任务运行时。裸 `irq`、`multitask` 以及
`ax-*/irq`、`ax-*/multitask` 等旧输入不做 axbuild 特判，由 Cargo 按未知 feature
正常报错；调用方应直接删除它们。

该决策不意味着每个设备必须拥有独立硬件 IRQ。设备没有 IRQ、console 只能 raw output，
以及硬件确实不支持 trigger 或 affinity 都仍是合法状态；它们不等同于平台缺少 IRQ
基础能力。`host-test` dummy 只用于宿主测试，也不被视为真实平台后端。

## 依赖与接口契约

- `ax-plat` 固定依赖 `rdif-intc`，`ax-plat::irq`、`IrqIf` 和 console/time IRQ 方法
  始终公开；动态 x86_64、AArch64、RISC-V 与 LoongArch 平台继续提供真实后端。
- `ax-hal::irq`、`ax-runtime::irq`、IRQ context、handler、EOI 与 domain 类型始终可用，
  不改变 `IrqId`、设备 binding 或错误语义。
- `ax-task` 固定包含 scheduler、timer list、poll/wait、per-CPU task state 和同步依赖；
  timeout、sleep、IRQ notify 与 task timer API 始终使用调度器实现。
- `ax-runtime` 固定依赖 per-CPU IRQ 状态、interrupt-controller 接口和 task-aware
  `ax-sync`。`smp`、`preempt`、调度策略、`ipi`、`wake-ipi`、`task-irq` 与
  `debug-might-sleep-irq` 保留独立语义。

## 启动契约

主核按以下顺序建立运行时：

1. 初始化 per-CPU、early HAL、日志、allocator 和可选 paging/trap handler。
2. 执行 later platform init 与 boot IRQ probe；probe 失败保持原有失败语义。
3. 初始化 scheduler，并在配置了 IPI 时安装对应运行时能力。
4. 安装公共 IRQ dispatcher，注册 per-CPU timer（以及适用的 IPI）handler，启动
   timer 并 enable 本地 IRQ。
5. 安装需要的驱动 runtime glue，然后执行设备 probe。
6. 初始化 serial，完成 runtime console handoff，再初始化文件系统、网络、显示等服务。
7. 最后释放 SMP secondary CPU，调用构造器和应用入口。

secondary CPU 同样必须在发布初始化完成状态前建立 scheduler、per-CPU IRQ/timer 和本地
IRQ enable。该顺序防止设备 probe 早于 dispatcher、console worker 早于 scheduler，或
AP 在 IRQ/IPI 能力尚未就绪时对外可见。

## Console 与设备边界

Console/serial runtime 不再受 IRQ 或多任务 feature 控制。探测到可接管的 UART 时使用
owner-affine task 和 IRQ-backed input；没有匹配 UART 或硬件 console IRQ 时，raw output
仍可用，而 task input 明确返回 unsupported。进入 `Preparing` 后失败继续执行
fail-close，不能重新访问 early UART。

设备层始终安装真实 IRQ resolver、binding 和 registrar；单个设备没有 IRQ 时返回既有的
无 IRQ/unsupported 状态，不得静默丢弃声明或退化为空 registrar。

## 替代方案

曾考虑保留同名空 feature 作为迁移缓冲，但它会继续让旧配置看似有效，无法发现下游未
清理的转发链。也曾考虑仅默认启用而保留关闭能力，但这仍要求维护不可运行的 no-IRQ 与
single-task 分支。两者都不能缩小平台契约和测试矩阵，因此不采用。

## 迁移与验证

下游应直接删除 `irq`、`multitask` 及其 `crate/feature` 转发，不需要替换为新 feature。
静态审计必须确认 Cargo metadata 不再暴露这两个顶层 feature，受管 build TOML 不再声明
它们，Rust 中不存在对应 `cfg`、`cfg!` 或 `doc(cfg(...))`。host profile 使用
`ax-hal/host-test` 与 `ax-task/host-test`，并精确检查 IRQ entry、task 初始化和 timer 测试
发现。架构 QEMU、虚拟化扩展和物理板行为由现有 CI 矩阵验证。
