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
- `spin::RwLock<T>` is not directly covered by `ax-kspin` today.
  - Some sites may be safely downgraded to a mutex.
  - Read-heavy shared structures need a real internal RwLock design or a
    separate migration decision.
- `spin::Once` and `spin::Lazy` should be handled separately from lockdep
  mutex/RwLock migration.

Do not mechanically replace every `spin::Mutex` with `SpinNoPreempt`. Each site
needs a context check:

- whether it can run in IRQ context;
- whether preemption must be disabled;
- whether it is already protected by an outer critical section;
- whether the crate is meant to stay OS-neutral;
- whether lockdep visibility is actually required.

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
