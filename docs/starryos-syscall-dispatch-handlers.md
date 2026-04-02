# StarryOS 分发表 + mod.rs 入口函数（handler）

由 `scripts/render_starry_syscall_inventory.py --step 2` 生成。 **handler** 自 `handle_syscall` 的 `match` 臂解析（块形式 `=> { ... }` 取首个 `sys_*` 调用）。

**条目数**: 210

| # | syscall | section | cfgs | handler | in_catalog | impl_path |
|---|---------|---------|------|----------|------------|-----------|
| 1 | `ioctl` | fs ctl | — | `sys_ioctl` | yes | os/StarryOS/kernel/src/syscall/fs/ctl.rs |
| 2 | `chdir` | fs ctl | — | `sys_chdir` | — | — |
| 3 | `fchdir` | fs ctl | — | `sys_fchdir` | — | — |
| 4 | `chroot` | fs ctl | — | `sys_chroot` | — | — |
| 5 | `mkdir` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_mkdir` | — | — |
| 6 | `mkdirat` | fs ctl | — | `sys_mkdirat` | — | — |
| 7 | `getdents64` | fs ctl | — | `sys_getdents64` | — | — |
| 8 | `link` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_link` | — | — |
| 9 | `linkat` | fs ctl | — | `sys_linkat` | — | — |
| 10 | `rmdir` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_rmdir` | — | — |
| 11 | `unlink` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_unlink` | yes | os/StarryOS/kernel/src/syscall/fs/ctl.rs |
| 12 | `unlinkat` | fs ctl | — | `sys_unlinkat` | — | — |
| 13 | `getcwd` | fs ctl | — | `sys_getcwd` | yes | os/StarryOS/kernel/src/syscall/fs/ctl.rs |
| 14 | `symlink` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_symlink` | — | — |
| 15 | `symlinkat` | fs ctl | — | `sys_symlinkat` | — | — |
| 16 | `rename` | fs ctl | #[cfg(target_arch = "x86_64")] | `sys_rename` | — | — |
| 17 | `renameat` | fs ctl | #[cfg(not(target_arch = "riscv64"))] | `sys_renameat` | — | — |
| 18 | `renameat2` | fs ctl | — | `sys_renameat2` | — | — |
| 19 | `sync` | fs ctl | — | `sys_sync` | — | — |
| 20 | `syncfs` | fs ctl | — | `sys_syncfs` | — | — |
| 21 | `chown` | file ops | #[cfg(target_arch = "x86_64")] | `sys_chown` | — | — |
| 22 | `lchown` | file ops | #[cfg(target_arch = "x86_64")] | `sys_lchown` | — | — |
| 23 | `fchown` | file ops | — | `sys_fchown` | — | — |
| 24 | `fchownat` | file ops | — | `sys_fchownat` | — | — |
| 25 | `chmod` | file ops | #[cfg(target_arch = "x86_64")] | `sys_chmod` | — | — |
| 26 | `fchmod` | file ops | — | `sys_fchmod` | — | — |
| 27 | `fchmodat` | file ops | — | `sys_fchmodat` | — | — |
| 28 | `fchmodat2` | file ops | — | `sys_fchmodat` | — | — |
| 29 | `readlink` | file ops | #[cfg(target_arch = "x86_64")] | `sys_readlink` | — | — |
| 30 | `readlinkat` | file ops | — | `sys_readlinkat` | — | — |
| 31 | `utime` | file ops | #[cfg(target_arch = "x86_64")] | `sys_utime` | — | — |
| 32 | `utimes` | file ops | #[cfg(target_arch = "x86_64")] | `sys_utimes` | — | — |
| 33 | `utimensat` | file ops | — | `sys_utimensat` | — | — |
| 34 | `open` | fd ops | #[cfg(target_arch = "x86_64")] | `sys_open` | — | — |
| 35 | `openat` | fd ops | — | `sys_openat` | yes | os/StarryOS/kernel/src/syscall/fs/ |
| 36 | `close` | fd ops | — | `sys_close` | yes | os/StarryOS/kernel/src/syscall/fs/ |
| 37 | `close_range` | fd ops | — | `sys_close_range` | — | — |
| 38 | `dup` | fd ops | — | `sys_dup` | yes | os/StarryOS/kernel/src/syscall/fs/ |
| 39 | `dup2` | fd ops | #[cfg(target_arch = "x86_64")] | `sys_dup2` | — | — |
| 40 | `dup3` | fd ops | — | `sys_dup3` | — | — |
| 41 | `fcntl` | fd ops | — | `sys_fcntl` | yes | os/StarryOS/kernel/src/syscall/fs/fd_ops.rs |
| 42 | `flock` | fd ops | — | `sys_flock` | — | — |
| 43 | `read` | io | — | `sys_read` | yes | os/StarryOS/kernel/src/syscall/fs/ |
| 44 | `readv` | io | — | `sys_readv` | — | — |
| 45 | `write` | io | — | `sys_write` | yes | os/StarryOS/kernel/src/syscall/fs/ |
| 46 | `writev` | io | — | `sys_writev` | — | — |
| 47 | `lseek` | io | — | `sys_lseek` | yes | os/StarryOS/kernel/src/syscall/fs/io.rs |
| 48 | `truncate` | io | — | `sys_truncate` | — | — |
| 49 | `ftruncate` | io | — | `sys_ftruncate` | — | — |
| 50 | `fallocate` | io | — | `sys_fallocate` | — | — |
| 51 | `fsync` | io | — | `sys_fsync` | — | — |
| 52 | `fdatasync` | io | — | `sys_fdatasync` | — | — |
| 53 | `fadvise64` | io | — | `sys_fadvise64` | — | — |
| 54 | `pread64` | io | — | `sys_pread64` | — | — |
| 55 | `pwrite64` | io | — | `sys_pwrite64` | — | — |
| 56 | `preadv` | io | — | `sys_preadv` | — | — |
| 57 | `pwritev` | io | — | `sys_pwritev` | — | — |
| 58 | `preadv2` | io | — | `sys_preadv2` | — | — |
| 59 | `pwritev2` | io | — | `sys_pwritev2` | — | — |
| 60 | `sendfile` | io | — | `sys_sendfile` | — | — |
| 61 | `copy_file_range` | io | — | `sys_copy_file_range` | — | — |
| 62 | `splice` | io | — | `sys_splice` | — | — |
| 63 | `poll` | io mpx | #[cfg(target_arch = "x86_64")] | `sys_poll` | — | — |
| 64 | `ppoll` | io mpx | — | `sys_ppoll` | yes | os/StarryOS/kernel/src/syscall/io_mpx/poll.rs |
| 65 | `select` | io mpx | #[cfg(target_arch = "x86_64")] | `sys_select` | — | — |
| 66 | `pselect6` | io mpx | — | `sys_pselect6` | — | — |
| 67 | `epoll_create1` | io mpx | — | `sys_epoll_create1` | — | — |
| 68 | `epoll_ctl` | io mpx | — | `sys_epoll_ctl` | — | — |
| 69 | `epoll_pwait` | io mpx | — | `sys_epoll_pwait` | — | — |
| 70 | `epoll_pwait2` | io mpx | — | `sys_epoll_pwait2` | — | — |
| 71 | `mount` | fs mount | — | `sys_mount` | — | — |
| 72 | `umount2` | fs mount | — | `sys_umount2` | — | — |
| 73 | `pipe2` | pipe | — | `sys_pipe2` | yes | os/StarryOS/kernel/src/syscall/fs/pipe.rs |
| 74 | `pipe` | pipe | #[cfg(target_arch = "x86_64")] | `sys_pipe2` | — | — |
| 75 | `eventfd2` | event | — | `sys_eventfd2` | — | — |
| 76 | `pidfd_open` | pidfd | — | `sys_pidfd_open` | — | — |
| 77 | `pidfd_getfd` | pidfd | — | `sys_pidfd_getfd` | — | — |
| 78 | `pidfd_send_signal` | pidfd | — | `sys_pidfd_send_signal` | — | — |
| 79 | `memfd_create` | memfd | — | `sys_memfd_create` | — | — |
| 80 | `stat` | fs stat | #[cfg(target_arch = "x86_64")] | `sys_stat` | — | — |
| 81 | `fstat` | fs stat | — | `sys_fstat` | — | — |
| 82 | `lstat` | fs stat | #[cfg(target_arch = "x86_64")] | `sys_lstat` | — | — |
| 83 | `newfstatat` | fs stat | #[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))] | `sys_fstatat` | — | — |
| 84 | `fstatat` | fs stat | #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))] | `sys_fstatat` | — | — |
| 85 | `statx` | fs stat | — | `sys_statx` | — | — |
| 86 | `access` | fs stat | #[cfg(target_arch = "x86_64")] | `sys_access` | — | — |
| 87 | `faccessat` | fs stat | — | `sys_faccessat2` | — | — |
| 88 | `faccessat2` | fs stat | — | `sys_faccessat2` | — | — |
| 89 | `statfs` | fs stat | — | `sys_statfs` | — | — |
| 90 | `fstatfs` | fs stat | — | `sys_fstatfs` | — | — |
| 91 | `brk` | mm | — | `sys_brk` | — | — |
| 92 | `mmap` | mm | — | `sys_mmap` | — | — |
| 93 | `munmap` | mm | — | `sys_munmap` | — | — |
| 94 | `mprotect` | mm | — | `sys_mprotect` | — | — |
| 95 | `mincore` | mm | — | `sys_mincore` | — | — |
| 96 | `mremap` | mm | — | `sys_mremap` | — | — |
| 97 | `madvise` | mm | — | `sys_madvise` | — | — |
| 98 | `msync` | mm | — | `sys_msync` | — | — |
| 99 | `mlock` | mm | — | `sys_mlock` | — | — |
| 100 | `mlock2` | mm | — | `sys_mlock2` | — | — |
| 101 | `getpid` | task info | — | `sys_getpid` | — | — |
| 102 | `getppid` | task info | — | `sys_getppid` | — | — |
| 103 | `gettid` | task info | — | `sys_gettid` | — | — |
| 104 | `getrusage` | task info | — | `sys_getrusage` | — | — |
| 105 | `sched_yield` | task sched | — | `sys_sched_yield` | — | — |
| 106 | `nanosleep` | task sched | — | `sys_nanosleep` | — | — |
| 107 | `clock_nanosleep` | task sched | — | `sys_clock_nanosleep` | — | — |
| 108 | `sched_getaffinity` | task sched | — | `sys_sched_getaffinity` | — | — |
| 109 | `sched_setaffinity` | task sched | — | `sys_sched_setaffinity` | — | — |
| 110 | `sched_getscheduler` | task sched | — | `sys_sched_getscheduler` | — | — |
| 111 | `sched_setscheduler` | task sched | — | `sys_sched_setscheduler` | — | — |
| 112 | `sched_getparam` | task sched | — | `sys_sched_getparam` | — | — |
| 113 | `getpriority` | task sched | — | `sys_getpriority` | — | — |
| 114 | `execve` | task ops | — | `sys_execve` | yes | os/StarryOS/kernel/src/syscall/task/execve.rs |
| 115 | `set_tid_address` | task ops | — | `sys_set_tid_address` | — | — |
| 116 | `arch_prctl` | task ops | #[cfg(target_arch = "x86_64")] | `sys_arch_prctl` | — | — |
| 117 | `prctl` | task ops | — | `sys_prctl` | — | — |
| 118 | `prlimit64` | task ops | — | `sys_prlimit64` | — | — |
| 119 | `capget` | task ops | — | `sys_capget` | — | — |
| 120 | `capset` | task ops | — | `sys_capset` | — | — |
| 121 | `umask` | task ops | — | `sys_umask` | — | — |
| 122 | `setreuid` | task ops | — | `sys_setreuid` | — | — |
| 123 | `setresuid` | task ops | — | `sys_setresuid` | — | — |
| 124 | `setresgid` | task ops | — | `sys_setresgid` | — | — |
| 125 | `get_mempolicy` | task ops | — | `sys_get_mempolicy` | — | — |
| 126 | `clone` | task management | — | `sys_clone` | — | — |
| 127 | `clone3` | task management | — | `sys_clone3` | — | — |
| 128 | `fork` | task management | #[cfg(target_arch = "x86_64")] | `sys_fork` | — | — |
| 129 | `exit` | task management | — | `sys_exit` | — | — |
| 130 | `exit_group` | task management | — | `sys_exit_group` | — | — |
| 131 | `wait4` | task management | — | `sys_waitpid` | yes | os/StarryOS/kernel/src/syscall/task/wait.rs |
| 132 | `getsid` | task management | — | `sys_getsid` | — | — |
| 133 | `setsid` | task management | — | `sys_setsid` | — | — |
| 134 | `getpgid` | task management | — | `sys_getpgid` | — | — |
| 135 | `setpgid` | task management | — | `sys_setpgid` | — | — |
| 136 | `rt_sigprocmask` | signal | — | `sys_rt_sigprocmask` | — | — |
| 137 | `rt_sigaction` | signal | — | `sys_rt_sigaction` | — | — |
| 138 | `rt_sigpending` | signal | — | `sys_rt_sigpending` | — | — |
| 139 | `rt_sigreturn` | signal | — | `sys_rt_sigreturn` | — | — |
| 140 | `rt_sigtimedwait` | signal | — | `sys_rt_sigtimedwait` | — | — |
| 141 | `rt_sigsuspend` | signal | — | `sys_rt_sigsuspend` | — | — |
| 142 | `kill` | signal | — | `sys_kill` | — | — |
| 143 | `tkill` | signal | — | `sys_tkill` | — | — |
| 144 | `tgkill` | signal | — | `sys_tgkill` | — | — |
| 145 | `rt_sigqueueinfo` | signal | — | `sys_rt_sigqueueinfo` | — | — |
| 146 | `rt_tgsigqueueinfo` | signal | — | `sys_rt_tgsigqueueinfo` | — | — |
| 147 | `sigaltstack` | signal | — | `sys_sigaltstack` | — | — |
| 148 | `futex` | signal | — | `sys_futex` | yes | os/StarryOS/kernel/src/syscall/sync/futex.rs |
| 149 | `get_robust_list` | signal | — | `sys_get_robust_list` | — | — |
| 150 | `set_robust_list` | signal | — | `sys_set_robust_list` | — | — |
| 151 | `getuid` | sys | — | `sys_getuid` | — | — |
| 152 | `geteuid` | sys | — | `sys_geteuid` | — | — |
| 153 | `getgid` | sys | — | `sys_getgid` | — | — |
| 154 | `getegid` | sys | — | `sys_getegid` | — | — |
| 155 | `setuid` | sys | — | `sys_setuid` | — | — |
| 156 | `setgid` | sys | — | `sys_setgid` | — | — |
| 157 | `getgroups` | sys | — | `sys_getgroups` | — | — |
| 158 | `setgroups` | sys | — | `sys_setgroups` | — | — |
| 159 | `uname` | sys | — | `sys_uname` | — | — |
| 160 | `sysinfo` | sys | — | `sys_sysinfo` | — | — |
| 161 | `syslog` | sys | — | `sys_syslog` | — | — |
| 162 | `getrandom` | sys | — | `sys_getrandom` | — | — |
| 163 | `seccomp` | sys | — | `sys_seccomp` | — | — |
| 164 | `riscv_flush_icache` | sys | #[cfg(target_arch = "riscv64")] | `sys_riscv_flush_icache` | — | — |
| 165 | `membarrier` | sync | — | `sys_membarrier` | — | — |
| 166 | `gettimeofday` | time | — | `sys_gettimeofday` | — | — |
| 167 | `times` | time | — | `sys_times` | — | — |
| 168 | `clock_gettime` | time | — | `sys_clock_gettime` | yes | os/StarryOS/kernel/src/syscall/time.rs |
| 169 | `clock_getres` | time | — | `sys_clock_getres` | — | — |
| 170 | `getitimer` | time | — | `sys_getitimer` | — | — |
| 171 | `setitimer` | time | — | `sys_setitimer` | — | — |
| 172 | `msgget` | msg | — | `sys_msgget` | — | — |
| 173 | `msgsnd` | msg | — | `sys_msgsnd` | — | — |
| 174 | `msgrcv` | msg | — | `sys_msgrcv` | — | — |
| 175 | `msgctl` | msg | — | `sys_msgctl` | — | — |
| 176 | `shmget` | shm | — | `sys_shmget` | — | — |
| 177 | `shmat` | shm | — | `sys_shmat` | — | — |
| 178 | `shmctl` | shm | — | `sys_shmctl` | — | — |
| 179 | `shmdt` | shm | — | `sys_shmdt` | — | — |
| 180 | `socket` | net | — | `sys_socket` | — | — |
| 181 | `socketpair` | net | — | `sys_socketpair` | — | — |
| 182 | `bind` | net | — | `sys_bind` | — | — |
| 183 | `connect` | net | — | `sys_connect` | — | — |
| 184 | `getsockname` | net | — | `sys_getsockname` | — | — |
| 185 | `getpeername` | net | — | `sys_getpeername` | — | — |
| 186 | `listen` | net | — | `sys_listen` | — | — |
| 187 | `accept` | net | — | `sys_accept` | — | — |
| 188 | `accept4` | net | — | `sys_accept4` | — | — |
| 189 | `shutdown` | net | — | `sys_shutdown` | — | — |
| 190 | `sendto` | net | — | `sys_sendto` | — | — |
| 191 | `recvfrom` | net | — | `sys_recvfrom` | — | — |
| 192 | `sendmsg` | net | — | `sys_sendmsg` | — | — |
| 193 | `recvmsg` | net | — | `sys_recvmsg` | — | — |
| 194 | `getsockopt` | net | — | `sys_getsockopt` | — | — |
| 195 | `setsockopt` | net | — | `sys_setsockopt` | — | — |
| 196 | `signalfd4` | signal file descriptors | — | `sys_signalfd4` | — | — |
| 197 | `timerfd_create` | dummy fds | — | `sys_dummy_fd` | — | — |
| 198 | `fanotify_init` | dummy fds | — | `sys_dummy_fd` | — | — |
| 199 | `inotify_init1` | dummy fds | — | `sys_dummy_fd` | — | — |
| 200 | `userfaultfd` | dummy fds | — | `sys_dummy_fd` | — | — |
| 201 | `perf_event_open` | dummy fds | — | `sys_dummy_fd` | — | — |
| 202 | `io_uring_setup` | dummy fds | — | `sys_dummy_fd` | — | — |
| 203 | `bpf` | dummy fds | — | `sys_dummy_fd` | — | — |
| 204 | `fsopen` | dummy fds | — | `sys_dummy_fd` | — | — |
| 205 | `fspick` | dummy fds | — | `sys_dummy_fd` | — | — |
| 206 | `open_tree` | dummy fds | — | `sys_dummy_fd` | — | — |
| 207 | `memfd_secret` | dummy fds | — | `sys_dummy_fd` | — | — |
| 208 | `timer_create` | dummy fds | — | `Ok(0)` | — | — |
| 209 | `timer_gettime` | dummy fds | — | `Ok(0)` | — | — |
| 210 | `timer_settime` | dummy fds | — | `Ok(0)` | — | — |
