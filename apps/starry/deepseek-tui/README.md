# StarryOS DeepSeek TUI 示例

本目录提供在 StarryOS x86_64 QEMU 虚拟机中运行 [DeepSeek TUI](https://github.com/Hmbown/DeepSeek-TUI) 的手动示例。不参与 CI 测试套件。

## 前置依赖

- **Docker** — 用于在 Alpine 容器中编译 musl 动态链接的 deepseek 二进制（`libdbus-1.so.3` 在 musl 目标下需容器环境构建）
- **Rust 工具链** — 用于编译 StarryOS 内核（`cargo xtask starry qemu`）
- **e2fsprogs** — 提供 `debugfs`，用于向 rootfs ext4 镜像注入文件

## 文件清单

| 文件 | 功能 |
|---|---|
| `run_me.sh` | 总控脚本，支持 `--build`、`--smoke`、`--test`、`--shell`、`--api-key`、`--proxy` |
| `prepare_deepseek_assets.sh` | 编译 deepseek + deepseek-tui 二进制，提取至 `target/deepseek/assets/`"
| `prepare_deepseek_rootfs.sh` | 构建 Alpine rootfs，注入二进制、共享库、API Key、CA 证书 |
| `Dockerfile.build` | Alpine Docker 构建环境，用于编译 musl 动态链接的 deepseek 二进制 |
| `build-x86_64-unknown-none.toml` | StarryOS 内核构建配置 |
| `qemu-x86_64.toml` | 离线 smoke 测试 QEMU 配置（无网络需求） |
| `qemu-x86_64-deepseek-prime-test.toml` | 在线 C 素数测试 QEMU 配置（需要 API Key + 网络） |

## 构建流程说明

1. `prepare_deepseek_assets.sh` 优先使用 Docker（检测到 docker 命令时），在 Alpine 容器中用 musl 工具链从源码编译 deepseek + deepseek-tui，并进行动态链接（依赖 `libdbus-1.so.3` 和 `libgcc_s.so.1`）。构建产物存放在 `target/deepseek/assets/`。
2. `prepare_deepseek_rootfs.sh` 将上述二进制和共享库注入 Alpine rootfs ext4 镜像中。额外注入 CA 证书和 `DEEPSEEK_API_KEY` 环境变量（在线测试时）。
3. `cargo xtask starry qemu` 编译 StarryOS 内核（`x86_64-unknown-none`），挂载 rootfs，启动 QEMU 虚拟机。

## 使用方法

### 构建二进制

```bash
bash apps/starry/deepseek-tui/run_me.sh --build
```

或直接运行：

```bash
bash apps/starry/deepseek-tui/prepare_deepseek_assets.sh
```

### 离线 smoke 测试

验证 `deepseek --version`、`deepseek-tui --version`、`deepseek model list` 等基本命令，无需 API Key。

```bash
bash apps/starry/deepseek-tui/run_me.sh --smoke
```

预期输出包含：

```
STARRY_DEEPSEEK_STAGE_G_PASSED
```

### 在线 C 素数测试

向 DeepSeek 发送提示词，要求其编写 C 程序找出大于 998244352 的最小素数，编译并运行，验证输出是否为 998244353。

```bash
bash apps/starry/deepseek-tui/run_me.sh --test --api-key sk-your-key-here
```

如需代理：

```bash
bash apps/starry/deepseek-tui/run_me.sh --test --api-key sk-your-key-here --proxy http://10.0.2.2:7890
```

预期输出包含：

```
STARRY_DEEPSEEK_PRIME_TEST_DONE
STARRY_DEEPSEEK_PRIME_TEST_PASSED
```

### 交互式 shell

```bash
bash apps/starry/deepseek-tui/run_me.sh --shell
```

或带 API Key：

```bash
bash apps/starry/deepseek-tui/run_me.sh --shell --api-key sk-your-key-here
```

### 仅构建 rootfs（不启动 QEMU）

离线版：

```bash
bash apps/starry/deepseek-tui/run_me.sh --rootfs
```

在线版：

```bash
bash apps/starry/deepseek-tui/run_me.sh --rootfs --api-key sk-your-key-here
```

## 本地产物

- `target/deepseek/assets/` — deepseek + deepseek-tui 二进制及运行时共享库
- `target/deepseek/build/` — DeepSeek-TUI 源码克隆
- `tmp/axbuild/rootfs/rootfs-x86_64-deepseek*.img` — 构建好的 rootfs 镜像

以上均为不受版本控制的本地构建产物。

## 注意事项

- 仅支持 x86_64 QEMU，不支持其他架构
- 不覆盖 DeepSeek TUI 交互模式、默认 Linux Sandbox、CI 在线请求等场景
- 在线测试需要 `--api-key` 参数
- 二进制通过动态链接 `libdbus-1.so.3` 和 `libgcc_s.so.1`，rootfs 注入脚本会自动处理这些依赖
- **构建依赖 Docker**：`prepare_deepseek_assets.sh` 需要 Docker 运行时，在 Alpine 容器中编译 musl 动态链接的 deepseek 二进制。当前脚本在检测到 docker 命令时自动使用容器方式；若宿主机没有 Docker，会回退到本地构建（但可能因 dbus 交叉编译问题失败）
