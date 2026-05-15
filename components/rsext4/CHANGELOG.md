# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.3.7...rsext4-v0.4.0) - 2026-05-15

### Fixed

- *(loop)* replace map_or with is_none_or to silence clippy unnecessary_map_or ([#501](https://github.com/rcore-os/tgoskits/pull/501))
- *(rsext4)* avoid replaying clean journals on mount ([#539](https://github.com/rcore-os/tgoskits/pull/539))
- *(rsext4)* replay journal before mount repairs ([#531](https://github.com/rcore-os/tgoskits/pull/531))
- *(delete)* simplify debug message for inode link count ([#411](https://github.com/rcore-os/tgoskits/pull/411))
- *(rsext4)* bound data block cache growth ([#408](https://github.com/rcore-os/tgoskits/pull/408))
- *(rsext4)* repair JBD2 journal replay for Linux rootfs recovery ([#398](https://github.com/rcore-os/tgoskits/pull/398))

### Other

- *(repo)* remove tgmath example and refresh docs/deps
- *(sys_fallocate)* validate negative offset/len, use EOPNOTSUPP for unsupported modes,   reject huge offsets ([#441](https://github.com/rcore-os/tgoskits/pull/441))
- *(rsext4)* inherit workspace metadata
- *(repo)* split non-USB clippy cleanups ([#372](https://github.com/rcore-os/tgoskits/pull/372))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

## [0.3.7](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.3.6...rsext4-v0.3.7) - 2026-04-27

### Other

- sync ext4/rsext4 crash-consistency fixes from x-kernel ([#284](https://github.com/rcore-os/tgoskits/pull/284))
