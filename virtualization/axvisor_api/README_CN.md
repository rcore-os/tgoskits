<h1 align="center">axvisor_api</h1>

<p align="center">AxVisor API 相关 crate 工作区</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axvisor_api` 是 AxVisor 的 Host Runtime 抽象层。它把 Hypervisor Core
依赖的宿主能力统一收口为一组接口，使下层虚拟化组件不必直接依赖 ArceOS
运行时内部实现。

> axvisor_api 派生自 https://github.com/arceos-org/axvisor_api

## 当前接口模块

当前 API 主要按能力分组：

- `host`：CPU 枚举与宿主任务/线程辅助能力
- `task`：宿主任务句柄、等待队列与 vCPU 任务创建能力
- `memory`：页帧分配与地址转换
- `time`：单调时间、定时器注册与 one-shot timer 编程
- `irq`：宿主中断分发与 hook/handler 注册
- `platform`：启动固件发现与宿主资源交接
- `fs`：文件、目录、当前目录与标准输入输出能力
- `process`：进程退出能力
- `vmm`：VM/vCPU 上下文与中断注入辅助能力
- `arch`：体系结构相关的虚拟化钩子
- `console`：宿主控制台 I/O

## 工作区成员

- `axvisor_api_proc`

## 快速开始

```bash
# 进入工作区目录
cd virtualization/axvisor_api

# 代码格式化
cargo fmt --all

# 运行 clippy
cargo clippy --workspace --all-targets --all-features

# 运行测试
cargo test --workspace --all-features
```

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
