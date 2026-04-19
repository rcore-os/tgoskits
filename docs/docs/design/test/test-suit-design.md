---
sidebar_position: 4
sidebar_label: "Test-Suit 总览"
---

# test-suit 测试用例设计总览

`test-suit/` 是所有 OS 测试用例的统一入口，按操作系统划分为独立目录。每个目录由对应的 `cargo xtask <os>` 子命令负责发现、构建和运行测试。

## 顶层目录结构

```text
test-suit/
├── starryos/        # StarryOS 测试用例
├── axvisor/         # (暂无 test-suit 下的用例，板级测试配置在 os/axvisor/configs/ 中)
└── arceos/          # ArceOS 测试用例
```

## 文档拆分说明

原始设计文档已按系统边界和配置主题拆分，内容不删减，仅重组结构：

| 主题 | 文档 | 内容范围 |
|------|------|----------|
| StarryOS test-suit | [StarryOS test-suit 设计](/docs/design/test/starryos) | 分组、C/Rust/无源码用例、QEMU 与板级测试流程、rootfs 注入、新增用例方式 |
| Axvisor test-suit | [Axvisor test-suit 设计](/docs/design/test/axvisor) | QEMU、U-Boot、板级测试的硬编码测试组和新增方式 |
| ArceOS test-suit | [ArceOS test-suit 设计](/docs/design/test/arceos) | C/Rust 测试结构、发现机制、构建运行流程、构建配置与新增方式 |
| 配置与命名规范 | [test-suit 配置与命名规范](/docs/design/test/config) | QEMU/板级/构建配置格式汇总，以及目录、文件、架构命名规范 |

## 阅读建议

- 关注 StarryOS 测试编排时，优先阅读 [StarryOS test-suit 设计](/docs/design/test/starryos)
- 关注 Axvisor 测试注册和板级映射时，优先阅读 [Axvisor test-suit 设计](/docs/design/test/axvisor)
- 关注 ArceOS 测试包与 C 测试目录时，优先阅读 [ArceOS test-suit 设计](/docs/design/test/arceos)
- 关注 TOML 字段和命名约定时，优先阅读 [test-suit 配置与命名规范](/docs/design/test/config)
