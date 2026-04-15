# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.13](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.12...someboot-v0.1.13) - 2026-04-15

### Other

- ✨ feat: 添加 some-serial 统一串口驱动集合，支持 ARM PL011 和 NS16550 ([#75](https://github.com/drivercraft/sparreal-os/pull/75))

## [0.1.12](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.11...someboot-v0.1.12) - 2026-04-02

### Other

- ✨ feat: 添加 RISC-V64 架构支持 ([#65](https://github.com/drivercraft/sparreal-os/pull/65))

## [0.1.11](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.10...someboot-v0.1.11) - 2026-03-19

### Other

- ✨ feat: 添加 per-CPU 预分配支持 ([#62](https://github.com/drivercraft/sparreal-os/pull/62))

## [0.1.10](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.9...someboot-v0.1.10) - 2026-03-19

### Other

- ✨ feat: 添加x86_64支持 ([#60](https://github.com/drivercraft/sparreal-os/pull/60))

## [0.1.9](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.7...someboot-v0.1.9) - 2026-03-10

### Other

- ✨ feat: 更新架构初始化函数以支持中断和定时器设置 ([#50](https://github.com/drivercraft/sparreal-os/pull/50))

## [0.1.7](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.6...someboot-v0.1.7) - 2026-03-10

### Fixed

- multi-core SMP initialization and secondary CPU boot sequence ([#48](https://github.com/drivercraft/sparreal-os/pull/48))

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))

## [0.1.6](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.5...someboot-v0.1.6) - 2026-03-06

### Other

- 🛠️ fix: 更新 secondary_entry 函数以传递 cpu_meta 参数 ([#42](https://github.com/drivercraft/sparreal-os/pull/42))

## [0.1.5](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.4...someboot-v0.1.5) - 2026-03-05

### Other

- ✨ feat: 添加 read_byte 函数以读取字节数据 ([#40](https://github.com/drivercraft/sparreal-os/pull/40))

## [0.1.4](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.3...someboot-v0.1.4) - 2026-03-04

### Other

- ✨ feat: 重构设备驱动接口，移除 open/close 方法，添加 name 方法 ([#25](https://github.com/drivercraft/sparreal-os/pull/25))
- ✨ feat: 添加对 x86_64 和 riscv64 架构的编译支持 ([#23](https://github.com/drivercraft/sparreal-os/pull/23))
- ✨ feat: CI 增加真机测试 ([#22](https://github.com/drivercraft/sparreal-os/pull/22))
- ✨ feat: smp and precpu support ([#20](https://github.com/drivercraft/sparreal-os/pull/20))

## [0.1.3](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.2...someboot-v0.1.3) - 2026-02-13

### Other

- ✨ feat: 添加 PerCpuData 内存类型，优化内存映射和分配逻辑 ([#19](https://github.com/drivercraft/sparreal-os/pull/19))

## [0.1.2](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.1...someboot-v0.1.2) - 2026-02-10

### Other

- * ✨ feat(ranges-ext): 添加 ranges-ext 包及其相关功能和测试

## [0.1.1](https://github.com/drivercraft/sparreal-os/compare/someboot-v0.1.0...someboot-v0.1.1) - 2026-02-09

### Other

- release ([#12](https://github.com/drivercraft/sparreal-os/pull/12))

## [0.1.0](https://github.com/drivercraft/sparreal-os/releases/tag/someboot-v0.1.0) - 2026-02-09

### Added

- *(relocate)* 在重置时添加缓存刷新以确保内存一致性
- *(mmu)* 添加注释说明在启用 MMU 前 aarch64 的 `LDXR` 和 `LDAXR` 不可用
- *(mmu)* 使用 init_single_core 方法初始化引导表以支持单核场景
- *(mmu)* 优化 MMU 启用逻辑，添加错误处理并使用 StaticCell 管理引导表
- *(cmdline)* 使用 StaticCell 替换 CMDLINE 的静态数组，优化命令行设置和读取逻辑

### Other

- ♻️ refactor(chore): 清理项目配置并统一 Cargo 设置
- ♻️ refactor(mem): 重构内存初始化 API 以使用范围参数
- 🐛 fix(mmu): 修正页表条目共享属性为 INNER
- ♻️ refactor(mmu): 添加 PageTableInfo::zero() 辅助方法并优化条件编译
- ✨ feat(mem): 重构内存设置 API，引入类型安全的 PhysAddr 抽象
- ✨ feat(tests): 新增 DMA 操作跟踪工具，支持验证 DMA 操作的正确性
- ✨ feat(irq): 更新 thiserror 版本至 2.0.18，添加 AArch64 和 LoongArch64 中断处理模块
- ✨ feat(uspace): 添加用户空间支持，更新相关配置和实现
- ✨ feat(power): 添加 CPU 启动功能
- 🐛 fix(el2): 修正 Hypervisor 模式定时器寄存器使用
- ♻️ refactor(aarch64): 优化 PageTableInfo 导入,使用完整路径
- ♻️ refactor(aarch64, el2): 完善 Hypervisor 模式页表与定时器支持
- 🔧 chore(somehal): 移动构建脚本并增加栈大小，添加文档
- ♻️ refactor(sparreal-rt): 移除对 someboot 的直接依赖，统一通过 somehal 访问
- ♻️ refactor(irq): 将 IRQ 处理逻辑从 someboot 迁移到 somehal
- ♻️ refactor(timer, sync): 优化定时器和自旋锁实现，移除冗余日志
- 🐛 fix(timer): 重构定时器接口，替换 systimer 为 systick，更新相关函数调用
- 🐛 fix(arch): 注释掉 systimer IRQ 启用和禁用的调试日志
- 🔧 chore(aarch64): 移除 entry.rs 中未使用的 ArchTrait 导入
- 🐛 fix(aarch64): 修正 systick_irq_is_enabled 返回值逻辑
- ♻️ refactor(aarch64, loongarch64): 添加 trap_addr 函数并优化相关导入
- ♻️ refactor(aarch64): 改进 GIC CPU 接口使能检查逻辑
- ♻️ refactor(aarch64): 重命名中断处理函数并简化导入
- ♻️ refactor(aarch64): 完善中断处理和 GICv3 驱动集成
- ♻️ refactor(platform): 重构平台层初始化流程和模块组织
- 🔧 chore(version): 调整版本号以反映重命名后的架构
- ♻️ refactor(build): 重命名 somehal crate 为 someboot
