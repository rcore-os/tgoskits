# LoongArch 外部中断默认直通问题

LoongArch 当前在 `feature = "hypervisor"` 下，对 EIOINTC claim 出来的外部 IRQ 没有判断归属，而是默认认为外部 IRQ 都应该交给 guest interrupt controller。host 只完成物理中断控制器层面的 claim/complete，不执行设备级中断处理。

这意味着当前实现隐含了一个前提：**hypervisor 模式下所有 EIOINTC 外设中断都属于 guest**。这个前提只适合“外设全部直通给 guest，host 不拥有外设驱动”的 bring-up 场景；一旦 host 也需要处理某些设备，或存在多个 VM，这个模型就不够。

## 当前代码行为

外部中断进入 host 后，CPU 侧最初看到的是 EIOINTC 入口线：

```rust
IrqType::Io
```

随后 host 调用：

```rust
eiointc::claim_irq()
```

从 EIOINTC 读取真实外部 IRQ 号，并转换成：

```rust
IrqType::Ex(ex_irq)
```

在非 hypervisor 模式下，host 会查自己的 handler table：

```rust
if !IRQ_HANDLER_TABLE.handle(ex_irq) {
    debug!("Unhandled IRQ {irq:?}");
}
```

但在 hypervisor 模式下，host 不处理这个外部 IRQ：

```rust
trace!("Leaving passthrough external IRQ {ex_irq} to guest interrupt controller");
```

最后无论是否是 hypervisor 模式，host 都会执行：

```rust
eiointc::complete_irq(ex_irq);
```

这里的 `complete_irq()` 只是告诉物理 EIOINTC：这次 claim 出来的 IRQ 已经完成控制器层面的处理，可以继续投递后续中断。它不表示 guest 已经处理完设备，也不表示设备中断源已经被清除。

## 问题本质

当前代码没有回答这个问题：

```text
这个外部 IRQ 到底属于谁？
```

它直接把所有 `IrqType::Ex(ex_irq)` 都当成 guest passthrough IRQ。

实际 hypervisor 里通常需要区分：

```text
IRQ n -> host
IRQ n -> VM0
IRQ n -> VM1
IRQ n -> masked / reserved
```

如果没有 IRQ ownership/routing 表，就无法可靠支持：

- host 自己使用串口、磁盘、网卡或管理设备；
- 多 VM 共享同一个物理中断控制器；
- 部分设备 emulation，部分设备 passthrough；
- legacy IRQ、MSI、timer、IPI 等不同中断来源的不同处理策略。

## 为什么当前暂时能工作

当前 LoongArch bring-up 场景主要依赖 passthrough interrupt：

```rust
gintc_set_hwi_passthrough(0xff);
```

这表示 guest HWI 线被配置为 passthrough。对于“外设都给 guest”的单 VM 场景，host 看到外部 IRQ 后不做设备处理，只 complete 物理 EIOINTC，是可以解释得通的。

典型流程是：

```text
外设触发 IRQ
  -> EIOINTC 置 pending
  -> host trap 到外部中断
  -> host claim 得到 ex_irq
  -> host 不调用自己的 IRQ handler
  -> guest interrupt controller / HWI passthrough 负责让 guest 看到中断
  -> host complete EIOINTC
```

这个流程依赖一个条件：这个 IRQ 的设备语义确实由 guest 负责处理。

## 风险

如果某个外部 IRQ 实际应该由 host 处理，当前代码会跳过 host handler，导致 host 永远不处理该设备中断。

如果未来支持多个 VM，当前代码也无法判断这个 IRQ 应该注入哪个 VM、哪个 vCPU。

如果设备是 level-triggered，host complete EIOINTC 时 guest 可能还没有清设备中断源。之后是否重新触发依赖硬件和中断控制器语义，需要单独验证。

如果设备是 edge-triggered 或 MSI，需要确认 host 过早 complete 是否存在事件丢失风险。

## 与其他架构的差异

RISC-V 当前也有类似简化，但代码里已经明确留下 TODO：

```rust
// TODO: judge irq's ownership before handling (axvisor or any vm).
```

也就是说 RISC-V 当前也知道未来需要判断 IRQ 属于 axvisor 还是某个 VM。

AArch64 更偏向通过 passthrough SPI / vGIC 配置来描述 guest 拥有哪些中断，不是简单把所有外部 IRQ 都无条件视为 guest IRQ。

LoongArch 当前缺少类似的 ownership 判断，因此问题更集中地体现在 `IrqType::Ex(ex_irq)` 分支。

## 后续方向

需要引入 LoongArch IRQ ownership/routing 机制。外部 IRQ 处理应从：

```text
所有 Ex IRQ 默认 guest
```

演进为：

```text
claim physical IRQ
  -> 查询 IRQ owner
  -> owner 是 host：调用 IRQ_HANDLER_TABLE.handle(ex_irq)
  -> owner 是 guest：注入 guest 或依赖 passthrough HWI
  -> owner 不明确：mask 或 warn
  -> 按控制器语义 complete physical IRQ
```

同时需要明确：

- `complete_irq()` 与 guest 设备处理完成之间没有强同步关系；
- passthrough HWI 和 software injection 的适用边界；
- `Ex(ex_irq)` 到 guest vector/HWI 线的映射；
- 多 VM、多 vCPU 下 IRQ 注入目标；
- level-triggered、edge-triggered、MSI 的 complete/ack 时序。
