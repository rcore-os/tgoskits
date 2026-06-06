# x86_64 Native GDB MVP Technical Note

## Background

This change set targets a minimal but usable `x86_64` native GDB debugging flow on StarryOS.

The goal is not to implement full Linux ptrace compatibility. Instead, the goal is to support the smallest end-to-end workflow needed by a native CLI GDB MVP:

- debug a child process started by `run`
- observe the initial `execve` stop
- insert and hit software breakpoints
- read and write general-purpose registers
- continue execution
- single-step one user instruction
- support a non-interactive GDB batch workflow

## Implemented Capabilities

### 1. x86_64 ptrace register access

Implemented and validated:

- `PTRACE_GETREGS`
- `PTRACE_SETREGS`
- `PTRACE_GETREGSET`
- `PTRACE_SETREGSET`
- `PTRACE_GETSIGINFO`

This allows the tracer to inspect and update the stopped tracee state in a way that is sufficient for basic GDB workflows.

### 2. exec stop and traced stop visibility

Validated the ptrace stop chain on `x86_64`:

- `PTRACE_TRACEME`
- `waitpid(..., WUNTRACED)`
- initial `execve -> SIGTRAP` stop
- `PTRACE_CONT`
- signal suppression with `PTRACE_CONT(..., 0)`

This confirms that the control plane and the wait-visible event plane are both available for native GDB.

### 3. software breakpoint support

Validated:

- write `int3` (`0xCC`) into user code
- observe `SIGTRAP` on breakpoint hit
- restore original instruction byte
- continue execution successfully

This provides the minimum software-breakpoint capability needed by GDB.

### 4. x86_64 single-step support

Added the missing `x86_64` single-step path using the architecture-specific debug flow so that:

- `PTRACE_SINGLESTEP` executes one user instruction
- the tracee stops again with `SIGTRAP`
- the tracer can use this to implement a more realistic breakpoint recovery flow

### 5. more realistic breakpoint recovery path

Added and validated a test path that more closely matches real debugger behavior:

1. hit software breakpoint
2. move `RIP` back to the breakpoint address
3. restore the original byte
4. single-step the original instruction
5. reinsert the breakpoint
6. continue execution

This is a stronger guarantee than a test-only “jump past breakpoint and continue” shortcut.

### 6. native GDB batch-mode MVP

Added a StarryOS test case for non-interactive native GDB batch mode.

Current test strategy first isolates package-install behavior, then verifies the guest-side GDB workflow separately. This helps distinguish:

- guest package-install and resource issues
- ptrace semantic issues
- breakpoint/single-step semantic issues

## Tests

The following `x86_64` tests are used to validate the MVP chain:

- `test-ptrace-x86-regs`
- `test-ptrace-exec-stop`
- `test-ptrace-x86-breakpoint`
- `test-ptrace-x86-singlestep`
- `test-ptrace-x86-breakpoint-reinsert`
- `test-gdb-native-batch`

Together, these tests cover:

- traced stop visibility
- register access
- exec stop
- software breakpoint insertion
- breakpoint hit handling
- single-step
- breakpoint reinsertion
- non-interactive GDB consumption of the ptrace interface

## Cross-Architecture Build Hygiene

The x86_64-specific `#DB` / Trap Flag handling is explicitly gated behind `#[cfg(target_arch = "x86_64")]`.

This is necessary because:

- `ExceptionKind::Debug` is x86_64-specific
- `UserContext.rflags` is x86_64-specific

Without this gating, non-x86_64 Starry matrix builds fail at compile time.

## Known Limits

This MVP does **not** claim complete Linux ptrace coverage. The following are intentionally left out or not yet fully covered:

- `PTRACE_ATTACH`
- `PTRACE_SEIZE`
- `PTRACE_INTERRUPT`
- multi-threaded ptrace group-stop semantics
- `PTRACE_SYSCALL`
- `PTRACE_O_TRACEFORK/CLONE/EXEC`
- `/proc/<pid>/mem`
- hardware watchpoints
- floating-point register ptrace support
- interactive GDB TTY/readline polish

## Summary

This work establishes a practical first milestone:

- `x86_64`
- native CLI GDB
- child created by `run`
- software breakpoints
- register access
- continue
- single-step
- non-interactive batch-mode validation

In short, this is a usable `x86_64` native GDB MVP for StarryOS, not a full ptrace completion.
