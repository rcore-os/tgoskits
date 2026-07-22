# cpu-local

Typed ownership boundary for CPU-local architecture registers.

The crate owns the fixed `CpuAreaPrefix`, CPU binding/epoch validation,
current-thread publication, and task-pointer register operations. It does not
allocate CPU areas, define per-CPU variables, schedule tasks, or choose IRQ
policy; those responsibilities remain in `ax-percpu`, platform boot code, and
the scheduler.

| Architecture | CPU area | Current thread | TLS |
| --- | --- | --- | --- |
| x86_64 | GS base | GS runtime anchor | FS base |
| AArch64 | TPIDR_EL1/EL2 | SP_EL0 | TPIDR_EL0 |
| RISC-V | prefix recovery or `sscratch` | `tp=current`, `sscratch=0` | `tp=TLS`, `sscratch=CPU base` |
| LoongArch64 | r21, mirrored in KS3 | `tp=current` | `tp=TLS` |

LoongArch KS4 and KS5 are deliberately outside this contract and remain
available to vCPU scratch state.

The `tls` feature selects the TLS-owning image mode; without it the current
thread occupies the architecture task-pointer register. `host-test` provides a
thread-local register model for host-side tests. These are the crate's only
features; no runtime mode enum or ABI version is retained inside one final
image.

Scheduler publication follows a strict sequence: validate the pinned binding,
bind the next task header, prepare all fallible architecture work, consume a
`PreparedThreadSwitch` to publish the next header immediately before the raw
switch, then consume `PreviousThreadBinding` in the incoming tail. Dropping an
uncommitted prepared token rolls the next binding back. The binding epoch is a
runtime stale-tail guard, not an ABI version.

`CpuPin` can only be created by the higher-ranked `with_cpu_pin` boundary and
cannot escape its migration guard. `ExclusiveCpu` additionally represents
excluded local IRQ/re-entry and conflicting remote access. This crate validates
those capabilities but does not itself disable preemption or interrupts.

The exact initialized `CpuAreaRef` address is the layout identity. There is no
ABI version, generation, or cookie inside one final image. The task binding
epoch is intentionally retained because it rejects an obsolete incoming switch
tail after the same task has been rebound.

| Operation | Required protection |
| --- | --- |
| Atomic per-CPU scalar | Migration disabled; local IRQs may remain enabled |
| Shared `T: Sync` object | Migration disabled; object-owned synchronization |
| Local mutable object | Migration, IRQ/re-entry, and remote conflicts excluded |
| Scheduler switch | IRQs and migration disabled; prepared/previous tokens consumed |
| vCPU execution | Migration disabled; host registers restored before host Rust |
| CPU-area installation | CPU offline, traps disabled, area exclusively owned |

Licensed under Apache-2.0.
