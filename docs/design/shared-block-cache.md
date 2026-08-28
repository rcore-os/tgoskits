# Shared Block Cache

Status: proposed for PR [#2171](https://github.com/rcore-os/tgoskits/pull/2171).

## Problem

`ax-fs-ng` previously passed filesystem block requests directly to the block
runtime while `rsext4` kept a small private block cache. That arrangement had
three problems:

1. two filesystem instances or partitions backed by the same physical device
   could not share one authoritative cached view;
2. cache writeback, runtime-direct I/O, journal ownership, and global sync had
   no common coherence boundary;
3. the private cache duplicated block-device state while providing neither a
   bounded per-device reclaim policy nor a device-wide flush operation.

The users of this feature are the `ax-fs-ng` ext4 and FAT adapters, `rsext4`
metadata and journal paths, allocator reclaim, filesystem shutdown, and the
StarryOS durability syscalls that reach these layers.

## Success Criteria

- Every live filesystem consumer of one runtime block-device handle resolves
  to one cache tree.
- Small metadata-shaped requests use bounded deferred writeback, while large
  requests can remain device-direct without returning stale cached bytes.
- A direct write that reports an indeterminate partial failure cannot leave an
  overlapping cached folio authoritative.
- Journal descriptor, data, flush barrier, and commit-record ordering remains
  unchanged when writes are deferred.
- Global sync and last-consumer teardown attempt all reachable dirty cache
  trees without extending device lifetime indefinitely.
- Allocation, geometry, registry, writeback, and device failures remain typed
  and observable at kernel boundaries.
- Deterministic host tests cover shared views, partition offsets, partial I/O
  failure, writeback retry, reclaim, registry teardown, and journal re-editing;
  the block benchmark verifies content across independent file descriptors.

## Necessity Evidence

At base commit `1d7bfd75492669a0dd09e8ff762d1edbe694c508`,
`fs/rsext4/src/blockdev/cached_device.rs` owns a four-entry private clock cache,
while `fs/ax-fs-ng/src/block.rs` exposes direct runtime-backed devices. There is
no device-wide cache identity or invalidation operation. A concrete failure
sequence is:

1. one filesystem wrapper reads block 0 and retains its old bytes;
2. another request performs a 16-block device-direct write;
3. the device commits the first eight blocks and reports an error for the
   aggregate request;
4. a later one-block read returns the old cached block 0 unless the failed
   request invalidates its whole overlap.

`failed_direct_write_invalidates_partially_updated_folios` reproduces this with
a deterministic device mock. Before invalidation it returns zero-filled cache
bytes even though storage contains the new pattern. The related runtime test
holds a second submitted window incomplete and proves that returning after the
first failed window would let a caller repopulate the invalidated range before
all hardware writes become terminal.

Keeping the prior design leaves this stale-read class unresolved whenever
buffered metadata-shaped traffic and multi-folio direct traffic share a
device. Adding independent caches to each filesystem would preserve the same
cross-cache race and duplicate the required writeback/error state machine.

## Non-goals

- Replacing the file page cache or implementing Linux writeback scheduling.
- Adding asynchronous `FsBlockDevice` methods or per-folio wait queues.
- Changing the on-disk ext4/JBD2 format or commit sequence.
- Guaranteeing durability from `sync_file_range(2)`, which Linux also defines
  as a range writeback interface rather than a complete storage barrier.
- Correcting StarryOS's pre-existing root-only filesystem-metadata scope for
  `sync(2)`. This PR adds a device-wide block-cache stage and fixes the return
  ABI; iterating every mounted filesystem requires a separate mount-registry
  change and multi-mount persistence test.
- Hiding an invalid cache geometry or resource failure by silently falling
  back to an uncached device, except for the existing `InvalidRequest`
  compatibility boundary.

## Reference Model

The design is compared against Linux v7.1 commit
[`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`](https://github.com/torvalds/linux/commit/8cd9520d35a6c38db6567e97dd93b1f11f185dc6):

- [`fs/buffer.c`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/buffer.c)
  supplies the `buffer_head` uptodate/dirty and synchronous writeback model;
- [`block/bdev.c`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/block/bdev.c)
  supplies the one-address-space-per-block-device ownership model;
- [`fs/sync.c`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/sync.c#L97-L113)
  supplies the best-effort global `sync(2)` rule, including its unconditional
  successful syscall return;
- [`block/fops.c`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/block/fops.c)
  supplies the aggregate direct-I/O rule that submitted work reaches terminal
  completion before the synchronous caller returns.

TGOSKits borrows these ownership and coherence rules, not Linux's background
flusher implementation. The current block boundary is synchronous and has no
observable writeback state, so each device uses one sleepable tree lock and
per-operation synchronous writeback.

Linux `sync(2)` iterates every superblock before syncing block devices. StarryOS
currently syncs only its root filesystem metadata before the new device-wide
cache stage. This document does not claim parity for that pre-existing mount
scope limitation; it uses Linux here for block-cache ownership, direct-I/O
completion, ordering, and the syscall's unconditional return value.

## Internal Prior Art and Overlap

The base and history checks for this design cover:

- the private `rsext4` clock cache in
  `fs/rsext4/src/blockdev/cached_device.rs` and its journal adapter;
- the existing file page cache and reclaim boundary under
  `fs/ax-fs-ng/src/file/cache`;
- the owned block runtime lifecycle under
  `fs/ax-fs-ng/src/block/runtime`, stabilized by commit `f96452ce89`;
- reclaim lock-order work from commit `2da2bb4d19` and PR
  [#2170](https://github.com/rcore-os/tgoskits/pull/2170);
- prior `rsext4` cache changes `d974c5ae817cb5b4f810d97cd248d676d7cd6aea`
  and `631f0449e4cc80557e4abec75eae64b4be6e03bb`;
- the originally introduced private cache and replay invalidation in PR
  [#971](https://github.com/rcore-os/tgoskits/pull/971).

No base symbol provides `sync_all_block_caches`, a shared physical-LBA cache,
or a cross-wrapper direct-I/O coherence boundary. The selected design replaces
the private device cache rather than stacking another cache below it. The open
PR relationships with #2015 and #1957 are classified under Migration and
Rollback because they overlap implementation and ownership but do not already
provide this exact shared boundary.

## Alternatives

### Keep the existing private `rsext4` cache

This is the smallest change, but FAT and other consumers remain uncached and
two filesystem instances over one device retain independent views. It also
cannot provide one device-wide sync or reclaim boundary. This option is
rejected because it does not meet the shared-view success criterion.

### Add a cache to every filesystem adapter

Per-filesystem caches preserve local ownership, but duplicate geometry,
writeback, error, and eviction logic. Partitions sharing a runtime device would
still require a second cross-cache invalidation protocol. This option is
rejected because it moves the same coherence problem to a more complex layer.

### Share a cache at the runtime block-device boundary

This is the selected option. The runtime handle supplies a stable device
identity, filesystem adapters already cross `FsBlockDevice`, and one tree can
coordinate buffered and direct requests before they reach hardware. The cost
is a new shared lifecycle and lock boundary, which is kept inside `ax-fs-ng`
and documented below.

### Cache all requests

Making large file-data I/O populate the block cache would duplicate the file
page cache and increase memory traffic. The selected split buffers requests
contained within one folio and leaves multi-folio requests direct. This is a
request-shape approximation of Linux's metadata/data split and can be replaced
later without changing the registry or `FsBlockDevice` API.

## Layering and Dependency Direction

- `rdif-block` and hardware drivers remain unaware of filesystem caches.
- `ax-fs-ng::block::runtime` owns request planning, DMA, submission windows,
  terminal completion, barriers, and device handles.
- `ax-fs-ng::block::cache` wraps `FsBlockDevice`, owns folios and the per-device
  registry, and depends only on the synchronous block capability.
- ext4 and FAT adapters consume the wrapper but do not reach into cache state.
- `rsext4` keeps journal and metadata ownership; it hands block traffic to the
  wrapper and never depends on `ax-fs-ng` cache types.
- StarryOS calls the exported device-wide sync operation after its page-cache
  and filesystem stages; cache types do not depend on StarryOS.

No new crate or reverse dependency is introduced.

## Ownership and Lifetime

The registry is keyed by the allocation identity of a live
`BlockDeviceHandle`. A live `BlockCacheShared` owns:

- a weakly registered identity entry;
- one bounded `BlockAddressSpace`;
- one equivalent endpoint for global writeback;
- an atomic count of filesystem wrappers, excluding temporary global-sync
  references.

Wrappers share the tree but retain their own region mapping. Partition offsets
are applied before cache lookup, so the key space is physical LBA, not
partition-relative LBA. The last wrapper performs writeback while the tree is
still upgradeable, then releases the endpoint. Registry entries are weak;
creation and global sync prune stale entries. Drop and allocator reclaim use
nonblocking registry/tree acquisition so they do not introduce a destructor or
memory-pressure lock inversion.

The lock order is registry before tree when both are required. Device I/O is
never issued while holding the registry lock. One sleepable tree lock
serializes cached state with direct requests across wrappers of the same
device. The endpoint lock is used only by global writeback.

## Cache State and Resource Bound

One folio is 4 KiB or one device block, whichever is larger. Each device tree
contains at most 1024 folios: 4 MiB for 512-byte or 4-KiB devices. Each folio
has per-block uptodate and dirty state, while an ordered frame index records
which folios need writeback. The fixed-capacity LRU allocates a replacement
before evicting an existing entry so allocation failure preserves prior state.

Clean folios can be reclaimed without I/O. A dirty eviction first writes its
dirty runs; a failed write leaves the folio dirty and retryable. Global sync
reserves a snapshot of live trees before releasing the registry lock. Reserve
failure is `BlockError::NoMemory`, not an implicit partial snapshot.

## I/O and Coherence State Machine

### Buffered requests

A request contained within one folio reads only missing slots. Writes update
folio bytes, set per-slot dirty state, and defer device I/O until writeback,
eviction, explicit flush, global sync, or last-consumer teardown.

### Direct reads

Before a multi-folio read, overlapping dirty slots are written to the device.
The direct read then runs against current device contents and overlays only
clean cached slots; newer dirty slots remain authoritative if the policy is
extended to permit them.

### Direct writes

Before a multi-folio write, overlapping dirty slots are written back. A fully
successful direct write overlays its bytes onto every already-cached folio. If
the device reports an error, its successfully written prefix is unknown, so
the cache discards every overlapping folio. The next buffered read must fetch
the device's observable contents again.

The synchronous runtime must drain every submission window already handed to
hardware before returning the first write error. It stops submitting new
windows after the first error. This prevents a caller from invalidating and
then repopulating a folio while an earlier direct request can still modify the
same device range.

## Journal and Durability Ordering

`rsext4` retains ownership of transaction state. Once a held metadata buffer
is queued, ordinary dirty writeback cannot write its home block before the
commit record. Re-editing an already queued held buffer refreshes the pending
journal snapshot before switching buffers, flushing, or committing.

Cache `flush()` performs dirty writeback before the device barrier. Therefore
the existing sequence remains descriptor/data write, barrier, commit-record
write, barrier. A writeback failure preserves dirty ownership and prevents the
later barrier or commit stage from being reported as successful.

StarryOS `sync(2)` attempts page-cache, root-filesystem, and device-cache
stages even after an earlier error. Matching Linux, it logs writeback failures
but returns zero. The log is the only guaranteed observation of an error from
that `sync(2)` invocation; there is no persistent writeback-error record that a
later syscall must reproduce. Descriptor-specific interfaces such as
`fsync(2)` and `syncfs(2)` retain their own typed error paths for the work they
perform.

## Error and Recovery Rules

- Invalid geometry is `InvalidRequest` and may use the existing uncached
  compatibility path.
- Registry collision, runtime state, allocation, device, and writeback errors
  propagate as typed `BlockError` values.
- Failed buffered writeback retains dirty data for retry.
- Failed direct write invalidates its entire overlapping cache range.
- Global sync and filesystem shutdown attempt later devices after an earlier
  failure, retaining the first internal error for logging or typed callers.
- Drop cannot return an error; it logs a failed final writeback.

## Migration and Rollback

The cache is introduced behind the existing ext4/FAT `FsBlockDevice` factory;
filesystem public APIs and disk formats do not change. Migration replaces the
private `rsext4` device cache only after the journal held-buffer tests pass.
Rollback removes the wrapper and restores direct block devices plus the prior
private cache; no on-disk data migration is required.

Open PR [#2015](https://github.com/rcore-os/tgoskits/pull/2015) overlaps
`rsext4` journal and data-cache performance work and is complementary only
after its writeback ownership is rebased onto this boundary. Open PR
[#1957](https://github.com/rcore-os/tgoskits/pull/1957) broadly overlaps
`ax-fs-ng` and `rsext4` cache/flush semantics and must be rebased or have its
equivalent pieces dropped according to merge order. Neither PR should be
merged by resolving textual conflicts without revalidating the state-machine
and durability tests named here.

## Validation Plan

Deterministic host tests must cover:

- repeated reads and partial-folio slot state;
- deferred writes, merged writeback, eviction, retry, and barrier ordering;
- shared wrappers and physically distinct partition offsets;
- direct-read/write coherence and a direct write that commits a prefix before
  returning an error;
- draining already submitted runtime windows before returning that error;
- fallible registry allocation, stale weak entries, last-consumer teardown,
  and nonblocking reclaim/drop paths;
- JBD2 queue, re-edit, switch, flush, and commit ownership transitions;
- `sync(2)` attempting every stage while returning Linux-compatible success.

The runnable `block-io-bench` validates deterministic contents for repeated
reads and truncate/rewrite generations observed through an already-open file
descriptor. QEMU validation covers the public StarryOS syscall and application
paths. CI must additionally pass formatting, targeted clippy configurations,
workspace host tests, and the StarryOS architecture matrix.

The author-side commands and required observations are:

```text
cargo test -p ax-fs-ng --features 'host-test ext4 vfs'
  -> all cache/runtime unit tests and root-selector integration tests pass
cargo test -p rsext4 --features host-test
  -> journal ownership and re-edit regressions pass
cargo test -p starry-kernel
  -> sync stage regression and host kernel tests pass
cargo xtask clippy --package ax-fs-ng
cargo xtask clippy --package rsext4
cargo xtask clippy --package starry-kernel
  -> every configured check passes with warnings denied
cargo xtask starry test qemu --arch x86_64 -c qemu/system/syscall-test-sync
  -> test-sync reaches STARRY_SYSTEM_TEST_PASSED and the grouped runner passes
cargo xtask starry app qemu -t block-io-bench --arch x86_64
  -> every initial/coherence generation verifies and BLOCK_BENCH_APP_PASSED appears
```

The helper-level sync regression injects errors into all three stages and
requires every closure to run while the result remains `Ok(0)`. The direct
QEMU syscall case verifies the public success path. Fault-injected device and
multi-mount durability remain separate infrastructure work because the current
QEMU test environment exposes neither a controllable block-error endpoint nor
a second independently recoverable mount.

## Review Boundaries

Before merge, reviewers for filesystem/cache ownership and StarryOS syscall
semantics must independently accept this design. Implementation review should
then verify the code against the success criteria, I/O state machine, lock
order, error rules, open-PR relationship, and validation plan above.
