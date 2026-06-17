# StarryOS DeepSeek TUI Example

这个目录提供 StarryOS x86_64 QEMU 中运行 [DeepSeek TUI](https://github.com/Hmbown/DeepSeek-TUI) 的手动示例。它不是 `test-suit/starryos` 用例，也不会进入 CI；构建 DeepSeek TUI、注入 rootfs、在线请求模型都只会在用户显式执行本目录脚本或 QEMU 配置时发生。

## 前置依赖

- **Docker**：用于在 Alpine 容器中编译 musl 动态链接的 deepseek 二进制。没有 Docker 时脚本会尝试本地构建，但可能因 dbus 交叉编译问题失败。
- **Rust 工具链**：用于编译 StarryOS 内核和 xtask。
- **e2fsprogs**：提供 `debugfs`，`cargo xtask starry app qemu` 注入 overlay 时会使用。

## 文件清单

| 文件 | 功能 |
|---|---|
| `prebuild.sh` | app runner 调用的预构建脚本，准备 deepseek 资产并生成 rootfs overlay |
| `run_me.sh` | 便捷脚本，支持 `--build`、`--smoke`、`--test`、`--shell`、`--api-key`、`--proxy` |
| `prepare_deepseek_assets.sh` | 编译 deepseek + deepseek-tui 二进制，提取至 `target/deepseek/assets/` |
| `prepare_deepseek_rootfs.sh` | 旧的手动 standalone rootfs 构建脚本，默认流程不再依赖它 |
| `Dockerfile.build` | Alpine Docker 构建环境 |
| `build-x86_64-unknown-none.toml` | StarryOS 内核构建配置 |
| `qemu-x86_64.toml` | 离线 smoke 测试 QEMU 配置 |
| `qemu-x86_64-deepseek-prime-test.toml` | 在线 C 素数测试 QEMU 配置，需要 API Key + 网络 |
| `qemu-x86_64-shell.toml` | 交互式 shell QEMU 配置 |

## 默认流程

DeepSeek TUI 现在和 `codex-cli` 一样走 Starry app 形式：

1. `cargo xtask starry app qemu -t deepseek-tui ...` 准备标准 `rootfs-x86_64-alpine.img`。
2. app runner 执行 `prebuild.sh`。
3. `prebuild.sh` 调用 `prepare_deepseek_assets.sh` 构建或复用 `target/deepseek/assets/` 中的 deepseek 资产。
4. `prebuild.sh` 把二进制、共享库、CA 证书以及可选的 API key/代理环境文件写入 overlay。
5. xtask 将 overlay 注入标准 Alpine rootfs 后启动 QEMU。

因此默认路径不会再查找或下载 `rootfs-x86_64-deepseek*.img`。

## 使用方法

### 列出 app

```bash
cargo xtask starry app list --kind qemu
```

`deepseek-tui` 后面应显示 `prebuild`。

### 离线 smoke 测试

验证 `deepseek --version`、`deepseek-tui --version`、`deepseek model list` 等基本命令，无需 API Key。

```bash
cargo xtask starry app qemu -t deepseek-tui --arch x86_64
```

或使用便捷脚本：

```bash
bash apps/starry/deepseek-tui/run_me.sh --smoke
```

预期输出包含：

```text
STARRY_DEEPSEEK_STAGE_G_PASSED
```

### 在线 C 素数测试

向 DeepSeek 发送提示词，要求其编写 C 程序找出大于 998244352 的最小素数，编译并运行，验证输出是否为 998244353。

```bash
DEEPSEEK_API_KEY=sk-your-key-here \
  cargo xtask starry app qemu \
    -t deepseek-tui \
    --arch x86_64 \
    --qemu-config apps/starry/deepseek-tui/qemu-x86_64-deepseek-prime-test.toml
```

如需代理：

```bash
DEEPSEEK_API_KEY=sk-your-key-here \
DEEPSEEK_ONLINE_PROXY=http://10.0.2.2:7890 \
  cargo xtask starry app qemu \
    -t deepseek-tui \
    --arch x86_64 \
    --qemu-config apps/starry/deepseek-tui/qemu-x86_64-deepseek-prime-test.toml
```

便捷脚本等价写法：

```bash
bash apps/starry/deepseek-tui/run_me.sh --test --api-key sk-your-key-here --proxy http://10.0.2.2:7890
```

预期输出包含：

```text
STARRY_DEEPSEEK_PRIME_TEST_DONE
STARRY_DEEPSEEK_PRIME_TEST_PASSED
```

### 交互式 shell

```bash
cargo xtask starry app qemu \
  -t deepseek-tui \
  --arch x86_64 \
  --qemu-config apps/starry/deepseek-tui/qemu-x86_64-shell.toml
```

或带 API Key/代理：

```bash
bash apps/starry/deepseek-tui/run_me.sh --shell --api-key sk-your-key-here --proxy http://10.0.2.2:7890
```

进入 `root@starry` 后可手动运行：

```sh
deepseek --version
deepseek-tui --version
deepseek model list
deepseek-tui
```

### 仅构建 host 资产

```bash
bash apps/starry/deepseek-tui/run_me.sh --build
```

或直接运行：

```bash
bash apps/starry/deepseek-tui/prepare_deepseek_assets.sh
```

### 旧 standalone rootfs 路径

保留 `prepare_deepseek_rootfs.sh` 和 `run_me.sh --rootfs` 仅用于手动兼容场景。默认 app 流程不需要它们。

## 本地产物

- `target/deepseek/assets/`：deepseek + deepseek-tui 二进制及运行时共享库
- `target/deepseek/build/`：DeepSeek-TUI 源码克隆
- `tmp/axbuild/starry-app/deepseek-tui/overlay/`：最近一次 app prebuild 生成的 overlay
- `tmp/axbuild/rootfs/rootfs-x86_64-alpine.img`：标准 Alpine rootfs
- `tmp/axbuild/rootfs/rootfs-x86_64-deepseek*.img`：仅旧 standalone rootfs 脚本会生成

以上均为不受版本控制的本地构建产物。

## 边界

- 仅支持 x86_64 QEMU。
- 不覆盖 DeepSeek TUI 默认 Linux sandbox、CI 在线请求模型，或其他架构上的运行。
- 在线测试需要 `DEEPSEEK_API_KEY` 或 `run_me.sh --test --api-key`。
