# ArceOS Rust std support

ArceOS std applications are built with Cargo's std-aware build mode. The app
uses Rust's upstream `std` for a built-in `*-unknown-linux-musl` target, while
the application enables its `arceos` feature to depend on `ax-std` directly, so
`app + ax-std + axruntime` are compiled in one Cargo dependency graph. The fake
`libc.a` only satisfies the fixed library name requested by the compiler; the
real libc/syscall compatibility symbols come from `ax-std`.

This path does not require changes to `rust-lang/rust`; it uses built-in
linux-musl targets directly.

## Run the std examples

Examples are under `examples/std`.

Use `axbuild`/`xtask` instead of running Cargo in the example directory
directly. `axbuild` maps the ArceOS bare-metal target to the matching built-in
linux-musl target, creates empty fake `libc.a`/`libunwind.a` placeholders,
and installs the linker wrapper.

Example:

```bash
cargo xtask arceos test qemu \
  --target x86_64-unknown-none \
  --test-group rust \
  --test-case helloworld
```

The std build path uses:

- `-Z build-std=std,panic_abort`
- `-Z build-std-features=`
- `panic = "abort"`
- empty fake `libc.a`
- empty fake `libunwind.a`

## Select ArceOS features

An app declares the ArceOS-side features it needs behind its app-local
`arceos` feature. Without this feature, the same app remains an ordinary Rust
`std` app and does not depend on `ax-std`.

```toml
[features]
default = []
arceos = ["dep:ax-std", "ax-std/fs", "ax-std/net", "ax-std/multitask"]

[dependencies]
ax-std = { workspace = true, optional = true }
```

Keep logging and normal Cargo features for app-local choices. For example:

```toml
[features]
default = []
dns = []

[package.metadata.axstd]
features = ["log-level-debug"]
```

`axbuild` automatically enables `arceos` for ArceOS std builds when the app
declares it. It combines app features and `package.metadata.axstd.features`,
maps ArceOS backend features to `ax-std/*`, and adds the platform feature for
the selected ArceOS target before linking the app.

## Runtime compatibility

Use a Cargo feature such as `arceos` for ArceOS std-specific behavior:

```rust
#[cfg(feature = "arceos")]
const API_BASE: &str = "http://10.0.2.2:8080/v1";
```

Do not add an external runtime shim dependency, and do not gate ArceOS std
behavior on a synthetic target OS name or custom rustc cfg.

## Disk image preparation

Examples that need a filesystem expect `disk.img` in the example/test working
directory. Both ext4 and FAT32 filesystems are supported.

```bash
dd if=/dev/zero of=disk.img bs=1M count=64
mkfs.ext4 -F disk.img
```

or:

```bash
truncate -s 64M disk.img
mkfs.vfat -F 32 disk.img
```
