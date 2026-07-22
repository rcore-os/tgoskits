# ax-percpu-macros

Procedural macros used by `ax-percpu`. Applications should import
`ax_percpu::def_percpu` instead of depending on this crate directly.

`#[def_percpu]` emits three pieces of final-image metadata:

- uninitialized typed storage in `.percpu.template.storage`;
- a typed constructor registration in `.percpu.init`;
- an alignment descriptor in `.percpu.align`.

Primitive integers and booleans use their matching atomic representation.
Other objects retain their Rust type and are constructed once in each final
runtime CPU area. The generated access wrapper calculates a template-relative
offset and delegates all current and remote access checks to `ax-percpu`.

This crate has no Cargo features or runtime storage backend. Linker layout,
dynamic allocation, initialization, CPU pinning, and register ownership belong
to `ax-percpu`, the platform, and `cpu-local` respectively.

```bash
cargo test -p ax-percpu-macros
cargo xtask clippy --package ax-percpu-macros
```

Licensed under Apache-2.0.
