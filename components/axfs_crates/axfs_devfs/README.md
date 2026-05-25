# ax-fs-devfs

Device filesystem implementation for ArceOS.

This crate implements the current `ax-fs-vfs` object model directly. It does
not expose or depend on the removed legacy `VfsOps` / `VfsNodeOps` interface.

## Provided Nodes

- `null`
- `zero`
- `urandom`

## Usage

```rust
let fs = ax_fs_devfs::new();
let root = ax_fs_vfs::Mountpoint::new_root(&fs).root_location();
```

The returned value is an `ax_fs_vfs::Filesystem` and can be mounted through the
normal `Location::mount()` path.
