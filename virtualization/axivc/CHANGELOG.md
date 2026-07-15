# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-15

### Added

- Initial `axivc` crate for AxVisor inter-VM shared-memory communication.
- Added fixed shared-memory region layout and two SPSC message rings.
- Added request and acknowledgement message helpers.
- Added peer-event wait helpers for IRQ wakeup with bounded fallback polling.
- Added English and Chinese README files, Apache-2.0 license text, and crate
  local ignore rules.
