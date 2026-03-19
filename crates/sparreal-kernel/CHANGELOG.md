# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.15.2](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.15.1...sparreal-kernel-v0.15.2) - 2026-03-19

### Other

- ✨ feat: 添加x86_64支持 ([#60](https://github.com/drivercraft/sparreal-os/pull/60))

## [0.15.1](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.15.0...sparreal-kernel-v0.15.1) - 2026-03-10

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))
- ✨ feat: 添加驱动测试技能文档，添加 NVMe ([#44](https://github.com/drivercraft/sparreal-os/pull/44))

## [0.15.0](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.14.0...sparreal-kernel-v0.15.0) - 2026-03-04

### Other

- ✨ feat: smp and precpu support ([#20](https://github.com/drivercraft/sparreal-os/pull/20))

## [0.14.0](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.13.1...sparreal-kernel-v0.14.0) - 2026-02-13

### Other

- ✨ feat: 添加 PerCpuData 内存类型，优化内存映射和分配逻辑 ([#19](https://github.com/drivercraft/sparreal-os/pull/19))

## [0.13.1](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.13.0...sparreal-kernel-v0.13.1) - 2026-02-09

### Other

- release ([#12](https://github.com/drivercraft/sparreal-os/pull/12))

## [0.13.0](https://github.com/drivercraft/sparreal-os/compare/sparreal-kernel-v0.12.5...sparreal-kernel-v0.13.0) - 2026-02-09

### Added

- 添加用户页表管理功能，更新相关接口以支持用户态页表操作
- 更新内核启动逻辑，添加启动时的 logo 输出
- 更新 panic 处理逻辑，移除无限循环并调用系统关机函数
- 更新内存管理模块，添加内核页表锁定机制，优化页表初始化逻辑
- 重构内存管理和重定位模块，添加 MMU 初始化后处理函数，清理未使用代码
- 更新内存管理和控制台逻辑，添加调试内存描述符支持，优化页表映射
- 重构内存重定位逻辑，添加偏移支持并优化相关接口
- 更新 heapless 依赖版本，重命名 iomap 为 ioremap，添加 iounmap 方法以支持 I/O 内存映射
- 重构内存管理相关代码，添加 PageTable 操作接口，优化映射逻辑并移除冗余模块
- 添加 MemConfig 的 Display 实现，优化控制台输出格式；更新分页映射以包含内存类型和大小信息
- integrate ranges-ext and num-align for improved memory management
- 重构内存分配器，更新全局分配器名称并优化相关实现
- 完成深度挖掘结果文档，详细记录核心模块分析与架构支持
- 添加 Sparreal OS 各模块的架构文档，包括内核、硬件抽象层、平台运行时、异步和定时器测试套件
- 添加调试信息以增强内核页表初始化和内存映射的可追踪性
- 实现内核页表管理功能，更新页表大小为4KB，并调整相关代码以支持新配置
- 重命名 MMU 设置函数为 enable_paging，更新相关调用以反映新名称
- 添加 MMU 设置功能，更新相关接口以支持内存管理
- 重构系统定时器接口，添加 IRQ 启用、禁用及状态检查功能
- 重构内存管理，添加页表信息结构，更新相关函数以支持内核和用户页表操作
- 添加内核页表物理地址和ASID的获取与设置函数，重构相关模块
- 添加 page-table-generic 依赖，重构内存管理和页表相关功能
- 添加 DMA API 支持，重构相关模块和文档
- 更新依赖项，重构内存地址处理，优化类型定义和对齐功能
- 重构中断处理相关函数，优化 IRQ ID 类型的使用
- 添加对无标准库环境的支持，优化相关模块配置
- 添加单CPU异步执行器及相关任务管理功能
- 添加异步模块并调整模块导入顺序
- 在内存模块中添加条件编译支持，以适应无操作系统环境
- 添加定时器中断确认功能，重构相关接口以支持软中断管理
- 重构系统定时器接口，支持以滴答和持续时间设置定时器，添加获取定时器频率和当前滴答计数的功能
- 重构定时器相关功能，统一命名为systimer并实现启用、禁用及设置下一个事件的功能
- 添加定时器功能，支持一键定时器调度和状态检查
- 将init函数的可见性更改为pub(crate)，限制其在当前crate内可用
- 实现IRQ处理函数，支持中断处理逻辑并优化Spinlock获取方法
- 添加IRQ处理相关功能，重构定时器初始化和中断保护逻辑
- 添加对中断处理的支持，重构相关逻辑并优化定时器处理
- 添加对LoongArch64架构的支持，优化中断处理和上下文切换逻辑
- 添加中断安全的Spinlock实现，支持互斥锁和自旋锁功能
- 添加byte-unit依赖并实现内存页大小功能
- integrate byte-unit crate and enhance memory management
- 添加qemu-la64配置文件，更新loongarch64构建配置，重构内存分配逻辑，优化控制台输出
- 添加kernutil模块，重构内存管理逻辑，移除os-helper支持
- 添加内存管理和控制台功能，重构日志系统，优化模块结构
- 添加os-helper支持，重构内存管理逻辑，优化内存分配和地址转换
- 添加somehal-macros支持，重构内核和用户空间的入口逻辑，优化Cargo配置
- 添加os-helper模块，整合内存描述符管理，优化内存映射设置

### Fixed

- 修正调试信息格式，确保物理地址以十六进制格式输出
- 修改地址处理函数为引用传递，优化内存对齐检查逻辑

### Other

- ♻️ refactor(chore): 清理项目配置并统一 Cargo 设置
- ♻️ refactor(mem): 重构内存初始化 API 以使用范围参数
- 💥 feat(dma-api): 更新 dma-api 版本至 0.7.0，调整依赖项配置
- ✨ feat(config): 更新 Cargo 配置，添加 xtask 及相关命令，调整构建和测试配置
- ✨ feat(platform): 添加 DeviceTree 结构及相关功能，更新 Cargo.toml 依赖
- ♻️ refactor(timer, irq): 移除冗余的调试日志输出
- ♻️ refactor(irq): 将 IRQ 处理逻辑从 someboot 迁移到 somehal
- ♻️ refactor(timer, sync): 优化定时器和自旋锁实现，移除冗余日志
- 🐛 fix(timer): 重构定时器接口，替换 systimer 为 systick，更新相关函数调用
- 🐛 fix(timer): 增加最小延迟以处理零/接近零延迟的边缘情况
- ♻️ refactor(irq): 优化注册中断处理程序的锁定逻辑
- ♻️ refactor(platform): 为 LoongArch64 添加平台抽象层实现并调整驱动初始化
- ♻️ refactor(platform): 重构平台层初始化流程和模块组织
- ✨ feat(drivers): 集成 rdrive 驱动框架,添加 FDT 和 PCIe 支持
- ♻️ refactor(linker): 移除冗余的驱动程序段定义，优化链接脚本
- ♻️ refactor(memory): 移除不必要的物理地址与虚拟地址转换函数，优化内存接口
- ♻️ refactor(loongarch64): 清理编译警告和 clippy 警告
- ♻️ refactor(hal): 重构地址转换方法，分离RAM和IO地址映射
- ♻️ refactor(irq): 重构中断ID类型系统,统一使用IrqId替代SoftIrqId
- ♻️ refactor(mem): 改进内存映射逻辑,统一KImage和MMIO处理
- ♻️ refactor(hal): 移除PageTableOp trait,直接使用page_table_generic
- ♻️ refactor(mem): 重构内存管理接口,统一boot table管理和ioremap实现
- 🎨 style: 代码格式化和文档优化
- 优化日志记录格式，简化调试信息输出
- 移除未使用的CRange结构体，简化内存地址模块
- 优化自旋锁实现，简化锁获取和释放逻辑
- init
