# 示例与演示流程

TGOSKits 的顶层 `examples/` 目录用于放置可运行的场景示例，而不是通用
workspace 组件模板。系统级或组件级示例仍应优先放在对应子系统目录中，例如
`os/arceos/examples/`、`components/*/examples/` 或 `test-suit/`。

## StarryOS 板端示例

StarryOS 场景示例放在：

```text
examples/starry/<case>/
```

每个 case 至少包含：

```text
init.sh
build-<target>.toml
board-<board>.toml
```

运行方式：

```bash
cargo starry example board -t <case>
```

`init.sh` 会被 `cargo starry example board` 读取并作为 Starry shell 的启动命令发送到
板端；`board-<board>.toml` 继续提供 board type、shell prefix、匹配规则和超时；
`build-<target>.toml` 提供 StarryOS 内核构建配置。

第一个 StarryOS 场景示例是：

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

该示例假设板端 rootfs 已经预装 `/usr/bin/uvc-fps` 以及 `libuvc`、`libusb` 等运行时
依赖。示例目录中附带的 Rust std 项目用于构建这个用户态程序，但不会被 root
workspace 自动构建。

## 新增组件或示例

- 新增通用可复用组件时，放到合适的 `components/`、`drivers/`、`platform/` 或
  `os/*/modules/` 子目录，并同步 workspace、文档和验证白名单。
- 新增 ArceOS 应用示例时，优先使用 `os/arceos/examples/`。
- 新增 StarryOS 板端场景时，使用 `examples/starry/<case>/`，并确保 case 可以通过
  `cargo starry example board -t <case>` 被发现。
- 新增 CI 回归用例时，使用 `test-suit/`，不要把 CI-only 行为混入顶层 examples。
