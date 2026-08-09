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
| systemd reopens an `O_PATH` configuration fd through `/proc/self/fd/N` after its pathname becomes unresolvable | Linux procfd entries are magic links to the referenced inode: reopening an unlinked regular file through a held `O_PATH` fd succeeds unless the new open uses `O_NOFOLLOW` | PID 1 opened the pinned `system.conf` location successfully, but reopening `/proc/self/fd/4` returned `ENOENT` and stopped manager configuration loading | Resolved by reopening the filesystem `File` from its held `Location` with the requested flags; pipe handling and `O_NOFOLLOW` symlink behavior remain separate | `test-suit/starryos/qemu/system/bugfix-procfd-reopen-unlinked-file/` changed from deterministic `ENOENT` to pass. The 2026-08-06 rootless Podman rerun completed generators, queued `Multi-User System`, and started journald before the later `systemd-sysctl` failure | Re-run when procfd magic-link, `O_PATH`, unlink, mount-tree, or open-file-description semantics change |
| glibc `pidfd_spawn` requests `clone3(CLONE_INTO_CGROUP)` | Linux clone3 places the child in the cgroup represented by the supplied cgroup2 directory fd before it runs | systemd executor initially inherited the parent cgroup and failed to attach to the service leaf | Resolved by binding the opened cgroup2 directory to a stable node and committing fork membership before task publication | `test-suit/starryos/qemu/system/cgroup-basic/` changed from 49 pass/1 fail to 50 pass/0 fail | Re-run when clone3, pidfd spawn, cgroup namespaces, or fork publication ordering changes |
| systemd-executor receives serialized invocation state over an inherited fd | systemd 260.2 clears `FD_CLOEXEC` on the serialization fd before `pidfd_spawn`; the executor deserializes it after exec | Service startup after `multi-user.target` is queued | Open finding: glibc `pidfd_spawn` still reports `EBADF`; no matcher relaxation or service mask is accepted | A focused regression must reproduce the non-CLOEXEC fd across `CLONE_VM|CLONE_VFORK|CLONE_PIDFD|CLONE_INTO_CGROUP` and exec before correction | Revisit after the focused red/green regression and the same real app command both pass |
| `systemd-journald` enables receive timestamps on its Unix datagram socket | Linux v6.18 `sock_set_timestamp`, `unix_dgram_sendmsg`, and `__sock_recv_timestamp`: `SO_TIMESTAMP` is per receiver, records wall time before queue insertion, conditionally emits `SOL_SOCKET/SCM_TIMESTAMP`, and fills a missing timestamp when enabling races with an already queued packet | Journald initialization after systemd queued `Multi-User System`; `setsockopt(SOL_SOCKET, SO_TIMESTAMP)` returned `ENOPROTOOPT` | Resolved in the Unix datagram/seqpacket option, queue-time metadata, and timeval cmsg boundaries; no protocol-wide fake support added | `test-suit/starryos/qemu/system/bugfix-socket-timestamp/` changed from 14 pass/11 fail to 30 pass/0 fail, matching the host Linux oracle; adjacent QoS cmsg and seqpacket regressions also pass | Re-run when Unix queueing, `recvmsg`, ancillary layout, timestamp options, or wall-clock plumbing changes |
| Journald reads the current hostname from proc sysctl | Linux v6.18 `kernel/utsname_sysctl.c` exposes `/proc/sys/kernel/hostname` from the caller's current UTS namespace and emits a newline-terminated value | Journald continued past `SO_TIMESTAMP`, printed `Collecting audit messages is disabled`, then failed to open `/proc/sys/kernel/hostname` with `ENOENT` | Resolved with a dynamic read-only proc sysctl node backed by `nsproxy.uts_ns.nodename`; no duplicate hostname state or userspace fallback added | `test-suit/starryos/qemu/system/bugfix-proc-sys-kernel-hostname/` changed from deterministic `ENOENT` after 3 checks to 12/12 pass; the existing namespace regression also passed 13/13 | Re-run when procfs sysctl reads, UTS namespace cloning, `sethostname`, or `uname` semantics change |
| systemd passes a pathname Unix stream listener to journald | Linux `socket(7)` defines `SO_ACCEPTCONN` as a read-only integer that is zero before `listen(2)` and one afterwards; systemd 260.2 uses it while identifying inherited Varlink listeners | After the hostname correction, journald reported `1 unknown file descriptors passed, closing.` before `Collecting audit messages is disabled` | Resolved by exposing the owning transport's listener state through `SocketOps`, separating Unix bind from listen, and returning that state for `SO_ACCEPTCONN`; no syscall-layer shadow state added | `test-suit/starryos/qemu/system/bugfix-unix-listener-introspection/` changed from 12/17 to 17/17, matching the host Linux oracle; adjacent accept4 and seqpacket regressions also pass | Re-run when socket listener state, Unix namespace bind slots, accept/connect, or `SO_ACCEPTCONN` handling changes |
| Journald polls Starry's write-only `/dev/kmsg` fd | Linux v6.12 `kernel/printk/printk.c::devkmsg_poll()` reports `EPOLLIN | EPOLLRDNORM` only when a log record is available and does not report unconditional write readiness | Starry's `/dev/kmsg` read side returns EOF, but the generic device fallback advertised `POLLIN | POLLOUT`; journald therefore observed a readiness event it could not consume | Resolved by making `Kmsg` explicitly pollable with empty readiness until read-history support exists; no synthetic record, wakeup, or journald special case was added | The kernel axtest changed from `AXTEST_SUMMARY pass=397 fail=1` to `pass=398 fail=0`; the 4-vCPU NixOS rerun registered fd 8 but never delivered it, completed journal flush, and moved the strict failure to `systemd-udevd.service` | Re-run when `/dev/kmsg` gains read-history support, log-record wakeups, or its poll contract changes |
| `epoll_wait(2)` rotates more ready level-triggered fds than fit in `maxevents` | [`epoll_wait(2)`](https://man7.org/linux/man-pages/man2/epoll_wait.2.html) requires successive calls to round-robin through more ready fds than fit in the output buffer; Linux v6.12 `fs/eventpoll.c::ep_send_events()` moves the current ready list aside and appends still-ready reported entries after entries that were not reported | Journald repeatedly received the same persistent ready descriptors while its Varlink listener remained behind them; `journalctl --flush` connected and sent its 47-byte request, but the listener was accepted only after PID 1 timed out and terminated the client | Focused kernel correction in the epoll ready-list requeue order: preserve unreported ready entries before level-triggered entries already returned by the current call; no journald or AF_UNIX special case | `test-suit/starryos/qemu/system/bugfix-epoll-lt-fairness/` passes on Linux after one distractor event, changed from eight consecutive distractor events on Starry to pass after one, and the 4-vCPU NixOS rerun completed journal flush, udevd, journal catalog, and UTMP before the later `nix-channel-init.service` failure | Re-run when epoll ready-list ordering, LT requeue, `maxevents`, or partial user-copy handling changes |
| An epoll interest becomes ready between consuming a stale wakeup and registering its replacement waker | Linux poll wait paths register interest before their readiness recheck so a transition in that window cannot be lost; the recheck must use only the subscribed mask rather than unrelated persistent readiness | The NixOS udevd path repeatedly added, modified, waited on, and deleted epoll interests; a readiness transition during rearm could otherwise remain invisible until an unrelated wakeup | Register the replacement waker first, then recheck only events matching that interest and enqueue through the normal interest waker; persistent `EPOLLOUT` cannot create phantom `EPOLLIN` | The deterministic kernel axtest changed from `AXTEST_SUMMARY pass=396 fail=1` to `pass=398 fail=0`; the same 4-vCPU NixOS rerun reached `Started Rule-based Manager for Device Events and Files.` and continued through UTMP | Re-run when `Pollable::register`, epoll spurious-wakeup handling, interest masks, or consume-to-register ordering changes |
| `systemd-udevd.service` creates mount/UTS namespace sandbox state | systemd 260.2 maps status 226 to `EXIT_NAMESPACE`; its upstream unit declares `PrivateMounts=yes` and `ProtectHostname=yes` | After journal flush and static `/dev` nodes, udevd exited with `226 << 8` | Initial no-sandbox profile exception: only this unit overrides `PrivateMounts=false` and `ProtectHostname=false`; no global systemd sandbox setting is changed | Generated drop-in inspection and the later real QEMU run reached `Started Rule-based Manager for Device Events and Files.` | Restore each upstream directive after its exact Starry mount/UTS sandbox operation has a Linux oracle and a focused red/green regression |
| `systemd-machine-id-commit.service` persists the boot machine ID | systemd 260.2 documents this unit as committing a transient machine ID to persistent storage; NixOS container instances may use a transient identity | On the immutable generated artifact, `systemd-machine-id-setup` exited 1 while saving the transient ID | Initial Stage-2 profile exception: precisely mask `systemd-machine-id-commit.service`; transient identity remains available and persistent per-instance identity remains out of scope | Generated unit is `/dev/null`; subsequent QEMU run no longer reported this unit failure | Revisit when the image has a supported per-instance writable identity-store design; do not replace the exception with a shared baked-in machine ID |
| `/proc/<pid>/exe` after `execve` resolves a program opened through a bind mount | [`proc_pid_exe(5)`](https://man7.org/linux/man-pages/man5/proc_pid_exe.5.html) exposes the pathname of the executed command. The path is rooted at the mount through which the executable was resolved, not its bind source's parent chain. | NixOS stage 2 executed PID 1 through a bind-mounted `/nix/store` path. The prior VFS walk recorded `/nix/store/nix/store/...`; `readlink -f /proc/1/exe` then exited `256`. | Resolved in `Location::absolute_path()`: collect names only up to each mount root, then resume from that mount's attachment location. No procfs special case or NixOS workaround was added. | `absolute_path_rebases_bind_mount_source_at_mountpoint` changed from `/nix/store/nix/store/systemd` to `/nix/store/systemd` and passed in `axfs-ng-vfs`. `test-suit/starryos/qemu/system/bugfix-proc-pid-exe-readlink/` compiles and covers direct procfs readlink, canonicalization, and exec through a bind-mounted pathname; its grouped QEMU execution remains pending because the read-only source mount cannot satisfy Axbuild's create/delete Alpine-rootfs lock protocol. The 8-vCPU app run crossed stage-2 `readlink -f /proc/1/exe` with exit code 0 and reached the marker service. | Re-run when `Location::absolute_path`, bind/move mount attachment, execve path capture, or procfs executable-link behavior changes. |
| `register-nix-paths.service` and `systemd-tmpfiles-setup.service` after local-fs startup | Nix registration loads the actual 144,084-byte `/nix-path-registration` then updates the system profile; the exact same Nix 2.34.8 commands in an isolated Linux state completed in 0.30 seconds and 0.10 seconds respectively. Its trace uses only SQLite `F_SETLK`/`F_GETLK` record locks, not `kcmp`, `keyctl`, or `fcntl(1027)`. The artifact's full systemd 260.2 tmpfiles rules, passwd/group data, and `--create --remove --boot --exclude-prefix=/dev` completed with status 0 in a root-capable Linux `--root` baseline (0.00 seconds at 0.01-second display precision) | The run with the two exceptions crossed sysctl, journal flush, static `/dev`, and udevd, then left `Register Nix Store Paths` and `Create System Files and Directories` active until the 600-second outer timeout | Open diagnostic finding; task 99 is register-nix-paths and task 100 is systemd-tmpfiles. The offline Linux baseline excludes inherent tmpfiles rule/account-data slowness, but does not recreate the guest's live `/run`/`/proc` state. Read-only file-time diagnostics and PID-1 `kcmp`/`fcntl(1027)` remain uncorrelated with the two actual execs | No focused regression yet; strict matcher did not emit the pass marker | Reduce one task 99/100 VFS operation to a Linux/Starry deterministic red/green regression, then repair only its owning subsystem |
| `systemd-journald` appends an entry and obtains the current boot ID | systemd 260.2 [`sd_id128_get_boot()`](https://github.com/systemd/systemd/blob/v260.2/src/libsystemd/sd-id128/sd-id128.c#L169-L193) reads `/proc/sys/kernel/random/boot_id` as a non-null canonical UUID; [`journal_file_append_entry()`](https://github.com/systemd/systemd/blob/v260.2/src/libsystemd/sd-journal/journal-file.c#L2527-L2573) returns that read error before appending. The same direct `open`/`fstat`/`read`/`lseek` C probe passed 9/9 in the project Linux container. | The journal file open returned its expected initial `ENOENT`, then the boot-ID read returned `ENOENT`; journald reported its generic journal-write error immediately afterwards. | Resolved in procfs with one immutable per-kernel-boot UUIDv4 value, exposed as read-only `/proc/sys/kernel/random/boot_id`; no machine-ID derivation, journal mask, or service override was added. | `test-suit/starryos/qemu/system/bugfix-proc-sys-kernel-random-boot-id/` changed from `open` `ENOENT` to 9/9 pass, including `0444`, exact 37-byte format, EOF, seek/re-read, and two-reader stability. The 2026-08-05 NixOS rerun then reached `Started Journal Service.` and received the flush request without another boot-ID error. | Re-run when procfs initialization, wall-clock entropy, pseudo-file permissions, or boot-identity lifecycle changes. |
| systemd observes mount-table changes through `/proc/<pid>/{mountinfo,mounts}` polling | Linux 6.6 `fs/proc_namespace.c::mounts_poll()` stores one observed mount-namespace event value per open file description and reports `POLLPRI | POLLERR` when `fs/namespace.c::touch_mnt_namespace()` advances that namespace's event counter and wakes poll waiters | The util-linux mount helper completed the `move_mount(2)` operation for `/run/wrappers`, but systemd PID 1 did not receive a mountinfo change event, did not observe the published mount, and reported the mount unit protocol failure | Resolved with namespace-scoped mount-table generations, per-open consumed change events, and notifications after successful mount, bind, move, remount, propagation, `mount_setattr`, unmount, and `pivot_root` publication; no systemd workaround or host-side mount was added | The Linux oracle printed `STARRY_MOUNTINFO_POLL_NOTIFY_PASSED`. The focused Starry regression changed from `ready=0 revents=0` to `POLLPRI | POLLERR`, verified that a repeated poll consumes the event, and passed through the grouped QEMU runner. The real NixOS rerun printed `[  OK  ] Mounted /run/wrappers.`, reached `Local File Systems`, and started `Register Nix Store Paths` without the former mount-unit failure. | Re-run when mount-namespace cloning, proc mount-table generation, poll wakeup, or any mount-tree mutation path changes. |

## Run evidence

### 2026-08-07 8-vCPU Stage-2 acceptance pass

- The prepared host environment ran the acceptance payload:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`.
  The builder reused the existing app-owned rootfs after manifest and ext4
  validation; it did not rebuild or switch the host NixOS system.
- QEMU used `-smp 8`. The guest completed activation, journal flush, both
  `/dev` tmpfiles phases, udevd, Nix store registration, SUID wrapper creation,
  resolvconf, and the extra networking commands. The ACL boundary remained
  green: `setfacl` and `resolvconf-start` both exited with code 0.
- The guest reached `Multi-User System` and emitted the complete ordered
  contract: `STARRY_NIXOS_PHASE=pid1`,
  `STARRY_NIXOS_PHASE=activation`, `STARRY_NIXOS_PHASE=systemd`,
  `STARRY_NIXOS_PHASE=marker`, and `STARRY_NIXOS_SYSTEM_PASSED`.
- The marker then requested `systemctl --force --force poweroff`; QEMU returned
  successfully after the guest filesystem sync, with no manual interruption.

### 2026-08-06 bind-mounted executable-path acceptance rerun

- The independent VFS regression
  `cargo test -p axfs-ng-vfs absolute_path_rebases_bind_mount_source_at_mountpoint --lib`
  passed in the rootless Podman validation container. It directly checks that a
  bind of `/nix/store` onto itself reports `/nix/store/systemd`, rather than the
  former `/nix/store/nix/store/systemd`.
- The full CI-like command reused the validated app-owned artifact without
  building a rootfs or invoking host Nix:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`.
  QEMU used the configured `-smp 8`; the manifest identified
  `/nix/store/xzfda0azx2fh54hl61gfdbs2rkc1cz35-nixos-system-starrynixos-starry-nixos-stage2`
  and image SHA-256
  `fb1da6aa0cc59c3e6977450c975ba823396943552dcd2f9e4fec711c3801c4fc`.
- Stage 2 completed the former blocker: its `readlink` process exited 0 before
  systemd 260.2 started. The run then completed tmpfiles, udevd, Nix store path
  registration, SUID wrapper creation, and reached
  `starry-nixos-marker.service`.
- The strict terminal result was now
  `hello: command not found` in that marker service. `pkgs.hello` is part of
  `environment.systemPackages`, but not of this service's explicit `path`;
  this is a declarative service PATH configuration issue, not a recurrence of
  the VFS or procfs executable-path behavior.
- No `STARRY_NIXOS_SYSTEM_PASSED` marker was emitted, so T028 remains
  incomplete. The run is valid crossing evidence for the bind-mount path fix.

### 2026-08-06 epoll readiness progress acceptance rerun

- `bugfix-epoll-lt-fairness` first failed after returning the persistent
  distractor on all eight `epoll_wait(..., maxevents=1)` calls. The Linux oracle
  returned the listener after one distractor event. After the ready-list order
  correction, the current Podman/QEMU rerun printed
  `PASS: listener returned after 1 persistent distractor events`,
  `STARRY_EPOLL_LT_FAIRNESS_PASSED`, and
  `STARRY_GROUPED_TESTS_PASSED`.
- The deterministic consume-to-register race axtest changed from
  `AXTEST_SUMMARY pass=396 fail=1 skip=0 total=397` to
  `pass=398 fail=0 skip=0 total=398`, proving readiness observed immediately
  after replacement-waker registration is requeued.
- `cargo fmt --all -- --check` passed, and
  `cargo xtask clippy --package starry-kernel` passed all 25 configurations in
  `ghcr.io/rcore-os/tgoskits-container:latest` with `.ci-cache` mounts.
- The full app command was
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in the same Podman image. Artifact reuse performed manifest, lock hash, image
  hash, ext4, and closure-content validation; it did not invoke host Nix or
  modify the host `/nix/store`.
- QEMU used `-smp 4`; CPU 1, 2, and 3 initialized, and NVMe reported four
  hardware contexts across four CPUs. The run completed journal flush, static
  device-node setup, udevd startup, journal catalog rebuild, and
  `systemd-update-utmp` with exit code 0. It also completed Nix store path
  registration.
- The next strict terminal failure was
  `Failed to start Initialize NixOS Channel.` after the guest had run for about
  233 seconds. `STARRY_NIXOS_SYSTEM_PASSED` was not emitted, so T028 remains
  incomplete and this run is crossing evidence for the epoll correction rather
  than a complete Stage-2 acceptance pass.
- Logs:
  `.ci-cache/tmp/bugfix-epoll-lt-fairness-verified.log` and
  `.ci-cache/tmp/starrynixos-epoll-fairness-verified-smp4.log`.

### 2026-08-06 `/dev/kmsg` poll correction acceptance rerun

- The focused kernel axtest first failed with
  `kmsg_reports_only_supported_poll_events` and
  `AXTEST_SUMMARY pass=397 fail=1 skip=0 total=398`. After matching the Linux
  readiness contract, `kmsg_reports_no_readiness_without_read_side` passed with
  `AXTEST_SUMMARY pass=398 fail=0 skip=0 total=398` and `AXTEST_SUITE_OK`.
- `cargo fmt --all` passed, and
  `cargo xtask clippy --package starry-kernel` passed all 25 configurations.
  These checks ran in Podman with target and tool state under `.ci-cache`.
- The published image and manifest passed the builder's strict reuse validation:
  lock SHA-256 `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4`,
  image SHA-256
  `9b1208f33534975a7b786342b557351ce016e454d444849da58e524b044943b1`,
  system
  `/nix/store/q2j5y05w2l4nhvsgzd3b7g49rn92lpkn-nixos-system-starrynixos-starry-nixos-stage2`,
  and systemd `260.2`.
- CI-like execution used
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in `ghcr.io/rcore-os/tgoskits-container:latest`, with
  `.ci-cache/{cargo,rustup,target-nixos-kmsg,tmp}` for isolated build and
  temporary state. QEMU ran with `-smp 4`; no host Nix command or host
  `/nix/store` mutation was used.
- Journald registered fd 8 for `EPOLLIN`, but no readiness delivery for that fd
  appeared. It printed `Received client request to flush runtime journal.`;
  `journalctl` exited 0, and systemd printed
  `Finished Flush Journal to Persistent Storage.` The previous unconsumable
  `/dev/kmsg` readiness boundary was therefore crossed.
- The strict matcher later stopped at
  `Failed to start Rule-based Manager for Device Events and Files.` No
  `STARRY_NIXOS_SYSTEM_PASSED` marker was emitted, so T028 remains incomplete.
  Log: `.ci-cache/nixos-kmsg-poll-green.log`.

### 2026-08-05 boot-ID correction acceptance rerun

- CI-like execution used
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in `ghcr.io/rcore-os/tgoskits-container:latest`, with rootless Podman,
  `--network none`, a read-only `/workspace` source mount, and only
  `.ci-cache/{axbuild-tmp,cargo,rustup,target,tmp}` writable. The run did not
  invoke host Nix, mutate `/nix/store`, or rebuild the NixOS rootfs.
- The reused generated system identified itself as
  `/nix/store/q2j5y05w2l4nhvsgzd3b7g49rn92lpkn-nixos-system-starrynixos-starry-nixos-stage2`;
  systemd was `260.2`.
- Journald printed `Collecting audit messages is disabled.`, reached
  `Started Journal Service.`, and later logged `Received client request to flush
  runtime journal.` The prior `/proc/sys/kernel/random/boot_id` `ENOENT` and
  generic journal-write error did not recur.
- The strict matcher then rejected the boot on `Failed to start Apply Kernel
  Variables.` `Failed to start Flush Journal to Persistent Storage.` followed.
  Later `fcntl(1027)` and `kcmp` diagnostics are recorded only as observations;
  this run does not attribute those later failures to a new kernel subsystem.
  No `STARRY_NIXOS_SYSTEM_PASSED` marker was emitted.

### 2026-08-05 fresh-rootfs acceptance rerun

- Host construction used the existing direnv environment directly:
  `bash apps/starry/nixos/build-rootfs.sh`; no `nix develop` was used.
- The rebuilt artifact manifest records system closure
  `/nix/store/2kf72bk9h4gkw2g10h9392iqy9mwjyy7-nixos-system-starrynixos-starry-nixos-stage2`,
  systemd `260.2`, and image SHA-256
  `890bdf2de5921d470403fc6eac25355bcb657803b1f18a3fb167ebfcaaad8e74`.
- CI-like execution used
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in `ghcr.io/rcore-os/tgoskits-container:latest`, mounting
  `.ci-cache/{cargo,rustup,tmp}` only for disposable tool and temporary state.
- `systemd-sysctl.service` completed with exit code 0 after the profile removed
  the unenforceable `kernel.pid_max` and `vm.max_map_count` writes. The static
  `/dev` node job also completed successfully.
- Journald then reported repeated `ENOENT` writes to
  `/run/log/journal/<machine-id>/system.journal`. The strict matcher stopped on
  `Failed to start Flush Journal to Persistent Storage.` No pass marker was
  emitted. This is the current bounded finding, not a support claim.

### 2026-08-05 mountinfo notification rerun

- The focused regression first failed on the prior kernel with
  `ready=0 revents=0 expected POLLPRI|POLLERR`, then passed after the correction
  with `STARRY_MOUNTINFO_POLL_NOTIFY_PASSED` and
  `result: 1/1 case(s) passed`.
- Both focused and real-system runs used rootless Podman with `--network none`,
  a read-only `/workspace`, and writable state only under `.ci-cache`. The real
  run set `STARRY_NIXOS_REUSE_ROOTFS=1`; neither run invoked host Nix or changed
  the host `/nix/store`.
- The real-system run completed generators, queued `Multi-User System`, started
  journald, completed `systemd-sysctl`, flushed the journal, and completed both
  static-device-node jobs. It then printed `[  OK  ] Mounted /run/wrappers.`,
  reached `Local File Systems`, and started `Register Nix Store Paths`.
- The run was stopped after crossing the recorded mountinfo boundary because it
  had entered the later Nix registration phase. It did not emit
  `STARRY_NIXOS_SYSTEM_PASSED`, so T028 and the final acceptance claim remain
  incomplete.

### 2026-08-03 machine-id exception acceptance rerun

- The first container attempt mounted the repository at a different absolute
  path and therefore failed before QEMU because the published manifest records
  `/workspace/...`; this is an artifact-path validation failure, not guest
  evidence.
- The rerun mounted the repository at `/workspace` and used:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in `ghcr.io/rcore-os/tgoskits-container:latest`, with
  `.ci-cache/{cargo,rustup,tmp}` only as tool and temporary-data caches.
- Artifact identity:
  `/nix/store/211p2xi0fbxi7fq6dq3zjryl46wk3dmz-nixos-system-starrynixos-starry-nixos-stage2`;
  systemd `260.2`; image SHA-256
  `0afa089a2950c990842cb5e8a2d66eae4086a8ef519cc19458b870d92ec2e641`.
- Generated configuration retained only the documented udevd unit-local
  sandbox override and precisely masked
  `systemd-machine-id-commit.service`; no global systemd sandbox setting was
  disabled.
- Crossing evidence included successful `systemd-sysctl`, journal flush,
  static device-node creation, and `Rule-based Manager for Device Events and
  Files`. The machine-id commit failure did not recur.
- `/run/wrappers` still failed and `nix-suid-wrappers.service` consequently
  failed, but systemd continued; this was not the terminal acceptance result.
- The runner timed out after 600 seconds with `Register Nix Store Paths` and
  `Create System Files and Directories` still active. No
  `STARRY_NIXOS_SYSTEM_PASSED` marker was emitted. Repeated read-only
  file-time diagnostics are recorded as a hypothesis only, pending exact unit
  commands, a Linux oracle, and a deterministic Starry regression.

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

### 2026-08-02 `/proc/sys/kernel/hostname` correction

- Linux oracle and focused test:
  `test-suit/starryos/qemu/system/bugfix-proc-sys-kernel-hostname/`.
- Red result: `gethostname` and `uname` agreed, then
  `open("/proc/sys/kernel/hostname", O_RDONLY)` failed with `ENOENT`
  after 3 passing checks.
- Green result: 12/12 checks passed, including exact `hostname + "\n"`
  contents, no NUL padding, EOF, seek-to-start, and repeat-read behavior;
  `STARRY_PROC_SYS_HOSTNAME_PASSED` and the grouped success marker were
  emitted.
- Adjacent namespace result:
  `qemu/system/syscall-test-namespace` passed 13/13, including child-only
  `unshare(CLONE_NEWUTS)` plus `sethostname` isolation.
- Quality gates: `cargo fmt --all` passed and
  `cargo xtask clippy --package starry-kernel` passed all 25 checks,
  including both aarch64 system configurations.
- Real app payload:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in the project Podman image with `.ci-cache` used only for caches.
- Crossing evidence: systemd printed `Hostname set to <starrynixos>`;
  journald no longer logged an `ENOENT` for `/proc/sys/kernel/hostname` and
  continued through `Collecting audit messages is disabled`.
- New bounded result: journald then exited with status 1 without identifying
  the failed operation. The run was stopped after this evidence because
  several unrelated systemd jobs had no time limit. The container exit status
  137 is therefore a deliberate bounded stop, not a test pass or kernel crash.
- Log: `.ci-cache/tmp/starrynixos-after-proc-hostname.log`.
- `STARRY_NIXOS_SYSTEM_PASSED` was not emitted; T028 remains incomplete.

### 2026-08-02 Unix listener introspection correction

- Linux oracle and focused test:
  `test-suit/starryos/qemu/system/bugfix-unix-listener-introspection/`.
- Red result: 12/17 checks passed. Four `SO_ACCEPTCONN` queries returned
  `ENOPROTOOPT`, and a pathname Unix stream incorrectly accepted a connection
  after `bind(2)` but before `listen(2)`.
- Green result: 17/17 checks passed, including pre-listen zero, post-listen one,
  duplicated-fd state, exact option length, pathname introspection, and
  pre-listen `ECONNREFUSED`; both focused and grouped success markers were
  emitted.
- Quality gates: `cargo fmt --all` passed; targeted `ax-net` clippy passed 3/3
  checks; targeted `starry-kernel` clippy passed 25/25 checks. Adjacent
  `syscall-test-accept4` passed 29/29 and `syscall-test-seqpacket` passed 73/73.
- Real app payload:
  `STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64`
  in the project Podman image with `.ci-cache` used only for caches.
- Crossing evidence: the complete log contains zero instances of
  `unknown file descriptors passed`, one `Collecting audit messages` record,
  seven handoff-timestamp credential diagnostics, and two notification-datagram
  credential diagnostics. Journald therefore crossed inherited Varlink listener
  identification and entered later datagram processing.
- New bounded result: `systemd-journalctl.socket` failed with result `timeout`;
  journald and sysctl startup operations then timed out. The 180-second outer
  timeout exited with status 124.
- Log: `.ci-cache/tmp/starrynixos-after-so-acceptconn.log`.
- `STARRY_NIXOS_SYSTEM_PASSED` was not emitted; T028 remains incomplete.
