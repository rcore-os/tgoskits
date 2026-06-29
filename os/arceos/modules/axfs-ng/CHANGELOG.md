# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.21...ax-fs-ng-v0.6.0) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))
- *(lockdep)* resolve Starry lock order regressions ([#1375](https://github.com/rcore-os/tgoskits/pull/1375))

### Other

- Merge pull request #1336 from sdio-host2-physical-model

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.20...ax-fs-ng-v0.5.21) - 2026-06-23

### Other

- updated the following local packages: ax-kspin, dma-api, rsext4, rdif-block, ax-sync

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.19...ax-fs-ng-v0.5.20) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.18...ax-fs-ng-v0.5.19) - 2026-06-12

### Other

- updated the following local packages: ax-hal, ax-alloc, ax-sync

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.17...ax-fs-ng-v0.5.18) - 2026-06-11

### Fixed

- *(starry-mm)* bound file-backed mmap populate at EOF ([#1164](https://github.com/rcore-os/tgoskits/pull/1164))
- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.16...ax-fs-ng-v0.5.17) - 2026-06-09

### Added

- *(rsext4)* fine-grained locking for SMP scalability ([#1057](https://github.com/rcore-os/tgoskits/pull/1057))
- *(vfs)* pass uid/gid through creation path to filesystem nodes ([#1097](https://github.com/rcore-os/tgoskits/pull/1097))

### Fixed

- *(axfs-ng)* zero the partial last page when truncating a file shorter ([#1124](https://github.com/rcore-os/tgoskits/pull/1124))
- *(locking)* narrow spinlock scope in VFS and Starry paths ([#1146](https://github.com/rcore-os/tgoskits/pull/1146))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.15...ax-fs-ng-v0.5.16) - 2026-06-03

### Added

- *(mm)* add page reclaim for file-backed memory pressure (rebased) ([#1007](https://github.com/rcore-os/tgoskits/pull/1007))
- *(Starry)* support MariaDB ([#906](https://github.com/rcore-os/tgoskits/pull/906))

### Fixed

- *(rsext4)* use physical byte offset in readdir to fix rm -rf skipping entries ([#1001](https://github.com/rcore-os/tgoskits/pull/1001))
- *(ci)* stabilize Starry LoongArch apk-curl test ([#959](https://github.com/rcore-os/tgoskits/pull/959))
- *(starry)* align mount and umount2 semantics with Linux ([#876](https://github.com/rcore-os/tgoskits/pull/876))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))
- *(ax-fs-ng)* complete direct device transfers ([#800](https://github.com/rcore-os/tgoskits/pull/800))
- *(rsext4)* rmdir returns ENOTEMPTY on non-empty dirs, rename rejects cross-type overwrites ([#854](https://github.com/rcore-os/tgoskits/pull/854))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.14...ax-fs-ng-v0.5.15) - 2026-05-22

### Added

- *(starryos)* add Lua/LuaRocks runtime coverage and fix rmdir ENOTEMPTY ([#777](https://github.com/rcore-os/tgoskits/pull/777))

### Fixed

- *(starry-kernel)* open/openat deep — 6 类跨子系统改造 (stacked on #719) ([#720](https://github.com/rcore-os/tgoskits/pull/720))
- *(starry-kernel)* open/openat 15 类局部修复 ([#719](https://github.com/rcore-os/tgoskits/pull/719))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.13...ax-fs-ng-v0.5.14) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-alloc, ax-driver, axfs-ng-vfs, ax-io, ax-hal, ax-sync

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.12...ax-fs-ng-v0.5.13) - 2026-05-15

### Added

- *(starry-kernel)* add runtime dynamic debug control ([#446](https://github.com/rcore-os/tgoskits/pull/446))
- *(mm)* track backend split metadata and generate real /proc maps output ([#306](https://github.com/rcore-os/tgoskits/pull/306))

### Fixed

- *(axdriver_block)* when the partition type is MBR, select ext4 and active partition as rootfs
- *(rsext4)* bound data block cache growth ([#408](https://github.com/rcore-os/tgoskits/pull/408))
- *(fs)* mkdir("/") returns EINVAL instead of EEXIST ([#375](https://github.com/rcore-os/tgoskits/pull/375))
- implement close_all_fds function and enhance pipe and syscall handling ([#305](https://github.com/rcore-os/tgoskits/pull/305))

### Other

- Merge pull request #554 from rcore-os/feat/sg2002-pr383
- *(sys_fallocate)* validate negative offset/len, use EOPNOTSUPP for unsupported modes,   reject huge offsets ([#441](https://github.com/rcore-os/tgoskits/pull/441))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.5.11...ax-fs-ng-v0.5.12) - 2026-04-27

### Fixed

- *(fcntl)* return correct access mode flags in F_GETFL ([#260](https://github.com/rcore-os/tgoskits/pull/260))
- *(file)* reject O_WRONLY/O_RDWR on directories with EISDIR ([#253](https://github.com/rcore-os/tgoskits/pull/253))

### Other

- sync ext4/rsext4 crash-consistency fixes from x-kernel ([#284](https://github.com/rcore-os/tgoskits/pull/284))
