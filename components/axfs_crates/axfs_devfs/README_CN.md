# ax-fs-devfs

ArceOS 的设备文件系统实现。

该 crate 直接实现当前 `ax-fs-vfs` 对象模型，不暴露也不依赖已经移除的旧
`VfsOps` / `VfsNodeOps` 接口。

## 提供的节点

- `null`
- `zero`
- `urandom`

## 使用方式

```rust
let fs = ax_fs_devfs::new();
let root = ax_fs_vfs::Mountpoint::new_root(&fs).root_location();
```

返回值是 `ax_fs_vfs::Filesystem`，可以通过标准的 `Location::mount()` 路径挂载。
