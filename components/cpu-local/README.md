# cpu-local

Typed ownership boundary for CPU-local architecture registers and synchronous
execution-context state.

The crate owns the fixed `CpuAreaPrefix`, architecture current-context source,
context CPU binding epochs, context-switch transactions, and the
architecture-selected preemption word. It does not allocate CPU areas, define
per-CPU variables, own tasks or run queues, choose scheduling policy, manage
IRQs, or deliver IPIs. Those responsibilities remain in `ax-percpu`, platform
boot code, `ax-runtime`, and the task layer.

| Architecture/image | CPU area | Current context | Kernel TLS |
| --- | --- | --- | --- |
| x86_64 | GS base | GS runtime anchor | FS base when enabled |
| AArch64 | TPIDR_EL1/EL2 | SP_EL0 | TPIDR_EL0 when enabled |
| RISC-V without TLS | header back-reference | `tp` | unavailable |
| RISC-V with TLS | `sscratch` | CPU runtime anchor | `tp` |
| LoongArch64 without TLS | r21, mirrored in KS3 | `tp` | unavailable |
| LoongArch64 with TLS | r21, mirrored in KS3 | CPU runtime anchor | `tp` |

Each final image selects exactly one current-context source. There is no second
pointer that is updated and cross-checked. AArch64 temporarily lends SP_EL0 to
userspace, so its user-transition assembly spills the current header in the
pinned kernel stack and restores it before returning to Rust. LoongArch KS4 and
KS5 remain outside this contract for vCPU scratch state.

The `tls` feature requests kernel TLS. The `uspace` feature takes precedence:
when both are enabled, the image uses the same register ownership as `uspace`
alone and does not provide kernel TLS. User-space TLS remains the responsibility
of the user-context APIs. `build.rs` derives the internal `kernel_tls` cfg from
these features; it is not a user-configurable feature. `kernel_tls` and
`install_kernel_tls` are available only in the effective kernel TLS mode.
`host-test` provides a thread-local register model; there is no runtime ABI mode
inside one final image.

Context publication follows a strict transaction: validate the outgoing
binding, bind the next `ExecutionContextHeader`, prepare fallible architecture
work, consume `PreparedContextSwitch` at the final IRQ-disabled boundary,
install the selected current source in the naked switch tail when required,
then consume `PreviousContextBinding` in the incoming tail. Dropping an
uncommitted prepared token rolls the next binding back. The binding epoch is a
stale-tail guard, not an ABI version.

`ExecutionContextHeader` starts with the CPU binding at offset zero and contains
only architecture/context mechanisms and an immutable distinction between the
permanent pre-runtime placeholder and an owned context. A runtime may embed it
as the first field of its own wrapper and recover that wrapper directly from
the current header address. `cpu-local` has no task owner pointer, runtime
cookie, run-queue publication, or scheduler baton.

Preemption is an architecture-selected linear capability. x86_64 owns its word
in the CPU runtime anchor; load/store architectures own it in the current
execution-context header. `enter_preemption` returns a non-`Send`, non-`Sync`,
non-`Copy` `PreemptionToken` bound to that exact word. A final pending exit
returns `PendingPreemption` without consuming the last depth. The runtime must
first claim its scheduler baton, then call `release` and enter its safe point.
Task policy and baton state never enter this crate.

The architecture register backend exposes one current-preemption snapshot
operation. Its trait default follows the current execution-context header, so
new load/store backends inherit the portable behavior. A backend may override
that operation when its selected owner has a cheaper native representation;
the override must retain the same read-only, advisory snapshot contract. This
keeps architecture choices below the shared CPU-local and runtime APIs.

On a CPU-owned preemption architecture, the exclusion covering a raw context
switch belongs to the CPU where each side executes. If a suspended context
resumes on another CPU, the runtime uses the hidden
`handoff_preemption_after_context_switch` operation to consume its old linear
proof and adopt the equivalent switch depth left on the resumed CPU. A context
running for the first time has no suspended caller, so its first-entry tail uses
the hidden `release_initial_context_preemption` operation. Context-owned
architectures keep the original token owner, start the new header enabled, and
perform neither CPU-owner transfer nor initial release.

`CpuPin` can only be created by the higher-ranked `with_cpu_pin` boundary and
cannot escape its migration guard. `ExclusiveCpu` additionally represents
excluded local IRQ/re-entry and conflicting remote access. The crate validates
those capabilities but does not itself mask interrupts.

Low-level owner code that must select CPU-owned state before constructing a
`CpuPin` can use the hidden, non-escaping `CurrentCpuArea` boundary. This path
reads the architecture CPU-area base directly and deliberately does not validate
current execution-context publication. The caller must keep the selected CPU
fixed; mutable access additionally excludes IRQ/re-entry and remote conflicts.
No runtime path uses this boundary yet; it is reserved for future low-level
execution-context owners and offline CPU bootstrap integration.

The exact initialized `CpuAreaRef` address is the layout identity. There is no
ABI version, layout generation, owner cookie, or provider FFI inside one final
image. `someboot` still performs only raw area allocation and CPU startup;
`axplat-dyn` validates the frozen layout before binding each `CpuAreaRef`.

CPU 入口状态的字段布局由 `ax_cpu::registers::CpuEntryState` 定义；RISC-V 另有 `TaskEntryState`。`cpu-local` 依赖这些类型，在 `src/cpu_entry.rs` 核对预留空间的大小、对齐和立即数范围，并从自身真实布局生成隐藏的绝对链接符号。ax-cpu 不依赖 `cpu-local`，也不定义其 `ExecutionContextHeader` 或区域前缀。

四架构的生产寄存器访问也统一调用 `ax_cpu::registers`。本包继续决定寄存器的运行期用途、验证 CPU 区域与当前任务，并向 x86 GS 相对原语提交真实字段偏移。`increment_gs_u32`、`decrement_gs_u32` 和 `compare_exchange_gs_u32` 只执行机器操作，抢占深度、pending 位和 token 生命周期仍由本包管理。原有宿主模型保留在本包的测试边界，ax-cpu 不提供模型或测试替身。

`__AX_CPU_AREA_ARCH_STATE_OFFSET` 定位每核入口状态；RISC-V 的 `__AX_CPU_TASK_ARCH_STATE_OFFSET` 和 `__AX_CPU_TASK_CPU_BASE_OFFSET` 分别定位任务暂存区和 CPU 基址字段。入口汇编只读取链接后的立即数，不增加运行期初始化、动态重定位或恢复 TLS 之前的 Rust 回调。RISC-V 的整个预留区必须位于正的 12 位有符号立即数范围内，防止 `%lo` 截断后变为负偏移。LoongArch 的共享 scratch CSR 分配从 `ax_cpu::registers` 取得。

| Operation | Required protection |
| --- | --- |
| Atomic per-CPU scalar | Migration disabled; local IRQs may remain enabled |
| Shared `T: Sync` object | Migration disabled; object-owned synchronization |
| Local mutable object | Migration, IRQ/re-entry, and remote conflicts excluded |
| Pre-pin CPU-owner object | Migration and context switches excluded; mutable access also excludes IRQ/re-entry and remote conflicts |
| Context switch | IRQs and migration disabled; prepared/previous tokens consumed |
| Preemption safe point | IRQs disabled; runtime baton claimed before pending release |
| vCPU execution | Migration disabled; host registers restored before host Rust |
| CPU-area installation | CPU offline, traps disabled, area exclusively owned |

Licensed under Apache-2.0.
