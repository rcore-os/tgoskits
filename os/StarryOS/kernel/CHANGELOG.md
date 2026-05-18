# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.9...starry-kernel-v0.5.10) - 2026-05-15

### Added

- *(starry-kernel)* add chmod/fchmod/chown/fchown/fchmodat/faccessat/umask syscall coverage and centralized absolute-path handling ([#605](https://github.com/rcore-os/tgoskits/pull/605))
- *(net)* expand AF_NETLINK with rtnetlink/uevent/genl plumbing ([#512](https://github.com/rcore-os/tgoskits/pull/512))
- *(starry)* sysfs symlinks + evdev minor base 64 + /run/udev seed for weston ([#508](https://github.com/rcore-os/tgoskits/pull/508))
- *(starry)* implement advisory file locks (fcntl POSIX/OFD, flock) ([#472](https://github.com/rcore-os/tgoskits/pull/472))
- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))
- *(starry-kernel)* add runtime dynamic debug control ([#446](https://github.com/rcore-os/tgoskits/pull/446))
- *(starryos/procfs)* implement /proc/stat, /proc/cpuinfo, /proc/uptime; fix /proc/meminfo and sysinfo() ([#452](https://github.com/rcore-os/tgoskits/pull/452))
- *(starry-usbfs)* expose USB devices through usbfs and sysfs
- *(starry-net)* add minimal netlink socket support
- *(starry-syscall)* add timerfd and file handle support
- *(irq)* pass IRQ/event number to registered handlers
- *(timer)* implement POSIX timer syscalls (timer_create/settime/gettime/delete ([#341](https://github.com/rcore-os/tgoskits/pull/341))
- *(realtek-rtl8125)* complete OrangePi board bringup ([#404](https://github.com/rcore-os/tgoskits/pull/404))
- feat(vfork) + fix(execve): implement vfork parent blocking and fix exec under CLONE_VM ([#377](https://github.com/rcore-os/tgoskits/pull/377))
- *(starry)* implement mremap ([#205](https://github.com/rcore-os/tgoskits/pull/205))
- *(mm)* track backend split metadata and generate real /proc maps output ([#306](https://github.com/rcore-os/tgoskits/pull/306))
- *(ax-net-ng)* add ICMP raw socket support ([#368](https://github.com/rcore-os/tgoskits/pull/368))
- *(net)* migrate ax-net to crates.io smoltcp ([#410](https://github.com/rcore-os/tgoskits/pull/410))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))
- *(console)* add interrupt-driven console input ([#343](https://github.com/rcore-os/tgoskits/pull/343))

### Fixed

- *(starry-kernel)* align fchownat semantics and add syscall coverage ([#588](https://github.com/rcore-os/tgoskits/pull/588))
- *(evdev)* expose eventN for all input devices and plumb EVIOCGPROP/EVIOCGABS ([#513](https://github.com/rcore-os/tgoskits/pull/513))
- *(starry)* close TOCTOU windows in FutexGuard::drop and close_all_fds ([#498](https://github.com/rcore-os/tgoskits/pull/498))
- *(starry-kernel)* avoid poll and epoll wait user-buffer panics ([#523](https://github.com/rcore-os/tgoskits/pull/523))
- *(ext4)* use Linux-compatible old/new_encode_dev for device rdev ([#518](https://github.com/rcore-os/tgoskits/pull/518))
- *(starry-kernel)* close CLI compatibility gaps ([#524](https://github.com/rcore-os/tgoskits/pull/524))
- *(loop)* replace map_or with is_none_or to silence clippy unnecessary_map_or ([#501](https://github.com/rcore-os/tgoskits/pull/501))
- *(starry)* harden futex wait and robust-list semantics ([#545](https://github.com/rcore-os/tgoskits/pull/545))
- *(tty)* drain console input without RX IRQ ([#569](https://github.com/rcore-os/tgoskits/pull/569))
- *(packet)* support busybox arping ([#484](https://github.com/rcore-os/tgoskits/pull/484))
- *(epoll)* drain ready list once per epoll_wait in LT mode ([#504](https://github.com/rcore-os/tgoskits/pull/504))
- *(sched)* support busybox nice priority syscalls ([#477](https://github.com/rcore-os/tgoskits/pull/477))
- return correct values for zombie pids in getsid/getpgid/getpriority ([#500](https://github.com/rcore-os/tgoskits/pull/500))
- *(proc)* expose arp table for busybox arp ([#480](https://github.com/rcore-os/tgoskits/pull/480))
- *(tty)* poll_read with VMIN=0 no longer reports POLLIN when buffer is empty ([#502](https://github.com/rcore-os/tgoskits/pull/502))
- *(busybox-hwclock)* remove fake RTC device for correct hwclock behavior ([#521](https://github.com/rcore-os/tgoskits/pull/521))
- *(starry-kernel)* avoid tmpfs cwd cleanup panic ([#525](https://github.com/rcore-os/tgoskits/pull/525))
- *(proc)* expose init pid for busybox pidof ([#482](https://github.com/rcore-os/tgoskits/pull/482))
- *(loop)* add BLKSSZGET and BLKPBSZGET ioctl handlers ([#489](https://github.com/rcore-os/tgoskits/pull/489))
- *(ioctl)* suppress kernel warnings for block device ioctls on non-block fds ([#491](https://github.com/rcore-os/tgoskits/pull/491))
- *(blockdev)* busybox arch/blkid/blkdiscard/blockdev + loop device Linux semantics ([#465](https://github.com/rcore-os/tgoskits/pull/465))
- *(file)* reject invalid linkat flags and preserve symlink semantics ([#449](https://github.com/rcore-os/tgoskits/pull/449))
- *(file)* honor RENAME_NOREPLACE in renameat2 ([#451](https://github.com/rcore-os/tgoskits/pull/451))
- *(arceos)* adjust dynamic platform and network integration
- *(starry-syscall)* improve user memory and write buffer handling
- *(starry-task)* normalize cloned user return state
- *(mm)* accept fd 0 for file mmap and ignore fd for anonymous mappings ([#450](https://github.com/rcore-os/tgoskits/pull/450))
- *(starryos)* reject invalid pipe2 flags with EINVAL ([#268](https://github.com/rcore-os/tgoskits/pull/268))
- isolate signal check blocking per thread ([#224](https://github.com/rcore-os/tgoskits/pull/224))
- *(starry)* reject negative ftruncate length ([#209](https://github.com/rcore-os/tgoskits/pull/209))
- *(net)* preserve unix bind success after chown ([#313](https://github.com/rcore-os/tgoskits/pull/313))
- *(console)* keep UART writes raw ([#402](https://github.com/rcore-os/tgoskits/pull/402))
- *(starryos)* validate pwrite64 fd before zero-length return ([#381](https://github.com/rcore-os/tgoskits/pull/381))
- implement close_all_fds function and enhance pipe and syscall handling ([#305](https://github.com/rcore-os/tgoskits/pull/305))
- *(starryos)* Fix eventfd read and write semantics in StarryOS. ([#370](https://github.com/rcore-os/tgoskits/pull/370))

### Other

- Adds a StarryOS YOLOv8 UVC camera demo for OrangePi 5 Plus with RKNN/NPU inference and HTTP MJPEG streaming. ([#574](https://github.com/rcore-os/tgoskits/pull/574))
- Implement blocking behavior for message queue system calls ([#488](https://github.com/rcore-os/tgoskits/pull/488))
- Fix/syscall brk semantics ([#486](https://github.com/rcore-os/tgoskits/pull/486))
- busybox_ipaddr ([#481](https://github.com/rcore-os/tgoskits/pull/481))
- *(sys_truncate/sys_ftruncate)* reject empty path, huge length, huge length, read-only file, etc. ([#466](https://github.com/rcore-os/tgoskits/pull/466))
- *(sys fadvise64)* reject invalid/closed fd with ebadf, negative len with einval ([#444](https://github.com/rcore-os/tgoskits/pull/444))
- *(repo)* remove tgmath example and refresh docs/deps
- fchmodat2 invalid flag validation and legacy fchmodat flagless d ([#462](https://github.com/rcore-os/tgoskits/pull/462))
- faccessat2 mode/flag validation and legacy faccessat dispatch ([#460](https://github.com/rcore-os/tgoskits/pull/460))
- wait4 invalid option validation rejects waitid-only and unkno ([#461](https://github.com/rcore-os/tgoskits/pull/461))
- Merge pull request #463 from hongdy22/codex/round-914-file_directory_semantics-readlinkat-zero-size-bu-20260508120844
- mknodat mode type-zero regular file semantics and S_IFDIR errno
- *(sys_fallocate)* validate negative offset/len, use EOPNOTSUPP for unsupported modes,   reject huge offsets ([#441](https://github.com/rcore-os/tgoskits/pull/441))
- (bugfix) sys_clock_getres return EINVAL when clockid is invalid ([#430](https://github.com/rcore-os/tgoskits/pull/430))
- *(kernel)* remove unused user interpreter base constants and clean up socket handling ([#421](https://github.com/rcore-os/tgoskits/pull/421))
- Implement vfork, getpgrp, and time syscalls with test enhancements ([#409](https://github.com/rcore-os/tgoskits/pull/409))
- *(session)* comprehensive tests for setsid, getsid, setpgid, getpgid ([#336](https://github.com/rcore-os/tgoskits/pull/336))
- *(starryos)* inherit workspace metadata
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

### Fixed

- *(mmap)* accept fd 0 for file mappings and ignore fd for anonymous mappings
- *(linkat)* reject invalid flags and preserve symlink hard-link semantics
- *(renameat2)* reject unknown flags and implement `RENAME_NOREPLACE`

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
