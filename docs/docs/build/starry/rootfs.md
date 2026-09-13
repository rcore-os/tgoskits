---
sidebar_position: 8
sidebar_label: "rootfs 准备"
---

# StarryOS rootfs 准备

`cargo xtask starry rootfs` 按架构准备默认 managed rootfs，并打印 image storage 中的最终路径。这是 StarryOS 独有的便捷命令；[ArceOS](../arceos/runtime) 和 [Axvisor](../axvisor/runtime) 不暴露对应子命令，它们在各自运行路径中准备所选 rootfs。

## 1. 命令

`rootfs` 子命令只接受架构选择参数，执行后会在标准输出打印可供脚本消费的镜像路径。

```bash
cargo xtask starry rootfs [--arch <ARCH>]
```

参数表如下。

| 参数 | 说明 |
|------|------|
| `--arch <ARCH>` | 目标架构（默认 `riscv64`） |

## 2. 行为

`rootfs(starry, args)` 的执行流程：

1. 解析架构（默认 `riscv64`），通过 `starry_target_for_arch_checked` 校验并得到 target triple
2. 调用 `ensure_rootfs_in_tmp_dir(workspace_root, arch, target)`：
   - 按架构默认镜像名（`rootfs-<arch>-alpine.img`）在 image storage 中查找
   - 缺失时从远端注册表拉取、SHA-256 校验、解压（详见 [镜像管理](../image)）
3. 打印最终 rootfs 镜像路径：`rootfs ready at <path>`

## 3. 与运行命令的关系

`cargo xtask starry qemu` 和 `cargo xtask starry test qemu` 在运行前会自动调用 `ensure_qemu_rootfs_ready` 完成 rootfs 准备，因此**大多数情况下不需要手动执行 `rootfs` 命令**。它的用途是：

- **预拉取**：在无网络环境（如离线 CI）前预先下载好 rootfs
- **路径确认**：获取 image storage 中 rootfs 的实际路径，供其他工具或脚本使用
- **调试**：验证 rootfs 拉取链路是否正常

## 4. 用法示例

以下示例覆盖默认架构、显式架构和脚本取路径三种常见用法。

```bash
# 预拉取默认架构的 rootfs
cargo xtask starry rootfs

# 预拉取指定架构
cargo xtask starry rootfs --arch aarch64

# 在脚本中获取路径
ROOTFS=$(cargo xtask starry rootfs --arch riscv64 2>/dev/null | grep -oP '(?<=rootfs ready at ).+')
echo "rootfs at: $ROOTFS"
```
