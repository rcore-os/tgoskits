# Architect Mode Rules — StarryOS

- `arceos/` excluded from workspace — modifying ax-* crates requires publishing new versions to crates.io before StarryOS can consume them. Plan accordingly.
- Memory model differs by arch: aarch64/loongarch64 use separate user/kernel page tables; riscv64/x86_64 copy kernel mappings into user page tables. Changes to VM code must account for both models.
- `#[extern_trait]` (extern-trait crate) enables cross-crate trait implementation with FFI-like vtable dispatch — used for task and memory access traits. New cross-crate trait impls should consider this pattern.
- `AssumeSync<T>` in `kernel/src/task/mod.rs`: `#[repr(transparent)]` wrapper that unsafely implements `Sync` for thread-local state exclusively accessed during context switches. Do not replicate this pattern without equivalent safety guarantees.
- `#[page_fault_handler]` enables CoW and lazy allocation for user memory — architectural dependency for the VM subsystem's on-demand paging.
- Syscall dispatcher matches `syscalls::Sysno` enum; unimplemented syscalls return `AxError::Unsupported`. Adding syscalls requires matching the enum variant, not defining new dispatch mechanisms.
- `NO_AXSTD=y` changes feature prefix from `axstd/` to `axfeat/` — affects feature resolution in Cargo.toml and build system. Architecture changes that touch feature flags must handle both modes.
