---
sidebar_position: 2
sidebar_label: "配置加载流程"
---

# 配置加载流程

Axvisor 的配置加载需要把**构建时配置**和**运行时 Guest 配置**一起看。

## 加载链路

```
根命令 -> axbuild 解析架构/板级配置 -> Build Info 生成
    -> VMM 加载 VM 配置和 Guest 镜像
    -> /guest/... 配置参与启动路径
```

## 为什么容易出问题

- 构建配置和运行配置分散在不同目录
- `setup_qemu.sh` 会准备运行所需的外部资源
- 同一个 VM 配置可能受仓库默认值和运行时文件覆盖的共同影响

建议配合 [QEMU 部署](../../manual/deploy/qemu) 一起阅读。
