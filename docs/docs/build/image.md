# 镜像管理

axbuild 负责从 [rcore-os/tgosimages](https://github.com/rcore-os/tgosimages) 获取 rootfs 和其他运行镜像。镜像管理分为两个独立目录：

- `download_dir` 保存从 registry 指向的压缩归档，由 axbuild 校验和覆盖。
- `extract_dir` 保存解压后的工作镜像，允许构建脚本、调试工具和 QEMU 修改。

## 配置

axbuild 第一次读取镜像配置时会生成 `<workspace>/tmp/axbuild/.image.toml`：

```toml
registry = "https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/default.toml"
download_dir = "/tmp/tgosimages"
extract_dir = "<workspace>/tmp/axbuild/rootfs"
```

下载目录取系统临时目录；Linux 上默认为 `/tmp/tgosimages`。解压目录仍位于当前 workspace，避免不同源码工作区共享可修改的 rootfs。

配置文件是由 axbuild 管理的本机配置，已被 `.gitignore` 忽略。读取时只关心 `registry`、`download_dir` 和 `extract_dir`，其他字段不解释也不迁移。三个当前字段必须完整且类型正确；字段缺失、类型无效或文件不是有效 TOML 时，axbuild 使用全部默认值重新生成。读取后配置会被回写为只包含三个当前字段的规范格式，因此旧字段和任意额外字段都会被删除。需要固定镜像版本时，应提供完整的当前格式配置。

也可以使用环境变量或命令行覆盖目录：

| 配置 | 环境变量 | 命令行 |
|------|----------|--------|
| 下载目录 | `TGOS_IMAGE_DOWNLOAD_DIR` | `-D/--download-dir` |
| 解压目录 | `TGOS_IMAGE_EXTRACT_DIR` | `-E/--extract-dir` |
| registry | — | `-R/--registry` |

优先级为：命令行、环境变量、`.image.toml`。

例如，把下载缓存放到持久目录，同时把可修改 rootfs 留在当前工作区：

```bash
TGOS_IMAGE_DOWNLOAD_DIR=/data/tgosimages \
  cargo xtask starry qemu --arch riscv64
```

Linux 上的默认目录结构如下：

```text
/tmp/tgosimages/
├── images.toml
└── rootfs-riscv64-alpine.img.tar.xz

<workspace>/tmp/axbuild/rootfs/
└── rootfs-riscv64-alpine.img
```

`images.toml` 是本次获取并展开 includes 后的 registry 副本，仅用于查看和诊断，不参与 registry 新旧判断。

## 更新规则

每次创建镜像存储时，axbuild 都会获取配置中的 registry，不使用同步时间戳或过期天数。

准备 managed rootfs 时执行以下流程：

1. 从 registry 解析镜像名称、版本、下载 URL 和 SHA-256。
2. 计算 `download_dir` 中现有归档的 SHA-256。
3. SHA-256 一致时复用归档，不访问镜像下载 URL。
4. 归档不存在或 SHA-256 不一致时，重新下载并覆盖归档。
5. 只有归档本次被新增或替换，或者目标 rootfs 不存在时，才重新解压。
6. 归档未变化且 rootfs 已存在时，直接保留工作 rootfs。

因此，对 `extract_dir` 中 rootfs 的修改不会触发重新下载或重新解压。registry 指向新归档并给出不同 SHA-256 时，axbuild 才会下载新归档并重建工作 rootfs。

下载先写入同目录的 `.part` 文件，SHA-256 校验通过后才成为正式归档。校验失败的下载不会保留为正式文件。

## 固定版本调试

日常构建使用 `default.toml` 跟踪当前版本。本地需要长期修改 rootfs 时，将忽略提交的 `.image.toml` 指向不可变的版本 registry：

```toml
registry = "https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/v0.0.11.toml"
download_dir = "/tmp/tgosimages"
extract_dir = "/home/user/tgoskits/tmp/axbuild/rootfs"
```

只要该版本 registry 中的归档 SHA-256 不变，后续运行就会复用归档并保留修改后的 rootfs。升级时将 `registry` 改为另一个版本文件，或恢复为 `default.toml`。

版本 registry 和对应 release 归档发布后必须保持不可变；rootfs 内容变化时应发布新版本。

`image pull` 也接受 `name:version`：

```bash
cargo xtask image -R https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/v0.0.11.toml \
  pull rootfs-riscv64-alpine.img:0.0.11
```

指定版本必须存在于所选 registry 中。

## 命令

列出 registry 中的镜像：

```bash
cargo xtask image ls
cargo xtask image ls --verbose rootfs
```

准备指定 rootfs：

```bash
cargo xtask image pull rootfs-riscv64-alpine.img
```

按架构准备默认 rootfs：

```bash
cargo xtask image pull --arch riscv64
```

只下载通用镜像归档，不解压：

```bash
cargo xtask image pull qemu-aarch64 --no-extract
```

通用镜像默认解压到 `extract_dir/<name>`。Managed rootfs 则直接输出为 `extract_dir/<rootfs-name>.img`。

计算或校验本地文件 SHA-256：

```bash
cargo xtask image check tmp/axbuild/rootfs/rootfs-riscv64-alpine.img
cargo xtask image check rootfs.img --sha256 <expected-sha256>
```

扩展 ext rootfs：

```bash
cargo xtask image resize tmp/axbuild/rootfs/rootfs-riscv64-alpine.img --size-mib 2048
cargo xtask image resize rootfs.img --size-mib 2048 --output resized.img
```

## CI 配置

自托管 runner 默认复用 `/tmp/tgosimages` 中的下载缓存，无需额外配置。需要覆盖时仍只应共享下载目录：

```yaml
env:
  TGOS_IMAGE_DOWNLOAD_DIR: /tmp/tgosimages
```

不要默认跨任务共享 `extract_dir`。其中的 rootfs 允许被测试和 QEMU 修改，共享会让不同任务互相污染。未设置 `TGOS_IMAGE_EXTRACT_DIR` 时，它保持为当前 workspace 下的 `tmp/axbuild/rootfs`。

## 故障处理

registry 获取失败时，axbuild 会直接报错，不会用历史 registry 或固定 fallback 冒充最新版本。

归档损坏时无需手动清理；下次准备镜像会校验失败并重新下载。需要主动恢复工作 rootfs 时，删除 `extract_dir` 中对应的 rootfs 文件，再次运行准备命令：

```bash
rm tmp/axbuild/rootfs/rootfs-riscv64-alpine.img
cargo xtask image pull --arch riscv64
```
