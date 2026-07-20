# LoongArch64 DMW and Kernel Address Space

LoongArch64 platforms may enable DMW (Direct Mapping Window) for the cached
physical direct map. In the QEMU virt platform, `phys_to_virt()` uses the
`0x9000_0000_0000_0000` DMW window:

```text
VA = 0x9000_0000_0000_0000 + PA
```

This range is translated by DMW hardware and does not consult the kernel page
table. Therefore it must not be treated as ordinary page-table-backed kernel
virtual memory.

The page-table-backed kernel address space should use a non-DMW address range,
and must still be a legal mapped virtual address. With the current
LoongArch64 page-table configuration (`VALEN = 48`), bits `[63:48]` must be a
sign extension of bit 47. For example:

```toml
kernel-aspace-base = "0xFFFF_8000_0000_0000"
kernel-aspace-size = "0x0000_7fff_ffff_f000"
```

Temporary kernel mappings such as `vmap`, eBPF ring-buffer aliases, module
memory, and trampoline pages should be allocated from this page-table-backed
kernel address space. Device MMIO is the exception: LoongArch64 `iomap()`
returns the uncached DMW alias (`0x8000... | PA`) so register accesses do not
accidentally use the cached DMW window.

Do not add a second page-table mapping for DMW direct-map RAM. Besides being
redundant, it can conflict with real kernel virtual mappings because the current
LoongArch64 page-table implementation indexes only the low 48 virtual-address
bits. For example, mappings in `0x9000_...` and page-table-backed mappings can
share the same page-table indexes if their low 48 bits are equal.

`ax-mm` therefore maps ordinary physical memory through `phys_to_virt()` only
when the resulting virtual range is contained in the configured kernel address
space. On DMW platforms this skips the hardware direct-map range; on platforms
where the direct map is page-table-backed, the existing mapping behavior is
preserved.


## References
https://www.kernel.org/doc/html/v6.1/loongarch/introduction.html#virtual-memory
