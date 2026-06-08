# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3](https://github.com/drivercraft/fdt-parser/compare/fdt-edit-v0.2.0...fdt-edit-v0.2.3) - 2026-03-09

### Added

- 添加对时钟属性的解析功能并更新相关测试

### Other

- 更新Cargo.toml中的readme字段
- release ([#11](https://github.com/drivercraft/fdt-parser/pull/11))

## [0.2.0](https://github.com/drivercraft/fdt-parser/compare/fdt-edit-v0.1.5...fdt-edit-v0.2.0) - 2026-03-09

### Added

- enhance FDT parser library with comprehensive improvements
- *(fdt)* [**breaking**] change memory method to return iterator for multiple nodes
- 实现设备地址到CPU物理地址的转换功能，优化节点结构，更新版本号至0.1.2

### Other

- Add inherited interrupt-parent lookup ([#10](https://github.com/drivercraft/fdt-parser/pull/10))
- *(tests)* 简化PCI测试中的if let嵌套结构
- improve iter ([#6](https://github.com/drivercraft/fdt-parser/pull/6))
- translate Chinese comments to English and enhance documentation
