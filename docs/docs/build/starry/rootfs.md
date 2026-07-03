---
sidebar_position: 8
sidebar_label: "rootfs 准备"
---

# StarryOS rootfs 准备

`cargo xtask starry rootfs` 按架构准备默认 managed rootfs，并打印 image storage 中的最终路径。这是 StarryOS 独有的便捷命令，[ArceOS](../arceos/runtime) 和 [Axvisor](../axvisor/runtime) 在运行时自动准备 rootfs 但不暴露独立命令。

## 命令

```bash
cargo xtask starry rootfs [--arch <ARCH>]
```

| 参数 | 说明 |
|------|------|
| `--arch <ARCH>` | 目标架构（默认 `riscv64`） |

## 行为

`rootfs(starry, args)` 的执行流程：

1. 解析架构（默认 `riscv64`），通过 `starry_target_for_arch_checked` 校验并得到 target triple
2. 调用 `ensure_rootfs_in_tmp_dir(workspace_root, arch, target)`：
   - 按架构默认镜像名（`rootfs-<arch>-alpine.img`）在 image storage 中查找
   - 缺失时从远端注册表拉取、SHA-256 校验、解压（详见 [镜像管理](../image)）
3. 打印最终 rootfs 镜像路径：`rootfs ready at <path>`

## 与运行命令的关系

`cargo starry qemu` 和 `cargo starry test qemu` 在运行前会自动调用 `ensure_qemu_rootfs_ready` 完成 rootfs 准备，因此**大多数情况下不需要手动执行 `rootfs` 命令**。它的用途是：

- **预拉取**：在无网络环境（如离线 CI）前预先下载好 rootfs
- **路径确认**：获取 image storage 中 rootfs 的实际路径，供其他工具或脚本使用
- **调试**：验证 rootfs 拉取链路是否正常

## 用法示例

```bash
# 预拉取默认架构的 rootfs
cargo starry rootfs

# 预拉取指定架构
cargo starry rootfs --arch aarch64

# 在脚本中获取路径
ROOTFS=$(cargo starry rootfs --arch riscv64 2>/dev/null | grep -oP '(?<=rootfs ready at ).+')
echo "rootfs at: $ROOTFS"
```
