# pip + uv 离线测试 (4-arch)

在 StarryOS（CPython 3.14 musl rootfs）上验证 Python 包管理器 **pip 26.1.2** 与 **uv 0.11.19** 的常用操作。**全部离线**（本地 wheel / 本地 uv 二进制），覆盖 x86_64 / aarch64 / riscv64 / loongarch64 四个架构。

## 测试内容

`test_pipuv.sh` 分多个阶段，覆盖 pip 与 uv 的日常离线使用场景：

**pip（从 ensurepip 内置 wheel 自举 26.1.2）**
- `python3 --version`（3.14）/ `pip3 --version`（26.1.2）
- pip list / show / freeze / check
- `pip3 install --no-index --find-links /opt/wheels setuptools wheel`（本地 build backend）
- `pip3 wheel` / `pip3 download`（离线，针对本地 fixture 包）
- `python3 -m venv --without-pip` + 在 venv 内以同进程方式直装 pip / setuptools

**uv（本地二进制 0.11.19）**
- `uv --version`（0.11.19）/ `uv --help`
- `uv venv`（缓存放磁盘 `/root/.uvcache`，非 tmpfs）
- `uv pip install --no-index --find-links /opt/wheels ...` / `uv pip list`
- `uv run --no-project python3 -c ...`
- `uv run --no-project --script`（PEP 723 `# /// script` 内联元数据）

## 运行测试

```bash
# 单核 qemu, 逐架构 (x86_64 / aarch64 / riscv64 / loongarch64):
cargo xtask starry app qemu -t pip-uv --arch x86_64
```

（`cargo xtask starry app list` 可确认 `pip-uv` 被发现；本仓库无 `app run` 子命令。）

## 关于 test_pipuv.sh

`test_pipuv.sh` 是 **guest 内**脚本：由上面的 app 框架复制到 rootfs 的 `/usr/bin/`，在 QEMU 启动后由 guest 执行。它按 guest 环境写死路径（`/opt/wheels`、`/root/.uvcache`、`/root/v`）、设置 `PIP_BREAK_SYSTEM_PACKAGES=1` 并执行 `pip3 install` / `pip3 uninstall`，因此**不要在 Linux 主机上直接 `sh` 它**——在主机上要么失败、要么污染主机的 Python 环境。只想离机审阅逻辑请直接阅读该源文件。

## 资产准备

测试运行**全部离线**，但镜像构建（`prebuild.sh`）需要以下本地资产预先就位（下载日 2026-06-09 的各源最新版）。干净环境中若任一资产缺失，构建会在进入 guest 前以非零状态失败，并打印缺失的具体路径（见下表来源 URL 自行获取后放到默认路径，或用环境变量改指它处）。

| 资产 | 版本 | 默认路径 | 来源 URL |
|---|---|---|---|
| pip wheel（架构无关） | 26.1.2 | `$HOME/rcore/download/pip-uv/pip-26.1.2-py3-none-any.whl` | PyPI：`https://pypi.org/pypi/pip/26.1.2/json` → `files.pythonhosted.org` 上的 `.whl` |
| uv（x86_64，静态 musl） | 0.11.19 | `$HOME/rcore/download/pip-uv/uv-x86_64-unknown-linux-musl.tar.gz` | `https://github.com/astral-sh/uv/releases/download/0.11.19/uv-x86_64-unknown-linux-musl.tar.gz` |
| uv（aarch64，静态 musl） | 0.11.19 | `$HOME/rcore/download/pip-uv/uv-aarch64-unknown-linux-musl.tar.gz` | `https://github.com/astral-sh/uv/releases/download/0.11.19/uv-aarch64-unknown-linux-musl.tar.gz` |
| uv（riscv64，静态 musl） | 0.11.19 | `$HOME/rcore/download/pip-uv/uv-riscv64gc-unknown-linux-musl.tar.gz` | `https://github.com/astral-sh/uv/releases/download/0.11.19/uv-riscv64gc-unknown-linux-musl.tar.gz` |
| uv（loongarch64，动态 musl） | 0.11.19-r0 | `$HOME/rcore/download/pip-uv/uv-loongarch64-uv-0.11.19-r0.apk` | `https://dl-cdn.alpinelinux.org/alpine/edge/community/loongarch64/uv-0.11.19-r0.apk`（astral-sh 官方不发布 loong 二进制，故取 Alpine community 同版本原生构建） |
| build-backend wheels | setuptools 82.0.1 / wheel 0.47.0 / packaging / six | `$HOME/rcore/pipuv-work/offline-wheels/*.whl` | PyPI（`pip download setuptools wheel packaging six -d <dir>` 离线下载） |

可选 fast-path：若 `$HOME/rcore/pipuv-work/uvbins/uv-<arch>` 已有预解压的 per-arch uv 二进制，`prebuild.sh` 直接复制它，跳过解包 tarball/apk。

资产路径可用环境变量覆盖（不在默认位置时）：

| 环境变量 | 默认值 | 含义 |
|---|---|---|
| `PIPUV_DOWNLOAD_DIR` | `$HOME/rcore/download/pip-uv` | pip wheel + per-arch uv tarball/apk |
| `PIPUV_WHEELS_DIR` | `$HOME/rcore/pipuv-work/offline-wheels` | 离线 build-backend wheels |
| `PIPUV_UVBINS_DIR` | `$HOME/rcore/pipuv-work/uvbins` | 预解压的 per-arch uv 二进制（fast-path，可选） |

校验（uv 静态二进制可在 host 直跑；loong 动态需在 rootfs 内验）：

```bash
./uvbins/uv-x86_64 --version              # uv 0.11.19 (x86_64-unknown-linux-musl)
strings <uv-bin> | grep -oE '0\.11\.19'   # 4-arch 二进制均命中 0.11.19
python3 -c "import zipfile; print('pip-26.1.2' in zipfile.ZipFile('pip-26.1.2-py3-none-any.whl').namelist()[0])"
```

## 工作原理

1. `prebuild.sh` 被 xtask 框架调用，通过 `qemu-user-static` 在 staging rootfs 中执行原生 `apk` 安装 `python3`（使用 `--no-scripts` 跳过 busybox trigger），并用 `readelf` 解析运行时依赖复制到 overlay。
2. 将本地 **pip wheel**（`download/pip-uv/pip-26.1.2-py3-none-any.whl`）写入 rootfs 的 ensurepip 内置 wheel 目录与 `/opt/wheels`，使测试可纯离线自举 pip 26.1.2。
3. 将本地 **build-backend wheels**（`pipuv-work/offline-wheels/*.whl`：setuptools / wheel / packaging / six）复制到 rootfs 的 `/opt/wheels`。
4. 将 **按架构对应的 uv 0.11.19 二进制**复制到 rootfs 的 `/usr/local/bin/uv`（x86_64 / aarch64 / riscv64 取自 astral-sh 官方 musl tar.gz；loongarch64 取自 Alpine edge community apk，因 astral-sh 官方不发布 loong 二进制）。
5. `test_pipuv.sh` 被复制到 overlay 的 `/usr/bin/`。
6. overlay 被注入到 rootfs 镜像中，QEMU 启动后执行 `/usr/bin/test_pipuv.sh`。

## 判定标准

- **通过**: 输出匹配 `STARRY_PIPUV_TESTS_PASSED`
- **失败**: 输出匹配 `STARRY_PIPUV_STAGE_.*_FAILED`，或出现 panic / page fault / segmentation fault
- **超时**: x86_64 = 1800 秒；aarch64 / riscv64 = 3600 秒；loongarch64 = 5400 秒（TCG 下 uv 较慢）

## 测试范围：离线（stages 1–17）+ 在线（stages 18–20）

**stages 1–17（离线功能覆盖）**：pip / uv 的全命令族（install / list / show / freeze / check / wheel / download / uninstall / venv / `python3 -m pip` / `uv pip` / `uv run` / PEP 723）均以 `--no-index --find-links /opt/wheels`（pip）与本地二进制 + 本地 wheel（uv）的离线形式验证。

**stages 18–20（在线安装，真实 TCP，走本地 wheel 索引，无外网）**：额外覆盖在线安装的**完整真实网络路径**。测试框架按各 `qemu-*.toml` 的 `[host_http_server]` 配置，在宿主 `127.0.0.1:18390` 起一个静态 wheel 索引（目录 `online-index/`，仅含数个小的纯 Python wheel）；guest 经 QEMU user-mode 网络（SLIRP）以 `10.0.2.2:18390` 直连宿主，真实经过 TCP 握手 + HTTP 下载 + 依赖解析 + 安装 + import：

- stage 18 `pip install`（markdown-it-py，真实拉取其依赖 mdurl）
- stage 19 `python3 -m pip install`（six，无依赖）
- stage 20 `uv pip install`（markdown-it-py，装入 uv venv）

该在线测试是**自包含（hermetic）**的：不依赖外网 / DNS / PyPI 可达性，在 CI 中确定性可复现。注意 pip 对纯 HTTP 索引需 `--trusted-host`；安装目标置于 ext4 磁盘（`/root`）而非 RAM 盘 `/tmp`（tmpfs 读回会损坏已安装文件）。
