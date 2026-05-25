# ax-fs-ramfs

In-memory filesystem implementation for ArceOS.

This crate implements the current `ax-fs-vfs` object model directly. It does
not expose or depend on the removed legacy `VfsOps` / `VfsNodeOps` interface.

## Features

- In-memory directories and regular files.
- Hard links share the same underlying file node.
- Symbolic links store their target in memory.
- `rename` rebuilds directory entry references so absolute paths remain correct.

## Usage

```rust
let fs = ax_fs_ramfs::new();
let root = ax_fs_vfs::Mountpoint::new_root(&fs).root_location();
```

The returned value is an `ax_fs_vfs::Filesystem` and can be mounted through the
normal `Location::mount()` path.
