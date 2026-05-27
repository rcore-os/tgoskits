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

## 运行测试

```bash
cargo xtask starry app run -t pip
```

## 在 Linux 主机上快速验证测试脚本

```bash
sh apps/starry/pip/test_pip.sh
```

## 工作原理

1. `prebuild.sh` 被 xtask 框架调用，使用宿主机 `apk --root` 在 staging rootfs 中安装 `python3 py3-pip`
2. 通过 `readelf` 解析运行时依赖并复制到 overlay
3. `test_pip.sh` 被复制到 overlay 的 `/usr/bin/`
4. overlay 被注入到 rootfs 镜像中
5. QEMU 启动后执行 `/usr/bin/test_pip.sh`

## 判定标准

- **通过**: 输出匹配 `STARRY_PIP_TESTS_PASSED`
- **失败**: 输出匹配 `STARRY_PIP_STAGE_.*_FAILED`，或出现 panic / page fault / segmentation fault
- **超时**: 600 秒
