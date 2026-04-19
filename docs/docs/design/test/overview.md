---
sidebar_position: 1
sidebar_label: "总览"
---

# 测试套件总览

`test-suit/` 是所有 OS 测试用例的统一入口，按操作系统划分为独立目录。每个目录由对应的 `cargo xtask <os>` 子命令负责发现、构建和运行测试。

## 目录结构总览

```text
test-suit/
├── starryos/        # StarryOS 测试用例
├── axvisor/         # (暂无 test-suit 下的用例，板级测试配置在 os/axvisor/configs/ 中)
└── arceos/          # ArceOS 测试用例
```

除 `test-suit/` 下的 OS 级测试外，仓库还维护两类主机端（host）自动化验证：

| 验证类型 | 入口命令 | 配置来源 |
|----------|---------|---------|
| 标准库测试 | `cargo xtask test` | `scripts/test/std_crates.csv`（包白名单） |
| Clippy 检查 | `cargo xtask clippy` | `scripts/test/clippy_crates.csv`（包清单） |

标准库测试对白名单中的每个 crate 执行 `cargo test -p <package>`，验证其在当前工具链下能否通过编译和单元测试。这两类验证均在 CI 的 container 环境中运行，与 OS 级 QEMU 测试共享同一基础镜像。

## 文档组织说明

原始设计文档已按系统边界拆分；共享总览和命名约定也分别独立成文：

| 主题 | 文档 | 内容范围 |
|------|------|----------|
| 测试体系总览 | 本文 | 目录结构、文档组织、阅读指引 |
| 命名规则与共享配置 | [命名规则与共享配置](/docs/design/test/naming) | 共享配置文件类型、目录/文件命名规则、架构命名、发现路径约定 |
| 测试基础设施与环境 | [测试基础设施与环境](/docs/design/test/infrastructure) | Container 镜像设计、CI 集成方式、镜像发布流程与触发条件 |
| StarryOS 测试套件 | [StarryOS 测试套件设计](/docs/design/test/starryos) | 分组、现有用例清单、C/Rust/无源码用例、QEMU 与板级测试流程、新增用例指南 |
| Axvisor 测试套件 | [Axvisor 测试套件设计](/docs/design/test/axvisor) | QEMU、U-Boot、板级测试的硬编码测试组和新增指南 |
| ArceOS 测试套件 | [ArceOS 测试套件设计](/docs/design/test/arceos) | C/Rust 测试结构、发现机制、构建运行流程、构建配置与新增指南 |

## 阅读指引

- 了解测试体系全貌和各文档定位时，阅读本文
- 关注共享配置类型和文件命名约定时，优先阅读 [命名规则与共享配置](/docs/design/test/naming)
- 关注 Container 镜像、CI 环境和发布流程时，优先阅读 [测试基础设施与环境](/docs/design/test/infrastructure)
- 关注 StarryOS 测试编排时，优先阅读 [StarryOS 测试套件设计](/docs/design/test/starryos)
- 关注 Axvisor 测试注册和板级映射时，优先阅读 [Axvisor 测试套件设计](/docs/design/test/axvisor)
- 关注 ArceOS 测试包与 C 测试目录时，优先阅读 [ArceOS 测试套件设计](/docs/design/test/arceos)
