# ax-percpu

Typed per-CPU layout, initialization, and access for `no_std` kernels.

`ax-percpu` is dynamic-only. The final ELF contains one layout template; the
platform allocates one writable runtime area per CPU and initializes the
complete layout before any CPU is bound. Architecture register ownership is
provided by the separate `cpu-local` crate.

## Runtime contract

The initialization sequence is fixed:

1. The linker retains exactly one `.percpu.template` plus the
   `.percpu.init` and `.percpu.align` descriptor tables.
2. The platform allocates shutdown-lifetime storage for every CPU area.
3. `initialize_layout(PerCpuLayoutInitV2)` validates the complete geometry and
   descriptor tables.
4. Each `CpuAreaPrefixV2` and each typed value is constructed once at its final
   runtime address.
5. The layout is frozen; the platform may then install `area(cpu).binding()`.

There is no linked runtime layout or static per-CPU replication. Changing the
runtime CPU count therefore does not change the ELF template size.

The linker contract uses only these output sections:

- `.percpu.template`
- `.percpu.init`
- `.percpu.align`

Generated storage is placed in `.percpu.template.storage`; the fixed prefix
and end sentinel use `.percpu.template.header` and `.percpu.template.end`.
Linker boundaries use `__PERCPU_*` and `__CPU_LOCAL_*` names.

## Typed access

```rust,no_run
#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

fn set_cpu_id(pin: &ax_percpu::CpuPin, cpu_id: usize) {
    let bound = ax_percpu::bound_current(pin).expect("CPU area must be bound");
    CPU_ID.write_current(&bound, cpu_id);
    assert_eq!(CPU_ID.read_current(&bound), cpu_id);
}
```

Primitive values use the matching atomic storage type. Object initializers are
retained as typed descriptor thunks and construct one independent value in
each final runtime area; arbitrary Rust object bytes are never duplicated from
the ELF template.

Safe current access requires `BoundCpuPin`, which verifies the live binding
against the frozen layout while borrowing the caller's migration pin. Mutable
object access remains `unsafe` because CPU pinning alone cannot prove exclusive
Rust aliasing.

## Host tests

The crate exposes one feature, `host-test`. It provides:

```rust,ignore
let layout = ax_percpu::host_test::initialize(
    core::num::NonZeroU32::new(4).unwrap(),
)?;
let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0)?)?;
unsafe { cpu_local::raw::install_binding(area.binding())? };
```

The helper owns process-lifetime dynamic storage and initializes it once.
Each modeled CPU thread must explicitly install its own binding.

## Validation

```bash
cargo test -p ax-percpu --features host-test
cargo test -p ax-percpu-macros
cargo xtask clippy --package ax-percpu
```

Licensed under Apache-2.0.
