# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.7.2...starry-kernel-v0.7.3) - 2026-07-08

### Other

- updated the following local packages: axklib, axplat-dyn, ax-hal, ax-hal, ax-runtime, ax-alloc, ax-driver, ax-ipi, ax-mm, ax-task, ax-sync, ax-display, ax-dma, ax-fs-ng, ax-net, ax-input, ax-std, sg2002-tpu

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.7.1...starry-kernel-v0.7.2) - 2026-07-08

### Added

- add jpu after camera pipeline in sg2002 platform ([#1540](https://github.com/rcore-os/tgoskits/pull/1540))

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.7.0...starry-kernel-v0.7.1) - 2026-07-08

### Added

- *(starry)* report EOPNOTSUPP for SIOCETHTOOL and expose /proc/pid/mountinfo ([#1508](https://github.com/rcore-os/tgoskits/pull/1508))

### Fixed

- *(starry-kernel)* reject private mmap faults past eof ([#1534](https://github.com/rcore-os/tgoskits/pull/1534))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.6...starry-kernel-v0.7.0) - 2026-07-07

### Added

- *(starry)* add /proc/vmstat with pgfault and nr_free_pages ([#1525](https://github.com/rcore-os/tgoskits/pull/1525))
- *(starry)* add nix test (no sandbox currently) and kernel regression suite ([#1125](https://github.com/rcore-os/tgoskits/pull/1125))
- *(starry)* back /proc/diskstats, /proc/net/dev and /proc/mounts with real data ([#1504](https://github.com/rcore-os/tgoskits/pull/1504))
- *(starry)* support rtnetlink IPv4 configuration ([#1497](https://github.com/rcore-os/tgoskits/pull/1497))
- *(starry-perf)* replace magic numbers in perf/ebpf/tracepoint; add BPF helpers, O_NONBLOCK, and regression test records ([#1412](https://github.com/rcore-os/tgoskits/pull/1412))
- *(crab-usb)* add RK3588 EHCI USB2 host ([#1481](https://github.com/rcore-os/tgoskits/pull/1481))
- *(starry-kernel)* add RK3588 PWM sysfs support ([#1468](https://github.com/rcore-os/tgoskits/pull/1468))

### Fixed

- *(starry-kernel)* track pipe endpoint state explicitly ([#1531](https://github.com/rcore-os/tgoskits/pull/1531))
- *(ci)* restore Starry ptrace and Axvisor RISC-V tests ([#1521](https://github.com/rcore-os/tgoskits/pull/1521))
- *(starry)* harden path, random, and icmp behavior ([#1517](https://github.com/rcore-os/tgoskits/pull/1517))
- *(starry)* align nanosleep and shm attach semantics ([#1514](https://github.com/rcore-os/tgoskits/pull/1514))
- *(starry)* accept read-only mmap fdatasync and IP_PKTINFO/IPV6 pktinfo sockopts ([#1505](https://github.com/rcore-os/tgoskits/pull/1505))
- *(starry-process)* wake blocked sibling on group-exit for prompt aspace reclaim ([#1500](https://github.com/rcore-os/tgoskits/pull/1500))
- *(starry)* apply termios serial format and handle tty drain ioctls ([#1484](https://github.com/rcore-os/tgoskits/pull/1484))
- *(starry)* tighten LTP-derived syscall compatibility guards ([#1488](https://github.com/rcore-os/tgoskits/pull/1488))

### Other

- *(starry)* add gateway and higress reverse-proxy carpets ([#1502](https://github.com/rcore-os/tgoskits/pull/1502))
- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.6.6](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.5...starry-kernel-v0.6.6) - 2026-07-02

### Added

- *(axtest)* simplify kernel test targets ([#1470](https://github.com/rcore-os/tgoskits/pull/1470))
- *(rockchip-jpeg)* add RK3588 hardware JPEG decoder (VDPU720) with MPP /dev/mpp_service ([#1456](https://github.com/rcore-os/tgoskits/pull/1456))
- *(axtest)* add ArceOS QEMU smoke coverage ([#1365](https://github.com/rcore-os/tgoskits/pull/1365))
- *(starry)* ARM PMUv3 hardware-PMU perf support (perf stat / record / report) ([#1395](https://github.com/rcore-os/tgoskits/pull/1395))
- *(kspin)* add lockdep-aware spin rwlock ([#1397](https://github.com/rcore-os/tgoskits/pull/1397))

### Fixed

- *(starry-kernel)* make evdev polling demand driven ([#1450](https://github.com/rcore-os/tgoskits/pull/1450))
- *(starry-kernel)* sync riscv ptrace single-step text ([#1444](https://github.com/rcore-os/tgoskits/pull/1444))
- *(starry-kernel)* resolve PMU IRQ through typed domain
- *(irq)* close domain runtime review gaps
- *(irq)* separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))

### Other

- fix LTP-derived syscall conformance gaps ([#1464](https://github.com/rcore-os/tgoskits/pull/1464))
- *(ax-driver)* remove static platform compatibility ([#1463](https://github.com/rcore-os/tgoskits/pull/1463))
- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- # feat(starry): add Qt6 calculator test + fix input event delivery ([#1396](https://github.com/rcore-os/tgoskits/pull/1396))
- *(platforms)* remove LoongArch static platform ([#1428](https://github.com/rcore-os/tgoskits/pull/1428))
- *(build)* generate build.rs Rust sources with quote ([#1422](https://github.com/rcore-os/tgoskits/pull/1422))
- *(starry-kernel)* move arch runtime helpers into HAL ([#1427](https://github.com/rcore-os/tgoskits/pull/1427))
- *(ax-runtime)* resolve device IRQ bindings to IrqId
- Revert "fix(irq): separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))" ([#1424](https://github.com/rcore-os/tgoskits/pull/1424))

## [0.6.5](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.4...starry-kernel-v0.6.5) - 2026-06-27

### Added

- *(starry-kernel)* add USB serial tty support

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))
- *(rknpu)* honor GEM cache flags for mmap ([#1364](https://github.com/rcore-os/tgoskits/pull/1364))
- *(starry-kernel)* Cow RSS per-VA charge tracking, and /proc memory stats improvements ([#1173](https://github.com/rcore-os/tgoskits/pull/1173))
- *(starry-kernel)* align socket QoS options with Linux ([#1319](https://github.com/rcore-os/tgoskits/pull/1319))
- *(axcpu)* deliver x86_64 #DE (divide error) as SIGFPE/FPE_INTDIV ([#1367](https://github.com/rcore-os/tgoskits/pull/1367))

### Other

- Merge pull request #1378 from rcore-os/feat/starry-usb-serial-tty
- *(starry-kernel)* move USB serial logic into driver crate
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))
- # feat(starry): implement PRIME dma-buf for card0 + add ffplay Wayland integration test ([#1268](https://github.com/rcore-os/tgoskits/pull/1268))

## [0.6.4](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.3...starry-kernel-v0.6.4) - 2026-06-23

### Added

- *(starry)* support reboot syscall ([#1358](https://github.com/rcore-os/tgoskits/pull/1358))

### Fixed

- *(starry)* non-blocking tty serial read respects O_NONBLOCK

### Other

- *(starry-kernel)* consolidate rknpu DRM ioctl helpers into drm.rs ([#1351](https://github.com/rcore-os/tgoskits/pull/1351))
- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))
- Tpu kworker clean ([#1352](https://github.com/rcore-os/tgoskits/pull/1352))

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.2...starry-kernel-v0.6.3) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(starry)* enhance VmPeak & VmHWM  ([#1316](https://github.com/rcore-os/tgoskits/pull/1316))
- *(poll)* add irq-safe deferred notifications ([#1278](https://github.com/rcore-os/tgoskits/pull/1278))
- *(starry-kmod)* support LoongArch DMW-backed kmods ([#1279](https://github.com/rcore-os/tgoskits/pull/1279))
- *(ax-net)* add multi-interface support with per-interface routing, DNS, and SO_BINDTODEVICE ([#1244](https://github.com/rcore-os/tgoskits/pull/1244))
- *(starry)* implement execveat syscall ([#1144](https://github.com/rcore-os/tgoskits/pull/1144))
- runtime Wi-Fi AP/STA mode switch for AIC8800 on SG2002 (LicheeRV Nano) ([#1266](https://github.com/rcore-os/tgoskits/pull/1266))
- tpu add tdma irq. ([#1269](https://github.com/rcore-os/tgoskits/pull/1269))
- *(starry-kernel)* extend gdb support to aarch64 and loongarch64 ([#1247](https://github.com/rcore-os/tgoskits/pull/1247))
- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Fixed

- *(starry-signal)* populate siginfo.si_addr for synchronous SIGSEGV ([#1331](https://github.com/rcore-os/tgoskits/pull/1331))
- *(starry-kernel)* align x86 ptrace gdb support ([#1314](https://github.com/rcore-os/tgoskits/pull/1314))
- *(starry)* keep tmpfs directory cookies stable ([#1326](https://github.com/rcore-os/tgoskits/pull/1326))
- *(starry)* prepopulate cold user pages before copy ([#1328](https://github.com/rcore-os/tgoskits/pull/1328))
- *(starry-kernel)* filter console mouse escape reports ([#1302](https://github.com/rcore-os/tgoskits/pull/1302))
- *(starry)* assign the init process real PID 1 ([#1233](https://github.com/rcore-os/tgoskits/pull/1233))
- *(starry-kernel)* improve multiarch GDB ptrace support ([#1292](https://github.com/rcore-os/tgoskits/pull/1292))
- *(starry)* widen loongarch64 user VA window to 128 TiB (match aarch64/x86_64) ([#1280](https://github.com/rcore-os/tgoskits/pull/1280))
- *(starry)* report ENOSYS for the unimplemented new mount API ([#1241](https://github.com/rcore-os/tgoskits/pull/1241))
- *(starry)* map sg2002 tty serial MMIO via iomap ([#1270](https://github.com/rcore-os/tgoskits/pull/1270))
- *(starry)* align app qemu boot flow and own BPF JIT memory ([#1256](https://github.com/rcore-os/tgoskits/pull/1256))
- *(starry)* provide /sys/fs/cgroup mount point in sysfs ([#1243](https://github.com/rcore-os/tgoskits/pull/1243))

### Other

- Feat/gdb smoke x86 64 native ([#1330](https://github.com/rcore-os/tgoskits/pull/1330))
- Feat/x86 64 ptrace clean ([#1062](https://github.com/rcore-os/tgoskits/pull/1062))
- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))
- seccomp and capablities ([#1275](https://github.com/rcore-os/tgoskits/pull/1275))
- overlayfs ([#1223](https://github.com/rcore-os/tgoskits/pull/1223))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.1...starry-kernel-v0.6.2) - 2026-06-12

### Added

- *(starry)* add axbuild kmod support ([#1232](https://github.com/rcore-os/tgoskits/pull/1232))
- *(starry-mm)* file-backed mmap readahead (batched page-fault fill) ([#1217](https://github.com/rcore-os/tgoskits/pull/1217))
- *(axruntime)* add runtime IRQ registration adapters

### Fixed

- *(axtask)* improve might_sleep diagnostics and coverage ([#1235](https://github.com/rcore-os/tgoskits/pull/1235))
- *(axtask)* use monotonic deadlines for sleeps ([#1240](https://github.com/rcore-os/tgoskits/pull/1240))
- *(starry)* address fd table, wait, and timerfd regressions ([#1237](https://github.com/rcore-os/tgoskits/pull/1237))
- *(starry-kernel)* align membarrier commands with Linux UAPI ([#1225](https://github.com/rcore-os/tgoskits/pull/1225))

### Other

- *(starry)* rename axnet crate references to ax-net ([#1220](https://github.com/rcore-os/tgoskits/pull/1220))
- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.6.0...starry-kernel-v0.6.1) - 2026-06-11

### Added

- *(starry)* expose root block device /dev/vda + strengthen busybox applet tests ([#1213](https://github.com/rcore-os/tgoskits/pull/1213))
- *(orangepi-5-plus-uvc-rknn)* add RKNN bench validation ([#1189](https://github.com/rcore-os/tgoskits/pull/1189))

### Fixed

- *(starry)* avoid console size probe on dynamic platform
- *(starry)* Linux-compat foundational fixes (pseudofs/mm/syscall) + regression tests ([#1114](https://github.com/rcore-os/tgoskits/pull/1114))
- *(starry)* support eBPF ringbuf mmap on LoongArch DMW ([#1208](https://github.com/rcore-os/tgoskits/pull/1208))
- *(starry-mm)* bound file-backed mmap populate at EOF ([#1164](https://github.com/rcore-os/tgoskits/pull/1164))
- *(starry-kernel)* route legacy getrlimit/setrlimit through prlimit64 ([#1210](https://github.com/rcore-os/tgoskits/pull/1210))
- *(starry)* FIOCLEX/FIONCLEX ioctl + /proc status ctxt_switches + quiet non-tty ioctl probes ([#1168](https://github.com/rcore-os/tgoskits/pull/1168))
- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))
- *(starry-kernel)* stabilize Starry syscall CI tests ([#1209](https://github.com/rcore-os/tgoskits/pull/1209))

### Other

- *(starry)* load executables from a resolved Location instead of re-resolving the path ([#1193](https://github.com/rcore-os/tgoskits/pull/1193))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.13...starry-kernel-v0.6.0) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(starry-kernel)* detect fcntl lock deadlocks ([#1055](https://github.com/rcore-os/tgoskits/pull/1055))
- *(starry-proc)* add common /proc/sys and /proc/filesystems stub files ([#1121](https://github.com/rcore-os/tgoskits/pull/1121))
- *(starry-kernel)* improve GDB ptrace usability ([#1167](https://github.com/rcore-os/tgoskits/pull/1167))
- *(starry-kernel)* support futex WAKE_OP ([#1052](https://github.com/rcore-os/tgoskits/pull/1052))
- *(starry-kernel)* expose process memory stats via /proc ([#1171](https://github.com/rcore-os/tgoskits/pull/1171))
- *(starry-kernel)* implement io_uring lite ([#1042](https://github.com/rcore-os/tgoskits/pull/1042))
- *(starry-kernel)* implement TCP_INFO sockopt ([#1044](https://github.com/rcore-os/tgoskits/pull/1044))
- *(starry-kernel)* eBPF kernel runtime (tracepoint / kprobe / perf) ([#886](https://github.com/rcore-os/tgoskits/pull/886))
- *(backtrace)* add showcase workflow ([#1094](https://github.com/rcore-os/tgoskits/pull/1094))
- *(starry-kernel)* support waitid P_PIDFD ([#1051](https://github.com/rcore-os/tgoskits/pull/1051))
- *(starry-kernel)* add unshare, procfs namespace files, and claw-code tests ([#1031](https://github.com/rcore-os/tgoskits/pull/1031))
- *(vfs)* pass uid/gid through creation path to filesystem nodes ([#1097](https://github.com/rcore-os/tgoskits/pull/1097))

### Fixed

- *(starry)* reject closing invalid file descriptors
- *(axcpu)* preserve loongarch64 LASX state for Git HTTPS ([#1178](https://github.com/rcore-os/tgoskits/pull/1178))
- *(starry-net)* epoll_pwait user-buffer alignment + netlink MSG_PEEK/TRUNC/DONTWAIT (Go network servers) ([#921](https://github.com/rcore-os/tgoskits/pull/921))
- *(starry-mm)* reject overflowing addr+length in mmap instead of wrapping ([#1120](https://github.com/rcore-os/tgoskits/pull/1120))
- *(starry-mm)* make mlock fault the range in and report ENOMEM on holes ([#1122](https://github.com/rcore-os/tgoskits/pull/1122))
- complete io_destroy ([#1165](https://github.com/rcore-os/tgoskits/pull/1165))
- *(locking)* narrow spinlock scope in VFS and Starry paths ([#1146](https://github.com/rcore-os/tgoskits/pull/1146))
- *(axcpu-aarch64)* emulate EL0 MRS reads of ID_AA64* feature registers ([#1128](https://github.com/rcore-os/tgoskits/pull/1128))
- *(starry-mm)* mprotect returns ENOMEM on unmapped holes within the range ([#918](https://github.com/rcore-os/tgoskits/pull/918))
- *(starry-net)* accept oversized addrlen in netlink bind/connect ([#1119](https://github.com/rcore-os/tgoskits/pull/1119))
- *(starry-ipc)* correct ShmidDs layout to match Linux shmid64_ds ([#1118](https://github.com/rcore-os/tgoskits/pull/1118))
- *(lockdep)* resolve Starry lock ordering and log print issues ([#1103](https://github.com/rcore-os/tgoskits/pull/1103))
- *(starry,nginx)* multi-worker signal interruption and EPOLLEXCLUSIVE handling ([#1018](https://github.com/rcore-os/tgoskits/pull/1018))

### Other

- Merge pull request #1147 from 1301182193/feat/debian_MySQL

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.12...starry-kernel-v0.5.13) - 2026-06-03

### Added

- *(starry-kernel)* support `FUTEX_WAKE_OP` in Starry futex syscall handling.
- *(starry-kernel)* port LKM loader + cargo xtask starry kmod build ([#851](https://github.com/rcore-os/tgoskits/pull/851))
- *(starryos)* expose K230 KPU device ([#1054](https://github.com/rcore-os/tgoskits/pull/1054))
- *(starry-kernel)* implement child subreaper ([#1050](https://github.com/rcore-os/tgoskits/pull/1050))
- *(starry-kernel)* support waitid P_PGID ([#1032](https://github.com/rcore-os/tgoskits/pull/1032))
- *(starry-kernel)* implement xattr store ([#1040](https://github.com/rcore-os/tgoskits/pull/1040))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(mm)* add page reclaim for file-backed memory pressure (rebased) ([#1007](https://github.com/rcore-os/tgoskits/pull/1007))
- *(drm)* per-buffer dumb allocation with GEM-refcounted mmap pages ([#514](https://github.com/rcore-os/tgoskits/pull/514))
- *(starry-kernel)* port eBPF runtime (ebpf/, perf/, kprobe wiring) ([#850](https://github.com/rcore-os/tgoskits/pull/850))
- *(Starry)* support MariaDB ([#906](https://github.com/rcore-os/tgoskits/pull/906))
- *(starry-kernel)* support cgroup2 hierarchy mkdir and rmdir ([#1015](https://github.com/rcore-os/tgoskits/pull/1015))
- *(axtask)* replace PREV_TASK Weak<AxTask> with raw pointer ([#996](https://github.com/rcore-os/tgoskits/pull/996))
- *(starry-kernel)* add initial cgroup2 support ([#989](https://github.com/rcore-os/tgoskits/pull/989))
- *(starry-kernel)* add initial GDB ptrace support ([#931](https://github.com/rcore-os/tgoskits/pull/931))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(starry-kernel)* add LKM support via kmod-loader integration ([#849](https://github.com/rcore-os/tgoskits/pull/849))
- *(starry-kernel)* add eBPF subsystem (maps, VM, helpers, perf events) ([#848](https://github.com/rcore-os/tgoskits/pull/848))
- *(starry-kernel)* add kprobe support ([#847](https://github.com/rcore-os/tgoskits/pull/847))
- *(starry-task)* implement sys_getcpu ([#924](https://github.com/rcore-os/tgoskits/pull/924))
- *(starry-kernel)* add inotifywait support ([#894](https://github.com/rcore-os/tgoskits/pull/894))
- *(starry)* add userspace test for prlimit64 syscall ([#801](https://github.com/rcore-os/tgoskits/pull/801))
- *(ax-net)* implement SO_TYPE, SO_PROTOCOL, SO_DOMAIN socket options ([#884](https://github.com/rcore-os/tgoskits/pull/884))
- *(starry-kernel)* implement xattr syscall stubs for rsext4 ([#882](https://github.com/rcore-os/tgoskits/pull/882))
- *(starry-kernel)* add /proc/self/statm and /proc/loadavg, add procps test ([#853](https://github.com/rcore-os/tgoskits/pull/853))
- *(starry)* expose dumpable in procfs status and complete uid/gid tests ([#757](https://github.com/rcore-os/tgoskits/pull/757))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))
- *(starryos)* add OpenSSH app test and implement PR_SET_NO_NEW_PRIVS ([#810](https://github.com/rcore-os/tgoskits/pull/810))
- *(starry-kernel)* implement waitid syscall ([#781](https://github.com/rcore-os/tgoskits/pull/781))

### Fixed

- *(starry-kernel)* avoid patching cratesio ax-errno ([#1081](https://github.com/rcore-os/tgoskits/pull/1081))
- *(kernel)* fix riscv64 static-pie segfault in ELF loader ([#1033](https://github.com/rcore-os/tgoskits/pull/1033))
- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))
- *(starry-kernel)* validate sync_file_range flags and offsets ([#823](https://github.com/rcore-os/tgoskits/pull/823))
- *(starry-net)* SIOCGIFINDEX + non-zero SIOCGIFCONF sizing for OpenJDK NetworkInterface ([#923](https://github.com/rcore-os/tgoskits/pull/923))
- *(loongarch64)* make userspace LSX usable (preserve FP/LSX state + fix uc_mcontext offset + advertise AT_HWCAP) ([#917](https://github.com/rcore-os/tgoskits/pull/917))
- *(axbuild)* skip disabled grouped C subcases ([#942](https://github.com/rcore-os/tgoskits/pull/942))
- *(starry-kernel,x86-qemu-q35)* probe terminal size, deliver SIGWINCH, batch ONLCR writes ([#913](https://github.com/rcore-os/tgoskits/pull/913))
- *(starry-task)* suspend on SIGSTOP instead of killing (job control) ([#925](https://github.com/rcore-os/tgoskits/pull/925))
- *(starry-mm)* fix use-after-free when evicting a page-cache page shared across split file mappings ([#920](https://github.com/rcore-os/tgoskits/pull/920))
- *(starry)* align mount and umount2 semantics with Linux ([#876](https://github.com/rcore-os/tgoskits/pull/876))
- *(starry)* repair SG2002 CI build ([#929](https://github.com/rcore-os/tgoskits/pull/929))
- *(starry-kernel)* add Threads: line to /proc/[pid]/status and implement /proc/[pid]/statm ([#915](https://github.com/rcore-os/tgoskits/pull/915))
- *(starry-kernel)* copy under-aligned epoll_event byte-wise (fixes Go netpoll EFAULT) ([#914](https://github.com/rcore-os/tgoskits/pull/914))
- *(starry-kernel)* close EPOLLET race window and NoEvent busy-loop ([#910](https://github.com/rcore-os/tgoskits/pull/910))
- *(starry-kernel)* handle COW write faults from kernel-mode user-memory writes ([#909](https://github.com/rcore-os/tgoskits/pull/909))
- *(starry-kernel)* align file sync syscalls with Linux semantics ([#903](https://github.com/rcore-os/tgoskits/pull/903))
- *(epoll,sigmask-related)* align sigsetsize checks with linux abi ([#900](https://github.com/rcore-os/tgoskits/pull/900))
- *(starry-kernel)* correct splice error handling ([#896](https://github.com/rcore-os/tgoskits/pull/896))
- *(starry)* snapshot thread context in file syscalls ([#885](https://github.com/rcore-os/tgoskits/pull/885))
- *(starry)* avoid teardown usercopy from kernel tasks ([#878](https://github.com/rcore-os/tgoskits/pull/878))
- *(signal)* add wake_task after signal delivery and dumpable/no_new_privs fields ([#797](https://github.com/rcore-os/tgoskits/pull/797))
- *(starry-kernel)* support TCP socket FIONREAD ([#869](https://github.com/rcore-os/tgoskits/pull/869))
- *(starry)* preserve vfork parent blocking ([#693](https://github.com/rcore-os/tgoskits/pull/693))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))
- *(starry)* expose SMP CPU topology in sysfs ([#842](https://github.com/rcore-os/tgoskits/pull/842))
- *(starry)* implement conservative riscv hwprobe ([#843](https://github.com/rcore-os/tgoskits/pull/843))
- *(starry-kernel)* pidfd open/getfd/send_signal Linux conformance ([#707](https://github.com/rcore-os/tgoskits/pull/707))
- *(busybox-run_parts)* retry non-ELF executables via /bin/sh in execve ([#517](https://github.com/rcore-os/tgoskits/pull/517))

### Other

- namespace实现 ([#981](https://github.com/rcore-os/tgoskits/pull/981))
- *(ci)* bump Rust toolchain to nightly-2026-05-28 and fix clippy ([#1027](https://github.com/rcore-os/tgoskits/pull/1027))
- *(starry-kernel)* add memtrack alloc backtrace e2e ([#1020](https://github.com/rcore-os/tgoskits/pull/1020))
- *(sched)* add sched-family test suite and fix kernel scheduler sys… ([#986](https://github.com/rcore-os/tgoskits/pull/986))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- replace SpinNoIrq with ax_sync::Mutex to allow sleeping during init ([#964](https://github.com/rcore-os/tgoskits/pull/964))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))
- Adds support for kernel symbol dumping via kallsyms ([#837](https://github.com/rcore-os/tgoskits/pull/837))
- Remove ARM PL011 UART driver and integrate DesignWare APB UART support ([#965](https://github.com/rcore-os/tgoskits/pull/965))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Fix/starryos： 完善了StarryOS的sys_membarrier和sys_rseq两个系统调用。 ([#897](https://github.com/rcore-os/tgoskits/pull/897))
- Test socket syscalls ([#871](https://github.com/rcore-os/tgoskits/pull/871))
- *(syscall)* add test-shm-family shm syscall conformance suite ([#865](https://github.com/rcore-os/tgoskits/pull/865))
- *(starryos)* add capget syscall test and fix NULL datap handling ([#863](https://github.com/rcore-os/tgoskits/pull/863))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))
- *(syscall)* add regression tests for StarryOS signal extension syscalls and fixup ([#806](https://github.com/rcore-os/tgoskits/pull/806))
- *(syscall)* add comprehensive select/poll/pselect6/ppoll deep test suite ([#679](https://github.com/rcore-os/tgoskits/pull/679))

### Added

- *(waitid)* support `P_PIDFD` targets for process pidfds.
- *(prctl)* implement `PR_SET_CHILD_SUBREAPER` and `PR_GET_CHILD_SUBREAPER` with orphan reparenting tests.

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.11...starry-kernel-v0.5.12) - 2026-05-22

### Added

- add sg2002 USB UVC camera with ESP-compatible ioctl ([#791](https://github.com/rcore-os/tgoskits/pull/791))
- *(starry)* add utimensat test case and fix kernel bugs ([#763](https://github.com/rcore-os/tgoskits/pull/763))
- *(drm)* per-buffer memory allocation for Weston bringup ([#667](https://github.com/rcore-os/tgoskits/pull/667))
- *(starry)* support multi-threaded execve ([#273](https://github.com/rcore-os/tgoskits/pull/273))

### Fixed

- *(starry-kernel)* open/openat deep — 6 类跨子系统改造 (stacked on #719) ([#720](https://github.com/rcore-os/tgoskits/pull/720))
- *(ax-task)* migrate tasks after affinity updates ([#825](https://github.com/rcore-os/tgoskits/pull/825))
- *(starry-kernel)* open/openat 15 类局部修复 ([#719](https://github.com/rcore-os/tgoskits/pull/719))
- *(starry-kernel)* handle tty cursor position report ([#776](https://github.com/rcore-os/tgoskits/pull/776))
- *(starry)* sys_sendfile
- *(starry-kernel)* PR_SET/GET_DUMPABLE + setuid 自动清 ([#718](https://github.com/rcore-os/tgoskits/pull/718))

### Other

- Add kernel tracepoint infrastructure and debugfs integration ([#673](https://github.com/rcore-os/tgoskits/pull/673))
- *(starry)* add signalfd4 test case, fix ssi_pid/ssi_uid ([#683](https://github.com/rcore-os/tgoskits/pull/683))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/starry-kernel-v0.5.10...starry-kernel-v0.5.11) - 2026-05-19

### Added

- *(starry-kernel)* add anonymous memfd, seals, and pidfd tests ([#565](https://github.com/rcore-os/tgoskits/pull/565))

### Fixed

- *(starry)* tolerate robust futex cleanup faults ([#692](https://github.com/rcore-os/tgoskits/pull/692))
- *(net)* correct UDP sendto/recvfrom/sendmsg/recvmsg semantics to match Linux ABI ([#598](https://github.com/rcore-os/tgoskits/pull/598))
- *(starry-kernel)* MAP_FIXED failure preserves prior mapping ([#691](https://github.com/rcore-os/tgoskits/pull/691))
- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))
- *(starry)* reject invalid umount2 flags ([#699](https://github.com/rcore-os/tgoskits/pull/699))
- *(starry)* support v4-mapped IPv6 sockets ([#694](https://github.com/rcore-os/tgoskits/pull/694))

### Other

- Merge branch 'pr-717' into dev
- *(starry)* add uname/sysinfo coverage and minimal syslog syscall support ([#705](https://github.com/rcore-os/tgoskits/pull/705))

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
- *(ax-net)* add ICMP raw socket support ([#368](https://github.com/rcore-os/tgoskits/pull/368))
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
