---
sidebar_position: 5
sidebar_label: "ostool 与执行后端"
---

# ostool 与执行后端

`cargo xtask` 并不是所有构建逻辑的终点。统一入口解析出请求后，会通过 `ostool` 相关 API 完成编译、镜像生成或模拟器启动。

## 分层架构

| 层级 | 职责 |
|------|------|
| CLI 层 (`tg-xtask`) | 用户体验和参数统一 |
| `axbuild` 层 | 系统维度的流程编排 |
| `ostool` 层 | 更底层的编译、镜像、模拟器动作 |

## 源码阅读提示

如果在 `xtask/` 里没找到最终行为，还需要继续查看：

- `scripts/axbuild/src/*`
- `command_flow` 模块
- `ostool` 相关调用点

相关文档：[构建流程](./flow) | [构建系统完整说明](/docs/design/reference/build-system)
