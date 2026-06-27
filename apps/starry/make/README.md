# Starry Make Test

`make` 是一个 StarryOS 应用级构建能力测试。它在 prebuild 阶段把 Alpine 的 `make` 二进制缓存并注入 guest，然后在 StarryOS 内使用 Makefile 构建、测试、安装并重新执行一个小型脚本项目。

## 覆盖能力

- guest 启动后不执行网络安装，避免外部网络和大型包安装影响测试稳定性。
- 执行 `make clean`、`make -j2`、`make test`、`make install`，覆盖常见 Makefile 工作流。
- 使用两个源文件生成可执行脚本，证明 make 能处理依赖、生成目标和安装目标。
- 执行构建产物和安装后的产物，证明构建结果可在 Starry 中正常运行。

## 运行方式

```bash
cargo xtask starry app qemu -t make --arch x86_64
cargo xtask starry app qemu -t make --arch riscv64
```

成功输出包含：

```text
MAKE_TEST_PASSED
```
