---
name: rust-code-quality
description: 编写、修改、重构或审查本 TGOSKits 仓库中的 Rust 代码时使用。提供所有 Rust 改动都应满足的实现质量、命名、函数、注释、错误处理、代码异味和确定性测试基线；公共接口、并发或 unsafe 专项问题由对应技能补充。
---

# Rust 代码质量

## 1. 适用方式

本技能是所有 Rust 实现与审查的基础约束。先理解调用链、所有权、失败路径和验证入口，再判断是否同时读取公共接口、并发、`unsafe`、功能开发或领域技能。规则重叠时采用能够更直接保护实际不变量的一项，不复制多份相同结论。

开始实现或审查前，完整阅读 [实现与表达](references/implementation.md)。任务涉及运行时失败、错误传播、错误修复、测试新增或验证证据时，再完整阅读 [错误与测试](references/errors-and-tests.md)。

## 2. 相邻技能

本技能只提供通用基线。下列语义出现时还要读取对应技能：

- 公共或共享接口、类型、错误、软件包、模块、依赖或宏边界：`rust-api-design`；
- 锁、原子操作、异步执行、中断、任务调度或共享状态：`rust-concurrency-safety`；
- `unsafe`、外部函数接口、用户内存、内存映射输入输出或直接内存访问：`rust-unsafe-safety`；
- 新增或扩展用户可见行为、软件包、子系统、平台或硬件能力：`feature-development`；
- 测试策略、层级选择、确定性回归、必要性或低价值测试审计：`test-quality`；
- 可移植驱动、平台启动、系统调用或测试套件：继续读取命中的领域技能。

## 3. 项目验证

使用仓库任务工具作为验证入口。修改代码后运行 `cargo fmt`，静态检查使用 `cargo xtask clippy` 或定向的 `cargo xtask clippy --package <软件包>`，标准库测试使用 `cargo xtask test` 或 `cargo xtask test --since <引用>`。ArceOS、StarryOS 和 Axvisor 的构建、测试与运行也使用相应的 `cargo xtask` 子命令。

不得用原生 Cargo 命令替代已有的项目入口，也不得通过新增 `allow` 属性、削弱测试、放宽匹配规则或静默跳过来制造通过结果。项目任务工具确实没有入口时，先检查其实现，再使用能够精确复现项目参数的特殊命令并说明原因。
