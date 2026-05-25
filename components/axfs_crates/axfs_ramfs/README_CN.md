# ax-fs-ramfs

ArceOS 的内存文件系统实现。

该 crate 直接实现当前 `ax-fs-vfs` 对象模型，不暴露也不依赖已经移除的旧
`VfsOps` / `VfsNodeOps` 接口。

## 功能

- 内存目录和普通文件。
- 硬链接共享同一个底层文件节点。
- 符号链接在内存中保存 target。
- `rename` 会重建目录项引用，保证绝对路径仍然正确。

## 使用方式

```rust
let fs = ax_fs_ramfs::new();
let root = ax_fs_vfs::Mountpoint::new_root(&fs).root_location();
```

返回值是 `ax_fs_vfs::Filesystem`，可以通过标准的 `Location::mount()` 路径挂载。
