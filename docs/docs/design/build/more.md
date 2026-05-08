---
sidebar_position: 4
sidebar_label: "Snapshot 与 Build Info"
---

# Snapshot 与 Build Info

统一入口能把命令写得比较短，很大程度上依赖两类上下文信息：

- **Snapshot**：保存最近一次解析出来的请求与参数状态
- **Build Info**：把构建配置转成后续执行流程真正需要的上下文

## 为什么需要了解

- 有些"命令没改但行为变了"的现象来自 snapshot 复用
- 某些平台或配置问题需要沿着 Build Info 的生成路径向前追
- Axvisor 的配置链路比另外两套系统更依赖 Build Info 与外部文件

## 适用场景

- 排查参数解析异常
- 新增或重构子命令
- 核对不同系统的默认配置查找顺序

详细说明：[构建系统完整说明](/docs/design/reference/build-system)
