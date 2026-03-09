---
name: bare-test-driver-tests
description: "Use when creating, restoring, or debugging a bare-test driver test in this repository; covers cargo t no_std tests, QEMU device setup, phased recovery, runner config, and link-script issues."
---

# Bare-Test Driver Tests

Use this skill when a driver test in this repository runs through bare-test and `cargo t`, especially for AArch64 `target_os = none` cases.

## Goals

- Create a new bare-test driver test that boots reliably.
- Debug why a no_std driver test hangs before or during device initialization.
- Restore a broken driver test in phases instead of reintroducing the full test body at once.

## Workflow

### 1. Confirm the runner path

- Run the test with `cargo t`, not plain `cargo test`, when comparing with existing bare-test flows.
- Treat `.cargo/config.toml` and `.cargo/config-test.toml` as the primary source of test runner and link settings.
- Avoid per-crate `build.rs` test link injection unless it is strictly required and verified not to duplicate global test link args.

### 2. Start from the smallest passing test

- Use the same skeleton as `simple_bare_test`: `#![no_std]`, `#![no_main]`, `#[bare_test::tests]`, one smoke test, one timeout-protected test.
- First success criterion: serial output reaches `begin test` and `All tests passed`.

### 3. Put runtime environment in the right file

- Put QEMU runtime args in `.qemu.toml` for the test package.
- Do not assume `bare-test.toml` is consumed by `cargo t`.
- Put device attachments, success regex, fail regex, and image paths in `.qemu.toml`.
- Use package-relative paths that remain valid from the package directory, for example `../../../target/...`.

### 4. Restore the driver test in phases

Restore one layer at a time. After each layer, run `cargo t ... -- --show-output` and confirm the last printed marker.

1. Framework boot only.
2. Platform descriptor and FDT parsing.
3. PCIe host bridge discovery and range setup.
4. Endpoint enumeration and target device discovery.
5. Device initialization, such as `Nvme::new`.
6. Admin-path queries, such as namespace enumeration.
7. Data-path verification, such as block read and write loops.

### 5. Add guardrails at each phase

- Add a `#[timeout = ...]` on every test that touches hardware discovery or I/O.
- Print a unique marker before each major step so the last visible line identifies the hang point.
- Keep each phase small enough that a timeout points to one subsystem, not an entire integration stack.

## Decision Points

- If the test never prints `begin test`, inspect link setup, runner config, and duplicate build-script link args first.
- If the test boots but does not see the device, inspect `.qemu.toml` device args and relative file paths.
- If enumeration works but init hangs, stop and isolate the constructor path in its own timeout-protected test.
- If admin commands work but data I/O hangs, isolate one block read or write before restoring loops.

## Completion Checks

- The minimal framework test passes with `cargo t`.
- The QEMU command line contains the expected attached devices.
- The test prints phase markers in order without silent hangs.
- Each restored phase either passes or fails at a single well-identified step.
- Full I/O logic is restored only after discovery and controller init are stable.

## Anti-Patterns

- Do not restore the full driver integration test in one edit.
- Do not rely on `bare-test.toml` alone for QEMU devices when using `cargo t`.
- Do not keep a crate-local `build.rs` that duplicates global test link configuration unless you have verified the merged rustc args.

## Example Prompts

- Create a new bare-test for a PCIe driver in this repo and stage it from boot smoke test to device enumeration.
- Debug why `cargo t -p my-driver --target aarch64-unknown-none-softfloat` hangs before any serial output.
- Restore this bare-test in phases and stop after controller discovery.
