# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
