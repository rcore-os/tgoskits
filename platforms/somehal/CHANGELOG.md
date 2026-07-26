# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/somehal-v0.8.0...somehal-v0.8.1) - 2026-07-23

### Added

- *(axvisor)* Enhance AxLoader and Asus NUC15CRH support with fixes ([#1555](https://github.com/rcore-os/tgoskits/pull/1555))

### Other

- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.8...somehal-v0.8.0) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.7.8](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.7...somehal-v0.7.8) - 2026-07-08

### Fixed

- *(platforms)* route DMA cache sync through platform cache ops ([#1542](https://github.com/rcore-os/tgoskits/pull/1542))

## [0.7.7](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.6...somehal-v0.7.7) - 2026-07-08

### Other

- updated the following local packages: someboot

## [0.7.6](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.5...somehal-v0.7.6) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, rdrive

## [0.7.5](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.4...somehal-v0.7.5) - 2026-07-07

### Added

- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))

### Other

- *(somehal)* cache IRQ routes in CPU interfaces ([#1494](https://github.com/rcore-os/tgoskits/pull/1494))
- *(platforms)* move someboot and somehal-macros and add documents ([#1485](https://github.com/rcore-os/tgoskits/pull/1485))

## [0.7.4](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.3...somehal-v0.7.4) - 2026-07-02

### Added

- *(somehal)* allocate interrupt controller domains
- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(somehal)* validate GIC runtime INTIDs
- *(somehal)* fast-path x86 IOAPIC IRQ enable
- *(somehal)* enable GIC private IRQs without controller locks
- *(irq)* avoid hard irq controller locks
- *(irq)* close domain runtime review gaps

### Other

- *(somehal)* restructure RISC-V IRQ routing ([#1443](https://github.com/rcore-os/tgoskits/pull/1443))
- *(somehal)* restructure LoongArch IRQ routing ([#1442](https://github.com/rcore-os/tgoskits/pull/1442))
- *(somehal)* modernize x86 qemu irq routing ([#1430](https://github.com/rcore-os/tgoskits/pull/1430))

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.2...somehal-v0.7.3) - 2026-06-27

### Added

- *(ax-runtime)* generate banner build info ([#1373](https://github.com/rcore-os/tgoskits/pull/1373))

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.1...somehal-v0.7.2) - 2026-06-23

### Fixed

- *(platform)* support AArch64 HVF timer boot ([#1334](https://github.com/rcore-os/tgoskits/pull/1334))

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/somehal-v0.7.0...somehal-v0.7.1) - 2026-06-22

### Added

- *(ax-runtime)* prefer UEFI RTC on dynamic platform ([#1294](https://github.com/rcore-os/tgoskits/pull/1294))

### Fixed

- *(somehal)* send x86 helper IPI on IPI vector ([#1297](https://github.com/rcore-os/tgoskits/pull/1297))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/somehal-v0.6.10...somehal-v0.7.0) - 2026-06-12

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))
- *(axruntime)* ensure aarch64 SMP IPI readiness before app init ([#1196](https://github.com/rcore-os/tgoskits/pull/1196))
- *(somehal)* route LoongArch ACPI GSIs through PCH-PIC
- *(loongarch64)* ack timer irq before dispatch ([#1222](https://github.com/rcore-os/tgoskits/pull/1222))

### Other

- *(irq)* carry ACPI IRQ routing metadata
- *(rdrive)* carry probe context and PCI INTx routes

## [0.6.10](https://github.com/rcore-os/tgoskits/compare/somehal-v0.6.9...somehal-v0.6.10) - 2026-06-11

### Added

- *(somehal)* support dynamic CPU and interrupt hooks

## [0.6.9](https://github.com/rcore-os/tgoskits/compare/somehal-v0.6.8...somehal-v0.6.9) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(somehal)* register x86 ACPI IOAPIC through rdrive ([#1155](https://github.com/rcore-os/tgoskits/pull/1155))

### Fixed

- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

### Other

- *(arceos)* consolidate Rust QEMU test suite ([#1174](https://github.com/rcore-os/tgoskits/pull/1174))

## [0.6.8](https://github.com/rcore-os/tgoskits/compare/somehal-v0.6.7...somehal-v0.6.8) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))

### Other

- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

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
