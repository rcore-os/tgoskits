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

The `tls` feature selects the final-image register assignment. `host-test`
provides a thread-local register model. These are the crate's only features;
there is no runtime ABI mode inside one final image.

Context publication follows a strict transaction: validate the outgoing
binding, bind the next `ExecutionContextHeader`, prepare fallible architecture
work, consume `PreparedContextSwitch` at the final IRQ-disabled boundary,
install the selected current source in the naked switch tail when required,
then consume `PreviousContextBinding` in the incoming tail. Dropping an
uncommitted prepared token rolls the next binding back. The binding epoch is a
stale-tail guard, not an ABI version.

`ExecutionContextHeader` starts with the CPU binding at offset zero and contains
only architecture/context mechanisms. A runtime may embed it as the first
field of its own wrapper and recover that wrapper directly from the current
header address. `cpu-local` has no task owner pointer, runtime cookie, run-queue
publication, or scheduler baton.

Preemption is an architecture-selected linear capability. x86_64 owns its word
in the CPU runtime anchor; load/store architectures own it in the current
execution-context header. `enter_preemption` returns a non-`Send`, non-`Sync`,
non-`Copy` `PreemptionToken` bound to that exact word. A final pending exit
returns `PendingPreemption` without consuming the last depth. The runtime must
first claim its scheduler baton, then call `release` and enter its safe point.
Task policy and baton state never enter this crate.

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
