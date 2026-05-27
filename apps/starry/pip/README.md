# pip 测试

在 StarryOS (Alpine rootfs) 上验证 Python pip 的常用操作。

## 测试内容

25 个阶段，覆盖日常 pip 使用场景：

- venv 创建/激活/销毁
- pip install / uninstall / upgrade / --dry-run
- pip list / show / freeze / check
- pip install -r requirements.txt
- pip install from local directory / local wheel
- pip wheel / pip download
- pip cache dir / info / purge
- pip install -e (editable)
- pip list --format=json / --outdated
- pip config / pip debug

## 前置依赖

- 主机已安装 `debugfs` (e2fsprogs) 和对应架构的 `qemu-*-static`
- Alpine rootfs 镜像 (由 `cargo xtask starry rootfs` 生成)

## 运行测试

```bash
# 在 StarryOS QEMU 上运行
cargo xtask starry app run -t pip

# 指定架构 (默认 x86_64)
cargo xtask starry app run -t pip --arch aarch64
```

## 在 Linux 主机上快速验证测试脚本

```bash
# 直接在主机 shell 中运行测试脚本 (需要 python3 和 pip)
sh apps/starry/pip/test_pip.sh
```

## 工作原理

1. `prebuild.sh` 被 xtask 框架调用，通过 qemu-user 在 staging rootfs 中执行 `apk add python3 py3-pip`
2. 安装的 Python/pip 文件和 `test_pip.sh` 被复制到 overlay 目录
3. overlay 被注入到 rootfs 镜像中
4. QEMU 启动后执行 `sh /usr/bin/test_pip.sh`
5. 测试脚本逐阶段输出 `STARRY_PIP_STAGE_N: ... OK`，最终输出 `STARRY_PIP_TESTS_PASSED`

## 判定标准

- **通过**: 输出匹配 `STARRY_PIP_TESTS_PASSED`
- **失败**: 输出匹配 `STARRY_PIP_STAGE_.*_FAILED`，或出现 panic / page fault / segmentation fault
- **超时**: 600 秒
