# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/some-serial-v0.5.1...some-serial-v0.6.0) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/some-serial-v0.5.0...some-serial-v0.5.1) - 2026-06-09

### Other

- updated the following local packages: ax-kspin, rdrive, rdif-intc, rdif-serial

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/some-serial-v0.4.1...some-serial-v0.5.0) - 2026-06-03

### Added

- *(some-serial)* add Rockchip FIQ debugger UART ([#980](https://github.com/rcore-os/tgoskits/pull/980))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- Remove ARM PL011 UART driver and integrate DesignWare APB UART support ([#965](https://github.com/rcore-os/tgoskits/pull/965))

## [0.4.0](https://github.com/drivercraft/sparreal-os/compare/some-serial-v0.3.1...some-serial-v0.4.0) - 2026-04-15

### Other

- ✨ feat: 添加 some-serial 统一串口驱动集合，支持 ARM PL011 和 NS16550 ([#75](https://github.com/drivercraft/sparreal-os/pull/75))
# 更新日志

本项目的所有重要变更都会记录在这个文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/)，
并且本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

### 计划中
- 添加更多ARM平台支持
- 优化中断处理性能
- 添加DMA支持
- 改进错误处理机制

## [0.1.0] - 2024-01-XX

### 新增
- ✨ 完整的ARM PL011 UART驱动实现
- ✅ 支持no_std环境的裸机系统
- 🔄 完整的中断驱动通信支持
- 📦 16字节FIFO发送/接收支持
- ⚙️ 灵活的串口配置（波特率、数据位、停止位、校验位）
- 🔄 内置回环测试模式
- 🔒 基于RAII的资源管理
- 🧪 全面的测试套件覆盖

### 技术特性
- 支持AArch64和x86_64无标准库目标
- 集成bare-test框架进行裸机测试
- 使用tock-registers进行硬件寄存器抽象
- 支持可配置的FIFO触发级别
- 内存安全的指针操作

### 文档
- 📚 完整的API文档
- 📖 详细的使用指南和示例
- 🧪 测试覆盖说明

### 开发工具
- 🔄 GitHub Actions CI/CD流程
- 🔍 代码格式检查（rustfmt）
- 🛡️ 静态分析（clippy）
- 📊 代码覆盖率报告
- 🔒 安全审计检查

## [0.0.1] - 开发版本

### 初始版本
- 项目初始化
- 基础PL011驱动框架

---

## 版本说明

### 版本号格式
本项目使用语义化版本号：`MAJOR.MINOR.PATCH`

- **MAJOR**: 不兼容的API变更
- **MINOR**: 向后兼容的功能新增
- **PATCH**: 向后兼容的问题修正

### 变更类型
- `新增` - 新功能
- `变更` - 对现有功能的变更
- `弃用` - 即将移除的功能
- `移除` - 已移除的功能
- `修复` - 问题修复
- `安全` - 安全相关的修复

### 支持政策
- 当前版本：完全支持，包括新功能和问题修复
- 前一个主版本：仅关键安全更新和问题修复
- 更早版本：不再支持

### 获取帮助
- 📖 查看[文档](https://docs.rs/some-serial)
- 🐛 报告问题：[GitHub Issues](https://github.com/username/some-serial/issues)
- 💬 讨论和问题：[GitHub Discussions](https://github.com/username/some-serial/discussions)