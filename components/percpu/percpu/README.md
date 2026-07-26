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
3. `initialize_layout(PerCpuRegion)` validates the complete geometry and
   descriptor tables before the first destination write.
4. Each `CpuAreaPrefix` and each typed value is constructed once at its final
   runtime address.
5. The layout is frozen; the platform may then install
   `area(cpu).cpu_area()` through its offline-CPU boundary.

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

fn set_cpu_id(pin: &ax_percpu::CpuPin<'_>, cpu_id: usize) {
    ax_percpu::current_area(pin).expect("CPU area must match the frozen layout");
    CPU_ID.write_current(pin, cpu_id);
    assert_eq!(CPU_ID.read_current(pin), cpu_id);
}
```

Primitive values use the matching atomic storage type. Object initializers are
retained as typed descriptor thunks and construct one independent value in
each final runtime area; arbitrary Rust object bytes are never duplicated from
the ELF template.

Current access requires a scoped `CpuPin`, which validates the live register,
area self pointer, and CPU index when it is created. A mutable object borrow
additionally requires `ExclusiveCpu`; only the unsafe guard integration can
create that stronger capability after excluding IRQ/re-entry and conflicting
remote access.

| Operation | Required protection |
| --- | --- |
| Atomic scalar | Migration disabled; local IRQs may remain enabled |
| Shared `T: Sync` object | Migration disabled; the object synchronizes itself |
| Local mutable object | Migration, IRQ/re-entry, and conflicting remote access excluded |
| Scheduler switch | IRQs and migration disabled; transactional tokens consumed |
| vCPU run | Migration disabled; exit assembly restores host registers before Rust |
| CPU-area initialization | CPU offline and raw area exclusively owned |

## Host tests

The crate exposes one feature, `host-test`. It provides:

```rust,ignore
let layout = ax_percpu::host_test::initialize(
    core::num::NonZeroU32::new(4).unwrap(),
)?;
let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0)?)?;
unsafe { cpu_local::install_cpu_area(area.cpu_area()?)? };
unsafe {
    ax_percpu::with_cpu_pin(|pin| {
        assert_eq!(ax_percpu::current_area(pin), Ok(area));
    })?;
}
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
