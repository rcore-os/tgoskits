# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.5.0...rsext4-v0.6.0) - 2026-06-09

### Added

- *(rsext4)* fine-grained locking for SMP scalability ([#1057](https://github.com/rcore-os/tgoskits/pull/1057))
- *(vfs)* pass uid/gid through creation path to filesystem nodes ([#1097](https://github.com/rcore-os/tgoskits/pull/1097))

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.4.1...rsext4-v0.5.0) - 2026-06-03

### Added

- *(rsext4)* replace single-block cache with multi-entry clock LRU (CACHE_ENTRIES=4, 16 KiB) ([#971](https://github.com/rcore-os/tgoskits/pull/971))
- *(starry)* add SG2002 board boot support ([#834](https://github.com/rcore-os/tgoskits/pull/834))

### Fixed

- *(rsext4)* use physical byte offset in readdir to fix rm -rf skipping entries ([#1001](https://github.com/rcore-os/tgoskits/pull/1001))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))
- *(rsext4)* rmdir returns ENOTEMPTY on non-empty dirs, rename rejects cross-type overwrites ([#854](https://github.com/rcore-os/tgoskits/pull/854))

### Other

- *(ci)* bump Rust toolchain to nightly-2026-05-28 and fix clippy ([#1027](https://github.com/rcore-os/tgoskits/pull/1027))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor journal recovery and partition scanning logic ([#927](https://github.com/rcore-os/tgoskits/pull/927))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.4.0...rsext4-v0.4.1) - 2026-05-22

### Fixed

- *(repo)* improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))
- *(rsext4)* preserve directory inode generation ([#828](https://github.com/rcore-os/tgoskits/pull/828))
- *(axfs-ng-vfs)* allow file rename into child dirs and fix ext4 dentry delete ([#807](https://github.com/rcore-os/tgoskits/pull/807))

### Other

- Revert " fix(repo): improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))" ([#838](https://github.com/rcore-os/tgoskits/pull/838))

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
