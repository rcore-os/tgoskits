# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add Linux-compatible signed and unsigned legacy, half-MD4, and TEA directory
  hashes with typed major/minor results and checked HTree root/index parsing.
- Add Linux-compatible one-block linear-to-HTree conversion, leaf insertion and
  splitting, collision separator encoding, root promotion, internal-node
  splitting, leaf-only indexed deletion, checksums, and transactional rollback
  across directory, index, inode, and allocation metadata.
- Add typed extent preallocation with Linux-compatible unwritten extent encoding,
  `KEEP_SIZE`, partial-write conversion, remount, and e2fsck coverage.
- Add typed `ZERO_RANGE` and `PUNCH_HOLE` operations for extent-backed files,
  finite legacy-indirect hole punching, and unwritten-aware truncate cleanup.
- Add cluster-aligned `COLLAPSE_RANGE` and `INSERT_RANGE` extent transforms with
  dynamic block-size, multi-leaf, unwritten-state, remount, and e2fsck coverage.
- Add typed inode extent inspection for extent and legacy mappings, including
  sparse, unwritten, directory, bounded, inline/external xattr, and dynamic
  block-size coverage.
- Add typed inode-number extended-attribute operations with checked inline and
  external-block codecs, Linux hashes/checksums, create/replace policy, and
  dynamic block-size Linux-image coverage.
- Add a durable external-xattr benchmark workload and report its final
  Linux/dev comparison without retaining development-only raw samples.
- Add typed user-visible inode flags and project identifiers to the owned core
  metadata DTOs without exposing the on-disk inode representation.
- Add typed linear/HTree directory cursors, bounded hash-range leaf
  enumeration, Linux 64-bit directory-cookie encoding, private collision
  continuation, and a machine-readable HTree readdir benchmark workload.
- Add a persistent directory change attribute to the stable inode DTO and bind
  private VFS directory continuation state to the observed value.
- Add Linux-style per-open HTree hash-range caching behind an opaque directory
  reader, with mutation invalidation and VFS/Starry open-description ownership.

### Removed

- Remove the fabricated inode-only HTree root metadata query; hash version and
  depth now come exclusively from the checked on-disk root block.
- Remove the public HTree parser/manager module; wire structures and lookup
  state are private core implementation details.
- Remove the non-Linux `set_symlink_target` mutation; symbolic-link targets are
  now supplied only as part of atomic inode creation or inode replacement.
- Remove misspelled compatibility entry points and replace the misspelled
  `BlcokGroupLayout` type and pluralized layout fields with the canonical
  `BlockGroupLayout` API.
- Remove the descriptor-style `OpenFile/open/read_at/write_at/lseek` API, the
  redundant `mv`/`rename_with_options` aliases, and unused crypto/key mount
  provider placeholders; callers now use inode-owned operations and one typed
  rename entry point directly.
- Remove prefix-only group-descriptor checksum methods; checksum ownership now
  stays inside full-record mount and persistence paths.
- Rename the misspelled `JournalSuperBllockS` disk type to
  `JournalSuperBlock` without a compatibility alias.

### Fixed

- Validate HTree root and internal-node geometry, count/limit headers, entry
  ordering, logical block ranges, cycles, and index/data checksums before lookup,
  including mandatory directory-entry tails on metadata-checksummed leaf blocks.
- Preserve all 28 logical block bits in HTree index entries and decode Linux's
  compact directory record lengths when checking large index blocks.
- Use the HTree hash-space EOF for indexed-directory `SEEK_END` while retaining
  byte-size endpoints for linear directories through a typed VFS capability.
- Increment the ext4 on-disk inode version for directory entry mutations and
  discard stale HTree collision continuation after concurrent changes.
- Follow Linux HTree collision continuations across leaf and parent-index
  boundaries, preserve continuation I/O errors, and report the true byte offset
  of matched directory entries.
- Return owned HTree lookup results instead of extending cache borrows to
  `'static`, and limit linear fallback to Linux-style bad-index errors while
  preserving I/O and checksum failures.
- Reject invalid default directory-hash versions during feature negotiation and
  persist an unambiguous signed-byte policy on writable indexed filesystems.
- Reuse the current Linux-style JBD2 owner for nested metadata helpers without
  reserving a second credit budget, while rolling back only a failed nested
  scope and keeping the outer handle usable.
- Restore filesystem metadata caches and allocation counters when an xattr
  journal handle aborts, and queue its inode, bitmap, group descriptor, and
  superblock images under one bounded transaction.
- Prevent dirty journal-owned metadata buffers from reaching home blocks before
  commit or while rolling back a failed handle.
- Preserve unmodeled inline-xattr bytes across inode-cache mutations and include
  the complete raw inode record in metadata checksums.
- Preserve extent allocation and inode block accounting when publishing an
  updated external extent leaf fails during removal.
- Replay JBD2 in scan, revoke, and replay passes so a later transaction can
  revoke an earlier transaction's logged payload without hiding newer data.
- Create the internal journal explicitly during mkfs, and reject missing,
  unlinked, non-regular, or encrypted journal inodes on mount instead of
  repairing them.
- Reject ambiguous internal/external journal declarations before mount
  mutation, and report an external-only journal as a missing injected device
  capability instead of silently using the filesystem device.
- Copy only metadata payloads touched by a transaction while retaining complete
  rollback ownership for allocator, bitmap, inode, and superblock state.
- Restore physical metadata preimages when a journal-disabled transaction
  fails, including shared external-xattr COW allocation and inode state.
- Reject short inode records and invalid `i_extra_isize` values with typed
  corruption errors before the inode cache decodes any on-disk fields.
- Validate Linux group-descriptor sizes before decoding, use 32-byte strides
  without `64bit`, and preserve/checksum extension bytes through byte 1024.
- Decode and encode JBD2 superblocks through checked 1024-byte prefixes and
  accept Linux V1 journals without interpreting their V2-only extension area.
- Publish hard-link target and parent inode records together with the directory
  metadata block under one bounded transaction, restoring disk and cache state
  when directory publication fails.
- Keep directory-growth block bitmaps, group descriptors, the superblock, and
  extent metadata inside the Linux-sized hard-link transaction boundary.
- Publish unlink and empty-directory removal atomically with parent/target
  inode updates and the classic orphan head under Linux-sized transactions.
- Reclaim orphaned inodes under Linux-sized truncate credits while atomically
  persisting block/inode bitmaps, group counters, orphan links, and checksums.
- Create regular, special, symlink, and directory inodes under one Linux-sized
  namespace transaction with ordered payload publication and full cache undo.
- Write and replay one JBD2 transaction across multiple descriptor blocks while
  accounting descriptor overhead in journal-ring credit reservations.
- Publish replace and exchange rename operations atomically across both names,
  parent links, directory parent records, target links, and orphan state.
- Keep ext4 directory names as raw bytes through VFS and commit Starry/ArceOS
  open-directory positions only after the corresponding output record succeeds.

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.8.0...rsext4-v0.8.1) - 2026-08-27

### Added

- *(ax-fs-ng)* add shared block cache between block and filesystem layers ([#2171](https://github.com/rcore-os/tgoskits/pull/2171))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.8...rsext4-v0.8.0) - 2026-08-20

### Fixed

- *(rsext4)* reject empty internal extent nodes ([#1968](https://github.com/rcore-os/tgoskits/pull/1968))
- *(rsext4)* propagate journal I/O failures without panicking ([#1967](https://github.com/rcore-os/tgoskits/pull/1967))

### Other

- *(axtest)* standardize Cargo and QEMU test flow ([#2088](https://github.com/rcore-os/tgoskits/pull/2088))
- *(sync)* unify lock primitives in ax-sync ([#1956](https://github.com/rcore-os/tgoskits/pull/1956))

## [0.7.8](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.7...rsext4-v0.7.8) - 2026-08-09

### Fixed

- *(axvisor)* correct shell filesystem command handling ([#1616](https://github.com/rcore-os/tgoskits/pull/1616))
- *(rsext4)* fix misspelled public API names in rsext4 ([#1881](https://github.com/rcore-os/tgoskits/pull/1881))

### Other

- *(repo)* move filesystem crates to fs/ directory ([#1867](https://github.com/rcore-os/tgoskits/pull/1867))

## [0.7.7](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.6...rsext4-v0.7.7) - 2026-08-03

### Other

- enhance axtest coverage for various starry-kernel contracts ([#1674](https://github.com/rcore-os/tgoskits/pull/1674))

## [0.7.6](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.5...rsext4-v0.7.6) - 2026-07-23

### Added

- *(starry)* run x86_64 self-build through the Starry app ([#1076](https://github.com/rcore-os/tgoskits/pull/1076))

## [0.7.5](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.4...rsext4-v0.7.5) - 2026-07-08

### Other

- updated the following local packages: ax-kspin

## [0.7.4](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.3...rsext4-v0.7.4) - 2026-07-07

### Added

- *(starry)* add nix test (no sandbox currently) and kernel regression suite ([#1125](https://github.com/rcore-os/tgoskits/pull/1125))

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.2...rsext4-v0.7.3) - 2026-07-02

### Other

- updated the following local packages: ax-kspin

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.1...rsext4-v0.7.2) - 2026-06-23

### Other

- updated the following local packages: ax-kspin

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.7.0...rsext4-v0.7.1) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/rsext4-v0.6.0...rsext4-v0.7.0) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

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
