# Rootfs 镜像管理

TGOSKits 的 StarryOS 和 Axvisor 运行需要配套的 rootfs 镜像。这些镜像托管在 [rcore-os/tgosimages](https://github.com/rcore-os/tgosimages) 仓库，通过 xtask 命令按需下载到本地 `target/rootfs/` 目录。

---

## 1. 镜像仓库

[tgosimages](https://github.com/rcore-os/tgosimages) 以 GitHub Release 形式发布各架构的 rootfs 镜像压缩包。当前版本：

| 配置项 | 值 |
|--------|-----|
| 仓库 | `rcore-os/tgosimages` |
| 版本标签 | `v0.0.5` |
| 格式 | `.img.tar.xz`（tar + xz 压缩的 ext4 镜像） |
| 下载 URL | `https://github.com/rcore-os/tgosimages/releases/download/v0.0.5/<filename>` |

## 2. 可用镜像

当前提供的 rootfs 镜像（基于 Alpine Linux）：

| 文件名 | 架构 | 基础发行版 | 用途 |
|--------|------|-----------|------|
| `rootfs-aarch64-alpine.img` | aarch64 | Alpine Linux | StarryOS / Axvisor aarch64 |
| `rootfs-riscv64-alpine.img` | riscv64 | Alpine Linux | StarryOS riscv64 |
| `rootfs-x86_64-alpine.img` | x86_64 | Alpine Linux | StarryOS / Axvisor x86_64 |
| `rootfs-loongarch64-alpine.img` | loongarch64 | Alpine Linux | StarryOS loongarch64 |

镜像内包含基本的用户态工具：busybox、shell、基础命令等。StarryOS 使用的 rootfs 还包含 Python 环境。

## 3. 本地存储

rootfs 镜像下载后存储在工作区的 `target/rootfs/` 目录下：

```
target/rootfs/
├── rootfs-aarch64-alpine.img
├── rootfs-aarch64-alpine.img.tar.xz    # 下载的压缩包（缓存）
├── rootfs-riscv64-alpine.img
├── rootfs-riscv64-alpine.img.tar.xz
├── rootfs-x86_64-alpine.img
├── rootfs-x86_64-alpine.img.tar.xz
└── ...
```

- **`.img` 文件**：解压后的 ext4 镜像，可直接被 QEMU 挂载使用
- **`.img.tar.xz` 文件**：从 GitHub Release 下载的压缩包，作为缓存避免重复下载

## 4. 下载与使用

### 4.1 通过 xtask 自动下载

xtask 在需要 rootfs 时会自动检查并按需下载：

```bash
# StarryOS — 首次运行会自动下载 rootfs
cargo xtask starry qemu --arch riscv64
cargo xtask starry qemu --arch aarch64

# Axvisor — 需要先准备 Guest 镜像
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
```

### 4.2 手动下载 rootfs

```bash
# 下载并解压到 target/rootfs/
mkdir -p target/rootfs
cd target/rootfs

# 下载 aarch64 rootfs
wget https://github.com/rcore-os/tgosimages/releases/download/v0.0.5/rootfs-aarch64-alpine.img.tar.xz
tar xJf rootfs-aarch64-alpine.img.tar.xz

# 下载 riscv64 rootfs
wget https://github.com/rcore-os/tgosimages/releases/download/v0.0.5/rootfs-riscv64-alpine.img.tar.xz
tar xJf rootfs-riscv64-alpine.img.tar.xz
```

### 4.3 StarryOS 专用命令

```bash
# 通过 xtask 下载 StarryOS rootfs
cargo xtask starry rootfs --arch riscv64
cargo xtask starry rootfs --arch aarch64
```

### 4.4 Axvisor 专用命令

Axvisor 的 rootfs 通常与 Guest 镜像一起准备：

```bash
# 使用官方脚本自动准备（推荐）
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)

# 或手动通过 xtask
cargo xtask axvisor qemu --rootfs alpine --arch aarch64
```

## 5. 镜像管理机制

### 5.1 按需下载

xtask 的 rootfs 管理逻辑位于 `scripts/axbuild/src/rootfs/store.rs`，核心流程：

1. 检查 `target/rootfs/<image>.img` 是否存在且 ≥ 1 MiB
2. 如存在，直接使用
3. 如不存在，检查对应的 `.tar.xz` 缓存
4. 如缓存不存在，从 GitHub Release 下载
5. 解压 `.tar.xz` 得到 `.img` 文件
6. 如解压失败，删除缓存包并重新下载

### 5.2 `--rootfs` 参数

xtask 命令的 `--rootfs` 参数支持以下格式：

| 参数值 | 解析结果 |
|--------|---------|
| `alpine` | `target/rootfs/rootfs-<arch>-alpine.img` |
| `debian` | `target/rootfs/rootfs-<arch>-debian.img` |
| `busybox` | `target/rootfs/rootfs-<arch>-busybox.img` |
| `/path/to/custom.img` | 直接使用指定路径 |

### 5.3 损坏检测

如果 `.img` 文件小于 1 MiB（可能是之前解压中断导致），xtask 会自动删除并重新下载。

## 6. 查看和修改 rootfs 内容

```bash
# 创建挂载点
mkdir -p /mnt/rootfs

# 挂载镜像
sudo mount -o loop target/rootfs/rootfs-riscv64-alpine.img /mnt/rootfs

# 查看内容
ls /mnt/rootfs
ls /mnt/rootfs/bin    # 用户态工具
ls /mnt/rootfs/lib    # 库文件

# 添加自定义程序
sudo cp my_program /mnt/rootfs/root/
sudo chmod +x /mnt/rootfs/root/my_program

# 卸载
sudo umount /mnt/rootfs
```

## 7. 构建自定义 rootfs

如需包含额外工具或库，可基于现有镜像修改或自行构建：

### 7.1 基于现有镜像修改

```bash
# 挂载并 chroot（需要 qemu-user-static）
sudo mount -o loop target/rootfs/rootfs-aarch64-alpine.img /mnt/rootfs
sudo cp /usr/bin/qemu-aarch64-static /mnt/rootfs/usr/bin/
sudo chroot /mnt/rootfs /bin/sh

# 在 chroot 环境中安装包
apk add python3 gcc
exit

sudo umount /mnt/rootfs
```

### 7.2 从零构建

可参考 [tgosimages](https://github.com/rcore-os/tgosimages) 仓库中的构建脚本，基于 Alpine Linux 的 `alpine-make-rootfs` 或 `debootstrap` 工具构建自定义镜像。

## 8. 更新镜像版本

镜像版本在 `scripts/axbuild/src/rootfs/store.rs` 中定义：

```rust
const TGOSIMAGES_ROOTFS_RELEASE: &str = "v0.0.5";
```

当 tgosimages 发布新版本时，更新此常量并提交即可。xtask 会在下次需要时自动下载新版本（旧缓存不会被自动清理）。

## 9. 常见问题

### Q: 下载速度慢？

GitHub Releases 在部分地区访问较慢。可手动下载压缩包后放入 `target/rootfs/` 目录：

```bash
# 使用镜像站点或代理下载后
cp rootfs-aarch64-alpine.img.tar.xz target/rootfs/
cd target/rootfs
tar xJf rootfs-aarch64-alpine.img.tar.xz
```

### Q: rootfs 和 StarryOS Makefile 的镜像路径不同？

- **xtask 路径**：`target/rootfs/rootfs-<arch>-alpine.img`
- **Makefile 路径**：`os/StarryOS/make/disk.img`

两者不互通。建议统一使用 xtask 入口。

### Q: 如何确认 rootfs 架构正确？

```bash
file target/rootfs/rootfs-riscv64-alpine.img
# 输出应包含对应架构信息
```

也可以挂载后检查：

```bash
sudo mount -o loop target/rootfs/rootfs-riscv64-alpine.img /mnt/rootfs
file /mnt/rootfs/bin/busybox
sudo umount /mnt/rootfs
```
