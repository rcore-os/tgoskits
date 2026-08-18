# axtest

A white-box unit testing framework for bare-metal `#![no_std]` environments in the ArceOS ecosystem (ArceOS, StarryOS, Axvisor).

Tests are registered at compile time via linker sections — no dynamic allocation, no runtime discovery. Results are reported in KTAP (Kernel Test Anything Protocol) format.

Use axtest only when the assertion needs QEMU or a physical board: target-architecture behavior, IRQ/preemption/per-CPU state, kernel scheduling, real devices, or a kernel-owned filesystem. Algorithms, parsers, state machines, validation, formatting, and other deterministic host-compatible code must use ordinary Rust `#[test]` cases and `cargo test` (or `cargo xtask test`). A crate may use both styles after splitting at that boundary.

Entry points are already configured in all three OSes. This guide covers how to write and run tests.

## Running Tests

```bash
# Every workspace crate with a direct axtest dev-dependency
cargo xtask ktest qemu

# Only execution units that support x86_64
cargo xtask ktest qemu --workspace --arch x86_64

# StarryOS kernel
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64

# Axvisor
cargo xtask ktest qemu -p axvisor --test axtest --arch x86_64

# SG2002 board-only test
cargo xtask ktest board -p arceos-axtest-sg2002-usb-msc \
  --test axtest -b aka-00-sg2002
```

## Writing Test Cases

After confirming that the case cannot run under the standard Rust harness, add a path-only direct `axtest` dev-dependency, then declare a
`harness = false` Cargo test target. Keep axtest sources under `tests/`:

```toml
[features]
axtest = []

[dev-dependencies]
axtest = { path = "<relative-path>/components/axtest/axtest" }
ax-hal = { path = "<relative-path>/os/arceos/modules/axhal" }
ax-std = { path = "<relative-path>/os/arceos/ulib/axstd" }

[[test]]
name = "axtest"
path = "tests/axtest.rs"
harness = false
required-features = ["axtest"]
```

Adjust each path relative to the package manifest. Workspace-internal
dev-dependencies must not specify `version` or use `workspace = true`, because
Cargo keeps versioned dev-dependencies in the published manifest and can then
reject the workspace's test-only dependency cycles. Path-only dev-dependencies
are stripped during publication. Registry dev-dependencies may still specify a
version normally.

The test file only needs the axtest module and test cases:

```rust
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ax_std as _;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn it_works() {
        ax_assert_eq!(1 + 1, 2);
    }
}
```

The `#[axtest::tests]` macro registers every `#[test]` function in the inline
module and generates the kernel test entry point. The entry point configures
printing, runs the suite, emits `AXTEST_SUITE_OK` / `AXTEST_SUITE_FAIL`, dumps
coverage when enabled, and powers the target off on success.

### Basic Test

No explicit return needed — `AxTestResult::Ok` is appended automatically on success:

```rust
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn basic() {
        ax_assert_eq!(2 + 2, 4);
    }
}
```

### Explicit Return

Return `AxTestResult` for fine-grained control:

```rust
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn with_result() -> axtest::AxTestResult {
        let value = some_computation();
        ax_assert!(value > 0);
        axtest::AxTestResult::Ok
    }
}
```

### Assertions

Three assertion macros are provided, all `#![no_std]` compatible. They return `AxTestResult::Failed` on failure (no panic):

| Macro | Usage |
|---|---|
| `ax_assert!(cond)` | Assert condition is true |
| `ax_assert_eq!(left, right)` | Assert equality |
| `ax_assert_ne!(left, right)` | Assert inequality |

Each accepts an optional format message:

```rust
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn assertions() {
        ax_assert!(true);
        ax_assert_eq!(1 + 1, 2, "basic math failed");
        ax_assert_ne!(a, b, "a should not equal b: {}", a);
    }
}
```

### Skipping Tests

```rust
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    #[ignore]
    fn not_ready_yet() { /* ... */ }

    #[test]
    #[ignore = "requires hardware device X"]
    fn hw_dependent() { /* ... */ }
}
```

### Expected Failure

```rust
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    #[should_panic]
    fn expected_to_fail() {
        ax_assert!(false);
    }
}
```

### Advanced: Custom Executor

Use the lower-level `#[axtest]` attribute when a test needs a named executor or
when the entry point is custom. Bind a test to a named executor after registering
it via `add_executor` in the entry point:

```rust
#[axtest(custom = "thread")]
fn threaded_test() {
    ax_assert!(true);
}
```

## Module Hooks

Use `#[def_mod]` to define per-module setup/teardown hooks. The module is automatically gated with `#[cfg(axtest)]`.

```rust
#[def_mod]
mod integration {
    use axtest::prelude::*;

    fn axtest_init(_desc: axtest::AxTestDescriptor) {
        // runs before each test in this module
    }

    fn axtest_exit(_desc: axtest::AxTestDescriptor) {
        // runs after each test in this module
    }

    #[axtest]
    fn test_with_setup() {
        ax_assert!(true);
    }
}
```

Both `axtest_init` and `axtest_exit` are optional — define only what you need.

## Advanced: Custom Executor

Implement the `AxTestExecutor` trait to run tests with a custom strategy (e.g., in a separate thread):

```rust
use axtest::{AxTestExecutor, AxTestResult};

#[derive(Default)]
struct ThreadExecutor;

impl AxTestExecutor for ThreadExecutor {
    fn name(&self) -> &'static str { "thread" }

    fn run(&self, test_fn: fn() -> AxTestResult) -> Result<AxTestResult, &'static str> {
        // spawn thread, run test_fn, join, return result
        Ok(test_fn())
    }
}
```

Register it in the entry point:

```rust
let summary = axtest::init()
    .add_executor(ThreadExecutor)
    .run_tests();
```

### Builder API

`axtest::init()` returns an `AxTestInitBuilder` with the following methods:

| Method | Description |
|---|---|
| `.add_executor(executor)` | Register a named executor |
| `.set_default(executor)` | Set the default executor |
| `.set_default_by_name("name")` | Set default executor by name |
| `.with_filter(&["crate_a", "crate_b"])` | Only run tests from specified crates |
| `.set_printer(fn)` | Set the output printer function |
| `.run_tests()` | Execute all tests and return `TestSummary` |

## How It Works

1. `#[axtest::tests]` converts module-local `#[test]` functions into `AxTestDescriptor`s in the `.axtest_array` linker section
2. The linker script collects all descriptors into a contiguous array
3. The generated entry point calls `axtest::run_kernel_tests()`
4. Results are printed in KTAP format with machine-parseable markers

```
AXTEST_BEGIN total=2
KTAP version 1
1..2
ok 1 my_tests::it_works
AXTEST_CASE status=pass module=my_tests name=it_works
ok 2 my_tests::explicit_result
AXTEST_CASE status=pass module=my_tests name=explicit_result
AXTEST_SUMMARY pass=2 fail=0 skip=0 total=2
```

### Build Integration

The `ktest` command builds kernel/QEMU/board tests as Cargo `[[test]]` targets with `harness = false`.
It discovers workspace packages through a direct axtest dev-dependency, expands each selected test target into one QEMU run per supported architecture, and injects `--cfg axtest` via `CARGO_ENCODED_RUSTFLAGS`:

```bash
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64
cargo xtask ktest qemu -p axvisor --test axtest --arch x86_64
```

The default runtime is ArceOS. StarryOS, Axvisor, and board-only packages declare
`[package.metadata.axtest] runtime = "starry|axvisor|board"`. Supported QEMU
architectures come from `[package.metadata.docs.rs].targets`; packages without
that metadata run only on x86_64. `--arch` filters a workspace plan to packages
that support the requested architecture.

QEMU configs continue to own platform arguments, timeout, `to_bin`, UEFI, devices, and rootfs. The ktest runner additionally requires `AXTEST_SUITE_OK` and rejects panic, suite-failure, or case-failure markers.

## Coverage

axtest supports LLVM source-based coverage via [xcover](https://crates.io/crates/xcover). The guest serializes `.profraw` data into memory, and the host extracts it through QEMU's monitor interface.

### Running with Coverage

Pass `--coverage` to `ktest qemu`:

```bash
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64 --coverage
```

Pass `--out-fmt html` together with `--coverage` to generate an HTML report automatically:

```bash
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64 --coverage --out-fmt html
```

The build tool will automatically:
1. Add the `axtest/coverage` Cargo feature
2. Inject `--cfg axtest_coverage`, `-Cinstrument-coverage`, `-Zno-profiler-runtime` into rustflags
3. Set up a QEMU monitor socket for memory extraction
4. Generate `<workspace>/coverage/<package>-<test>-<target>.profdata` and `<workspace>/coverage/<package>-<test>-<target>-html/index.html` when `--out-fmt html` is set

### How It Works

```
Guest                              Host (axbuild)
─────                              ──────────────
tests pass
  │
  ▼
axtest::dump_coverage()
  ├─ xcover::write_profraw(Vec)    capture guard scans stdout
  │   serializes LLVM profraw        │
  │   into guest memory              │
  └─ prints marker:                 parses marker, extracts addr/size
     AXTEST_COVERAGE status=ready      │
     addr=0x... size=...               ▼
                                    connects to QEMU monitor
                                    sends: memsave <addr> <size> <path>
                                      │
                                      ▼
                                    <path>/coverage.profraw saved
```

`dump_coverage()` is already called in all three OS entry points (ArceOS, StarryOS, Axvisor). No changes needed in test code.

### Using the Profraw File

The `.profraw` file is saved to `<workspace>/coverage/<package>-<test>-<target>.profraw`.

Convert and generate reports with standard LLVM tools:

```bash
# Merge profraw into profdata
llvm-profdata merge -sparse coverage.profraw -o coverage.profdata

# Generate HTML report
llvm-cov show target/<target>/debug/<binary> \
  -instr-profile=coverage.profdata \
  -format=html -output-dir=coverage-report

# Print summary
llvm-cov report target/<target>/debug/<binary> \
  -instr-profile=coverage.profdata
```

### Requirements

- **Unix host** — coverage capture uses QEMU monitor via Unix domain socket
- **Both flags** — `AXTEST=y` and `AXTEST_COVERAGE=y` must be set
- **Entry point** — `axtest::dump_coverage()` must be called after tests (already done in all OS entry points)

## Output Format

axtest emits KTAP-compatible output with additional sentinel lines for CI parsing:

| Line | Meaning |
|---|---|
| `AXTEST_BEGIN total=N` | Test session started, N tests discovered |
| `AXTEST_CASE status=pass\|fail\|skip module=M name=N` | Per-test result |
| `AXTEST_SUMMARY pass=P fail=F skip=S total=T` | Final summary |
