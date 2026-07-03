# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-errno-v0.6.0...ax-errno-v0.6.1) - 2026-07-02

### Other

- *(build)* generate build.rs Rust sources with quote ([#1422](https://github.com/rcore-os/tgoskits/pull/1422))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-errno-v0.5.0...ax-errno-v0.6.0) - 2026-05-22

### Fixed

- *(starry-kernel)* open/openat deep — 6 类跨子系统改造 (stacked on #719) ([#720](https://github.com/rcore-os/tgoskits/pull/720))

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/ax-errno-v0.4.8...ax-errno-v0.5.0) - 2026-05-19

### Fixed

- *(net)* correct UDP sendto/recvfrom/sendmsg/recvmsg semantics to match Linux ABI ([#598](https://github.com/rcore-os/tgoskits/pull/598))
