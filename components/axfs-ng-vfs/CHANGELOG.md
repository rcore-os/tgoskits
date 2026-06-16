# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.4.3...axfs-ng-vfs-v0.5.0) - 2026-06-09

### Added

- *(vfs)* pass uid/gid through creation path to filesystem nodes ([#1097](https://github.com/rcore-os/tgoskits/pull/1097))

### Fixed

- *(locking)* narrow spinlock scope in VFS and Starry paths ([#1146](https://github.com/rcore-os/tgoskits/pull/1146))
- *(lockdep)* resolve Starry lock ordering and log print issues ([#1103](https://github.com/rcore-os/tgoskits/pull/1103))

## [0.4.3](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.4.2...axfs-ng-vfs-v0.4.3) - 2026-06-03

### Added

- *(starry-kernel)* support cgroup2 hierarchy mkdir and rmdir ([#1015](https://github.com/rcore-os/tgoskits/pull/1015))

### Fixed

- *(axfs-ng-vfs)* skip children cache transfer on rename to avoid stale parent references ([#938](https://github.com/rcore-os/tgoskits/pull/938))
- *(ci)* stabilize Starry LoongArch apk-curl test ([#959](https://github.com/rcore-os/tgoskits/pull/959))
- *(starry)* align mount and umount2 semantics with Linux ([#876](https://github.com/rcore-os/tgoskits/pull/876))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.4.1...axfs-ng-vfs-v0.4.2) - 2026-05-22

### Fixed

- *(starry-kernel)* open/openat deep — 6 类跨子系统改造 (stacked on #719) ([#720](https://github.com/rcore-os/tgoskits/pull/720))
- *(axfs-ng-vfs)* allow file rename into child dirs and fix ext4 dentry delete ([#807](https://github.com/rcore-os/tgoskits/pull/807))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.4.0...axfs-ng-vfs-v0.4.1) - 2026-05-19

### Other

- updated the following local packages: ax-errno

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.3.8...axfs-ng-vfs-v0.4.0) - 2026-05-15

### Added

- *(starry-kernel)* add runtime dynamic debug control ([#446](https://github.com/rcore-os/tgoskits/pull/446))
- *(mm)* track backend split metadata and generate real /proc maps output ([#306](https://github.com/rcore-os/tgoskits/pull/306))

### Fixed

- *(ext4)* use Linux-compatible old/new_encode_dev for device rdev ([#518](https://github.com/rcore-os/tgoskits/pull/518))
- *(loop)* replace map_or with is_none_or to silence clippy unnecessary_map_or ([#501](https://github.com/rcore-os/tgoskits/pull/501))
- *(vfs)* hard links on tmpfs return empty data — propagate page cache on link ([#378](https://github.com/rcore-os/tgoskits/pull/378))

### Other

- Merge pull request #366 from rcore-os/fix-deps

## [0.3.8](https://github.com/rcore-os/tgoskits/compare/axfs-ng-vfs-v0.3.7...axfs-ng-vfs-v0.3.8) - 2026-04-27

### Fixed

- *(vfs)* preserve source DirEntry across rename ([#312](https://github.com/rcore-os/tgoskits/pull/312))
