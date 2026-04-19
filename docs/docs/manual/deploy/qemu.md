---
title: "QEMU 部署"
sidebar_label: "QEMU"
---

# QEMU 部署

QEMU 是当前仓库最重要的通用验证环境，三套系统都依赖它完成开发验证。

## 使用场景

| 系统 | 用途 |
|------|------|
| **ArceOS** | 最小样例验证（helloworld、网络、文件系统等） |
| **StarryOS** | rootfs 与用户态链路验证 |
| **Axvisor** | Guest 启动、配置与虚拟化路径验证 |

## Axvisor 推荐部署步骤

```bash
# 1. 准备 Guest 镜像和运行时资源
cd os/axvisor
./scripts/setup_qemu.sh arceos
cd ../..

# 2. 启动 Axvisor
cargo xtask axvisor qemu --arch aarch64
```

`setup_qemu.sh` 脚本会自动完成：

1. 下载并解压 Guest 镜像到临时目录
2. 从 `configs/vms/` 生成运行时 VM 配置
3. 修正 VM 配置中的 `kernel_path`
4. 复制 `rootfs.img` 到运行时目录

> 如需启动 Linux Guest，将脚本参数改为 `linux`。

## 开发注意事项

- `setup_qemu.sh` 负责准备大量运行时资源，不要跳过
- `cargo xtask axvisor build` 成功 **不代表** 能成功启动 Guest
- 真正启动前要确认：Guest 镜像、`kernel_path`、rootfs 和 VM 配置已对齐
- 多 Guest 场景需要调整 `configs/vms/*.toml` 与运行时配置

## 相关文档

- [ArceOS 快速上手](/docs/quickstart/arceos)
- [Axvisor 开发指南](/docs/design/systems/axvisor-guide)
- [AxVisor 内部机制](/docs/design/architecture/axvisor-internals)
- [Guest 管理入口](../guest/cmd)
