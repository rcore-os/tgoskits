# LoongArch64 DMW and Runtime Virtual-Address Layout

LoongArch64 has two independent address mechanisms that must not share one
configuration constant:

- DMW (Direct Mapping Window) translates physical aliases without consulting
  PGDL/PGDH. The current cached direct map is
  `VA = 0x9000_0000_0000_0000 + PA`; device MMIO uses the uncached
  `0x8000_0000_0000_0000 + PA` alias.
- Ordinary user mappings and vmalloc-style kernel mappings use the page-table
  walker. Their canonical ranges depend on CPUCFG1 `VALEN`.

The four-level 4-KiB page-table walker remains a compile-time contract and can
represent an architectural `VALEN` up to 48. Runtime detection does not change
the number of page-table levels. The `loongArch64` CPUCFG wrapper returns the
architectural `VALEN`, while Linux keeps the encoded `VALEN - 1` value as
`cpu_vabits`. Starry therefore derives the same boundaries as Linux:

```text
cpu_vabits = VALEN - 1
lower canonical half = [0, 1 << cpu_vabits)
upper canonical half = [0 - (1 << cpu_vabits), 2^64)
```

PGDL and PGDH are selected by `VA[VALEN-1]`, not a fixed `VA[47]`. Typical
layouts are:

| CPUCFG `VALEN` | lower-half end / `TASK_SIZE` capability | upper-half start |
| ---: | ---: | ---: |
| 48 | `0x0000_8000_0000_0000` | `0xffff_8000_0000_0000` |
| 40 | `0x0000_0080_0000_0000` | `0xffff_ff80_0000_0000` |

`someboot` publishes both halves as one validated
`VirtualAddressSpaceLayout`. `axplat-dyn` converts and caches this platform
capability. `ax-mm` allocates page-table-backed kernel virtual memory only from
the upper range. Starry intersects the lower range with its ABI policy and
captures the resulting `UserVirtualAddressLayout` in every new `AddrSpace`, so
fork keeps the exact layout and a syscall cannot observe a different
`TASK_SIZE` from exec.

Unsupported CPUCFG widths return a typed error and stop initialization; there
is no board-specific low-VA feature and no silent fixed-width fallback.

Temporary kernel mappings such as `vmap`, eBPF ring-buffer aliases, module
memory, task stacks, and trampoline pages must use the page-table-backed upper
range. `ax-mm` maps ordinary physical memory through `phys_to_virt()` only when
the resulting virtual range is inside that range. DMW RAM is therefore not
mapped a second time through PGDH. This also prevents a DMW address and an
ordinary virtual address with equal low page-table index bits from colliding in
the same materialized page-table view.

DMW still uses the existing physical-address (`PABITS`) contract. Changing
`VALEN` must not resize DMW, alter its cache attributes, or transfer ownership
of the physical direct map to the generic virtual allocator.

## Linux correspondence

- `arch/loongarch/kernel/cpu-probe.c::cpu_probe_addrbits()` computes
  `vm_map_base = 0 - (1 << cpu_vabits)`.
- `arch/loongarch/include/asm/processor.h` derives `TASK_SIZE64` from
  `min(cpu_vabits, VA_BITS)`.
- `arch/loongarch/include/asm/loongarch.h` documents PGDL/PGDH selection by
  `VA[VALEN-1]`.
- `arch/loongarch/mm/tlb.c::setup_ptwalker()` keeps the configured walker
  levels independent from the runtime CPU address width.

Architecture overview:
<https://www.kernel.org/doc/html/v6.1/loongarch/introduction.html#virtual-memory>
