# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.7.1...ax-net-ng-v0.7.2) - 2026-06-11

### Other

- updated the following local packages: ax-config, ax-hal, ax-hal, ax-task, ax-fs-ng, ax-sync

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.7.0...ax-net-ng-v0.7.1) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(starry-kernel)* implement TCP_INFO sockopt ([#1044](https://github.com/rcore-os/tgoskits/pull/1044))
- *(vfs)* pass uid/gid through creation path to filesystem nodes ([#1097](https://github.com/rcore-os/tgoskits/pull/1097))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.6.1...ax-net-ng-v0.7.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(axnet-ng)* implement SO_TYPE, SO_PROTOCOL, SO_DOMAIN socket options ([#884](https://github.com/rcore-os/tgoskits/pull/884))

### Fixed

- *(ax-net-ng)* drain entire ARP-pending queue, lift cache TTL and capacity ([#911](https://github.com/rcore-os/tgoskits/pull/911))
- *(starry-kernel)* support TCP socket FIONREAD ([#869](https://github.com/rcore-os/tgoskits/pull/869))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.6.0...ax-net-ng-v0.6.1) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axfs-ng-vfs, ax-hal, ax-task, ax-fs-ng, ax-io, ax-driver, ax-sync

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.5.13...ax-net-ng-v0.6.0) - 2026-05-19

### Fixed

- *(net)* correct UDP sendto/recvfrom/sendmsg/recvmsg semantics to match Linux ABI ([#598](https://github.com/rcore-os/tgoskits/pull/598))
- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))
- *(unix-stream,poll_io)* non-blocking accept, peer EOF, waker registration ([#697](https://github.com/rcore-os/tgoskits/pull/697))

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.5.12...ax-net-ng-v0.5.13) - 2026-05-15

### Added

- *(realtek-rtl8125)* complete OrangePi board bringup ([#404](https://github.com/rcore-os/tgoskits/pull/404))
- *(ax-net-ng)* add ICMP raw socket support ([#368](https://github.com/rcore-os/tgoskits/pull/368))
- *(net)* migrate ax-net to crates.io smoltcp ([#410](https://github.com/rcore-os/tgoskits/pull/410))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))

### Fixed

- *(ax-net-ng)* route connected socket wakeups by peer ([#583](https://github.com/rcore-os/tgoskits/pull/583))
- *(starry-kernel)* close CLI compatibility gaps ([#524](https://github.com/rcore-os/tgoskits/pull/524))
- UDP recv returns EAGAIN on unconnected socket, sendto dispatches loopback ([#529](https://github.com/rcore-os/tgoskits/pull/529))
- *(proc)* expose arp table for busybox arp ([#480](https://github.com/rcore-os/tgoskits/pull/480))
- *(axnet-ng)* call poll_interfaces() after TCP send to wake epoll waiters ([#485](https://github.com/rcore-os/tgoskits/pull/485))
- *(arceos)* adjust dynamic platform and network integration
- implement close_all_fds function and enhance pipe and syscall handling ([#305](https://github.com/rcore-os/tgoskits/pull/305))

### Other

- Adds a StarryOS YOLOv8 UVC camera demo for OrangePi 5 Plus with RKNN/NPU inference and HTTP MJPEG streaming. ([#574](https://github.com/rcore-os/tgoskits/pull/574))
- 增强 ArceOS 中 VirtIO Net、Vsock 及通用探测路径 ([#376](https://github.com/rcore-os/tgoskits/pull/376))
- separate TCP and UDP bind checks ([#543](https://github.com/rcore-os/tgoskits/pull/543))
- *(kernel)* remove unused user interpreter base constants and clean up socket handling ([#421](https://github.com/rcore-os/tgoskits/pull/421))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-net-ng-v0.5.11...ax-net-ng-v0.5.12) - 2026-04-27

### Fixed

- *(net)* return EOF on unix stream recv when peer sender dropped ([#311](https://github.com/rcore-os/tgoskits/pull/311))
