# IVC 客户机通信检查

本目录通过板卡 `/guest` 中的配套客户机镜像验证 IVC 通信。功能检查与传输基准使用不同的客户机组合，分别维护构建配置、交互配置和自动判定规则。

## 1. 测试场景

根据验证目标选择子目录，各场景的 README 包含准备、手动启动和自动执行命令。

### 1.1 功能检查

[functional](functional/README.md) 在同一次启动中验证 Linux、StarryOS、Zephyr 三条通信链路，以及超时、取消和恢复。`functional/configs/` 保存客户机定义与 `uboot-shell.toml`。

### 1.2 传输基准

[benchmark](benchmark/README.md) 验证 StarryOS 与 Zephyr 四组各 100 次传输的两端结果、字节数和块数。`benchmark/configs/` 保存客户机定义与 `uboot-shell.toml`。

## 2. 自动执行

从 TGOSKits 仓库根目录运行所需子目录，板卡服务地址由环境变量提供：

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/ivc/functional --server "$BOARD_SERVER"
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/ivc/benchmark --server "$BOARD_SERVER"
```

两个入口均须完成各自的客户机结果检查，且外层命令返回 0。选择父目录 `board-ballpick-robot/ivc` 会执行两个场景。
