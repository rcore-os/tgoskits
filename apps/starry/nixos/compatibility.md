# StarryNixOS compatibility evidence

This ledger records current x86_64 evidence. It must not be generalized to
other architectures or to full NixOS compatibility.

## Baseline identity

| Item | Value |
| --- | --- |
| Host system | `x86_64-linux` |
| Nix implementation | Lix 2.95.2 |
| QEMU | 11.0.2 |
| e2fsprogs | 1.47.4 |
| NixOS input | Recorded by `flake.lock` and the generated image manifest |
| Starry target | `x86_64-unknown-none` |

## Compatibility findings and exceptions

No compatibility exception is assumed before the first unmasked boot. For each
finding, record the smallest trigger, pinned Linux/NixOS oracle, observed boot
phase, disposition, focused regression path, and revisit condition. Core
activation, PID-1, multi-user target, and marker failures cannot be masked.

| Trigger | Linux oracle | Observed phase | Disposition | Regression | Revisit condition |
| --- | --- | --- | --- | --- | --- |
| Interactive getty units | NixOS container profile defaults | Configuration scope; no console service is started | Scope exclusion: no interactive shell is part of FR-007 evidence | Declarative evaluation and ordered marker contract | Revisit only for a separately scoped interactive-console feature |
| D-Bus and nscd services | FR-013 excludes the D-Bus service stack; the marker uses direct files and systemd state | Configuration scope; neither daemon is needed by the declared marker | Scope exclusion, not a workaround for a measured boot failure | Declarative evaluation and ordered marker contract | Revisit when a separately scoped service requires either daemon |
| NixOS stage-2 process substitution opens `/dev/fd/63` | Linux `proc_pid_fd(5)`: `/dev/fd` resolves through `/proc/self/fd`, including pipe descriptors | Toplevel init, before activation; the initial run failed to open the descriptor path | Resolved in the pipe/procfd open boundary; no mask added | `test-suit/starryos/qemu/system/bugfix-dev-fd-symlinks/` changed from deterministic `ENOENT` to pass | Re-run when procfs descriptor lookup, symlink following, pipe endpoint lifetime, or file-status flag semantics change |
| NixOS activation mounts `ramfs` at `/run/keys` | Linux kernel documentation defines `ramfs` as a mountable RAM-backed filesystem; Linux commit `8934827db5403eae57d4537114a9ff88b0a8460f` registers filesystem name `ramfs` and reports `RAMFS_MAGIC` | Activation; `mount` reported `unknown filesystem type 'ramfs'`, and `activate` exited nonzero | Resolved with a distinct ramfs identity backed by the existing in-memory filesystem machinery; no mask added | `test-suit/starryos/qemu/system/bugfix-ramfs-mount/` changed from deterministic `ENODEV` to pass | Re-run when the mount/filesystem implementation or the pinned NixOS activation contract changes |
| systemd tests whether paths are mount roots and compares mount IDs | Linux `statx(2)` exposes `STATX_ATTR_MOUNT_ROOT` and `STATX_MNT_ID`; `name_to_handle_at(2)` returns the same mount identity | PID-1 API-filesystem setup after activation | Resolved in the VFS statx/name-to-handle boundary; no guessed constant or userspace mask | `test-suit/starryos/qemu/system/bugfix-statx-mount-root/` changed from red to pass | Re-run when mount namespaces, mount IDs, statx, or file-handle semantics change |
| systemd `mkdir_parents()` probes existing `/sys/fs` and `/sys/fs/cgroup` | Linux `mkdir(2)` returns `EEXIST` for an existing visible directory, including a mountpoint | PID-1 root-cgroup setup returned `EPERM` | Resolved by checking the visible mount tree before asking a static pseudo-filesystem backend to create the entry | Exact `/sys/fs` and `/sys/fs/cgroup` checks in `test-suit/starryos/qemu/system/cgroup-basic/` changed from 45 pass/2 fail to 47 pass/0 fail | Re-run when VFS create semantics or static pseudo-filesystem topology changes |
| systemd executes its pinned executor through `/proc/self/fd/N` | Linux procfd entries are magic links to held file descriptions and remain executable after the original pathname disappears | Service spawn returned `ENOENT` although the executor closure and ELF interpreter existed | Resolved by executing the fd-held VFS location via `AT_EMPTY_PATH` semantics | Deleted-path executor coverage in `test-suit/starryos/qemu/system/bugfix-dev-fd-symlinks/` changed from `ENOENT` to pass | Re-run when procfd, execve, memfd, or close-on-exec semantics change |
| glibc `pidfd_spawn` requests `clone3(CLONE_INTO_CGROUP)` | Linux clone3 places the child in the cgroup represented by the supplied cgroup2 directory fd before it runs | systemd executor initially inherited the parent cgroup and failed to attach to the service leaf | Resolved by binding the opened cgroup2 directory to a stable node and committing fork membership before task publication | `test-suit/starryos/qemu/system/cgroup-basic/` changed from 49 pass/1 fail to 50 pass/0 fail | Re-run when clone3, pidfd spawn, cgroup namespaces, or fork publication ordering changes |
| systemd-executor receives serialized invocation state over an inherited fd | systemd 260.2 clears `FD_CLOEXEC` on the serialization fd before `pidfd_spawn`; the executor deserializes it after exec | Service startup after `multi-user.target` is queued | Open finding: glibc `pidfd_spawn` still reports `EBADF`; no matcher relaxation or service mask is accepted | A focused regression must reproduce the non-CLOEXEC fd across `CLONE_VM|CLONE_VFORK|CLONE_PIDFD|CLONE_INTO_CGROUP` and exec before correction | Revisit after the focused red/green regression and the same real app command both pass |
| `systemd-journald` enables receive timestamps on its Unix datagram socket | Linux v6.18 `sock_set_timestamp`, `unix_dgram_sendmsg`, and `__sock_recv_timestamp`: `SO_TIMESTAMP` is per receiver, records wall time before queue insertion, conditionally emits `SOL_SOCKET/SCM_TIMESTAMP`, and fills a missing timestamp when enabling races with an already queued packet | Journald initialization after systemd queued `Multi-User System`; `setsockopt(SOL_SOCKET, SO_TIMESTAMP)` returned `ENOPROTOOPT` | Resolved in the Unix datagram/seqpacket option, queue-time metadata, and timeval cmsg boundaries; no protocol-wide fake support added | `test-suit/starryos/qemu/system/bugfix-socket-timestamp/` changed from 14 pass/11 fail to 30 pass/0 fail, matching the host Linux oracle; adjacent QoS cmsg and seqpacket regressions also pass | Re-run when Unix queueing, `recvmsg`, ancillary layout, timestamp options, or wall-clock plumbing changes |
| Journald reads the current hostname from proc sysctl | Linux procfs exposes `/proc/sys/kernel/hostname` as the UTS namespace hostname and permits newline-terminated reads | Journald continued past `SO_TIMESTAMP`, printed `Collecting audit messages is disabled`, then failed to open `/proc/sys/kernel/hostname` with `ENOENT` | Open measured behavior gap; no fallback hostname, service mask, or matcher relaxation is accepted | A focused regression must first reproduce the missing proc sysctl path and its Linux-visible read semantics | Revisit after a deterministic red/green regression and the same real app command both pass |

## Run evidence

### 2026-08-01 initial unmasked run

- Command: `nix develop -c cargo xtask starry app qemu -t nixos --arch x86_64`
- Flake lock SHA-256: `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4`
- System closure: `/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2`
- systemd: `260.2`
- Image SHA-256: `889eb200dccd74fa6e8e3f43d8fa9e37b586996b87dbef31acc93806aaf4bb55`
- Observed progress: ext4 root mounted, generated `/init` ran as PID 1,
  printed `<<< NixOS Stage 2 >>>`, and began activation. No ordered
  `STARRY_NIXOS_PHASE=` record was reached.
- First exact divergence: `/init: line 114: /dev/fd/63: Operation not permitted`.
- Terminal result: phase-classified failure during toplevel init, before
  activation. PID 1 later received `SIGSEGV` at user address `0x8`; the QEMU
  matcher rejected the run on its fatal pattern.

This is failure evidence, not a StarryNixOS success claim. The `/dev/fd`
regression must fail on the current kernel before any correction is accepted,
then the same command must be repeated to discover the next first divergence.

### 2026-08-01 focused baseline and `/dev/fd` correction

- CI-like environment: `ghcr.io/rcore-os/tgoskits-container:latest`, with the
  repository mounted at its identical absolute host path and `.ci-cache/{apt,cargo,rustup,target,tmp}`
  used only for caches.
- Baseline probe command: `cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2`.
- Probe result: `STARRY_NIXOS_BASELINE_PROBES_PASSED`; PID 1 was visible and
  `/proc`, `/sys`, `/dev`, `/run`, and a cgroup2 hierarchy were usable.
- Focused red result: `bugfix-dev-fd-symlinks` verified all four static links,
  then `open("/dev/fd/3", ...)` failed with `ENOENT`.
- Focused green result after the correction:
  `STARRY_DEV_FD_SYMLINKS_PASSED` and `STARRY_GROUPED_TESTS_PASSED`.

### 2026-08-01 final bounded app run

- Container payload command:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`.
- Artifact reuse was accepted only after ext4, flake-lock, closure, target, and
  image-hash validation against the adjacent manifest.
- Flake lock SHA-256: `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4`.
- System closure: `/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2`.
- systemd: `260.2`; image SHA-256:
  `889eb200dccd74fa6e8e3f43d8fa9e37b586996b87dbef31acc93806aaf4bb55`.
- Progress after the correction: stage 2 entered activation, printed
  `running activation script...`, completed earlier special-filesystem steps,
  and later printed `starting systemd...`.
- First exact new divergence: `mount: /run/keys: unknown filesystem type 'ramfs'`;
  the `specialfs` activation snippet failed with status 32. Systemd subsequently
  exited because its API-filesystem mount-point checks returned
  `Protocol driver not attached`.
- Terminal result: QEMU stopped without the ordered phase sequence or success
  marker. This is a precisely bounded compatibility finding, not a passing
  StarryNixOS baseline.

### 2026-08-01 `ramfs` focused red regression

- Linux semantic basis:
  [kernel ramfs documentation](https://www.kernel.org/doc/html/latest/filesystems/ramfs-rootfs-initramfs.html)
  and pinned Linux
  [`fs/ramfs/inode.c`](https://github.com/torvalds/linux/blob/8934827db5403eae57d4537114a9ff88b0a8460f/fs/ramfs/inode.c#L260-L329).
- Command: `cargo xtask starry test qemu --arch x86_64 -c qemu/system/bugfix-ramfs-mount`
  inside `ghcr.io/rcore-os/tgoskits-container:latest`, with `.ci-cache/` used
  only for disposable build/tool caches.
- Direct ABI trigger: `syscall(SYS_mount, "none", mountpoint, "ramfs", 0,
  NULL)`.
- Red result: `mount(2)` returned `-1` with `errno=ENODEV`; the test emitted
  `STARRY_RAMFS_MOUNT_FAILED`, the grouped runner emitted
  `STARRY_GROUPED_TEST_FAILED`, and xtask exited nonzero.
- Intended green checks: mount success, `statfs.f_type == RAMFS_MAGIC`, regular
  file write/read round trip, and successful `umount2`.

### 2026-08-01 compatibility closure through cgroup placement

- `bugfix-ramfs-mount` passed after adding a distinct Linux-visible ramfs
  identity while retaining the in-memory data implementation.
- `bugfix-statx-mount-root` passed after exposing mount-root attributes and a
  mount ID consistent with `name_to_handle_at`.
- Exact sysfs topology checks in `cgroup-basic` changed from 45 pass/2 fail to
  47 pass/0 fail after existing visible directories began returning `EEXIST`.
- Deleted-path procfd execution in `bugfix-dev-fd-symlinks` changed from
  deterministic `ENOENT` to pass after exec began using the held file
  location.
- `clone3(CLONE_INTO_CGROUP)` membership changed from 49 pass/1 fail to
  50 pass/0 fail after atomic target-cgroup placement was implemented.
- All focused QEMU runs used the project container with `.ci-cache/` only as
  disposable package, toolchain, target, and temporary-data caches.

### 2026-08-01 latest bounded app run

- Command payload:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in `ghcr.io/rcore-os/tgoskits-container:latest`, with the repository mounted
  at the same absolute path and the validated published image reused.
- Activation completed, systemd ran as PID 1, and the default
  `Multi-User System` job was queued.
- The previous root-cgroup `EPERM`, executor `ENOENT`, and service-leaf cgroup
  `ENOENT` messages were absent after their focused fixes.
- First remaining divergence: systemd reported
  `Failed to spawn executor: Bad file descriptor`, sent `SIGTERM` to the
  created executor children, and the affected service jobs eventually timed
  out with result `resources`.
- Terminal result: the strict `.service: Failed with result` matcher rejected
  the run. The ordered marker sequence was not emitted, so this remains
  bounded failure evidence rather than a Stage-2 success claim.
