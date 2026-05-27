# External `spin` migration plan

Date: 2026-05-21

This plan follows the audit in
[`reports/external-spin-audit.md`](external-spin-audit.md). The goal is not only
to remove a third-party dependency from the build graph, but also to make the
lock migration process reviewable for future maintainers.

## Background

The workspace currently uses the external `spin` crate in two ways:

- direct `spin::{Mutex,RwLock,Once,Lazy}` use from TGOSKits crates;
- indirect use through third-party crates, most notably `lazy_static` with the
  `spin_no_std` feature.

The original lockdep follow-up was triggered by a visibility gap: external
`spin::{Mutex,RwLock}` locks do not participate in `ax-kspin` lockdep tracking.
However, replacing all `spin` use with `ax-kspin` in one step is not realistic,
because `ax-kspin` currently provides mutex-like spin locks only. It does not
provide `RwLock`, `Once`, or `Lazy`.

Therefore the migration should be split into two separate tracks:

- supply-chain decoupling: bring the external `spin` implementation into the
  repository so builds no longer depend on fetching it from crates.io or a
  registry mirror;
- semantic migration: gradually replace lockdep-relevant `spin` locks with
  TGOSKits-native synchronization primitives.

## Decision

Keep a local copy of the newer external `spin` implementation as an internal
component, initially based on `spin 0.10.0`.

The local component should remain API-compatible with upstream `spin` at first.
This keeps the first change low risk: existing `spin::Mutex`, `spin::RwLock`,
`spin::Once`, and `spin::Lazy` users continue to compile while the dependency is
resolved from the repository.

The upstream name `spin::Mutex` is semantically misleading in this kernel
context. It is a busy-wait mutual-exclusion primitive and has no path to sleep
while waiting. Therefore existing `spin::Mutex` users should first be treated as
non-sleeping locks and migrated into the `ax-kspin` family. Replacing them with
`ax_sync::Mutex` is a separate semantic change and should only happen after a
site proves that a sleepable lock is the correct design.

This first stage does not make those locks visible to lockdep. It only makes the
codebase independent from the external crate source and gives the project a
controlled place for future compatibility and migration work.

## Phase 1: remove workspace `spin 0.9` users

The lockfile currently contains both `spin 0.9.8` and `spin 0.10.0`.

Workspace-owned `spin = "0.9"` declarations should be upgraded first:

```text
components/axfs_crates/axfs_ramfs/Cargo.toml
components/axfs_crates/axfs_devfs/Cargo.toml
components/axdriver_crates/axdriver_net/Cargo.toml
```

These crates use only APIs that are still present in `spin 0.10`:

```text
spin::Mutex
spin::RwLock
spin::once::Once
```

After this step, any remaining `spin 0.9.8` should come from third-party
dependencies rather than direct workspace declarations.

## Phase 2: vendor `spin 0.10`

Add a local component for `spin 0.10.0`, preserving license and upstream
attribution.

Expected location:

```text
components/spin
```

The package should still be named `spin`, so existing source imports do not need
to change in this phase.

Cargo resolution should then be redirected to the local component, for example
with a root-level patch:

```toml
[patch.crates-io]
spin = { path = "components/spin" }
```

This should remove external registry dependency for `spin 0.10.0` while keeping
behavior unchanged.

## Phase 3: analyze and contain `lazy_static`

`lazy_static 1.5.0` with `spin_no_std` hard-codes:

```toml
spin = { version = "0.9.8", features = ["once"], default-features = false }
```

Its no-std implementation uses only `spin::Once`. This is not a primary lockdep
blind spot because it is an initialization primitive rather than a normal
runtime `Mutex` or `RwLock`.

For that reason, `lazy_static -> spin 0.9.8` should be treated as a separate
follow-up, not as a blocker for vendoring `spin 0.10`.

Possible resolutions:

- replace workspace `lazy_static!` call sites with `spin::Lazy`,
  `ax_lazyinit::LazyInit`, or a future TGOSKits-native `Once/Lazy`;
- vendor or patch `lazy_static` so its no-std path uses the local `spin 0.10` or
  a project-local Once primitive;
- keep it temporarily and document the remaining `spin 0.9.8` lockfile entry as
  an initialization-only residual dependency.

## Phase 4: semantic migration to TGOSKits primitives

Once `spin` is local and controlled, replace lockdep-relevant uses in priority
order.

Priority:

1. `components/axfs-ng-vfs`
   - This directly addresses the FAT32/VFS lockdep visibility gap.
2. `os/arceos/modules/axfs-ng`
   - Adjacent to VFS/FAT/ext4 paths and already mixes `ax_kspin` with external
     `spin`.
3. `os/StarryOS/kernel`
   - User-visible runtime locks that matter for Starry lockdep/debug runs.
4. `os/arceos/modules/axnet-ng`, `os/arceos/api/arceos_posix_api`, and
   `os/axvisor`
   - Runtime locks that may matter for broader lockdep coverage.
5. Drivers and portable components
   - Migrate only after checking dependency boundaries. Some driver crates may
     need a small synchronization abstraction instead of directly depending on
     ArceOS-specific `ax-kspin`.

Replacement rules:

- `spin::Mutex<T>` can usually become one of:
  - `ax_kspin::SpinNoPreempt<T>`;
  - `ax_kspin::SpinNoIrq<T>`;
  - `ax_kspin::SpinRaw<T>`.
- Do not read the upstream `Mutex` name as equivalent to `ax_sync::Mutex`.
  `spin::Mutex` is non-sleeping and busy-waits, so the migration default is an
  `ax-kspin` primitive. Moving a site to `ax_sync::Mutex` is a later design
  decision, not a mechanical replacement.
- `spin::RwLock<T>` is not directly covered by `ax-kspin` today.
  - Some sites may be safely downgraded to a mutex.
  - Read-heavy shared structures need a real internal RwLock design or a
    separate migration decision.
- `spin::Once` and `spin::Lazy` should be handled separately from lockdep
  mutex/RwLock migration.

Do not mechanically replace every `spin::Mutex` with `SpinNoPreempt`. Each site
needs a context check:

- whether it can run in IRQ context;
- whether lock acquisition itself happens with local IRQs enabled;
- whether the critical section can sleep, reschedule, fault on user memory, or
  call into a backend callback that can do so;
- whether preemption must be disabled;
- whether it is already protected by an outer critical section;
- whether the crate is meant to stay OS-neutral;
- whether lockdep visibility is actually required.

Follow-up `SpinNoPreempt` audit after the first VFS/axfs-ng migrations:

- `components/axfs-ng-vfs` initially aliased its internal VFS locks to
  `SpinNoPreempt`. That exposed a real Starry tmpfs panic when
  `Location::mount()` held a VFS mountpoint lock and called the filesystem
  backend's `root_dir()`. The immediate migration correction is to keep VFS
  metadata locks in the `ax-kspin` family but use `SpinNoIrq`. The backend
  callback issue should remain visible to `might_sleep`/lockdep and be handled
  as a separate lock-scope follow-up, not as part of the spin replacement step.
- `os/arceos/modules/axfs-ng` FAT and ext4 filesystem locks also use
  `SpinNoPreempt`. They protect large filesystem states and often wrap block
  I/O, sync, and flush paths. They are not good candidates for a mechanical
  `SpinNoIrq` replacement; they may need sleepable, lockdep-visible locking or
  smaller critical sections.
- Starry `epoll`, `pty`, and terminal metadata use short `SpinNoPreempt`
  critical sections. They are likely candidates for `SpinNoIrq` if the call
  sites are IRQ-enabled, but should still be reviewed for wakeup and tty lock
  ordering.
- Starry loop-device cache locking is tied to the ext4 block-device path and
  should be considered together with the axfs-ng ext4 lock strategy.

## Phase 4 status: production `spin::Mutex` migration

As of commit `44af7d3a1`, direct production uses of `spin::Mutex` and
`spin::MutexGuard` have been migrated away from workspace code, excluding the
vendored `components/spin` implementation itself.

The verification command:

```text
rg -n "^use spin::Mutex|^use spin::\{[^}]*Mutex|spin::Mutex|spin::MutexGuard|spin::mutex::" \
  --glob '*.rs' --glob '!components/spin/**'
```

now reports only:

```text
components/kspin/src/base.rs
```

This is a documentation reference to the original upstream implementation, not
a runtime lock site.

The production migration covered these groups:

- filesystem and VFS paths: `axfs-ng-vfs`, `axfs-ng`, `ax-fs`;
- networking and Starry runtime paths: `ax-net-ng`, Starry timer/netlink/usbfs
  and camera locks;
- virtualization and platform components: `axvisor`, `axvm`, `riscv_vplic`,
  `loongarch_vcpu`, `arm_vgic`, `axplat-dyn`;
- portable driver components: `rdrive`, `arm-scmi-rs`, `ramdisk`,
  `nvme-driver`, `rdif-serial`, `realtek-rtl8125`, `sg2002-tpu`, `crab-usb`;
- shared support crates: `dma-api`, `ax-driver-net`, `axdevice`.

The repository still keeps local `spin` for intentionally separate work:

- `spin::Once` and `spin::Lazy` initialization primitives;
- postponed `spin::RwLock` users;
- documentation references.

To make the completed `spin::Mutex` migration visible to Cargo, the local
`components/spin` default feature set no longer enables `mutex`, `spin_mutex`,
or `barrier`. The mutex implementation remains in the vendored component behind
explicit opt-in features, but default workspace users should not be able to name
`spin::Mutex` by accident. Any remaining default-feature consumer that still
uses `spin::Mutex`, `spin::MutexGuard`, or `spin::mutex::*` should now fail at
compile time.

Any lock-scope bugs exposed by `might_sleep` or lockdep after this migration
should be treated as useful follow-up findings. They are not a reason to hide
the original non-sleeping `spin::Mutex` semantics behind a sleepable
`ax_sync::Mutex` replacement.

## Validation strategy

For documentation-only planning changes, no build is required.

For future implementation changes:

- run `cargo fmt`;
- for each modified crate, run targeted clippy, preferably:

```text
cargo xtask clippy --package <crate>
```

- for `ax-kspin` or lockdep changes, also run the relevant lockdep tests when
  practical;
- for VFS/FAT changes, add or run a targeted FAT32/VFS case after the lock type
  migration, because improved lockdep visibility may expose the suspected ABBA
  ordering.

If `cargo tree` is needed, prefer using it only after dependency resolution is
known to work locally. During the audit, normal and offline `cargo tree` were
blocked by registry/index state around `sg200x-bsp = 0.6.0`, while direct
`Cargo.lock` parsing remained reliable.

## Expected milestones

1. Direct workspace `spin 0.9` declarations are upgraded to `spin 0.10`.
2. `components/spin` is added and `spin 0.10` resolves locally.
3. Remaining `spin 0.9` entries, if any, are attributed to `lazy_static` or other
   third-party dependencies.
4. `components/axfs-ng-vfs` no longer uses external `spin::{Mutex,RwLock}` for
   lockdep-relevant internal locks.
5. `os/arceos/modules/axfs-ng` moves lockdep-relevant runtime locks away from
   external `spin`; `FS_REGISTRY` is the first small step, while
   `CachedFile::append_lock` remains a separate `RwLock` design question.
6. Lockdep-enabled FAT32/VFS testing either confirms no report or exposes a real
   ordering issue for a separate ordering fix.
7. Production direct `spin::Mutex` and `spin::MutexGuard` uses are gone outside
   the vendored `components/spin` crate. The remaining direct match is limited
   to an `ax-kspin` documentation reference.
8. The vendored `components/spin` default features no longer expose `Mutex`.
   This turns accidental new default-feature `spin::Mutex` users into compile
   errors while preserving explicit compatibility features for the local copy.
