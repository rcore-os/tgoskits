# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.8...starry-kernel-v0.5.9) - 2026-04-27

### Added

- *(starry)* harden stat/fstatat/statx and add test-stat-family suite ([#300](https://github.com/rcore-os/tgoskits/pull/300))
- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))
- *(starry)* implement per-process credentials subsystem and permission checks ([#246](https://github.com/rcore-os/tgoskits/pull/246))
- *(prctl)* implement PR_SET_PDEATHSIG and PR_GET_PDEATHSIG with signal delivery ([#249](https://github.com/rcore-os/tgoskits/pull/249))
- *(platform)* parse physical memory size from DTB and fix RLIMIT_STACK default ([#248](https://github.com/rcore-os/tgoskits/pull/248))
- *(signal)* implement SA_RESTART syscall restart semantics ([#247](https://github.com/rcore-os/tgoskits/pull/247))

### Fixed

- *(starry)* stabilize shm deadlock regression
- *(syscall)* support preadv2/pwritev2 offset=-1 and reject unsupported flags ([#326](https://github.com/rcore-os/tgoskits/pull/326))
- *(starry)* accept sigaltstack with exactly MINSIGSTKSZ bytes ([#207](https://github.com/rcore-os/tgoskits/pull/207))
- *(file)* write on directory fd returns EBADF instead of EISDIR ([#324](https://github.com/rcore-os/tgoskits/pull/324))
- *(prlimit64)* allow raising hard limit instead of silent no-op ([#319](https://github.com/rcore-os/tgoskits/pull/319))
- *(epoll)* re-queue interest after EPOLL_CTL_MOD ([#314](https://github.com/rcore-os/tgoskits/pull/314))
- *(lseek)* return EINVAL for negative offset with SEEK_SET ([#303](https://github.com/rcore-os/tgoskits/pull/303))
- *(starry)* validate copy_file_range flags, file types, and overlap ([#211](https://github.com/rcore-os/tgoskits/pull/211))
- *(starry)* validate getrandom flags per Linux semantics ([#210](https://github.com/rcore-os/tgoskits/pull/210))
- *(tty)* improve termios handling and ensure safety in locking mecha… ([#308](https://github.com/rcore-os/tgoskits/pull/308))
- *(starry-kernel)* 修复 futex 等待中的用户态内存访问 ([#302](https://github.com/rcore-os/tgoskits/pull/302))
- *(ipc)* resolve SHM_MANAGER/shm_inner AB/BA deadlock under SMP ([#226](https://github.com/rcore-os/tgoskits/pull/226))
- *(mm/syscall)* fix pause() and NULL pointer validation for zero-length slices ([#296](https://github.com/rcore-os/tgoskits/pull/296))
- *(starry)* align mmap/munmap/mprotect error paths with Linux ([#285](https://github.com/rcore-os/tgoskits/pull/285))
- *(starry)* validate madvise advice/alignment/mapping per Linux ([#278](https://github.com/rcore-os/tgoskits/pull/278))
- respect pid in sched affinity syscalls ([#276](https://github.com/rcore-os/tgoskits/pull/276))
- fix/sys-pwritev2-read-at-to-write-at ([#280](https://github.com/rcore-os/tgoskits/pull/280))
- *(starry)* fix getgroups size=0 query and clock_gettime invalid clock_id behavior ([#208](https://github.com/rcore-os/tgoskits/pull/208))
- *(starry)* clamp clone3 user struct read length ([#269](https://github.com/rcore-os/tgoskits/pull/269))
- *(times)* use ProcessData for child CPU time instead of parent time ([#257](https://github.com/rcore-os/tgoskits/pull/257))
- *(starry)* fix fsync/fdatasync on directory fds and implement sync_file_range ([#251](https://github.com/rcore-os/tgoskits/pull/251))
- *(epoll)* fix epoll_pwait sigsetsize incompatibility with musl ([#250](https://github.com/rcore-os/tgoskits/pull/250))
- report real cpu affinity in proc status ([#267](https://github.com/rcore-os/tgoskits/pull/267))
- *(task)* process pending futex entry in exit_robust_list ([#259](https://github.com/rcore-os/tgoskits/pull/259))
- *(file)* return EISDIR instead of EBADF for directory read/write ([#264](https://github.com/rcore-os/tgoskits/pull/264))
- *(mm)* preserve mapping sharing type in sys_mremap ([#263](https://github.com/rcore-os/tgoskits/pull/263))
- *(tty)* read pgid from user arg in TIOCSPGRP handler ([#262](https://github.com/rcore-os/tgoskits/pull/262))
- *(ipc)* replace unwrap with error handling in sys_shmat ([#261](https://github.com/rcore-os/tgoskits/pull/261))
- *(fcntl)* return correct access mode flags in F_GETFL ([#260](https://github.com/rcore-os/tgoskits/pull/260))
- *(syscall)* add negative offset validation in sys_pwrite64 and pwritev ([#258](https://github.com/rcore-os/tgoskits/pull/258))
- *(file)* return correct errno for pipe fds instead of EPIPE ([#256](https://github.com/rcore-os/tgoskits/pull/256))

### Other

- *(starry)* reject invalid unlinkat flag bits with EINVAL ([#265](https://github.com/rcore-os/tgoskits/pull/265))
- *(starry)* add ioctl FIONBIO test and fix int parsing bug ([#255](https://github.com/rcore-os/tgoskits/pull/255))
- Implement RK3588 CRU driver with NPU support and enhancements ([#241](https://github.com/rcore-os/tgoskits/pull/241))
- Unifies breakpoint and debug trap handling across archs ([#244](https://github.com/rcore-os/tgoskits/pull/244))
