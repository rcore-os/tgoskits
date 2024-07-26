# percpu

[![Crates.io](https://img.shields.io/crates/v/percpu)](https://crates.io/crates/percpu)
[![Docs.rs](https://docs.rs/percpu/badge.svg)](https://docs.rs/percpu)
[![CI](https://github.com/arceos-org/percpu/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/percpu/actions/workflows/ci.yml)

Define and access per-CPU data structures.

All per-CPU data is placed into several contiguous memory regions called
**per-CPU data areas**, the number of which is the number of CPUs. Each CPU
has its own per-CPU data area. The architecture-specific thread pointer
register (e.g., `GS_BASE` on x86_64) is set to the base address of the area
on initialization.

When accessing the per-CPU data on the current CPU, it first use the thread
pointer register to obtain the corresponding per-CPU data area, and then add
an offset to access the corresponding field.

## Examples

```rust,no_run
#[percpu::def_percpu]
static CPU_ID: usize = 0;

// initialize per-CPU data for 4 CPUs.
percpu::init(4);
// set the thread pointer register to the per-CPU data area 0.
percpu::set_local_thread_pointer(0);

// access the per-CPU data `CPU_ID` on the current CPU.
println!("{}", CPU_ID.read_current()); // prints "0"
CPU_ID.write_current(1);
println!("{}", CPU_ID.read_current()); // prints "1"
```

Currently, you need to **modify the linker script manually**, add the following lines to your linker script:

```text,ignore
. = ALIGN(4K);
_percpu_start = .;
.percpu 0x0 (NOLOAD) : AT(_percpu_start) {
    _percpu_load_start = .;
    *(.percpu .percpu.*)
    _percpu_load_end = .;
    . = _percpu_load_start + ALIGN(64) * CPU_NUM;
}
. = _percpu_start + SIZEOF(.percpu);
```

## Cargo Features

- `sp-naive`: For **single-core** use. In this case, each per-CPU data is
just a global variable, architecture-specific thread pointer register is
not used.
- `preempt`: For **preemptible** system use. In this case, we need to disable
preemption when accessing per-CPU data. Otherwise, the data may be corrupted
when it's being accessing and the current thread happens to be preempted.
- `arm-el2`: For **ARM system** running at **EL2** use (e.g. hypervisors).
In this case, we use `TPIDR_EL2` instead of `TPIDR_EL1`
to store the base address of per-CPU data area.

## Note for RISC-V

Since RISC-V does not provide separate thread pointer registers for user and
kernel mode, we temporarily use the `gp` register to point to the per-CPU data
area, while the `tp` register is used for thread-local storage.
