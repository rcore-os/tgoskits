# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
