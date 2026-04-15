# Code Mode Rules — StarryOS

- **Dual error types**: Use `AxResult/AxError` for internal kernel ops, `LinuxResult/LinuxError` for syscall returns. Convert via `AxError → LinuxError → negative errno`. Use `ax_bail!` for early returns.
- **`#[extern_trait]`** (from extern-trait crate): implements traits defined in external crates via FFI-like vtable dispatch. Used in `kernel/src/task/mod.rs` and `kernel/src/mm/access.rs`.
- **`#[page_fault_handler]`**: registers a function as the page fault handler for user memory access / CoW / lazy allocation (`kernel/src/mm/access.rs`).
- **`nullable!` macro**: safely handles optional user-space pointers — returns `Ok(None)` for null (`kernel/src/mm/access.rs`).
- **`DummyFd` pattern**: stub FDs for unimplemented syscalls (timerfd, fanotify, etc.) return dummy FDs EXCEPT under QEMU where they return `AxError::Unsupported` (`kernel/src/syscall/fs/io.rs`).
- **`rustfmt.toml`**: `format_strings = true` aggressively reformats string literals — don't fight it. `group_imports = "StdExternalCrate"`, `imports_granularity = "Crate"`.
- **Edition 2024**: use `#[unsafe(no_mangle)]` not `#[no_mangle]` on entry points.
- **Global allows** in `kernel/src/lib.rs`: `missing_docs` and `clippy::not_unsafe_ptr_arg_deref` — don't add doc comments or unsafe ptr guards that conflict.
- **`#[rustfmt::skip]`** on `cfg_if!` blocks in `kernel/src/config/mod.rs` — don't remove it.
- **Arch-specific syscalls**: x86_64 has legacy syscalls (fork, open, dup2); aarch64/loongarch64 do not. Match `syscalls::Sysno` enum; return `AxError::Unsupported` for unimplemented.
