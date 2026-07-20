# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Return the allocation owner when MSI vector release fails, allowing callers
  to retain partially released hardware resources in a typed quarantine.

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/rdif-msi-v0.1.0...rdif-msi-v0.2.0) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))
