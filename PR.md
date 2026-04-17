# PR 描述

**标题**: fix(fs): support fsync on directory fds, add sync_file_range stub

## Bug 描述

StarryOS 的 `sys_fsync`/`sys_fdatasync` 在传入目录文件描述符时返回 `EBADF`，而 Linux 允许对目录 fd 调用 `fsync` 以刷新目录元数据（参考 `fsync(2)`）。PostgreSQL 在崩溃恢复后会对数据目录执行 `fsync`，StarryOS 返回错误导致 PostgreSQL 启动失败。

`sync_file_range` 系统调用未实现，调用时触发 unimplemented 路径返回 `ENOSYS`。PostgreSQL WAL 写入路径使用 `sync_file_range(2)` 作为性能优化手段，`ENOSYS` 会导致 WAL 刷写失败。

## 根本原因分析

`File::from_fd` 在 fd 指向目录时返回 `AxError::IsADirectory`，原来的 `sys_fsync` 直接将此错误向上传播。Linux 内核的 `vfs_fsync_range` 对目录 fd 同样执行 sync 操作（目录的 inode 也有脏页）；对于没有目录级 sync 实现的文件系统，静默返回 0 是正确行为。

`sync_file_range` 是建议性操作（advisory），内核可以选择不执行任何操作而直接返回 0，这在 Linux 文档中有明确说明（参考 `sync_file_range(2)` NOTES 节）。

## 修复方案

**`os/StarryOS/kernel/src/syscall/fs/io.rs`**

`sys_fsync` 和 `sys_fdatasync` 对 `File::from_fd` 的返回值分情况处理：
- `Ok(f)`：正常调用 `f.inner().sync()`。
- `Err(AxError::IsADirectory)`：返回 `Ok(0)`，与 Linux 对目录 fd fsync 的行为一致。
- 其他错误：向上传播。

**`os/StarryOS/kernel/src/syscall/mod.rs`**

新增 `Sysno::sync_file_range` 分发项，直接返回 `Ok(0)`。注释说明这是建议性优化接口，始终返回成功符合规范。

## 测试

测试用例位于 `test-suit/starryos/normal/test-fsync-dir/`，RISC-V 64 QEMU 运行。

测试覆盖：
- 对目录 fd 调用 `fsync` 返回 0
- 对目录 fd 调用 `fdatasync` 返回 0
- 对普通文件 fd 调用 `fsync`/`fdatasync` 仍返回 0（原有路径不受影响）
- `sync_file_range` 系统调用返回 0（通过 `syscall(SYS_sync_file_range, ...)` 直接调用）
