# percpu

[![Crates.io](https://img.shields.io/crates/v/percpu)](https://crates.io/crates/percpu)
[![Docs.rs](https://docs.rs/percpu/badge.svg)](https://docs.rs/percpu)
[![CI](https://github.com/arceos-org/percpu/actions/workflows/deploy.yml/badge.svg?branch=main)](https://github.com/arceos-org/percpu/actions/workflows/deploy.yml)

Define and access per-CPU data structures.

All per-CPU data is placed into several contiguous memory regions called
**per-CPU data areas**, the number of which is the number of CPUs. Each CPU
has its own per-CPU data area. The architecture-specific per-CPU register
(e.g., `GS_BASE` on x86_64) is set to the base address of the area on
initialization.

When accessing the per-CPU data on the current CPU, it first use the per-CPU
register to obtain the corresponding per-CPU data area, and then add an offset
to access the corresponding field.

## Supported Architectures

| Architecture | per-CPU Register Used  |
| ---          | ---                    |
| ARM (32-bit) | `TPIDRPRW` (c13)       |
| RISC-V       | `gp`                   |
| AArch64      | `TPIDR_ELx`            |
| x86_64       | `GS_BASE`              |
| LoongArch    | `$r21`                 |

> Notes for ARM (32-bit):
> We use `TPIDRPRW` (PL1 only Thread ID Register, CP15 c13) to store the
> per-CPU data area base address. This register is accessed via coprocessor
> instructions `mrc p15, 0, <Rt>, c13, c0, 4` (read) and
> `mcr p15, 0, <Rt>, c13, c0, 4` (write).

> Notes for RISC-V:
> Since RISC-V does not provide separate thread pointer registers for user and
> kernel mode, we temporarily use the `gp` register to point to the per-CPU data
> area, while the `tp` register is used for thread-local storage.

> Notes for AArch64:
> When feature `arm-el2` is enabled, `TPIDR_EL2` is used. Otherwise, `TPIDR_EL1`
> is used.

## Features

**Mode feature**:

- `sp-naive`: **Single-core** mode. Each per-CPU data is just a global variable.
  Architecture-specific thread pointer register is not used.

  | Feature    | Mode        | Per-CPU Data Area                         |
  |------------|-------------|-------------------------------------------|
  | `sp-naive` | Single-core | Global vars                               |
  | (none)     | Multi-core  | `.percpu` section or user-provided memory |

**Other features** (can be combined with any mode):

- `non-zero-vma`: Allows the `.percpu` section to be placed at a **non-zero VMA**.
  Required for Linux user-space programs as some linkers don't support VMA 0.
  `sp-naive` does not need `non-zero-vma` as it uses global variables.
- `preempt`: For **preemptible** system use. Disables preemption when accessing
  per-CPU data to prevent corruption.
- `arm-el2`: For **ARM system** running at **EL2** use (e.g. hypervisors).
  Uses `TPIDR_EL2` instead of `TPIDR_EL1`.

## Usage

### Initialization

Two methods are provided to initialize the per-CPU data areas:

- `init_in_place()`: Initialize using the `.percpu` section (static allocation),
  which should be reserved in the linker script. See [the example linker script](./test_percpu.x)
  for an example. Returns `Result<usize, InitError>`.
- `init(base, cpu_count)`: Initialize with user-provided memory (dynamic allocation),
  user must use `percpu_area_layout_expected(cpu_count)` to calculate the required
  memory size. It's highly recommended to align the memory to 4KiB page size.
  Returns `Result<usize, InitError>`.

After initialization, the per-CPU data areas are ready to be used. You can use
`init_percpu_reg(cpu_id)` on each CPU to set the per-CPU register to the base
address of the corresponding per-CPU data area.

### Accessing Per-CPU Data

To access the per-CPU data on the current CPU, you can use the `current_ptr`,
`current_ref_raw`, `current_ref`, `with_current` (recommended, it handles
preemption automatically), `reset_to_init`. Primitive unsigned types and booleans
can be accessed directly using `read_current`, `write_current` (with preemption
handling automatically) and `read_current_raw`, `write_current_raw` (without
preemption handling).

It's also possible to access the per-CPU data on other CPUs using `remote_ptr`,
`remote_ref_raw`, `remote_ref`. Such operations are intrinsically **unsafe** and
it's the caller's responsibility to ensure that the CPU ID is valid and that
data races will not happen.

To reset a per-CPU data to the initial value, you can use `reset_to_init`.

## Examples

```rust,no_run
#[percpu::def_percpu]
static CPU_ID: usize = 0;

// Option 1: Use init_in_place() to use the `.percpu` section (statically-
// allocated during linking). Enough space must be reserved for the `.percpu`
// section in the linker script.
percpu::init_in_place().unwrap();
percpu::init_percpu_reg(0);

// Option 2: Use init() with user-provided memory and cpu_count for dynamic
// initialization. The caller is responsible for allocating memory for the per-CPU
// data areas. Use `percpu_area_layout_expected()` to calculate the required memory
// size.
let cpu_count = 4;
let layout = percpu::percpu_area_layout_expected(cpu_count);
let base = unsafe { std::alloc::alloc(layout) as usize };
percpu::init(base as *mut u8, cpu_count).unwrap();
percpu::init_percpu_reg(0);

// Access the per-CPU data `CPU_ID` on the current CPU.
println!("{}", CPU_ID.read_current()); // prints "0"
CPU_ID.write_current(1);
println!("{}", CPU_ID.read_current()); // prints "1"
```

Currently, you need to **modify the linker script manually**, add the following
lines to your linker script:

```text,ignore
. = ALIGN(4K);
_percpu_start = .;
_percpu_end = _percpu_start + SIZEOF(.percpu);
.percpu 0x0 (NOLOAD) : AT(_percpu_start) {
    _percpu_load_start = .;
    *(.percpu .percpu.*)
    _percpu_load_end = .;
    // If you want to use the `.percpu` section for static initialization, you
    // need to reserve enough space for the `.percpu` section, as shown below.
    . = _percpu_load_start + ALIGN(64) * CPU_NUM;
}
. = _percpu_end;
```

