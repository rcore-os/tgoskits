# ax-sync

OS-independent synchronization interfaces for TGOSKits kernels and reusable
components.

## Primitives

- `SpinLock<T>` and `SpinRwLock<T>` select execution-context policy at each
  acquisition: ordinary methods disable preemption, `*_irqsave` methods also
  save and disable local interrupts, and `unsafe *_raw` methods leave context
  management to the caller.
- `Mutex<T>` is always a non-poisoning sleepable mutex. It is available only
  with the `sleep` feature and never aliases a spin lock.
- `PreemptGuard`, `IrqSaveGuard`, and `PreemptIrqSaveGuard` provide explicit
  critical-section guards.
- Lock metadata is fixed-layout wrapper state. The provider owns the single
  lock-class graph, held-lock stack, ordering checks, and diagnostics.

The crate declares runtime capabilities through `ax-crate-interface`.
ArceOS implements the provider in `ax-runtime`, which forwards every complete
transaction to the algorithms owned by `ax-task::sync`. Host tests must link a
test runtime provider through the same boundary; this crate contains no host
lock engine or fallback implementation.

## Features

- `sleep`: enable the sleepable mutex interface.
- `lock-api`: enable the IRQ-save raw mutex adapter required by `lock_api`.
- `host-test`: mark a host test composition; provider selection remains external.
- `axtest`: expose bare-metal coverage tests.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
