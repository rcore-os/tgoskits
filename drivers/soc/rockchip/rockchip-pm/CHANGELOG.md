# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2025-01-28

### Added
- Initial public release
- Support for RK3588 power domain management
- Power domain control API (`power_domain_on`, `power_domain_off`)
- Device tree compatible string based initialization
- Power domain name lookup functionality (`get_power_dowain_by_name`)
- Comprehensive documentation and examples
- Integration tests for NPU power domains
- CI/CD workflows for testing, documentation deployment, and releases

### Supported Hardware
- **RK3588**: Full support with 31 power domains
  - GPU, NPU, VCODEC, Video I/O, Image processing, Bus controllers, etc.
- **RK3568**: Placeholder (not implemented)

### Power Domains (RK3588)
- Compute: GPU, NPU, NPUTOP, NPU1, NPU2
- Video: VCODEC, VENC0, VENC1, RKVDEC0, RKVDEC1, AV1, VDPU
- Image: VI, ISP1, RGA30, RGA31
- Display: VOP, VO0, VO1
- Bus: PHP, GMAC, PCIE, SDIO, USB, SDMMC
- Audio: AUDIO
- Storage: NVM, NVM0, FEC

### Dependencies
- rdif-base (0.7): Driver framework
- tock-registers (0.10): Register access
- mbarrier (0.1): Memory barriers
- dma-api (0.5): DMA support
- log (0.4): Logging

### Development
- bare-test (0.7): Testing framework
- bare-test-macros (0.2): Test macros

[Unreleased]: https://github.com/drivercraft/rockchip-pm/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/drivercraft/rockchip-pm/releases/tag/v0.4.0
