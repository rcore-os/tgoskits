# axtest

A white-box unit testing framework for bare-metal `#![no_std]` environments in the ArceOS ecosystem (ArceOS, StarryOS, Axvisor).

Tests are registered at compile time via linker sections — no dynamic allocation, no runtime discovery. Results are reported in KTAP (Kernel Test Anything Protocol) format.

Entry points are already configured in all three OSes. This guide covers how to write and run tests.

## Running Tests

```bash
# ArceOS
cargo xtask arceos test qemu --test-group axtest --arch aarch64

# StarryOS
cargo xtask starry test qemu --test-group axtest --arch x86_64

# Axvisor
cargo xtask axvisor test qemu --test-group axtest --arch x86_64

# List available test cases
cargo xtask arceos test qemu --test-group axtest --list
```

## Writing Test Cases

Add `axtest.workspace = true` to your crate's `Cargo.toml`, then write tests gated with `#[cfg(axtest)]`:

```rust
#[cfg(axtest)]
mod my_tests {
    use axtest::prelude::*;

    #[axtest]
    fn it_works() {
        ax_assert_eq!(1 + 1, 2);
    }
}
```

### Basic Test

No explicit return needed — `AxTestResult::Ok` is appended automatically on success:

```rust
#[axtest]
fn basic() {
    ax_assert_eq!(2 + 2, 4);
}
```

### Explicit Return

Return `AxTestResult` for fine-grained control:

```rust
#[axtest]
fn with_result() -> axtest::AxTestResult {
    let value = some_computation();
    ax_assert!(value > 0);
    axtest::AxTestResult::Ok
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
#[axtest]
fn assertions() {
    ax_assert!(true);
    ax_assert_eq!(1 + 1, 2, "basic math failed");
    ax_assert_ne!(a, b, "a should not equal b: {}", a);
}
```

### Skipping Tests

```rust
#[axtest]
#[ignore]
fn not_ready_yet() { /* ... */ }

#[axtest]
#[ignore = "requires hardware device X"]
fn hw_dependent() { /* ... */ }
```

### Expected Failure

```rust
#[axtest]
#[should_panic]
fn expected_to_fail() {
    panic!("this is intentional");
}
```

### Custom Executor

Bind a test to a named executor (must be registered via `add_executor` in the entry point):

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

1. `#[axtest]` generates a `static AxTestDescriptor` in the `.axtest_array` linker section
2. The linker script collects all descriptors into a contiguous array
3. `axtest::init().run_tests()` reads the array and executes each test
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

The build tool injects `--cfg axtest` via `CARGO_ENCODED_RUSTFLAGS` when the test suite build config sets `AXTEST=y`:

```toml
# test-suit/<os>/axtest/qemu/build-aarch64-unknown-none-softfloat.toml
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"

[env]
AXTEST = "y"
```

QEMU configs use `success_regex` / `fail_regex` to match the output:

```toml
# test-suit/<os>/axtest/qemu/smoke/qemu-aarch64.toml
args = ["-nographic", "-cpu", "cortex-a72", "-machine", "virt,virtualization=on,gic-version=3", "-smp", "1", "-m", "128M"]
timeout = 60
success_regex = ["AXTEST_SUITE_OK"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b", "AXTEST_SUITE_FAIL", "AXTEST_CASE status=fail"]
to_bin = true
uefi = false
```

## Output Format

axtest emits KTAP-compatible output with additional sentinel lines for CI parsing:

| Line | Meaning |
|---|---|
| `AXTEST_BEGIN total=N` | Test session started, N tests discovered |
| `AXTEST_CASE status=pass\|fail\|skip module=M name=N` | Per-test result |
| `AXTEST_SUMMARY pass=P fail=F skip=S total=T` | Final summary |
