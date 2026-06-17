# StarryOS PicoClaw CLI example

这个目录提供一个 opt-in 的 PicoClaw 验证样例，用来确认 StarryOS x86_64 QEMU 对 PicoClaw 的支持情况。它不会进入默认 CI，也不会提交 PicoClaw 二进制、rootfs 镜像、在线配置或任何密钥。

## 目标层级

1. 离线 Smoke：验证静态 Linux x86_64 PicoClaw 二进制能启动、写入配置并执行本地 CLI。
2. 在线 Agent：注入 API key、代理和 CA 后，验证 `picoclaw agent -m ...` 能完成一次模型请求。
3. Gateway 服务：验证 `picoclaw gateway` 能在 StarryOS guest 中启动并响应本地 HTTP 健康检查。

## 准备离线 rootfs

```bash
apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh
```

脚本会复用或下载 `sipeed/picoclaw` 的 `v0.2.8` Linux x86_64 release tarball，校验 SHA-256，并把 `picoclaw` 与 `picoclaw-launcher` 注入到 Alpine rootfs 的 `/usr/local/bin/`。

运行离线 smoke：

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-alpine.img
```

成功标记：

```text
STARRY_PICOCLAW_OFFLINE_PASSED
```

## 准备在线 rootfs

可以让脚本生成最小在线配置和 `.security.yml`：

```bash
PICOCLAW_API_KEY=sk-... \
apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img \
  --proxy http://10.0.2.2:7890
```

如果没有设置 `PICOCLAW_API_KEY`，脚本会回退读取 `OPENAI_API_KEY`，再回退读取
`ANTHROPIC_AUTH_TOKEN`。当使用 `ANTHROPIC_AUTH_TOKEN` 时，脚本默认生成
Anthropic Messages 配置，并读取 `ANTHROPIC_BASE_URL` 作为 `api_base`。也可以用
`--api-key`、`--provider`、`--model` 和 `--api-base` 显式传入。

默认在线配置使用已验证的 Mimo OpenAI-compatible 端点：

```text
model_name = mimo-v25
provider = openai
model = mimo-v2.5
api_base = https://token-plan-cn.xiaomimimo.com/v1
enable_thinking = false
```

`mimo-v2.5` 默认可能进入 thinking/reasoning 模式，而 PicoClaw 当前
OpenAI-compatible 调用链没有回传 `reasoning_content`，所以脚本会为 Mimo
配置生成：

```json
"extra_body": {
  "chat_template_kwargs": {
    "enable_thinking": false
  }
}
```

也可以显式指定 provider、model 和 api_base：

```bash
apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img \
  --api-key "$PICOCLAW_API_KEY" \
  --provider openai \
  --model-name mimo-v25 \
  --model mimo-v2.5 \
  --api-base https://token-plan-cn.xiaomimimo.com/v1
```

也可以手动提供配置文件：

```bash
apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img \
  --config-json /path/to/config.json \
  --security-yml /path/to/.security.yml \
  --env-file /path/to/online-env
```

`online-env` 是普通 shell 文件，可包含 `http_proxy`、`https_proxy`、`all_proxy` 等导出语句。默认会注入 host 上的 `SSL_CERT_FILE`，如果该环境变量不存在，则尝试使用 `/etc/ssl/certs/ca-certificates.crt`。

运行在线 Agent smoke：

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img
```

成功标记：

```text
STARRY_PICOCLAW_AGENT_PASSED
```

## 运行 Gateway smoke

Gateway smoke 复用在线 rootfs，不会发起模型请求；它在 guest 内启动
`picoclaw gateway --allow-empty --host 127.0.0.1`，再用 `curl` 或 busybox
`wget` 请求 `/health`。

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-gateway.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img
```

成功标记：

```text
STARRY_PICOCLAW_GATEWAY_PASSED
```

## 当面演示脚本

如果要从 Docker 环境开始，一步一步演示 StarryOS 中的 PicoClaw 在线对话，可以运行：

```bash
PICOCLAW_API_KEY=... apps/starry/picoclaw-cli/demo_picoclaw_agent.sh
```

脚本会逐步暂停，依次展示 Docker 检查、rootfs 准备、StarryOS QEMU 启动，以及一次
可验收请求加多次 `picoclaw agent` 闲聊。没有提前设置 `PICOCLAW_API_KEY` 时，脚本会在本地隐藏输入。

默认演示配置为：

```text
provider = openai
model_name = mimo-v25
model = mimo-v2.5
api_base = https://token-plan-cn.xiaomimimo.com/v1
enable_thinking = false
```

成功时会看到：

```text
STARRY_PICOCLAW_AGENT_OK
STARRY_PICOCLAW_AGENT_PASSED
```

中间还会看到多段 `PicoClaw chat`，用于现场展示 StarryOS guest 内连续模型对话。

## 交互式长期使用

如果希望进入 StarryOS 后自己持续输入 PicoClaw 命令，可以运行：

```bash
PICOCLAW_API_KEY=... apps/starry/picoclaw-cli/run_picoclaw_interactive.sh
```

脚本默认创建或复用 `tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-user.img`，
然后启动一个不带自动退出条件的 StarryOS shell。进入 guest 后优先运行裸
`picoclaw agent`，它会进入持续交互模式，可以连续输入多轮消息：

```bash
picoclaw status
picoclaw agent
picoclaw gateway --allow-empty --host 127.0.0.1 --port 18790
```

交互式 QEMU 默认把 guest 的 `18790` 转发到宿主机。启动 gateway 后，宿主机可以访问：

```bash
curl http://127.0.0.1:18790/health
```

退出 QEMU 使用 `Ctrl-a x`。

## 边界

- 第一目标只覆盖 StarryOS x86_64 QEMU。
- 资产放在 `target/picoclaw/assets/`，rootfs 放在 `tmp/axbuild/rootfs/`。
- 如果 smoke 暴露 syscall 或 ABI 缺口，再按最小复现补 StarryOS 内核和对应回归测试。
