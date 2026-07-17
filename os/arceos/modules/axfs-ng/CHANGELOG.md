# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add generation-bound root filesystem freeze, detach, remount, and stale-handle semantics.
- Add non-blocking, typed freeze-drain progress for handoff orchestration.

### Changed

- Keep the remount recipe's block service private so filesystem callers cannot
  bypass freeze and controller ownership handoff with direct submissions.
- Move block request, DMA, IRQ, completion, worker, watchdog, and recovery ownership to the
  runtime; ax-fs-ng now consumes only a synchronous `BlockDevice` service.
- Retain and replay every successfully mounted filesystem across detach and remount, rather than
  reconstructing only the root mount.
- Require a checked `UnmanagedLocation` capability for raw file, cache, and context construction;
  detachable locations remain generation-bound across backend clones and mappings.
- Carry resolved locations as `FileLocation` capabilities so a caller cannot relabel a location
  from another filesystem runtime or mount generation as current.
- Require directory handles, cwd/root updates, and cache imports to prove the original runtime,
  generation, and mount namespace while retaining the operation admitted before a freeze.
- Add a higher-ranked restricted location-operation view for file, backend, cache, resolved-location,
  and directory handles so metadata and typed node state do not require a cloneable raw location.
- Resolve paths and mutate mount namespaces through one higher-ranked namespace-operation view;
  every returned location borrows the exact admitted generation lease or is retained as a checked
  generation capability.
- Make generation-bound cached and direct behavior handles share one counted open-handle lease
  across clones, while keeping retained `FileLocation` capabilities uncounted and cache-global
  shared state lease-free.
- Run current-directory queries under a generation operation lease so a freeze cannot race an
  untracked namespace read.
- Reuse the initiating operation lease across composite path resolution, dangling-symlink create,
  and open-time truncation so work admitted before freeze can finish without admitting new work.
- Pass directory-relative composite callbacks an operation-scoped filesystem-context view, and
  allow that admitted operation to publish counted handles after freeze begins without exposing
  the ordinary re-admitting `FsContext` API.
- Split root publication, context state, cached-file handles, and shared page-cache policy into
  focused modules while preserving the public filesystem API.

### Fixed

- Keep the global cached-file spin lock limited to registry membership: cache
  dirty-state checks, writeback, reclaim, and listener callbacks now run after
  a bounded Arc snapshot or detached registry handoff, avoiding PI-mutex
  acquisition with preemption disabled.
- Keep ext4 create, metadata update, symlink, unlink, and rename operations
  buffered until an explicit durability boundary instead of issuing a full
  filesystem and journal flush for every namespace mutation.
- Propagate terminal root-volume metadata I/O errors with their original
  `AxError` after exactly one block-service call; controller recovery and
  request resubmission remain owned by the block runtime.
- Keep ext4 buffered writes, appends, and length changes dirty in the page and
  filesystem caches until an explicit file sync, filesystem sync, or unmount;
  extending a cached file no longer forces a whole-filesystem flush per write.
- Write back dirty LRU pages in bounded 1 MiB contiguous runs under cache
  pressure, without promoting writeback-only pages or forcing `fsync`, so
  sequential writes do not collapse into one 4 KiB hardware request per page.
- Retry valid short backing writes until each dirty page run is fully consumed,
  and report a zero-length write as `WriteZero` instead of marking partial data clean.
- Fill consecutive cold page-cache misses with a syscall-independent, bounded
  1 MiB readahead window while preserving cached-page boundaries, so small
  sequential reads no longer submit one hardware request for every 4 KiB page.
- Run reclaim eviction callbacks without the listener lock, and reserve each
  popped page number through the callback decision so a concurrent cache miss
  cannot be overwritten by a refused eviction.
- Deduplicate inode-shared cache state in the global reclaim registry so hard
  links do not pin duplicate entries after their last external reference drops.
- Keep in-memory filesystem pages clean across truncate because they have no
  backing writeback path, while disk-backed truncation still records a new dirty
  generation for the zeroed partial-page tail.
- Reuse the operation admitted by `File::drop` for timestamp updates so a
  concurrent freeze cannot reject nested admission halfway through close.
- Reject overflowing block-region lengths instead of silently truncating the
  published region at `u64::MAX`.
- Propagate FAT flushes to the underlying block service without forcing durable flushes during
  cursor-only buffer writeback.
- Report frozen or stale files as terminal poll errors instead of silently returning no events.
- Reject mount namespaces retained from a previous runtime or mount generation instead of
  relabelling their VFS tree through a newly mounted filesystem context.
- Reject cross-runtime mount, move, link, and rename composition between restricted location
  views, while continuing to allow operations across mounts in one filesystem generation.

## [0.8.4](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.8.3...ax-fs-ng-v0.8.4) - 2026-07-10

### Other

- updated the following local packages: ax-sync

## [0.8.3](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.8.2...ax-fs-ng-v0.8.3) - 2026-07-08

### Other

- updated the following local packages: ax-sync

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.8.1...ax-fs-ng-v0.8.2) - 2026-07-08

### Other

- updated the following local packages: ax-sync

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.8.0...ax-fs-ng-v0.8.1) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, dma-api, axfs-ng-vfs, rsext4, rdif-block, ax-sync

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.7.0...ax-fs-ng-v0.8.0) - 2026-07-07

### Added

- *(starry)* add nix test (no sandbox currently) and kernel regression suite ([#1125](https://github.com/rcore-os/tgoskits/pull/1125))
- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))
- *(starry)* back /proc/diskstats, /proc/net/dev and /proc/mounts with real data ([#1504](https://github.com/rcore-os/tgoskits/pull/1504))

### Fixed

- *(block)* drive virtio-blk completions by IRQ ([#1512](https://github.com/rcore-os/tgoskits/pull/1512))
- *(starry-mm)* bound per-file page-cache pre-allocation to avoid OOM ([#1499](https://github.com/rcore-os/tgoskits/pull/1499))

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-fs-ng-v0.6.0...ax-fs-ng-v0.7.0) - 2026-07-02

### Added

- *(kspin)* add lockdep-aware spin rwlock ([#1397](https://github.com/rcore-os/tgoskits/pull/1397))

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))
- *(ax-fs-ng)* keep early IRQ block events ([#1429](https://github.com/rcore-os/tgoskits/pull/1429))
- *(irq)* separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))
- *(ax-runtime)* resolve device IRQ bindings to IrqId
- Revert "fix(irq): separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))" ([#1424](https://github.com/rcore-os/tgoskits/pull/1424))

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
