# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.6.0...usb-if-v0.7.0) - 2026-04-30

### Other

- Introduce Queue-Based Transfer API and Unified Endpoint Model ([#71](https://github.com/drivercraft/CrabUSB/pull/71))

## [0.6.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.5.1...usb-if-v0.6.0) - 2026-04-29

### Fixed

- *(xhci)* set chain bit on ISO TRBs and preserve raw config descriptor ([#69](https://github.com/drivercraft/CrabUSB/pull/69))

## [0.5.1](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.5.0...usb-if-v0.5.1) - 2026-01-28

### Other

- ♻️ refactor(hub): remove unused RouteString and clean up HubParams structure

## [0.5.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.4.0...usb-if-v0.5.0) - 2026-01-27

### Other

- ♻️ refactor(uvc): improve error handling and simplify UVC implementation
- Refactor USB Host Backend and Implement LibUSB Support
- update error handling to use anyhow for better error context and simplify imports across modules
- rename DeviceSpeed to Speed for consistency across modules
- update DeviceSpeed enum and simplify its usage across modules
- [fix] hub on real world works ([#52](https://github.com/drivercraft/CrabUSB/pull/52))
- [feat] add hub support ([#51](https://github.com/drivercraft/CrabUSB/pull/51))

## [0.4.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.3.2...usb-if-v0.4.0) - 2026-01-16

### Other

- [refactor] api improve ([#47](https://github.com/drivercraft/CrabUSB/pull/47))

## [0.3.2](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.3.1...usb-if-v0.3.2) - 2025-08-25

### Other

- update Rust toolchain version and enhance logging in USB handling

## [0.3.1](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.3.0...usb-if-v0.3.1) - 2025-08-12

### Other

- enhance error handling in libusb context and error modules ([#26](https://github.com/drivercraft/CrabUSB/pull/26))

## [0.3.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.2.2...usb-if-v0.3.0) - 2025-08-08

### Other

- Dev uvc ([#17](https://github.com/drivercraft/CrabUSB/pull/17))

## [0.2.2](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.2.1...usb-if-v0.2.2) - 2025-08-07

### Fixed

- transfer ring

### Other

- Merge branch 'main' of github.com:drivercraft/CrabUSB
- keyboard

## [0.2.1](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.2.0...usb-if-v0.2.1) - 2025-08-07

### Added

- libusb transfer ok

## [0.2.0](https://github.com/drivercraft/CrabUSB/compare/usb-if-v0.1.0...usb-if-v0.2.0) - 2025-08-05

### Other

- ci ([#11](https://github.com/drivercraft/CrabUSB/pull/11))
