# LoongArch Interrupt And Wakeup

本文记录 LoongArch guest 外部中断注入与 vCPU 唤醒闭环的问题。IRQ injection 和 idle wakeup 需要放在一起看，因为 guest 等待设备或 timer 时，最终依赖的是同一条链路：

```text
host receives timer / IPI / external IRQ
  -> hypervisor marks or injects guest virtual interrupt
  -> notify blocked vCPU task
  -> vCPU resumes guest execution
```

## 当前状态

LoongArch 当前已经能处理部分 guest timer 和异常路径，但完整的事件驱动唤醒闭环尚未完成。

RISC-V 平台会注册 virtual IRQ injector：

```rust
register_virtual_irq_injector(crate::hal::arch::inject_interrupt)
```

LoongArch 当前对应初始化仍为空：

```rust
pub(super) fn init_platform_irq_injector() {}
```

同时，LoongArch guest `idle` 当前使用短轮询处理：

```rust
AxVCpuExitReason::Idle => {
    trace!("VM[{vm_id}] run VCpu[{vcpu_id}] Idle");
    super::timer::check_events();
    busy_wait(Duration::from_micros(50));
}
```

这不是最终模型，而是 bring-up 阶段为了避免 guest 睡死的临时策略。

## 为什么 IRQ 和 idle 是同一个问题

标准虚拟化模型通常是：

```text
guest idle/halt
  -> vCPU task block
  -> virtual timer / external interrupt / IPI arrives
  -> hypervisor injects or marks virtual interrupt pending
  -> notify blocked vCPU task
  -> vCPU resumes guest execution
```

如果只有 IRQ injection，没有 notify blocked vCPU，guest 可能仍然睡着。

如果只有 idle wait，没有可靠 IRQ/timer notify，guest 可能在第一次 idle 后永久睡眠。

所以 LoongArch 后续需要补的是完整闭环，而不是单独补一个 IRQ handler 或单独改 `Idle` 分支。

## 当前短轮询的原因

如果把 `Idle` 直接改成：

```rust
wait(vm_id)
```

vCPU task 会进入 wait queue。当前 LoongArch 还没有完整实现：

```text
timer / IRQ arrives
  -> inject virtual interrupt
  -> notify target vCPU
```

因此 guest 可能在 idle 后无法醒来。

当前短轮询路径是：

```text
Idle exit
  -> check pending VMM timer events
  -> busy wait 50us
  -> re-enter guest
```

它能降低 guest idle loop 高频 VM exit 的开销，同时避免唤醒链路不完整导致 vCPU 睡死。

## 需要明确的中断语义

LoongArch 平台中断在 hypervisor 场景下需要明确：

- 哪些中断由 host Axvisor 消费；
- 哪些中断应该交给 guest；
- claim/complete 的时机由 host 负责还是 guest 负责；
- PCH PIC、EIOINTC、PCI legacy IRQ/MSI 如何映射到 guest interrupt；
- pending interrupt 如何唤醒 blocked vCPU。

这些语义会影响：

- guest timer；
- virtio-blk I/O 完成中断；
- PCI passthrough interrupt；
- guest idle 后等待设备中断；
- 多 vCPU IPI 或跨 CPU wakeup。

## 当前限制

- `busy_wait(Duration::from_micros(50))` 仍会消耗 host CPU。
- 50us 是经验值，不是根据最近 timer deadline 动态计算。
- `notify_vcpu_timer_expired()` 当前仍未实现。
- LoongArch `init_platform_irq_injector()` 当前为空。
- 外部中断到 guest virtual interrupt pending 的平台闭环尚未完成。
- 中断注入后唤醒 blocked vCPU 的闭环尚未完成。

## 后续工作

1. 实现 LoongArch `init_platform_irq_injector()`。
2. 明确 host IRQ handler 中 passthrough IRQ 的 claim/complete 策略。
3. 实现外部 IRQ 到 `vcpu.inject_interrupt()` 或架构 pending interrupt 的路径。
4. 为 VMM timer 增加查询最近 deadline 的接口。
5. 实现 `notify_vcpu_timer_expired(vm_id, vcpu_id)`。
6. timer、IPI、外部设备中断注入后通知目标 vCPU。
7. 在 `Idle` 分支中先检查 pending virtual interrupt。
8. 唤醒链路稳定后，把短轮询改成事件驱动 wait。
9. 用 virtio-blk I/O 完成中断验证整条链路。
