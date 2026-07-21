# ax-percpu

Architecture-independent per-CPU layout and typed access for `no_std` kernels.

`ax-percpu` owns template layout, symbol offsets, immutable runtime-area registration, and current/remote address calculation. Architecture register instructions live exclusively in the `cpu-local` leaf, whose only dependency is the value-only `trait-ffi` boundary.

Every runtime area begins with a fixed `CpuAreaPrefixV2`. A final-high platform
entry constructs one contiguous layout exactly once, then binds each CPU before
it becomes online:

```rust,ignore
unsafe {
    let layout = ax_percpu::PerCpuLayoutV1 {
        runtime_base,
        area_stride,
        area_count,
        flags: 0,
    };
    let init = ax_percpu::PerCpuLayoutInitV2::new(
        layout,
        generation,
        cookie,
        register_mode,
        host_level,
    );
    ax_percpu::initialize_layout(init)?;
}

let cpu = ax_percpu::CpuIndex::try_from(cpu_id)?;
let area = ax_percpu::area(cpu)?;
unsafe { cpu_local::raw::install_binding(area.binding())? };
```

Externally allocated areas are raw storage. `def_percpu` emits `MaybeUninit`
storage in `.percpu.storage` plus a typed initializer registration; final-high initialization first
validates the complete relative descriptor table and then constructs every
CPU-owned value independently with `ptr::write`. It never copies the loaded
bytes of an arbitrary Rust value into another allocation.

Initializer expressions must be const-evaluable values that do not refer to
their own generated per-CPU storage address. Rust const evaluation treats
separate statics as separate allocations, so a self-referential initializer
cannot be relocated into each destination. Such values must store an empty
state and establish self-pointers explicitly after binding through a
type-specific unsafe initialization protocol.

Safe current access requires a `BoundCpuPin` obtained by matching the
platform-published value-only binding against the installed layout while a
`CpuPin` prevents migration:

```rust,no_run
#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

fn set_cpu_id(pin: &ax_percpu::CpuPin, cpu_id: usize) {
    let bound_pin = ax_percpu::bound_current(pin).expect("CPU area must be bound");
    CPU_ID.write_current(&bound_pin, cpu_id);
    assert_eq!(CPU_ID.read_current(&bound_pin), cpu_id);
}
```

Primitive safe access uses matching `Atomic*` storage with Relaxed operations,
so hard-IRQ re-entry is data-race-free. Objects expose only `with_current_ref`
when `T: Sync`; mutable object access is explicitly unsafe because CPU binding
and migration stability do not prove exclusive aliasing.

The fixed header has a 64-byte minimum alignment, but it is not a limit on
per-CPU objects. `def_percpu` emits one ordinary Rust alignment descriptor per
storage object. The linker uses the actual `.percpu` output-section alignment
for the template base and every area stride, while layout validation checks the
same descriptor maximum before an area can be bound:

```rust,ignore
#[repr(align(8192))]
struct OverAligned(u8);

#[ax_percpu::def_percpu]
static VALUE: OverAligned = OverAligned(0);
```

Supported storage modes are the linked default, externally allocated `custom-base`, and single-CPU `sp-naive`. All modes expose the same variable API.

A kernel platform using `custom-base` must also select `linked-template`. Its
linker script must retain `.percpu_end` after every ordinary per-CPU input
section and retain `.ax_percpu.align` separately. `cpu-local` provides the
exact Rust-owned template bounds; the alignment descriptor table provides the
dynamic allocation requirement without relying on a target-triple heuristic
or an architecture-specific load/store sequence.

A non-kernel host consumer using `custom-base` may link source-only tests, but
runtime layout or value access fails explicitly unless that consumer selects a
host storage fixture. Use `sp-naive` for single-CPU model tests, or `host-test`
with the crate's linker fixture when replicated areas are part of the test.
Each `host-test` integration binary must provide its own explicit
`CpuLocalPlatformV1` fake; the library does not link a default provider that
could conflict with a real OS. The host register fixture is thread-local, so
every test-harness or modeled CPU thread must explicitly install its own
complete area binding before access; there is no process-global fallback.

## Validation

```bash
cargo test -p ax-percpu --features host-test,non-zero-vma
cargo test -p ax-percpu --features host-test,non-zero-vma,custom-base
cargo test -p ax-percpu --features host-test,non-zero-vma,sp-naive
cargo xtask clippy --package ax-percpu
```

Licensed under Apache-2.0.
