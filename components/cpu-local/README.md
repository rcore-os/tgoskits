# cpu-local

Typed ownership boundary for CPU-local architecture registers.

The crate owns the fixed `CpuAreaPrefixV2`, CPU binding/epoch validation,
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

The `tls` feature selects the `UnikernelTls` image mode; without it the image
uses `LinuxCurrent`. `host-test` provides a thread-local register model for
host-side tests. These are the crate's only features.

Scheduler publication follows a strict sequence: validate the pinned binding,
bind the next task header, prepare all fallible architecture work, publish the
next header and registers, perform the raw context switch, then have the
incoming tail withdraw the previous epoch. `CpuPin` is a capability supplied by
the caller's migration/IRQ guard; this crate does not disable preemption or
interrupts itself.

Licensed under Apache-2.0.
