# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.6](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.5...somehal-v0.6.6) - 2026-04-02

### Other

- ✨ feat: 添加 RISC-V64 架构支持 ([#65](https://github.com/drivercraft/sparreal-os/pull/65))

## [0.6.5](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.4...somehal-v0.6.5) - 2026-03-19

### Other

- ✨ feat: 添加 per-CPU 预分配支持 ([#62](https://github.com/drivercraft/sparreal-os/pull/62))

## [0.6.4](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.3...somehal-v0.6.4) - 2026-03-19

### Other

- ✨ feat: 添加x86_64支持 ([#60](https://github.com/drivercraft/sparreal-os/pull/60))

## [0.6.3](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.2...somehal-v0.6.3) - 2026-03-10

### Other

- ✨ feat: 更新架构初始化函数以支持中断和定时器设置 ([#50](https://github.com/drivercraft/sparreal-os/pull/50))

## [0.6.2](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.1...somehal-v0.6.2) - 2026-03-10

### Fixed

- multi-core SMP initialization and secondary CPU boot sequence ([#48](https://github.com/drivercraft/sparreal-os/pull/48))

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))

## [0.6.1](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.6.0...somehal-v0.6.1) - 2026-03-06

### Other

- 🛠️ fix: 更新 secondary_entry 函数以传递 cpu_meta 参数 ([#42](https://github.com/drivercraft/sparreal-os/pull/42))

## [0.6.0](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.5.2...somehal-v0.6.0) - 2026-03-04

### Other

- ✨ feat: 重构设备驱动接口，移除 open/close 方法，添加 name 方法 ([#25](https://github.com/drivercraft/sparreal-os/pull/25))
- ✨ feat: 添加对 x86_64 和 riscv64 架构的编译支持 ([#23](https://github.com/drivercraft/sparreal-os/pull/23))
- ✨ feat: smp and precpu support ([#20](https://github.com/drivercraft/sparreal-os/pull/20))

## [0.5.2](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.5.1...somehal-v0.5.2) - 2026-02-13

### Other

- updated the following local packages: kernutil

## [0.5.1](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.5.0...somehal-v0.5.1) - 2026-02-09

### Other

- release ([#12](https://github.com/drivercraft/sparreal-os/pull/12))

## [0.5.0](https://github.com/drivercraft/sparreal-os/compare/somehal-v0.4.5...somehal-v0.5.0) - 2026-02-09

### Other

- ✨ feat(mmio-api): 更新 mmio-api 版本并修改地址类型为 MmioAddr ([#10](https://github.com/drivercraft/sparreal-os/pull/10))
- ✨ feat(mmio-api): 添加内存映射 I/O 抽象 API 以支持操作系统内核开发 ([#9](https://github.com/drivercraft/sparreal-os/pull/9))
- 📝 docs(somehal): 更新 README 以反映 entry 宏的参数化改进
- ✨ feat(config): 更新 Cargo 配置，添加 xtask 及相关命令，调整构建和测试配置
- ♻️ refactor(platop): 更新 irq_set_enable 函数参数为未使用的变量，添加 dead code 忽略
- ♻️ refactor(loongarch64): 移除未使用的 IRQ 初始化函数
- ♻️ refactor(aarch64, el2): 完善 Hypervisor 模式页表与定时器支持
- 🎨 style(somehal): 移除 link.ld 中冗余的 STACK_SIZE 定义
- 🔧 chore(somehal): 移动构建脚本并增加栈大小，添加文档
- 📝 docs(somehal): 添加 IRQ 控制器初始化时机说明
- ♻️ refactor(gic): 重构 GIC 架构以支持 v2 和 v3 版本
- ♻️ refactor(timer, irq): 移除冗余的调试日志输出
- ♻️ refactor(sparreal-rt): 移除对 someboot 的直接依赖，统一通过 somehal 访问
- ♻️ refactor(irq): 将 IRQ 处理逻辑从 someboot 迁移到 somehal
- ♻️ refactor(aarch64): 完善中断处理和 GICv3 驱动集成
- ♻️ refactor(platform): 为 LoongArch64 添加平台抽象层实现并调整驱动初始化
- ♻️ refactor(platform): 重构平台层初始化流程和模块组织
- 🔧 chore(version): 调整版本号以反映重命名后的架构
- ♻️ refactor(platform): 重命名 someplat 平台层为 somehal
