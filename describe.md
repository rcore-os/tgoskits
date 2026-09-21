# openat 权限修复说明

本文说明 [Issue #1761](https://github.com/rcore-os/tgoskits/issues/1761) 的问题、修复内容和验证结果。

## 1. 问题

### 1.1 原因

StarryOS 原来只检查新建文件的父目录权限。打开已有文件时没有根据调用者身份和文件 mode 检查最终 inode，因此普通用户可能读写其他用户的文件，`O_TRUNC` 还可能在权限不足时清空文件。

Linux 的相关语义可参考 [`fs/namei.c`](https://github.com/torvalds/linux/blob/v6.1/fs/namei.c) 和 [`fs/open.c`](https://github.com/torvalds/linux/blob/v6.1/fs/open.c)：路径组件需要搜索权限，已有文件需要读写权限，`O_TRUNC` 需要写权限，`O_PATH` 不需要最终 inode 的读写权限。

### 1.2 影响

问题影响 `open`、`openat`、`openat2` 以及 `/proc/self/fd` 重开文件的路径。无权限的请求可能成功，或者造成文件内容被截断。

## 2. 修复

### 2.1 最终 inode 权限检查

[`FsContext::check_permission`](fs/ax-fs-ng/src/fs_core/context.rs) 统一处理 owner、group、other、补充组和 capability。 [`OpenOptions::check_open_access`](fs/ax-fs-ng/src/file/open.rs) 根据打开标志调用它：

- `O_RDONLY` 检查读权限。
- `O_WRONLY`、`O_RDWR` 和 `O_TRUNC` 检查写权限。
- `O_PATH` 跳过最终 inode 的读写检查，但仍保留路径搜索检查。
- 权限检查发生在创建后端、获取写访问和 `set_len(0)` 之前。

新建文件使用父目录权限控制，不会因为新文件的 mode 为 `000` 而拒绝本次创建。只读文件系统检查仍在修改操作之前执行。

### 2.2 统一打开入口

[`DirNode::open_file_with_status`](fs/axfs-ng-vfs/src/node/dir.rs) 和 [`Location::open_file_with_status`](fs/axfs-ng-vfs/src/mount/mod.rs) 返回文件是否由本次调用创建，避免把已有文件误判为新建文件。

StarryOS 的 `openat2` 特殊路径和 `/proc/self/fd` 重开路径改用 `open_loc_with_credentials`，确保用户凭据不会被绕过。相关入口位于 [`fd_ops.rs`](os/StarryOS/kernel/src/syscall/fs/fd_ops.rs)。

`Cred` 到 `MutationCredentials` 的转换统一位于 [`syscall/fs/mod.rs`](os/StarryOS/kernel/src/syscall/fs/mod.rs)，`fd_ops.rs` 和 `ctl.rs` 共同复用这一处映射，避免新增凭据字段时出现路径漂移。

## 3. 测试与验证

### 3.1 回归测试

测试文件为 [`bugfix-bug-dir-mutation-permissions/src/main.c`](test-suit/starryos/qemu/system/bugfix-bug-dir-mutation-permissions/src/main.c)，新增覆盖：

- 普通用户无权限读、写或执行 `O_TRUNC`。
- 失败的 `O_TRUNC` 不改变文件大小。
- `O_PATH`、文件所有者和补充组权限。
- `CAP_DAC_READ_SEARCH` 与 `CAP_DAC_OVERRIDE` 的区别。

同时在 [`file/open.rs`](fs/ax-fs-ng/src/file/open.rs) 增加宿主侧单元测试，覆盖 owner/group/other 模式选择、`O_TRUNC` 写权限、`O_PATH` 绕过和新建 inode 分支。

### 3.2 验证结果

`cargo fmt --check`、`git diff --check`、回归测试 C 语法检查、`cargo xtask clippy --package ax-fs-ng` 和 `cargo xtask clippy --package axfs-ng-vfs` 均通过；`cargo xtask test` 也完成了 `ax-fs-ng` 的宿主测试 profile。

完整 `cargo xtask test` 和 StarryOS clippy 仍受环境限制：`axvm`、`virtualization-tests`、`starry-kernel` 在构建阶段因缺少 `libclang.so` 被 `bindgen` 阻塞。因此尚未获得实际 QEMU 运行结果。
